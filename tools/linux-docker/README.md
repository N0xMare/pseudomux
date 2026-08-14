# Deterministic Linux Docker portability evidence

This directory is an acquisition and evidence envelope for pmux's tracked test
suite. It does not parse Claude transcripts, emulate the pmux actor, or decide
whether a turn is semantically correct. Those contracts belong to production
Rust and the L0-L5 tests listed in `docs/testing.md`. In particular, the former
Python fake-Claude/state-machine black box has been removed; the exact-release
`pseudomux-e2e` Rust test owns the shipped full-stack behavior.

The runner is used only after Gate A and macOS promotion have frozen one exact
canonical source digest. It builds one Linux image per requested architecture,
then executes without network access, source/config mounts, provider
credentials, or a real `claude` executable. A Docker result is deterministic
portability evidence, not credentialed native-Linux Claude support.

## Invocation

Compute the frozen digest with the same fail-closed implementation used inside
the image:

```bash
SOURCE_SHA256="$(tools/linux-docker/source_digest.py "$PWD")"
```

After independently confirming that this is the reviewed frozen digest, run
both cells with an independently reviewed multi-architecture Rust base-image
index digest and explicit consent for the runner's scoped Docker mutations:

```bash
tools/linux-docker/run.sh \
  --source-sha256 "$SOURCE_SHA256" \
  --base-image docker.io/library/rust:1.88.0-bookworm@sha256:<multiarch-index> \
  --acknowledge-docker \
  --platform all
```

An explicit, empty evidence destination can be supplied:

```bash
tools/linux-docker/run.sh \
  --source-sha256 "$SOURCE_SHA256" \
  --base-image docker.io/library/rust:1.88.0-bookworm@sha256:<multiarch-index> \
  --acknowledge-docker \
  --platform arm64 \
  --output "$PWD/.context/linux-docker/manual-arm64"
```

`--source-sha256` and a digest-qualified `--base-image` are mandatory: the tool
refuses to choose or silently update either identity. The base digest must be a
multi-architecture index containing exact arm64 and amd64 manifests; the
runner records the raw index and verifies both requested platform mappings.
`--acknowledge-docker` authorizes creation and exact removal of only the names
reserved in that run's private resource ledger. It does not authorize a prune,
removal by wildcard, reuse of a pre-existing object, publishing, or real-Claude
usage.

## Frozen-source and binary binding

`source_digest.py` hashes the declared Docker source context using canonical
relative path, permission mode, size, and content SHA-256. It rejects unknown
top-level inputs, symlinks, special files, and files that change during the
read. The host captures this full manifest before and after all cells. The
Dockerfile checks the same expected digest immediately after `COPY`, and each
offline container recomputes it before and after its gates.

Portable source identity is deliberately separate from host Git provenance.
The host brackets the complete run with bounded, receipted Git queries and
binds the workspace `.git` entry, Git/common directories, `HEAD`, loose or
packed refs, the main index, repository and worktree configuration,
`info/exclude`, `info/attributes`, and the worktree sparse-checkout file.
System/global configuration and per-user excludes/attributes are neutralized;
repository/worktree config that imports an external include is rejected.
Split-index mode is also rejected before any query because ordinary `git
status` can legitimately rewrite its shared backing index during observation,
preventing one immutable before/after control identity. Sparse-checkout is
supported because its configuration, pattern file, and main index are all
bound. Each cell consumes the exact before capture, after capture, and derived
stability record only after the run-level after capture succeeds.

The image prebuilds, and the offline suite rebuilds, `--release --bins`. Before
the Rust full-stack test runs, `evidence.py` captures the exact canonical path,
size, mode, device/inode, owner, and SHA-256 of all eight exercised candidate
executables:

- `pmux`
- `pmuxd`
- `pmux-rmuxd`
- `pmux-launcher`
- `pmux-hook`
- `pmux-mcp`
- `claude-p`
- `pmux-test-claude`

Missing files, symlinks, aliases, non-executable modes, paths outside the exact
release directory, or any mutation before the final check fail the cell. The
host binding report requires identical host/container source manifests, exact
stable host-revision captures, an unchanged complete binary manifest, the
requested architecture, a credential-free system report, and a passing suite
result.

## Offline Gate A coverage

Image construction is the only networked phase. It installs locked Rust 1.88,
the pinned nightly used by the tracked fuzz runner, cargo-fuzz 0.13.2, Node,
ShellCheck, and Python packaging/lint tools, then prefetches and compiles their
dependency graphs. The created test container has `--network none` and runs as
uid 10001 with a zero effective capability mask after the one root-supervised
permission probe.

The digest-qualified Rust base image and the exact Debian package versions
resolved during the build are recorded in evidence. Debian repositories are
not snapshot-pinned, so the runner claims verified inputs and resolution—not
byte-for-byte rebuild reproducibility across repository time. The Docker
daemon's bundled frontend is used instead of a mutable external Dockerfile
frontend; its behavior is retained in the Buildx logs but is not independently
content-addressed.

The ordered suite runs the platform-applicable `docs/testing.md` manifest:

- Rust formatting, locked all-target/all-feature check, strict Clippy,
  rustdoc warnings, and ordinary tests for both the root workspace and the
  intentionally excluded standalone vendored `rmux-client` and `rmux-server`
  crates. The server lane check/tests use pmux's exact `--no-default-features`
  product configuration. All-feature rustdoc denies warnings; Clippy denies
  every warning except the immutable upstream `collapsible_else_if` and
  `uninlined_format_args` style debt, with the provenance gate preventing
  those exact category allowances from hiding a broader source change;
- TypeScript typecheck/tests and Python tests/Ruff lint+format checks, plus actual offline npm
  tarball and Python wheel construction, isolated installation, public API/type
  import, archive hashing, and exact temporary cleanup without publishing;
- package-smoke, Phase-0, Linux evidence-tool, and fail-closed residue-scanner
  self-tests plus Bash syntax and ShellCheck gates over both Gate A scripts;
- the exact bounded transcript, actor, and client property/model gates and the
  production fuzz runner with its pinned toolchain and 50,000 runs per target;
- serialized native-service/private-rmux PTY, owner-loss, lifecycle-fault,
  concurrency, resource, 24-cycle soak, and release size-scaling gates;
- the exact-release Rust full-stack E2E plus every CLI, MCP, facade, launcher,
  hook, sidecar, and daemon process black box; and
- release-directory native-service/lifecycle/resource/soak reruns wherever the
  tracked test support accepts an explicit candidate directory, followed by a
  read-only exact-candidate/cache/socket/test-runtime residue audit.

The published `rmux-server` package is not a closed integration-test artifact:
Windows targets and source-ledger integration tests refer to repository-only
files omitted from the crates.io archive. Consequently `cargo fmt --all` cannot
enumerate the standalone package, and `cargo test --all-targets` eventually
tries to read paths that do not exist. The lane compiles every target, executes
the complete patch-owned EOF/control regression set by filtering the library
tests to the `pane_io::tests` module the patch adds them to -- a module and not
a name list, because `--exact` against a name nobody wrote runs zero tests and
exits zero, so the fourteen per-lane `--exact` cells this replaces would have
skipped a fifteenth regression in silence -- under
the exact product feature set, and runs Rust 1.88 `rustfmt --check` directly
over `src/lib.rs` and `build.rs`,
which traverses the production module tree and internal regressions. The
offline provenance regression fixes every other published file, while pmux's
actual-sidecar process suite owns the shipped attach boundary. The complete
upstream library sweep is diagnostic rather than promotional because one
unrelated hard-coded five-second lifecycle test demonstrated timing variance;
`docs/testing.md` records the exact observation and rationale.

The artifact `platform-exclusions.json` names the only claim exclusions:
real-Claude macOS promotion, native-host PTY timing/calibration, and future
credentialed native-Linux Claude support. Platform-specific `cfg` behavior is
still compiled/tested for the actual container target; unsupported v1 surfaces
remain closed by tracked rejection tests rather than being silently skipped.

## Isolation, cleanup, and evidence

The container keeps three disjoint planes. `/workspace` is the frozen source
and receives no Cargo, TypeScript, or fuzz output. The networked image build
creates exactly eight release executables in `/opt/pmux-candidate/bin`; that
owner-private directory is made non-writable before runtime and is captured
before the root-only probe. Every runtime compiler/test output instead goes to
the precreated owner-private `/var/tmp/pmux-linux-suite/validation` tree. A
fresh reproduction build is staged there and must match the frozen candidate's
exact names, modes, sizes, hashes, and bytes, while cross-plane inode and path
identity are deliberately not compared. Test harnesses execute the frozen
candidate through `PMUX_TEST_BIN_DIR`/`PMUX_E2E_BIN_DIR`; they never use the
candidate directory as `CARGO_TARGET_DIR`. Source and candidate identities are
revalidated after all gates, then the complete validation tree is removed.

The root supervisor retains only `CHOWN`, `DAC_READ_SEARCH`, `KILL`, `SETGID`,
and `SETUID` long enough to prove that the owner can ping the real release
pmuxd socket and uid 10002 cannot traverse/connect. Pmuxd runs in a dedicated
POSIX session. The probe verifies process/session, socket, runtime, and private
temporary-tree cleanup before product gates begin. Root never runs transcript,
PTY, package, fake-child, or full-stack tests.

Each cell reserves unique builder, image, and container names in an append-only
mode-0600 ledger before creation. Created objects receive exact identities;
after Buildx bootstrap, the builder is additionally fenced to its exact
BuildKit node-container ID (or a complete stable inspect-record digest on a
driver without that conventional node).
Pre-existing names, creation races, failed creates, or ambiguous receipts are
never adopted. Such a resource is recorded as `ownership_unconfirmed`, left
untouched, and fails the cell. Loaded images are additionally bound through the
Buildx IID file. Cleanup revalidates the tag-to-content-ID binding, then removes
only the unique per-cell tag; another tag sharing the same content ID remains
untouched. Containers are removed by their exact creation ID and builders by
their unique reserved name after revalidating the BuildKit node identity;
the EXIT trap repeats this scoped cleanup after ordinary failures or signals.
The runner never invokes Docker prune, broad list-driven cleanup, volumes, host
mounts, or unrelated containers/images/builders. A missing, replaced, or
unconfirmed object is a failed cleanup, not permission to remove something
else.

Evidence directories are mode 0700 and regular files mode 0600. Final JSON
manifests are atomically published and fsynced; resource-ledger records are
locked, bounded, append-only, and fsynced. Copied trees reject symlinks and
special files before their private modes are revalidated. Logs are diagnostic;
only the structured source, binary, system, result, binding, and cleanup
records establish a portability verdict.

Run the tooling-only checks without Docker:

```bash
bash -n scripts/pmuxd-run.sh scripts/gate-a-fuzz.sh scripts/gate-a-residue.sh \
  tools/linux-docker/run.sh tools/linux-docker/inside.sh tools/linux-docker/suite.sh
shellcheck scripts/pmuxd-run.sh scripts/gate-a-fuzz.sh scripts/gate-a-residue.sh \
  tools/linux-docker/run.sh tools/linux-docker/inside.sh tools/linux-docker/suite.sh
scripts/gate-a-residue.sh --self-test-disappearing-temp-root
PYTHONDONTWRITEBYTECODE=1 \
  python3 -m unittest discover -s tools/evidence_common/tests -v
PYTHONDONTWRITEBYTECODE=1 \
  python3 -m unittest discover -s tools/package-smoke/tests -v
PYTHONDONTWRITEBYTECODE=1 \
  python3 -m unittest discover -s tools/linux-docker/tests -v
python3 -m ruff check --no-cache \
  tools/evidence_common tools/package-smoke tools/linux-docker
python3 -m ruff format --check --no-cache \
  tools/evidence_common tools/package-smoke tools/linux-docker
```

These tests exercise digest/path/mode races, credential and consent guards,
atomic artifact publication, complete release-binary binding, platform
parsing, exact cleanup planning, and early runner failures. They never call
Docker or Claude.
