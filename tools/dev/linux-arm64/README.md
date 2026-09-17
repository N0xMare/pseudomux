# The linux/aarch64 measurement lane

A native `linux/arm64` container, driven from an Apple Silicon Darwin host, in
which the `linux`/`aarch64` compatibility cell is measured and promoted. It is
the lane behind `evidence/pooled-transcript-drain-linux-aarch64.json`,
`evidence/promoted-profile-2.1.272-linux-aarch64.json` and
`evidence/promotion-2.1.272-linux-aarch64.json`.

## What it measures, and why a container is admissible here

`os` and `arch` in a promoted cell come from `std::env::consts::OS`/`ARCH` —
compile-time constants of the `pmuxd` binary. This lane builds `pmuxd` inside
an `arm64` Debian image, so it reports `linux`/`aarch64`, which is what it is.
Nothing is emulated: Docker Desktop's daemon on this host is a native `arm64`
daemon (`docker version --format '{{.Server.Arch}}'` prints `arm64`), so the
CPU under the measurement is the host's own silicon and the timing the drain
is sensitive to is real. `run.sh preflight` refuses to run when the daemon
reports anything else.

`docs/engineering/tart-linux-guest.md` §3–§4 says a container on this Mac is
"a linux dev-loop mirror, never a promotion path". Its argument is about pmux
**lying**: an `x86_64-unknown-linux-gnu` build under Rosetta reports
`linux`/`x86_64`, matches the shipped promoted linux cell, and silently applies
a 250 ms drain measured on a 128-core x86 host to a translated one. That
remains refused, and this lane cannot do it: it builds and measures the
`aarch64` triple only, under an identity no shipped cell claimed before this
one. **The receipts record that they were taken in a container on a Darwin
host**, which is the fact a later reader needs.

What it does not establish: anything about `linux`/`x86_64`, anything about a
non-container `linux`/`aarch64` host, and anything outside a minified cell
beyond what `living_pmux_run.py`'s ladder covers.

## The Claude Code binaries

Two official **linux-arm64 (glibc)** builds are installed at the path every
tool in `tools/dev` expects,
`$HOME/.local/share/pmux/claude/<version>/claude`. Each comes from the npm
platform package `@anthropic-ai/claude-code-linux-arm64`, and
`install-claude.sh` verifies the registry's own `dist.integrity` sha512 over
the downloaded bytes *and* a pinned sha256 of the extracted executable before
either is installed. `package/claude` is a native ELF; no node runtime is
installed in the image.

| version | executable sha256 |
| ------- | ----------------- |
| 2.1.258 | `43dc490af55262edcb3e9b1cb315de22cc09ccb08bd52a4c39bc5eabaa63100f` |
| 2.1.272 | `214a90efdd16ee0ea81132ffecced588dba394d178cc494f285ba04b5288c8de` |

`run.sh versions` prints both digests and both `claude --version` lines from
inside the container.

**Two versions, because one cannot be promoted.**
`compatibility.rs::every_promoted_drain_is_the_pooled_bound_and_not_a_per_version_fit`
requires the pooled receipt to name at least two Claude versions, each with a
reachable arrival of its own — "a pooled bound over one version is a
per-version fit wearing a different noun". 2.1.258 is the version macos pools
with 2.1.272, so this cell pools the same pair. The promoted range is still
`--floor 2.1.272` through 2.1.272; pooling a version below the floor can only
raise the bound, which is the conservative direction.

## Credentials

The pool authenticates from the Claude Code file credential store on the host,
bind-mounted **read-write** at `/home/pmux/.claude` inside the container. Its
host path is derived from the checkout's own location,

    <parent of this checkout>/plak/.plak/harbor/claude-linux-securestorage

and `PMUX_LANE_STORE` overrides it. The container path is the unsuffixed store — the `--pool-securestorage-dir empty`
default (`docs/spec/02-operator.md`) — which matters because `drain_n50.py` and
`living_pmux_run.py` expose no pin, so the unsuffixed location is the one
mount that serves the whole chain. It is read-write because a refreshed OAuth
token has to be written back or the next run starts logged out.

It is a bind mount and nothing else. It is never copied into the image, never
baked into a layer, never passed on argv, never printed, and never written to
a receipt. Transcripts do not land there: `pmuxd` replaces `CLAUDE_CONFIG_DIR`
with its own private isolation root inside the run's sandbox, and only the
credential file is read from the store.

**Rotating the pin.** If the store expires (`pmux doctor` reports
`needs_login`, or a turn comes back with a login screen), log in again on a
Linux host with `CLAUDE_CONFIG_DIR` pointed at a scratch directory and copy
the resulting `.credentials.json` into the store directory above, or re-run
whatever provisioned it. A macOS login cannot be reused: on Darwin the
credential lives in the login keychain, not in a file. Do not commit anything
from that directory and do not move it inside the checkout.

## The corpus

A campaign's transcripts are the daemon's Path B evidence mirror, which lives
inside the run's sandbox and is deleted with it. `drain_n50.py --corpus-out`
copies the `*.jsonl` out first, into `/corpus/<version>` in the container,
which `run.sh` bind-mounts from `~/.local/state/pmux-linux-arm64-corpus` on
the host. That is what makes a pooled bound over two versions measurable at
all.

The corpus is host-local and **is not committed**: it carries real prompts.
The receipt is the durable artifact, exactly as
`measure_transcript_drain.py`'s own `corpus.note` says.

## Running it

    tools/dev/linux-arm64/run.sh build
    tools/dev/linux-arm64/run.sh preflight

    # 50 real turns each; each keeps its corpus under /corpus/<version>.
    tools/dev/linux-arm64/run.sh drain 2.1.258
    tools/dev/linux-arm64/run.sh drain 2.1.272

    # No turns. The pooled bound, whole, over both corpora.
    tools/dev/linux-arm64/run.sh pool 500 2.1.258 2.1.272

    # No turns. The floor's own single-version receipt.
    tools/dev/linux-arm64/run.sh floor-receipt 2.1.272

    # Pin confirmation and the Full-cell ladder.
    tools/dev/linux-arm64/run.sh operator-eval 2.1.272
    tools/dev/linux-arm64/run.sh living-run 2.1.272

    # Drops --tested-claude-profile for linux/aarch64.
    tools/dev/linux-arm64/run.sh promote 2.1.272 --floor 2.1.272

    tools/dev/linux-arm64/run.sh shell

`pool` and `floor-receipt` are redirected on the **host**, so the receipt is
created by the host user in the host's tree. Everything else writes into
`/src/evidence` through the bind mount as the host user's uid (passed in by
`run.sh` at build and run time), and the container chowns that directory back
to the host uid and gid as it exits.

`promote` does not edit `crates/service/src/compatibility.rs`: the engine
reads that file (to find the shipped floor and to check that every
`RepromotionTrigger` still names a detector) and writes only its own receipt.
The `PROMOTED_PROFILES` row is transcribed by hand from the receipt's
generated `range_provenance` and from the pooled receipt's own numbers.

## Environment overrides

| variable | default |
| -------- | ------- |
| `PMUX_LANE_IMAGE` | `pmux-linux-arm64:dev` |
| `PMUX_LANE_STORE` | the harbor Linux credential store above |
| `PMUX_LANE_CORPUS_HOST` | `~/.local/state/pmux-linux-arm64-corpus` |
