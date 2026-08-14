from __future__ import annotations

import json
import hashlib
import os
import pathlib
import signal
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import unittest

TOOLS = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(TOOLS))

import evidence  # noqa: E402
import source_digest  # noqa: E402


BASE_INDEX_RAW = json.dumps(
    {
        "schemaVersion": 2,
        "manifests": [
            {
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "digest": "sha256:" + "a" * 64,
                "size": 100,
                "platform": {"architecture": "arm64", "os": "linux"},
            },
            {
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "digest": "sha256:" + "b" * 64,
                "size": 101,
                "platform": {"architecture": "amd64", "os": "linux"},
            },
        ],
        "mediaType": "application/vnd.oci.image.index.v1+json",
    },
    separators=(",", ":"),
    sort_keys=True,
)
BASE_IMAGE = (
    "docker.io/library/rust:1.88.0-bookworm@sha256:"
    + hashlib.sha256(BASE_INDEX_RAW.encode()).hexdigest()
)
IMAGE_ID = "sha256:" + "d" * 64
CONTAINER_ID = "e" * 64


FAKE_DOCKER_LAUNCHER = r"""
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static char *read_scenario(const char *path) {
    FILE *stream = fopen(path, "rb");
    if (stream == NULL) return NULL;
    char *value = calloc(1, 256);
    if (value == NULL) return NULL;
    size_t count = fread(value, 1, 255, stream);
    if (ferror(stream) || fclose(stream) != 0) return NULL;
    while (count > 0 && (value[count - 1] == '\n' || value[count - 1] == '\r')) {
        value[--count] = '\0';
    }
    return count > 0 ? value : NULL;
}

int main(int argc, char **argv) {
    char *scenario = read_scenario(PMUX_SCENARIO_FILE);
    if (scenario == NULL) return 91;
    if (setenv("PMUX_FAKE_DOCKER_STATE", PMUX_STATE_FILE, 1) != 0 ||
        setenv("PMUX_FAKE_DOCKER_SCENARIO", scenario, 1) != 0 ||
        setenv("PMUX_FAKE_DOCKER_MARKER", PMUX_MARKER_FILE, 1) != 0 ||
        setenv("PMUX_FAKE_BASE_IMAGE", PMUX_BASE_IMAGE, 1) != 0 ||
        setenv("PMUX_FAKE_BASE_INDEX_RAW", PMUX_BASE_INDEX_RAW, 1) != 0 ||
        setenv("PMUX_FAKE_SOURCE_DIGEST", PMUX_SOURCE_DIGEST, 1) != 0) return 92;
    if (setenv("PMUX_FAKE_BUILDX_PATH", PMUX_BUILDX_PATH, 1) != 0) return 92;
    char **child = calloc((size_t)argc + 2, sizeof(char *));
    if (child == NULL) return 93;
    child[0] = PMUX_PYTHON;
    child[1] = PMUX_SCRIPT;
    for (int index = 1; index < argc; ++index) child[index + 1] = argv[index];
    child[argc + 1] = NULL;
    execv(PMUX_PYTHON, child);
    fprintf(stderr, "fake Docker exec failed: %s\n", strerror(errno));
    return 94;
}
"""


class DockerfileClosureTests(unittest.TestCase):
    def test_snapshot_and_python_acquisition_are_exactly_hash_locked(self) -> None:
        dockerfile = (TOOLS / "Dockerfile").read_text(encoding="utf-8")
        requirements = (TOOLS / "python-requirements.txt").read_text(encoding="utf-8")
        self.assertIn("snapshot.debian.org/archive/debian/20250725T000000Z", dockerfile)
        self.assertIn(
            "snapshot.debian.org/archive/debian-security/20250725T000000Z",
            dockerfile,
        )
        for digest in (
            "919b6d130d8afa68a8680a24db6a09a9ccdc9226188b42079cd3a3d6fad028de",
            "ee3934d9fb7836e3bf303fad2c0b02d366020367fa4e0f4092dae51f82dd0425",
            "2cbfcb4744de07ab4aebbe19466d6de02065ce47846d2a8274a26f6e06b3e4ea",
        ):
            self.assertIn(f"{digest}  ", dockerfile)
        self.assertIn("--require-hashes", dockerfile)
        self.assertIn("--only-binary=:all:", dockerfile)
        self.assertIn("--no-deps", dockerfile)
        self.assertEqual(requirements.count("--hash=sha256:"), 5)
        self.assertIn(
            "e2438b6ee5a56701f219479b3bbd6b5c523ff779fa3de1c8d6fbadc4936d780a",
            dockerfile,
        )

    def test_runtime_source_and_candidate_are_root_owned_and_nonwritable(self) -> None:
        dockerfile = (TOOLS / "Dockerfile").read_text(encoding="utf-8")
        self.assertIn("COPY . /workspace", dockerfile)
        self.assertNotIn("COPY --chown=pmux:pmux . /workspace", dockerfile)
        self.assertNotIn("chown pmux:pmux /workspace", dockerfile)
        self.assertIn("-perm /022", dockerfile)
        self.assertIn("chown -R root:root /opt/pmux-candidate", dockerfile)
        self.assertIn(
            "chmod 0555 /opt/pmux-candidate /opt/pmux-candidate/bin", dockerfile
        )


FAKE_DOCKER = r"""#!/usr/bin/env python3
import json
import os
import pathlib
import sys
import time

state_path = pathlib.Path(os.environ["PMUX_FAKE_DOCKER_STATE"])
scenario = os.environ["PMUX_FAKE_DOCKER_SCENARIO"]
marker = pathlib.Path(os.environ["PMUX_FAKE_DOCKER_MARKER"])
base_image = os.environ["PMUX_FAKE_BASE_IMAGE"]
image_id = "sha256:" + "d" * 64
container_id = "e" * 64
builder_node_id = "f" * 64

try:
    state = json.loads(state_path.read_text(encoding="utf-8"))
except FileNotFoundError:
    state = {"calls": [], "builders": {}, "containers": {}, "images": {}}

arguments = sys.argv[1:]
state["calls"].append(arguments)

def save():
    temporary = state_path.with_suffix(".tmp")
    temporary.write_text(json.dumps(state, sort_keys=True), encoding="utf-8")
    temporary.replace(state_path)

def finish(status=0, output=""):
    save()
    if output:
        print(output)
    raise SystemExit(status)

def signal_edge():
    marker.write_text("ready", encoding="utf-8")
    save()
    time.sleep(0.5)

if arguments == ["version"]:
    finish(0, "docker-test")

if arguments == ["buildx", "version"]:
    finish(0, "buildx-test")

if arguments == ["info", "--format", "{{json .ClientInfo.Plugins}}"]:
    finish(0, json.dumps([{"Name": "buildx", "Path": os.environ["PMUX_FAKE_BUILDX_PATH"], "Version": "test"}], separators=(",", ":")))

if arguments[:3] == ["buildx", "imagetools", "inspect"]:
    state["calls"][-1] = arguments
    save()
    sys.stdout.write(os.environ["PMUX_FAKE_BASE_INDEX_RAW"])
    raise SystemExit(0)

if arguments[:2] == ["buildx", "inspect"]:
    bootstrap = "--bootstrap" in arguments
    name = arguments[-1]
    if scenario == "preexisting_builder" and not bootstrap:
        state["builders"][name] = "preexisting"
    if name not in state["builders"]:
        finish(1)
    if bootstrap:
        state["containers"][f"buildx_buildkit_{name}0"] = builder_node_id
    finish(0, f"Name: {name}\nPlatforms: linux/arm64, linux/amd64")

if arguments[:2] == ["buildx", "ls"]:
    finish(0, "\n".join(state["builders"]))

if arguments[:2] == ["container", "inspect"]:
    name = arguments[-1]
    if scenario == "preexisting_container" and name.startswith("pmux-linux-arm64-"):
        state["containers"][name] = container_id
    if name not in state["containers"]:
        finish(1)
    finish(0, state["containers"][name])

if arguments[:2] == ["container", "ls"]:
    finish(0, "\n".join(state["containers"]))

if arguments[:2] == ["image", "inspect"]:
    target = arguments[-1]
    if scenario == "preexisting_image" and target.startswith("pmux-linux-deterministic:"):
        state["images"][target] = image_id
    matching = None
    if target in state["images"]:
        matching = state["images"][target]
    elif target in state["images"].values():
        matching = target
    if matching is None:
        finish(1)
    if "--format" not in arguments:
        finish()
    template = arguments[arguments.index("--format") + 1]
    if template == "{{.Id}}":
        finish(0, matching)
    if "source-sha256" in template:
        finish(0, os.environ["PMUX_FAKE_SOURCE_DIGEST"])
    if template == "{{.Architecture}}":
        finish(0, "arm64")
    if "base-image" in template:
        finish(0, base_image)
    finish(1)

if arguments[:2] == ["image", "ls"]:
    finish(0, "\n".join(state["images"]))

if arguments[:2] == ["buildx", "create"]:
    name = arguments[arguments.index("--name") + 1]
    if scenario in {"raced_builder_failure", "signal_builder"}:
        state["builders"][name] = "raced"
        if scenario == "signal_builder":
            signal_edge()
        finish(41)
    state["builders"][name] = "owned"
    finish(0, name)

if arguments[:2] == ["buildx", "build"]:
    tag = arguments[arguments.index("--tag") + 1]
    iid_file = pathlib.Path(arguments[arguments.index("--iidfile") + 1])
    if scenario in {"raced_image_failure", "signal_image"}:
        state["images"][tag] = image_id
        if scenario == "signal_image":
            signal_edge()
        finish(42)
    iid_file.write_text(image_id + "\n", encoding="ascii")
    state["images"][tag] = image_id
    if scenario == "shared_image_id":
        state["images"]["unrelated:keep"] = image_id
    finish()

if arguments[:2] == ["buildx", "rm"]:
    name = arguments[-1]
    state["builders"].pop(name, None)
    state["containers"].pop(f"buildx_buildkit_{name}0", None)
    finish()

if arguments and arguments[0] == "create":
    name = arguments[arguments.index("--name") + 1]
    if scenario in {"raced_container_failure", "signal_container"}:
        state["containers"][name] = container_id
        if scenario == "signal_container":
            signal_edge()
        finish(43)
    state["containers"][name] = container_id
    finish(0, container_id)

if arguments[:2] == ["image", "rm"]:
    target = arguments[-1]
    if target in state["images"]:
        state["images"].pop(target)
    else:
        state["images"] = {
            name: identity
            for name, identity in state["images"].items()
            if identity != target
        }
    finish()

if arguments and arguments[0] == "rm":
    target = arguments[-1]
    state["containers"] = {
        name: identity
        for name, identity in state["containers"].items()
        if identity != target
    }
    finish()

finish(97)
"""


class DockerOwnershipTests(unittest.TestCase):
    def initialize_fixture_repository(self, workspace: pathlib.Path) -> None:
        """Build a hermetic one-commit Git repository inside the fixture workspace.

        ``run.sh`` runs ``source_digest.py --revision-capture``, whose first
        bounded query is ``rev-parse --verify HEAD^{commit}``; any stderr from a
        bounded Git query is fatal. A bare ``git init`` is therefore not enough
        -- a commitless repository makes that query write to stderr and abort the
        capture -- so the fixture needs at least one commit.

        Hermetic by construction: the repository lives in this test's temporary
        directory (never the developer's own clone), no command touches the
        network, and the ambient user/system Git configuration is disabled while
        author and committer identity is supplied explicitly, so this works on a
        host that has no global Git identity configured at all.
        """

        git = shutil.which("git")
        if git is None:
            self.skipTest("Git is required for Docker runner tests")
        identity = "pmux fixture"
        address = "fixture@pmux.invalid"
        environment = {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            # A directory that is never created, so an older Git that ignores
            # GIT_CONFIG_GLOBAL still cannot reach the developer's ~/.gitconfig.
            "HOME": str(self.root / "absent-fixture-home"),
            "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_CONFIG_SYSTEM": os.devnull,
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_ATTR_NOSYSTEM": "1",
            "GIT_TERMINAL_PROMPT": "0",
            "GIT_AUTHOR_NAME": identity,
            "GIT_AUTHOR_EMAIL": address,
            "GIT_COMMITTER_NAME": identity,
            "GIT_COMMITTER_EMAIL": address,
            "LANG": "C",
            "LC_ALL": "C",
        }
        commands = (
            [git, "-c", "init.defaultBranch=main", "init", "--quiet", str(workspace)],
            [git, "-C", str(workspace), "config", "user.email", address],
            [git, "-C", str(workspace), "config", "user.name", identity],
            [git, "-C", str(workspace), "add", "--all"],
            [
                git,
                "-C",
                str(workspace),
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "--no-verify",
                "--message",
                "fixture workspace",
            ],
        )
        for command in commands:
            subprocess.run(
                command,
                check=True,
                timeout=60,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                env=environment,
            )
        head = subprocess.run(
            [git, "-C", str(workspace), "rev-parse", "--verify", "HEAD^{commit}"],
            check=True,
            timeout=60,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=environment,
        )
        self.assertEqual(head.stderr, "")
        self.assertRegex(head.stdout.strip(), r"\A[0-9a-f]{40}(?:[0-9a-f]{24})?\Z")

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temporary.name).resolve()
        self.binary_directory = self.root / "bin"
        self.binary_directory.mkdir()
        self.workspace = self.root / "workspace"
        isolated_tools = self.workspace / "tools" / "linux-docker"
        isolated_tools.mkdir(parents=True)
        for name in (
            "Dockerfile",
            "bounded_runner.py",
            "evidence.py",
            "run.sh",
            "source_digest.py",
        ):
            shutil.copy2(TOOLS / name, isolated_tools / name)
        shared_tools = self.workspace / "tools" / "evidence_common"
        shared_tools.mkdir()
        shutil.copy2(
            TOOLS.parent / "evidence_common" / "bounded_process.py",
            shared_tools / "bounded_process.py",
        )
        shutil.copy2(
            TOOLS.parent / "evidence_common" / "managed_process.py",
            shared_tools / "managed_process.py",
        )
        self.initialize_fixture_repository(self.workspace)
        self.runner = isolated_tools / "run.sh"
        self.state = self.root / "docker-state.json"
        self.marker = self.root / "phase-marker"
        self.docker_socket_path = self.root / "docker.sock"
        self.docker_socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.docker_socket.bind(str(self.docker_socket_path))
        self.docker_socket.listen(1)
        self.scenario_file = self.root / "scenario"
        fake_script = self.root / "fake-docker.py"
        fake_script.write_text(FAKE_DOCKER, encoding="utf-8")
        self.digest, _ = source_digest.workspace_source_digest(self.workspace)
        docker = self.binary_directory / "docker"
        compiler = shutil.which("cc")
        if compiler is None:
            self.skipTest("a native C compiler is required for Docker runner tests")
        launcher_source = self.root / "fake-docker.c"
        launcher_source.write_text(FAKE_DOCKER_LAUNCHER, encoding="utf-8")
        definitions = {
            "PMUX_SCENARIO_FILE": str(self.scenario_file),
            "PMUX_STATE_FILE": str(self.state),
            "PMUX_MARKER_FILE": str(self.marker),
            "PMUX_BASE_IMAGE": BASE_IMAGE,
            "PMUX_BASE_INDEX_RAW": BASE_INDEX_RAW,
            "PMUX_SOURCE_DIGEST": self.digest,
            "PMUX_PYTHON": str(pathlib.Path(sys.executable).resolve(strict=True)),
            "PMUX_SCRIPT": str(fake_script),
            "PMUX_BUILDX_PATH": str(docker),
        }
        command = [compiler, "-std=c11", "-O0", "-o", str(docker)]
        command.extend(
            f"-D{name}={json.dumps(value)}" for name, value in definitions.items()
        )
        command.append(str(launcher_source))
        subprocess.run(command, check=True, timeout=30)

    def tearDown(self) -> None:
        self.docker_socket.close()
        self.temporary.cleanup()

    def environment(self, scenario: str) -> dict[str, str]:
        environment = os.environ.copy()
        environment.update(
            {
                "PATH": f"{self.binary_directory}:{environment['PATH']}",
                "PYTHONDONTWRITEBYTECODE": "1",
                "PMUX_FAKE_DOCKER_STATE": str(self.state),
                "PMUX_FAKE_DOCKER_SCENARIO": scenario,
                "PMUX_FAKE_DOCKER_MARKER": str(self.marker),
                "PMUX_FAKE_BASE_IMAGE": BASE_IMAGE,
                "PMUX_FAKE_BASE_INDEX_RAW": BASE_INDEX_RAW,
                "PMUX_FAKE_SOURCE_DIGEST": self.digest,
            }
        )
        return environment

    def command(self, output: pathlib.Path) -> list[str]:
        return [
            "bash",
            str(self.runner),
            "--source-sha256",
            self.digest,
            "--base-image",
            BASE_IMAGE,
            "--acknowledge-docker",
            "--docker-host",
            f"unix://{self.docker_socket_path}",
            "--platform",
            "arm64",
            "--output",
            str(output),
        ]

    def invoke(
        self, scenario: str
    ) -> tuple[subprocess.CompletedProcess[str], pathlib.Path]:
        self.scenario_file.write_text(scenario + "\n", encoding="ascii")
        self.digest, _ = source_digest.workspace_source_digest(self.workspace)
        output = self.root / f"evidence-{scenario}"
        result = subprocess.run(
            self.command(output),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=30,
            check=False,
            env=self.environment(scenario),
        )
        return result, output

    def calls(self) -> list[list[str]]:
        return json.loads(self.state.read_text(encoding="utf-8"))["calls"]

    def ledger_states(self, output: pathlib.Path) -> list[dict[str, object]]:
        ledger = output / "docker-resource-ledger.ndjson"
        payload = ledger.read_bytes()
        evidence._validate_ledger_records(payload)
        records = [
            evidence.strict_json_loads(
                line, description="test Docker resource-ledger record"
            )
            for line in payload.splitlines()
        ]
        return [record["payload"] for record in records]

    def assert_final_tree_is_exact(self, output: pathlib.Path) -> None:
        manifest = evidence.load_json(output / "host-evidence-tree-final.json")
        self.assertEqual(
            evidence.verify_regular_tree_manifest(output, manifest), manifest
        )

    def assert_no_removal(self, kind: str) -> None:
        calls = self.calls()
        if kind == "builder":
            self.assertFalse(any(call[:2] == ["buildx", "rm"] for call in calls))
        elif kind == "container":
            self.assertFalse(any(call and call[0] == "rm" for call in calls))
        else:
            self.assertFalse(any(call[:2] == ["image", "rm"] for call in calls))

    def test_preexisting_planned_objects_are_never_adopted_or_removed(self) -> None:
        for scenario, kind in (
            ("preexisting_builder", "builder"),
            ("preexisting_container", "container"),
            ("preexisting_image", "image"),
        ):
            with self.subTest(scenario=scenario):
                self.state.unlink(missing_ok=True)
                result, output = self.invoke(scenario)
                self.assertNotEqual(result.returncode, 0)
                self.assertTrue(self.state.exists(), result.stderr)
                self.assert_no_removal(kind)
                self.assert_final_tree_is_exact(output)

    def test_failed_raced_creations_never_grant_cleanup_authority(self) -> None:
        for scenario, kind in (
            ("raced_builder_failure", "builder"),
            ("raced_image_failure", "image"),
            ("raced_container_failure", "container"),
        ):
            with self.subTest(scenario=scenario):
                self.state.unlink(missing_ok=True)
                result, output = self.invoke(scenario)
                self.assertNotEqual(result.returncode, 0)
                self.assertTrue(self.state.exists(), result.stderr)
                self.assert_no_removal(kind)
                states = self.ledger_states(output)
                self.assertTrue(
                    any(
                        row["kind"] == kind and row["state"] == "ownership_unconfirmed"
                        for row in states
                    )
                )
                cleanup = evidence.load_json(output / "host-docker-cleanup.json")
                self.assertFalse(cleanup["exact_cleanup"])
                self.assertEqual(cleanup[f"unconfirmed_{kind}s"], 1)
                self.assert_final_tree_is_exact(output)

    def test_signals_during_ambiguous_creation_never_remove_the_raced_object(
        self,
    ) -> None:
        for scenario, kind in (
            ("signal_builder", "builder"),
            ("signal_image", "image"),
            ("signal_container", "container"),
        ):
            with self.subTest(scenario=scenario):
                self.state.unlink(missing_ok=True)
                self.marker.unlink(missing_ok=True)
                self.scenario_file.write_text(scenario + "\n", encoding="ascii")
                self.digest, _ = source_digest.workspace_source_digest(self.workspace)
                output = self.root / f"evidence-{scenario}"
                process = subprocess.Popen(
                    self.command(output),
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    text=True,
                    env=self.environment(scenario),
                )
                deadline = time.monotonic() + 20
                while not self.marker.exists() and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertTrue(
                    self.marker.exists(), "fake Docker did not reach signal edge"
                )
                process.send_signal(signal.SIGTERM)
                process.communicate(timeout=30)
                self.assertNotEqual(process.returncode, 0)
                self.assert_no_removal(kind)
                states = self.ledger_states(output)
                self.assertTrue(
                    any(
                        row["kind"] == kind and row["state"] == "ownership_unconfirmed"
                        for row in states
                    )
                )
                cleanup = evidence.load_json(output / "host-docker-cleanup.json")
                self.assertFalse(cleanup["exact_cleanup"])
                self.assertEqual(cleanup[f"unconfirmed_{kind}s"], 1)
                self.assert_final_tree_is_exact(output)

    def test_cleanup_removes_only_owned_tag_when_content_id_is_shared(self) -> None:
        result, output = self.invoke("shared_image_id")
        self.assertNotEqual(result.returncode, 0)
        state = json.loads(self.state.read_text(encoding="utf-8"))
        self.assertEqual(state["images"], {"unrelated:keep": IMAGE_ID})
        removal_calls = [call for call in state["calls"] if call[:2] == ["image", "rm"]]
        self.assertEqual(len(removal_calls), 1)
        self.assertTrue(
            removal_calls[0][-1].startswith("pmux-linux-deterministic:arm64-")
        )
        self.assertNotEqual(removal_calls[0][-1], IMAGE_ID)
        states = self.ledger_states(output)
        self.assertTrue(
            any(row["kind"] == "image" and row["state"] == "removed" for row in states)
        )
        self.assert_final_tree_is_exact(output)

    def test_hup_and_quit_are_forwarded_without_granting_cleanup_authority(
        self,
    ) -> None:
        for signal_number in (signal.SIGHUP, signal.SIGQUIT):
            with self.subTest(signal_number=signal_number):
                self.state.unlink(missing_ok=True)
                self.marker.unlink(missing_ok=True)
                scenario = "signal_builder"
                self.scenario_file.write_text(scenario + "\n", encoding="ascii")
                self.digest, _ = source_digest.workspace_source_digest(self.workspace)
                output = self.root / f"evidence-{scenario}-{signal_number}"
                process = subprocess.Popen(
                    self.command(output),
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    text=True,
                    env=self.environment(scenario),
                )
                deadline = time.monotonic() + 20
                while not self.marker.exists() and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertTrue(self.marker.exists())
                process.send_signal(signal_number)
                process.communicate(timeout=30)
                self.assertNotEqual(process.returncode, 0)
                self.assert_no_removal("builder")
                self.assert_final_tree_is_exact(output)


if __name__ == "__main__":
    unittest.main()
