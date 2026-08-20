# sandbox-spike.md

**A research spike on microsandbox (`https://github.com/superradcompany/microsandbox`), specifically
its Rust SDK, assessed for viability and optimal use inside pmux as a product feature.**

**This document builds nothing and proposes building nothing.** No Rust was written for it, no
dependency was added, no `Cargo.toml` was touched. It is analysis against evidence, and its
recommendation is negative.

`spec.md` is normative for product behaviour. `current-state.md` is normative for position, and
**§9.7 of that file already adjudicates this question** — Row S1, "sandboxing the child is DEFERRED
ENTIRELY". This file does not overturn §9.7. It does three things §9.7 could not: it characterises
microsandbox from its own documentation rather than from its name, it prices the change in the
project's own measured currency, and it finds one defect the adoption would introduce that nobody
has written down — a compatibility report that would pass `RequireTested` on a cell it does not
describe (§3.2).

Every quantity below carries one of five labels, and the fifth is the one this file leans on hardest:

- **MEASURED** — observed, with the instrument named.
- **DERIVED** — arithmetic over MEASURED numbers, shown in full.
- **CHOSEN** — a policy constant.
- **VERIFIED-EXTERNAL** — read out of microsandbox's own repository or documentation during this
  spike. It is not a measurement *by this project*: nothing in this file was executed against
  microsandbox. Two grades of it, and the distinction is kept because it is exactly the kind that
  decays into a false citation: quotes marked **byte-verified** were pulled through
  `gh api …/contents/<path> | base64 -d` and grepped in this shell, so they are the file's bytes;
  everything else was relayed by a page-fetching tool that summarises, and is reported as the
  substance of a passage rather than as its bytes.
- **UNVERIFIED** — not established. Stated as a gap, never inferred away.

**Nothing in this document was measured against a running microsandbox.** microsandbox was never
installed, never built, never executed. Every claim about its behaviour is VERIFIED-EXTERNAL at
best, which means it is a claim about what its authors say it does.

---

## 0. The answer, in one paragraph

microsandbox is a **microVM runtime with a Linux guest kernel**, on every host it supports including
macOS. That single fact decides the question. Claude Code's subscription credential on this host
lives in the **macOS Keychain** — MEASURED below — and `security(1)` does not exist inside a Linux
guest. So criterion 1 fails, and it fails in a way no configuration closes: the only two escapes
either hand the guest the operator's OAuth token (which is precisely the authority a sandbox was
supposed to remove) or require pmux to intercept TLS on the operator's own Anthropic session. The
mint-cost criterion, by contrast, **passes comfortably** — a microVM boot is ~2% of one mint, which
is the opposite of what a reader expects and is worth stating plainly. The PTY criterion is cleared
by the SDK and then failed one level up, by the compatibility key. **Recommendation: do not build
this.** Build the two things in §7 instead, neither of which is a sandbox and neither of which
touches auth, the PTY, or the compatibility evidence.

---

## 1. What microsandbox actually is

Fetched 2026-08-08. The repository resolved; so did `docs.microsandbox.dev` for some paths and
`raw.githubusercontent.com` for the rest. Two documentation URLs 404'd
(`docs.microsandbox.dev/guides/installation`, `docs.microsandbox.dev/references/cli/`) and were
routed around by reading the same files out of `docs/` in the repository.

**VERIFIED-EXTERNAL, from `README.md`:**

> **Microsandbox** runs **untrusted workloads** inside fast, local microVMs: AI agents, user code,
> plugins, CI jobs, dev environments, scrapers, and automation.

> **Instant Startup**: Average boot times[^boot-time] under 100 milliseconds.
>
> [^boot-time]: Boot time refers to guest boot on an M1 machine.

(The first of those two lines is a bullet whose leading `<img …>` badge is elided here; the text is
otherwise byte-exact, `microsandbox/README.md:36` and `microsandbox/README.md:410`.)

> **Warning**: Microsandbox is still **beta software**. Expect breaking changes, missing features,
> and rough edges.

**VERIFIED-EXTERNAL (byte-verified), from `docs/security/isolation.mdx:13` — the load-bearing
sentence of this whole document:** each sandbox has

> Its own **Linux kernel**, supplied by microsandbox (built from libkrunfw), not your host kernel.

Its host-facing surface is a fixed virtio device table (`:34-38` — `virtio-console` "The control
channel to `agentd`", `virtio-net`, `virtio-fs` "Host directories you explicitly mount",
`virtio-blk`, `virtio-rng`), and `:40`:

> There is no general-purpose passthrough. No host PCI devices, no host sockets, and no shared
> memory beyond these devices.

`:44` names what drives it: "`agentd`, the agent that runs as PID 1 inside the guest".

**VERIFIED-EXTERNAL (byte-verified), platform support**, `docs/troubleshooting/macos.mdx:7` and
`:23`:

> microsandbox local sandboxes on macOS require Apple Silicon. Intel Macs are not supported for the
> local runtime.

> microsandbox needs Apple Silicon; Rosetta does not make an Intel Mac supported for local sandboxes.

Linux requires KVM. The runtime installs to `~/.microsandbox`, binary `bin/msb`, library
`lib/libkrunfw.dylib`.

**VERIFIED-EXTERNAL, repository health** (`gh api repos/superradcompany/microsandbox`, 2026-08-08):
7,176 stars, 65 open issues, created 2024-10-03, last push 2026-08-08, Apache-2.0, not archived.
Releases v0.6.2 through v0.6.8 span 2026-07-01 to 2026-07-29 — six releases in four weeks. Active,
and moving fast enough that the beta warning is not decoration.

**VERIFIED-EXTERNAL, the Rust SDK.** Version 0.6.8 (2026-07-29), Apache-2.0, Rust 2024 edition.
Default features `keyring`, `net`, `prebuilt`; optional `ssh`. Dependencies named on `docs.rs`
include `tokio`, `async-compression`, **`sea-orm`**, `rustls`, `crossterm`. And:

> A native microsandbox runtime must be installed separately on the host system.

Two things follow that matter to this repository specifically. First, `cargo add microsandbox` is
not the whole cost: it brings a host-side install step outside cargo's control, which is a new
class of operator precondition pmux does not currently have. Second, this workspace's entire
dependency graph is **199 packages** (MEASURED: `grep -c '^\[\[package\]\]' Cargo.lock`); adding an
SDK whose own tree carries a full ORM is not a marginal change to that number. `lib.rs` characterises
the crate's tree as roughly 87–145 MB of dependencies and ~2.5M SLoC — VERIFIED-EXTERNAL, reported
rather than quoted, and not independently checked here.

**VERIFIED-EXTERNAL, the execution API** (`docs.microsandbox.dev/sdk/rust/execution`, and the SDK
sources `sdk/rust/lib/sandbox/{attach,exec}.rs`). It is richer than the READMEs suggest and the
richness matters, because it means microsandbox is **not** disqualified where a reader would expect:

- `exec()` / `shell()` buffered; `exec_stream()` / `shell_stream()` returning an `ExecHandle`.
- A `tty()` builder option — "Enable for interactive programs (shells, editors, top); disable for
  scripts" — that allocates a **guest PTY**.
- `ExecHandle::resize(rows, cols)` "adjusts PTY dimensions during execution", `take_stdin()`,
  `signal()`, `kill()`, `timeout()`, POSIX `rlimit()`.
- `attach()` / `attach_shell()`, whose doc comment is byte-verified at
  `sdk/rust/lib/sandbox/attach.rs:15-17`: "The host terminal is set to raw mode for the duration of
  the attach session. The guest process runs in a PTY, enabling terminal features (colors, line
  editing, Ctrl+C → SIGINT)." Default detach sequence `"ctrl-]"` (`:32`).

**VERIFIED-EXTERNAL (byte-verified), filesystem.** `docs/sandboxes/volumes.mdx:62`: "Mount a
directory from the host directly into the sandbox. Changes inside the sandbox are reflected on the
host, and vice versa." And `:126`: "Directory volumes mount through virtiofs. Disk volumes are raw
ext4 disk images managed by microsandbox and mount through virtio-blk." Memory is a ceiling rather
than a reservation — `docs/sandboxes/tuning.mdx:67`: "Reserving headroom is cheap: spare vCPUs stay
parked, and spare memory is only backed once the guest uses it."

**VERIFIED-EXTERNAL, lifecycle.** Sandboxes may be **detached**: one "survives after your process
exits", reconnectable via `Sandbox::get("worker")`. Each sandbox "runs as a child process of whatever
application creates it".

**VERIFIED-EXTERNAL, the stated threat model** (`docs/security/overview.mdx`). It protects against
guest-to-host escape, cross-sandbox reach, SSRF/exfiltration, and "Host filesystem disclosure: The
guest sees nothing of your host disk unless you mount it." It explicitly does **not** protect against
a compromised host — "If an attacker already controls your host, or the process that launches
sandboxes, they are on the trusted side" — nor against hypervisor bugs, nor does it verify image
signatures: "Pulled content is verified against its declared digest, but signatures and attestations
are not checked."

**Do not characterise this project from its name — and the instruction was right to insist.** The
name suggests a lightweight in-process syscall filter of the `seccomp`/seatbelt family. It is the
opposite: a full hardware-virtualised Linux VM per sandbox, running OCI images. Almost every
conclusion in this document turns on that correction.

---

## 2. What the threat model is now, and what a sandbox here would be *for*

### 2.1 The model cannot execute anything

MEASURED, three independent ways, and recorded at `docs/path-b.md` §2.2 verbatim:

> `--disallowedTools "*"` | **MEASURED TOTAL.** Removes tools, subagents **and bundled skills** —
> not hides them. Denied cell `input_tokens` 182-229 with `cache_creation: 0`; control cell
> `cache_creation: 29,272`. ~29k tokens of tool surface **absent**.

The third witness is structural, in the same row: "with no tool surface a `Task` subagent is
structurally unreachable, so a sidechain row is evidence the denial failed."

MEASURED corroboration from the other side, `docs/path-b.md` §0.2: a complete descendant inventory
of the live `claude` PID, sampled every 50 ms across four cells, is exactly
`security find-generic-password` and `caffeinate -i -t 300`. "No node, no python, no npx, in any
configuration."

### 2.2 Therefore a sandbox here does not protect the operator from the model

It protects them from **Claude Code the program**. This has to be said out loud, because a proposal
that implies otherwise is arguing for something `--disallowedTools "*"` already delivers, at zero
cost, today, on a shipped path.

The distinction is not academic; it changes what the candidate is measured against. The residual
authority after `--disallowedTools "*"` is:

| Authority | Who exercises it | Closed by the current design? |
|---|---|---|
| Read/write any path the operator's uid can reach | the **program**, not the model | **No.** `crates/service/src/claude_launch.rs:306-308` says so in terms: "it does not sandbox the filesystem, and any session's Bash tool can already read any absolute path" |
| Spawn processes | the **program** — MEASURED: `security(1)`, `caffeinate(1)` | No |
| Network egress | the **program** — MEASURED: 428 files, 6.2 MB from GCS on every fresh root (`claude_launch.rs:76-80`) | Only by one env var, `CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL=1`, whose *absence* silently restores it |
| The operator's Anthropic identity, quota and machine-wide login | the **program** | **No, by design** — `docs/path-b.md` §5.4, and isolating it is an explicit non-goal |

Every row in that table is the program, not the model. That is what a sandbox would be for.

---

## 3. The four criteria

### 3.1 Keychain access preserved — **FAILS, and no configuration closes it**

MEASURED on this host, 2026-08-08:

    $ test -e "$HOME/.claude/.credentials.json"          -> absent
    $ security find-generic-password -a "$USER" \
        -s "Claude Code-credentials"                     -> exit 0 (item present)

So on this host the subscription credential is in the macOS Keychain and **not** in a file. That
matches the mechanism read out of the 2.1.220 bundle at `docs/path-b.md` §5.1 — the keychain service
name is `` `Claude Code${Ds().OAUTH_FILE_SUFFIX}${e}${o}` `` with `o` the
`sha256(config_dir)[0:8]` suffix — and it is exactly the mechanism `config_isolation` exploits:
`crates/service/src/config_isolation.rs:8-10`, "Claude namespaces the macOS keychain SERVICE NAME by
`sha256(config_dir)[0..8]`, so a fresh root looks up an empty item".

**microsandbox's guest is Linux on macOS** (§1). `security(1)` is a macOS binary, and the Keychain
behind it is a macOS service reached through a Mach port on the host. The guest cannot get there, and
this is not an inference from the guest's OS alone — `isolation.mdx:40` closes it explicitly:
"**No host PCI devices, no host sockets**, and no shared memory beyond these devices." A Claude Code
running inside a microsandbox sandbox on this machine has no path to that credential, by the
candidate's own stated design. Criterion 1 fails outright for any shape that puts Claude Code inside
the guest.

There are exactly two escapes, and this document's sharpest finding is that **both of them defeat the
purpose of the sandbox**:

**(a) Proxy the keychain call out of the guest.** Run a host-side helper that answers
`find-generic-password` on the guest's behalf. This works, and it hands the guest the operator's
OAuth token — which is the single highest-value item in the blast radius. `docs/path-b.md` §5.4 is
unambiguous about what that radius already contains: "an OAuth refresh inside a cell rewrites the
operator's credential; a credential-clearing path inside a cell logs the operator out machine-wide;
usage, rate limits and subscription state are the operator's... and the blast radius of a compromised
cell still includes the caller's Anthropic identity." A sandbox that must hand over the credential to
function does not shrink that radius by one byte. It shrinks the *filesystem* radius while leaving
the *identity* radius exactly where it was.

**(b) microsandbox's own secret binding.** This is the one genuinely interesting option in the
candidate, and it deserves to be named rather than waved past, because it is the only mechanism
found in this spike that could close something `docs/path-b.md` §5.4 declares an explicit non-goal.
VERIFIED-EXTERNAL (byte-verified), `docs/security/secrets.mdx:11` and `:18-20`:

> Instead of putting a real credential inside the VM, microsandbox puts a **placeholder** there. The
> real value stays in host memory.

> 1. You bind a secret to an environment variable and list the hosts it's allowed for.
> 2. The guest's environment receives a placeholder (`$MSB_<env_var>` by default), never the real
>    value.
> 3. The workload uses the placeholder as if it were the credential, in a header, auth field, query
>    string, or body.
> 4. On egress to an allowed host, the host-side proxy decrypts the intercepted TLS, verifies the
>    request is really going where it claims, substitutes the real value, and forwards it upstream.

It is still the wrong answer here, and step 1 of that list is the reason. **The mechanism is shaped
for an environment-variable credential — that is, for API-key auth — which is precisely the auth mode
Path B's default policy strips.** `crates/protocol/src/v1/launch_environment.rs:77-88` lists
`SUBSCRIPTION_AUTH_KEYS`, and its first two entries are `ANTHROPIC_API_KEY` and
`ANTHROPIC_AUTH_TOKEN`; `docs/spec.md:562-566` states that the default `subscription` policy "removes
Anthropic API keys/tokens... This forces use of the interactive subscription authentication already
associated with the effective Claude config root." So the one microsandbox feature that could isolate
a credential fits the auth mode pmux deliberately does not use, and does not fit the one it does.
Adopting it would mean moving Path B off subscription auth onto a metered API key — a product
decision far larger than sandboxing, and one that makes criterion 1 moot by deleting its subject.

Three further reasons, in decreasing order of how badly it goes:

1. It requires pmux to **intercept TLS on the operator's own Anthropic session**. That is a new
   trust surface of a kind this codebase has never taken on, in a product whose secrets discipline is
   currently "secrets are 0600 inline files in a 0700 tempdir, never argv"
   (`docs/current-state.md:150`).
2. Substitution has stated limits — `secrets.mdx:51`, byte-verified: "Placeholders inside HTTP/2
   request bodies, non-identity-encoded (for example gzipped) bodies, or very large fixed-length
   bodies aren't substituted." Whether Claude Code's credential travels in a substitutable position
   is **UNVERIFIED** — nobody has looked.
3. It solves egress, not ingress. Under subscription auth Claude Code must first *believe* it is
   logged in, which means a credential-shaped artifact has to exist inside the guest. Producing one
   means driving a login flow, and `docs/path-b.md` §5.4 records that pmux has no path to that —
   "it would be a browser handoff inside a PTY the caller cannot see — and it is an explicit
   **non-goal**."

**Verdict: criterion 1 fails.** Not "fails pending work" — fails structurally, because the guest is a
different operating system from the one holding the secret.

### 3.2 A real PTY — **the SDK clears it; the compatibility key does not**

The SDK genuinely provides what the criterion asks for: a guest PTY with resize, stdin, signals and
raw-mode bridging (§1). §9.7's third objection — "It would put the PTY behind a proxy layer" — is not
refuted by that, but it is narrowed: the proxy is a virtio-console transport with a real PTY at each
end, not a line-buffered pipe.

The criterion fails one level up, and this is the part not previously written down anywhere.

**`CompatibilityReport.os` is a compile-time constant of the daemon.**
`crates/service/src/compatibility.rs:320-321`:

    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

`std::env::consts::OS` is baked in when `pmuxd` is compiled. The promoted set is one entry,
`crates/service/src/compatibility.rs:125-131`:

    pub const PROMOTED_PROFILES: &[PromotedProfile] = &[PromotedProfile {
        claude_version: "2.1.220",
        os: "macos",
        arch: "aarch64",
        terminal_profile: TerminalProfile::Transparent,
        input_transport: InputTransport::Sdk,
        transcript_drain_ms: 1_000,

and Path B hard-codes the strict policy — `crates/service/src/stateless.rs:228`,
`compatibility: CompatibilityPolicy::RequireTested`, with the comment above it explaining why:
"`AllowUntested` would let a pool instance run on a Claude whose composer geometry pmux has never
measured, which is the one input the fast path trusts."

Now run the two candidate shapes through that:

**Shape A — per-instance microVM, daemon stays on the host.** `pmuxd` is a macOS binary, so
`CompatibilityReport.os` reports **`"macos"`** for a child that is actually running on Linux. The
single promoted profile matches. `tested: true` is published. `transcript_drain_ms: 1000` is applied
— a value whose own provenance string (`compatibility.rs:132-134`) says it came from "456 turns in
189 Claude Code 2.1.220 transcripts on **macos/aarch64**". **`RequireTested` passes on a cell it does
not describe, silently.** That is the governing defect of this repository in executable form: a
report that states something it has not established, indistinguishable on the wire from one that has.

**Shape B — whole stack inside the VM.** `pmuxd` is compiled for Linux, `os` becomes `"linux"`, no
promoted profile matches, and every Path B start refuses with `UnsupportedClaudeVersion`
(`compatibility.rs:336-344`). That is the *correct* failure — loud, at admission, naming the missing
cell. But it means **Path B does not run at all** until a Linux cell is promoted, and promotion is
paid for out of a budget of **53 attempts** that `docs/current-state.md:3440` records as "53 of 100
attempts live" and `:1663` records as "already committed to Gate B coverage that has never run".

Two further PTY-adjacent facts, both MEASURED and both invalidated by a Linux guest:

- Half the measured descendant set is `caffeinate -i -t 300` (`docs/path-b.md` §0.2). `caffeinate(1)`
  is a macOS binary. Inside a Linux guest that inventory — the observation that retired the false
  "every Path B session spawns MCP servers silently" claim — is no longer the thing that was
  measured, and would have to be re-measured before it could be cited again.
- The four hard-coded Claude-TUI geometry constants are validated by "exactly 24 real turns on one
  compatibility cell" (`docs/current-state.md:1660-1661`), all macOS. `docs/current-state.md:1112`
  lists what those 24 turns prove — the `❯` anchor found, the all-whitespace-prefix test passed, the
  bracketed paste landed. None of it transfers to a different OS on evidence.

**Verdict: criterion 2 is met by microsandbox and failed by the composite.** Shape A fails
*silently*, which is worse than Shape B failing loudly.

### 3.3 No persistent on-screen notice — **not disqualified by microsandbox; unproven for the composite**

The disqualifying mechanism is precise. `docs/path-b.md` §2.3:

> `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC`, `DISABLE_TELEMETRY`, `DO_NOT_TRACK` and
> `CLAUDE_CODE_SAFE_MODE` were each MEASURED to **BREAK the cell 5/5**, by rendering a persistent
> notice that changes the screen shape and fails startup.

(And the flag/env distinction holds, on the newer of two rows: `docs/path-b.md` §13 records
"**MEASURED: IT DOES NOT**" for the `--safe-mode` *flag* — 5/5 probe cells reached `ready` and
answered their own token — and closes with "the flag and the environment variable are **not**
interchangeable: `CLAUDE_CODE_SAFE_MODE` breaks the cell 5/5 (§2.3) and `--safe-mode` breaks
nothing." Row `:188` still carries the older "was not probed" caveat that `:1902` retires; cite the
later one.)

microsandbox itself adds no chrome to the guest program's output. `attach()` puts the **host**
terminal into raw mode and bridges bytes; `exec_stream_with(|e| e.tty(true))` is a programmatic guest
PTY with no decoration. On its own behaviour, microsandbox passes.

What is **UNVERIFIED** is everything else that would change on the screen:

- Whatever OCI image is chosen may print a motd, an init banner or a login prompt. That is a property
  of the image, not of microsandbox, and it is the exact `prompt_glyph_offsets_from_bottom: [9]`
  failure mode if it happens.
- Every microVM mint gives Claude Code a **cold configuration root**, and `docs/path-b.md` §5.5
  names that as the one thing explicitly not measured: "**Unmeasured, and named so it is not mistaken
  for measured:** first-launch cost in a cold root. A fresh root has no cached feature flags, statsig
  state or settings; whether that shifts readiness or drain timing has never been measured".
- Claude Code's own first-run and platform-detection screens on Linux have never been observed by
  this project.

**Verdict: criterion 3 is not failed by microsandbox and is not passed by anything.** It is
unmeasurable without spending the budget §3.2 already accounts for.

### 3.4 Mint cost compatible with the pool — **CLEARED. This is the criterion the candidate wins.**

The anchors, all from `docs/path-b.md`:

| Quantity | Value | Label | Cite |
|---|---|---|---|
| Relaunch / full mint of one instance | **~4.4 s** | MEASURED | `:1549` "The ~4.4 s relaunch cost is MEASURED for a TUI launch" |
| `/clear` end to end, 50 ms drain | **703–756 ms** | MEASURED | `:1255-1256`, `:1278` |
| `/clear` end to end, shipped 1000 ms drain | **~1700 ms** | MEASURED | `:1256-1257` |
| `ADMISSION_WAIT_CEILING_MS` | 2500 ms | CHOSEN | `:1255` |
| Relaunch amortised at the 250-turn cap | 17.6 ms/turn | DERIVED | `:1179` |
| `MAX_POOL_SIZE = DEFAULT_POOL_SIZE` | **15** | CHOSEN | `:1159` |

The ~30 ms figure in §3.4 of `path-b.md` is the transcript rotation alone (`:1278` says so
explicitly); it is not the clear.

**The microVM boot is VERIFIED-EXTERNAL at "under 100 milliseconds", footnoted "guest boot on an M1
machine".** Read the footnote carefully: *guest boot* is the kernel coming up, not the workload
becoming ready. It is not comparable to pmux's 4.4 s, which is a *readiness* number. A third-party
write-up cited ~320 ms for the same operation; that is UNVERIFIED and is used below only as a
pessimistic bound.

**DERIVED, taking the vendor number at face value and — this assumption is the weak point, flagged
here rather than buried — assuming guest readiness equals host readiness:**

- mint 4,400 ms → 4,500 ms. **+100 ms, +2.3%.**
- Pessimistic bound at 320 ms: 4,400 → 4,720 ms. **+7.3%.**
- Amortised over the 250-turn recycle ceiling: `4500/250 = 18.0` ms/turn against the current 17.6 —
  **+0.4 ms/turn**. At the pessimistic bound, `4720/250 = 18.9` ms/turn, **+1.3 ms/turn**.
- Against the current MEASURED Path B turn — "**1,955 ms median** against Path A's 1,213 ms on the
  same clock, on the same daemon, in the same run" (`:133-134`) — +0.4 ms/turn is **0.02%**. Against
  the older ~550 ms figure `:1185` still quotes, **0.07%**. The conclusion is insensitive to which.

**So the mint criterion passes, and it passes by a wide margin.** No constant moves. The 2,500 ms
admission ceiling is a bound on *waiting*, not on minting, and is untouched. Anyone who rejects
microsandbox on "it would add seconds to mint" is rejecting it for the wrong reason.

**The assumption that could overturn this, stated plainly:** the guest-readiness term is
**UNVERIFIED**. Claude Code has never been launched in a Linux guest by this project, off a virtio-fs
or virtio-blk rootfs, and a Node bundle starting off virtio-fs is exactly the kind of thing that is
slower than native by a factor rather than a delta. If guest readiness were 2× host readiness, mint
becomes ~8.9 s and the arithmetic above is void. Nothing here establishes that it is not.

**The real pool cost is memory, not time.** The sizing rule is `pool_size × 1024MB` must fit the
budget (`:1221`), against MEASURED anchors of 375 MB at boot, +1.86 MB/turn, 486 MB/instance at turn
15, and 16 instances = 7,777 MB measured directly (`:1144-1146`, `:1207`). A per-instance microVM
adds a per-VM overhead **V** — VMM process, guest kernel, guest page cache, virtio-fs cache — that
**has never been measured on this host and is not measurable from documentation**.

DERIVED sensitivity, holding the current 15 × 1024 = 15,360 MB budget fixed,
`floor(15360 / (1024 + V))`:

| V (per-VM overhead, MB) | Pool size supported | Slots lost from the 15 cap |
|---:|---:|---:|
| 64 | 14 | 1 |
| 128 | 13 | 2 |
| 256 | 12 | 3 |
| 512 | 10 | 5 |
| 1024 | 7 | 8 |

**So: a per-instance microVM costs somewhere between 1 and 8 of the 15 slots at a fixed memory
budget, and which it is is unmeasured.** One factor pushes V down and should be said, because it cuts
against the pessimistic reading: `docs/path-b.md` §6 MEASURED that "**No copy-on-write
sharing** was observed" between host instances, so the per-instance JS heap is *already* unshared.
The delta is the guest kernel and VMM, not a duplicated Claude Code. And `docs/sandboxes/tuning.mdx:67`
— byte-verified — states "spare memory is only backed once the guest uses it", so `memory(N)` is a
cap rather than a reservation. V is plausibly at the small end. It is still unmeasured.

**Verdict: criterion 4 passes on time and is unpriced on memory, with a bounded worst case.**

---

## 4. What it would actually buy — and what it would not

The existing controls are all rules about **what pmux binds**. None of them is a rule about **what
the child may reach**. That distinction is the whole answer to "be specific about a threat it closes
that these do not."

| Existing control | What it actually constrains |
|---|---|
| Private per-cell config root, mandatory for `cell: minified` (`docs/path-b.md` §5.6) | Which directory pmux *hands* the child |
| Containment on `(device, inode)` ancestry across every directory every live claim binds (`docs/path-b.md` §5.6) | Which starts pmux *admits* |
| 0700 from birth, 0600 files (`spec.md:776`) | Which *other uids* can read |
| The nine closed isolation leaks (`docs/path-b.md` §0.1, §5.6) | Spellings and relations of directories pmux binds |
| `--disallowedTools "*"` | What the **model** can do |

A child that decides on its own to open `~/.ssh/id_ed25519` passes through every one of them
untouched. `claude_launch.rs:306-308` concedes exactly this, in the middle of explaining why `HOME`
is refused as a request key rather than bound as a resource: the reach is one "pmux cannot remove
anyway: it does not sandbox the filesystem".

**The specific threat a microVM closes that none of the above do: a supply-chain compromise of the
Claude Code distribution itself.** pmux launches an executable it does not build, does not pin, and
does not fingerprint. `LaunchSpec` (`crates/rmux/src/launch.rs:87-96`) carries
`pub executable: PathBuf`, and `LaunchSpec::validate` (`:112-128`) checks exactly four things — the
executable path is absolute, the cwd is absolute, no argument contains NUL, the environment is
well-formed. There is no hash, no `(device, inode)`, no mode check on the child binary. Identity is
established by version *string* (`native.rs:4247` runs `--version`).

A malicious Claude Code release, under that model, reads the operator's `~/.ssh`, `~/.aws`, and the
whole of `~/.claude/history.jsonl` — which `docs/path-b.md` §5.6 MEASURED at "1,556 rows, **49 distinct
projects in one file**" — and exfiltrates over the network connection it already opens. Every control
in the table above is silent: the private root does not contain the program, `--disallowedTools "*"`
does not apply because the model is not acting, containment is an admission predicate, and the 0700
modes are same-uid. A microVM with virtio-fs mounting only the cell root, plus a network policy
allowing only the Anthropic endpoint, closes exactly this class. **That threat is real and the honest
answer is to say so.**

Second thing it would buy: **structural network egress control**. Today the only thing standing
between a fresh root and a 6.2 MB third-party plugin tree is one env var in a one-entry table
(`claude_launch.rs:MINIFIED_CELL_ENVIRONMENT`), whose absence silently restores the download. A
network policy is a structural close of that class rather than a table entry that must stay correct.

**What it would not buy, and this is decisive:** credential isolation. §3.1(a) shows that any
workable shape hands the guest the operator's OAuth token, so `docs/path-b.md` §5.4 — the operator's
identity, quota, and machine-wide login all inside the blast radius — is **unchanged by the sandbox**.
The sandbox would close the filesystem and the network while leaving the highest-value item exactly
where it is.

**Three costs that are not in the criteria and must be counted anyway:**

1. **Transcript authority behind virtio-fs.** §9.7 objection 2, and this spike confirms the
   mechanism rather than merely repeating the concern: microsandbox's bind mounts *are* virtio-fs
   (`docs/sandboxes/volumes.mdx:126`, byte-verified — "Directory volumes mount through virtiofs").
   `crates/claude` reads the JSONL with type-enforced complete-line
   framing, a monotonic cursor, and fail-closed truncation detection; `docs/path-b.md` §5.3 MEASURED
   that every one of Claude's 25 transcript writes landed as a **new inode**, with a positive control
   proving the sampler *can* see a torn write. Whether that inode-replacement pattern survives
   virtio-fs is UNVERIFIED, and it is the premise the framing rests on.
2. **The reaping proof becomes an assertion about a VM.** §9.7 objection 4, confirmed against both
   sides: `crates/rmux/src/process_boundary.rs:5-8` requires that a session be "observed empty and no
   descendant observed by this process has escaped it", and microsandbox's guest processes are not in
   the host process table at all. Worse, **detached sandboxes survive the creating process** (§1),
   which introduces a live child pmux's boundary proof cannot reach and that outlives `pmuxd` — a new
   failure mode against `assert_pool_parent_drained`.
3. **A beta dependency on the launch path**, in a product whose Gate A receipt currently carries
   exactly one red cell and that cell is deliberate — `docs/current-state.md:1494` records C6 as "the
   **only** red cell in an otherwise 80/81 receipt", and the current receipt reads
   `FAIL 82/83 cells passed, 1 failed, 83 executed failed: linux_docker_self_tests`
   (`.context/gate-a-mutants/dead-code-pass/stdout.log:85`). Both figures carry the same single
   deliberate red; the cell count grew with the gate. This document originally claimed 82/83 "does not
   appear in this tree" — it does, but under `.context/`, which is the last line of `.gitignore` and so
   invisible to the `git grep` that was used to look. The cost of a beta dependency is not the bugs
   it has, it is that the receipt stops being a statement about pmux.

---

## 5. Where it fits: per-instance, per-daemon, or not at all

**Per-instance: no.** It fails criterion 1 (§3.1) and it introduces the silent-`os` defect (§3.2,
Shape A) — a `tested: true` on a cell nobody measured. That defect alone would be grounds to refuse
the shape even if the auth question were solved.

**Path A is not a candidate, by the owner's explicit decision** — isolation there is the user's
responsibility. `docs/path-b.md` §5.6 states the mechanism that follows from it: "**Path A keeps the
door**: an ordinary cell owns its own isolation story". So the question is only ever about Path B,
and §3.2 shows Path B is precisely where `RequireTested` makes the cost bite.

**Per-daemon (whole stack inside one sandbox): the only coherent shape, and still blocked on macOS.**
It is the direction `docs/current-state.md:1673-1675` already names as correct — "run the whole
stack inside a sandbox, not the child inside one... every property above is preserved unchanged, and
the isolation is enforced by something that is not pmux." This spike confirms that reasoning and adds
the reason it does not rescue microsandbox on macOS: **the guest is Linux**, so criterion 1 fails for
the whole stack exactly as it fails for the child. Moving the daemon inside does not move the
Keychain inside.

It also confirms that the slot is already occupied by something that works. `tools/linux-docker/`
runs the full gate suite inside a container (`current-state.md:1676`), and
`docs/gate-c-linux-handoff.md:245-248` states exactly what that is worth: the runner executes
"**without network access, source/config mounts, provider credentials, or a real `claude`
executable**. A Docker result is *deterministic portability evidence*, not credentialed native-Linux
Claude support." A microVM would produce the same class of evidence at a higher price, unless and
until the credential question is answered — and if the credential question is answered, the
container gets the benefit too.

**Not at all, for macOS v1: yes.** That is the recommendation.

---

## 6. The Linux question

The brief is right that "macOS seatbelt was the original sketch" is the framing to answer against.
One correction first, because it is the kind of thing this repository refuses to let slide:
**MEASURED — `git grep -iE "seatbelt|sandbox-exec|sandbox_exec" 09f5f41` returns zero matches**
(anchored to the commit before this file, which contains the words itself and would otherwise hit).
The seatbelt framing is not in the codebase. What *is* in the codebase is `docs/path-b.md` §5.4, "a
deny-by-default sandbox profile becomes writable", which is seatbelt-shaped and never names it. Any
future document should not cite this tree as the source of a seatbelt design; there isn't one.

What changes on Linux:

1. **Criterion 1 changes character completely, and nobody knows to what.** On Linux there is no
   Keychain. Claude Code's credential storage there is **UNVERIFIED by this project** — and the
   project cannot pretend otherwise, because `gate-c-linux-handoff.md:245-248` says Gate C runs with
   no credentials and no real `claude`. There is one hint in the tree and it is only a hint:
   `crates/e2e/tests/cross_cell_contamination.rs:154-157` carries a `.credentials.json` channel,
   under a comment at `:142-145` recording that the row was "NAMED by an unclassified-entry report
   from a real 2.1.220 launch". If Linux Claude Code keeps its credential in a **file** inside the
   config root, then criterion 1 stops being a structural block: a per-cell credential file is
   bind-mountable into a guest over virtio-fs, and the whole calculus inverts. **This is the single
   measurement that
   decides whether a sandbox is ever worth building for pmux**, and it costs no live-model attempts
   because it is an observation about the filesystem, not a turn.
2. **The measured descendant set is macOS-specific and dies.** Both of its two members are macOS
   binaries: `caffeinate(1)` has no Linux equivalent Claude would call, and `security(1)` is replaced
   by whatever the Linux credential path turns out to invoke — which is item 1, unmeasured. So the
   complete-descendant-inventory observation that RETRACTED the "every Path B session spawns MCP
   servers silently" claim (`docs/path-b.md` §10 item E3) has to be re-run in full before it can be
   cited on Linux at all.
3. **`PROMOTED_PROFILES` has no Linux entry**, so Path B refuses on Linux **today**, sandbox or no
   sandbox (`compatibility.rs:125-131`, `stateless.rs:228`). A Linux compatibility cell must be
   promoted before any of this is even reachable, and that comes out of the 53 attempts.
4. **microsandbox requires KVM.** A Linux host without nested virtualization — a common shape for
   cloud CI runners — cannot run it at all. `tools/linux-docker/` has no such requirement. That is a
   deployment constraint the incumbent lane does not carry.
5. **The one thing that gets easier:** on Linux, host and guest are the same OS family, so the guest
   is no longer a *different platform* — `os` would read `"linux"` on both sides of the boundary and
   the Shape A silent-mismatch of §3.2 would not arise in the same form. The mismatch would move to
   `arch` and to distro/libc, which is a smaller surface but not an empty one.

**Net: Linux is where this question becomes answerable, and it is not answerable yet.** Recommending
a sandbox for the Linux lane now would be recommending against a constraint (item 1) that has never
been measured — which is the confounded-probe failure of `path-b.md` §0.1 repeated with a new
subject.

---

## 7. Recommendation

**Do not build this.** Not per-instance, not per-daemon, not on macOS, and not on Linux yet.

The evidence points there for one reason that is structural and three that are expensive:

- **Structural:** microsandbox's guest is Linux; the credential is in the macOS Keychain; the only
  bridges either hand the guest the credential (defeating the point) or require pmux to MITM the
  operator's Anthropic session.
- Adoption would make `RequireTested` either pass silently on an unmeasured cell (Shape A) or refuse
  every Path B start until a Linux cell is promoted from 53 committed, irreplaceable attempts
  (Shape B).
- It would put transcript authority behind virtio-fs and convert a process-table reaping proof into
  an assertion about a VM's lifecycle, with detached sandboxes adding a child that outlives `pmuxd`.
- It is beta, it needs a host-side install outside cargo, and its SDK's tree is not small.

Note what is *not* on that list: mint cost. §3.4 clears it. This should not be rejected for being
slow, because it is not.

**Build these two things instead. Neither is a sandbox; neither touches auth, the PTY, or the
compatibility key; neither adds a dependency.**

**7.1 — Fingerprint the child binary at launch. Highest value-per-risk item in this spike.**

This project already fingerprints every executable it builds. `evidence/model-attempt-ledger.ndjson`
records, for **52 of its 77 rows**, a `binaries` map over `claude-p`, `pmux`, `pmux-hook`,
`pmux-launcher`, `pmux-mcp`, `pmux-rmuxd` and `pmuxd`, each with `sha256`, `device`, `inode`, `mode`,
`link_count`, `size`, `uid`, `modified_ns`, `changed_ns`. `executable_identity`
(`crates/e2e/tests/full_stack.rs:4033-4043`) computes `sha256`, `device`, `inode`, `length`, `mode`
and `links` for one path, and its caller (`:3962-3983`) applies it to eight executables and asserts
they are distinct files — "candidate names must identify distinct executable files" (`:3982`).

**The one binary that gets none of this is the one pmux actually launches.** `LaunchSpec::validate`
checks that `executable` is absolute and nothing else. So pmux records byte-exact identity for the
seven programs it wrote and version-string identity for the program it hands the operator's
credential to.

Recording `(sha256, device, inode, mode, size)` for the resolved `claude` executable at launch, in
the same shape `executable_identity` already produces, and publishing it beside `claude_version`,
would: give the supply-chain threat of §4 an *observable*, which it currently does not have; make
"the binary changed under a running pool" a detectable event; and cost approximately the code that
exists. Whether it should also *refuse* on an unexpected digest is a separate product decision and a
harder one — an operator's `claude` legitimately auto-updates — and this spike does not propose it.
**Measurement first, policy later.** That ordering is the one §0.3 of `path-b.md` mandates.

**7.2 — Measure where credentialed Claude Code on Linux keeps its credential.**

One probe. No live-model attempts spent — it is a filesystem and keyring observation, not a turn. It
is the hinge for item 1 of §6, and it converts the Linux sandbox question from unanswerable to
answerable. Run it under the §0.3 rules: one variable, a positive control (the macOS result in §3.1
is exactly that control), and prefer the mechanism to the outcome — read the bundle's storage
accessor, not just the resulting file.

**What would reopen this question.** All three, not any one:

1. microsandbox out of beta, with a stable Rust SDK surface.
2. A promoted Linux compatibility cell, so `RequireTested` has something to match.
3. A measured Linux credential path that a guest can be given **without** handing it the operator's
   machine-wide identity.

Until all three hold, `docs/current-state.md` §9.7 Row S1 stands exactly as written, and this
document is its evidence rather than its replacement.

---

## 8. What this document does not establish

Recorded here so no later reader mistakes a gap for a finding.

- **Nothing here was measured against microsandbox.** It was never installed, built, or run. Every
  §1 claim is VERIFIED-EXTERNAL — its authors' description of their own software.
- **The per-VM memory overhead V is unmeasured.** §3.4's table is a sensitivity analysis over an
  unknown parameter, not a result. The pool-size answer is a range because the input is a range.
- **The guest-readiness term is unmeasured**, and §3.4's conclusion depends on it. If Claude Code is
  materially slower to become ready in a Linux guest, the mint criterion flips from cleared to
  contested.
- **Whether Claude Code's credential sits in a position microsandbox's secret substitution can
  reach is unverified.** §3.1(b) is rejected on four other grounds, so the answer does not change
  the conclusion — but it is not known.
- **Whether Claude's new-inode-per-write transcript pattern survives virtio-fs is unverified.** It is
  the premise `CompleteLine` framing rests on, and this spike could not test it.
- **The Linux credential mechanism is unverified**, which is exactly why §7.2 exists.
- **No claim is made about `--disallowedTools "*"` under a different Claude version.** The 29,272 /
  182-229 measurement is 2.1.220 on macOS/aarch64, like everything else here.
- **This document changed no other file.** `current-state.md` §9.7 carries no pointer to it; adding
  one is a separate decision for whoever owns that file.
