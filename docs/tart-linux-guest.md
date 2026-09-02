# Tart as a Linux guest beside the native macOS battery

**Written 2026-09-01.** A feasibility note, not a lane: nothing here is installed or run. It answers one question the owner asked: can a Linux guest on this MacBook Pro verify the linux cell concurrently with the native macos battery, so both cells are checked before one commit?


## 0. Which repo

The owner's `openai/tart` is correct **as of 2026**, and `cirruslabs/tart` is the same
project's older home. Cirrus Labs announced on 2026-04-07 that it is joining OpenAI's
Agent Infrastructure team, that it would "relicense all of our source-available tools"
and that "we have also stopped charging licensing fees for them"
(https://cirruslabs.org/). GitHub now serves the repository at
https://github.com/openai/tart (API `full_name: "openai/tart"`,
https://api.github.com/repos/openai/tart); `cirruslabs/tart` links still resolve, and
the docs site is still https://tart.run/. https://tart.run/licensing/ still lists the old tiers (free = 100 CPU cores, Gold
$12k/yr), so the dropped-fees claim rests on the announcement alone; for a single
personal Mac it was free either way.

## 1. Tart facts

- **What it is.** "Tart is a virtualization toolset to build, run and manage macOS and
  Linux virtual machines (VMs) on Apple Silicon", built on Apple's
  `Virtualization.Framework`, needing macOS 13+ (https://github.com/openai/tart).
- **Guest architecture: arm64 only.** `Virtualization.Framework` virtualizes, it does
  not emulate, so every guest is aarch64. Tart's Ubuntu images are arm64
  (`tart clone ghcr.io/cirruslabs/ubuntu:latest ubuntu`,
  https://tart.run/quick-start/); Cirrus's own amd64 images are Docker runner images,
  not Tart VM images. The x86-guest request is
  https://github.com/cirruslabs/tart/issues/964, closed. **There is no x86_64 Linux
  guest under Tart.**
- **Rosetta for Linux guests.** `tart run --rosetta=ROSETTA` exposes Rosetta to a Linux
  guest so x86_64 **binaries** run inside an arm64 guest; the Packer plugin documents
  the option as "Whether to enable Rosetta support of a Linux guest VM. Useful for
  running non-arm64 binaries in the guest VM"
  (https://developer.hashicorp.com/packer/integrations/cirruslabs/tart/latest/components/builder/tart).
  The guest must mount it (`mount -t virtiofs ROSETTA /mnt/rosetta`) and register a
  binfmt handler.
- **Directory sharing.** `tart run --dir=name:~/path`; on Linux guests mount it with
  `mount -t virtiofs com.apple.virtio-fs.automount /mnt/shared`, or via `/etc/fstab`
  (https://tart.run/quick-start/, https://www.scaleway.com/en/docs/tutorials/run-manage-linux-vm-on-apple-silicon-tart/).
- **Networking/SSH.** NAT by default; `ssh admin@$(tart ip <vm>)`, credentials
  `admin`/`admin` (https://tart.run/quick-start/).
- **Concurrency.** The two-VM ceiling is the macOS EULA's two-instance clause
  (VZErrorDomain code 6,
  https://eclecticlight.co/2022/08/04/virtualisation-on-apple-silicon-macs-8-how-apple-limits-vms/,
  https://github.com/cirruslabs/tart/discussions/1054); no Linux cap is documented or
  reported.
- **Nested virtualization.** M3/M4 + macOS 15 only, Linux guests only, `--nested`
  (https://tart.run/faq/). Not needed here.
- **Sizing.** `tart set <vm> --cpu N --memory MB --disk-size GB`.

## 2. Mapping to pseudomux

**How pmux decides the cell.** `os`/`arch` come from `std::env::consts::OS` / `ARCH`
(`crates/service/src/compatibility.rs:715-716,759-760,921-922`) — **compile-time
constants of the daemon binary**, which the repo already identified as a hazard in
defect 74 (`docs/defect-log.md:5080-5115`): a macOS-built daemon supervising a Linux
child "reports `os: "macos"` ... `tested: true` is published, and a
`transcript_drain_ms` measured [on] macos/aarch64 is applied to a cell nobody has
measured."

**Consequence A — an honest arm64 guest is a new cell.** Build pmuxd inside an arm64
Ubuntu guest and it reports `linux`/`aarch64`. `PROMOTED_PROFILES`
(`compatibility.rs:483-541`) holds exactly two cells: macos/aarch64 2.1.220..=2.1.258
and linux/x86_64 2.1.227..=2.1.257. Nothing admits linux/aarch64, so every `pmux run`
is refused unless you pass `--tested-claude-profile`. Dropping the flag needs
`evidence/pooled-transcript-drain-linux-aarch64.json`, which does not exist;
`tools/dev/promote.py` exits 2 without it, and a first promotion on a new OS/arch needs
`--floor` — "Do not pass another OS's floor" (`tools/dev/promote.py:8-9`,
`tools/dev/README.md:51-53`).

**Consequence B — Rosetta would make pmux lie.** An `x86_64-unknown-linux-gnu` build of
pmuxd running under Rosetta inside that guest reports `linux`/`x86_64` and **matches the
promoted linux cell**, silently applying a 250 ms drain measured on real x86 silicon
(128-core host, `evidence/promotion-2.1.257-linux-x86_64.json` `host`) to an emulated
one. That is defect 74's failure mode with the arrow reversed, and the emulated timing
is the one thing the drain is sensitive to: the whole linux number is "max reachable
post-answer transcript arrival 118 ms ... x2.0 ... = 250 ms", explicitly a *measured*
pooled bound with named invalidation triggers (`compatibility.rs:520-541`,
`docs/version-drift.md`). A translated measurement is not admissible as a promotion
receipt under those rules; it is admissible only as a smoke test. Turn latency and
`operator_eval.py` grades are likewise timing- and token-shaped.

**Other guest-side facts.**
- **Auth.** Claude Max OAuth on macOS lives in the login keychain
  (`Claude Code-credentials`, service name namespaced by `sha256(config_dir)[0:8]`,
  `docs/defect-log.md:1797`, `docs/2.1.226-compatibility.md` §4.1). A Linux guest
  cannot reach it, and proxying it "hands the guest the operator's OAuth token"
  (`docs/defect-log.md:5094-5099`). The honest route is a separate `claude login` inside
  the guest writing `~/.claude/.credentials.json`. That is a second live session on the
  same Max subscription, and it spends real tokens from the same budget.
- **Binary.** ~200 MB download per guest; keep it on a persisted guest disk, not a
  virtiofs share.
- **UDS path length.** `sockaddr_un` is 108 bytes on Linux and the repo has already been
  bitten (`evidence/linux-pi-agentic-subagent-2.1.257-x86_64.json:28`). Inside a guest,
  put the runtime dir at `/tmp/pmux` — and note a virtiofs share is a poor place for a
  socket anyway.

## 3. Alternatives

- **Docker Desktop / OrbStack / colima `--platform linux/amd64`.** Same architectural
  lie plus a container. The repo *deleted* its linux-docker lane: it is a tombstone
  (`docs/current-state.md:263,293`, row C6) — "Historical freeze envelope, not a living
  pin." Re-introducing a container lane re-opens a settled decision.
- **Lima.** Same arm64-guest / Rosetta situation as Tart, less tidy image handling.
- **UTM/QEMU full x86_64 emulation.** The only way to get a genuine linux/x86_64 *guest*
  on this Mac, at roughly an order of magnitude slowdown. Timing receipts from it are
  worth less than Rosetta's, and interactive Claude Code in a TUI would be painful.
- **Remote x86 box over SSH — the status quo, and it is already the right answer.** The
  linux receipts were taken on a 128-core x86_64 Linux host. That is real silicon, and
  it is what the promoted linux cell describes.

## 4. Recommendation

**Use Tart, but only as a linux dev-loop mirror, never as a promotion path.**

- **What it buys.** `tools/dev/check.sh` (fmt/clippy/tests, `--push` e2e and process
  blackbox) and the Linux-specific code paths — glibc `openpty` signatures, POSIX-only
  walks, UDS limits, `.credentials.json` auth — verified concurrently with the native
  macOS battery, before a single commit, on one machine. No Linux cap is documented or
  reported, so the macOS pool keeps its 15 cells.
- **What it cannot buy.** A linux/x86_64 promotion receipt. Not with Rosetta, not with
  QEMU. Anything with a timing number in it still has to come from the x86 box.
- **Setup.** `brew install cirruslabs/cli/tart`; `tart clone ghcr.io/cirruslabs/ubuntu:latest pmux-linux`;
  `tart set pmux-linux --cpu 4 --memory 12288 --disk-size 80`;
  `tart run pmux-linux --no-graphics --dir=repo:$PWD` in the background. Leaves 6 cores
  and ~20 GB for the macOS pool. **Clone the repo inside the guest and `git fetch` from
  the host share** rather than building in the virtiofs mount: Cargo on virtiofs is slow
  and the target dir would collide with the host's. Drive it from the host with one
  script wrapping `ssh admin@$(tart ip pmux-linux)`, so `tools/dev/check.sh` runs on both
  cells in parallel and the host waits on both.
- **Effort.** Half a day to a working guest with a checkout, Rust toolchain and a green
  `check.sh`. Another half day for the one-command host wrapper. Add roughly an hour if
  you want a logged-in Claude Code in the guest for `operator_eval.py` smoke runs, and
  accept it spends real turns.
- **Do not introduce linux/aarch64 as a third promoted cell.** A promoted cell is a
  promise about hosts you do not own, and it would need its own pooled-drain corpus, its
  own floor, its own re-promotion discipline, and its own maintenance at every Claude
  Code version bump — for an architecture nobody is asking pmux to serve. Run the guest
  with `--tested-claude-profile` when a turn is needed, and leave `PROMOTED_PROFILES` at
  two cells.
