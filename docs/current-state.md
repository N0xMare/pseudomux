# current-state.md

**Position of pmux, 2026-08-18.** This file is normative for *where the project
stands*. `spec.md` is normative for product behaviour. `testing.md` is
normative for test ownership. `docs/path-b.md` is normative for the Path B
pool.

The 3,618-line 2026-08 essay that previously lived here is archived at
`docs/archive/current-state-2026-08.md`. The historical commit-message ledger
remains `docs/defect-log.md`. Neither archive is a CURRENT Path B document.

---

## 1. What pmux is

A local control plane that drives the **real interactive Claude Code TUI**
inside a private rmux PTY sidecar and treats **Claude's project JSONL** as the
sole semantic authority.

Two products share one binary:

| Product | Caller names | What happens |
| --- | --- | --- |
| **Path B** — the product | `(model, effort, prompt)` only | A warm minified cell answers; `/clear` recycles it. `pmux run` and MCP `run_stateless`. |
| **Path A** — interactive sessions | cwd, Claude binary, config root, session id | Full tool surface, attach, stored agents. Always compiled and served. |

Public **control** is an explicit owner-only Unix-domain socket. There is no
discovery and no client autostart. An **optional** loopback Anthropic Messages
facade (`--path-b-messages-bind`) sits in front of Path B so a harness such as
Pi can own tools and context. That listener is off unless given, loopback-only,
and is not a general HTTP/TCP control plane.

Windows and print-mode Claude are unsupported.

---

## 2. Path B as a harness engine

A Path B cell is `SessionCell::Minified`: `--disallowedTools *`, private
config root, empty cwd, REPLACE system prompt. The pool keys instances by
`(canonical model, effort argv)`. Membership in the idle set **is** the
emptiness proof. `/clear` is the recycle; remint happens only when
`turns_started` hits the recycle cap, at lease end, not mid-conversation.

### Sticky leases

`InstanceState::Leased` is a sixth live bucket. A Messages conversation pins
one instance: between turns the cell is not idle, not stealable by `pmux run`,
and not `/clear`ed. `x-pmux-conversation` is the pin; `x-pmux-cell` is
`s{slot}e{epoch}` (never a Claude `SessionId`). Release is
`POST /v1/conversations/{id}/release`; idle TTL is the backstop.

`pmux doctor`'s pool layer reports `leased` and `conversation_leases`
(`conversation`, `cell`, `state`). Occupancy for multi-agent is
`live − idle` = leased + in-flight.

### Messages facade

`--path-b-messages-bind HOST:PORT` binds loopback only. Auth is
**presence-only**: any non-empty `x-api-key` or `Authorization` is accepted;
loopback is the trust boundary. The first turn is flattened into a primer;
later turns type only the new suffix so Anthropic's prompt cache can hit.
Claude's tool surface stays denied; the harness runs tools and sends
`tool_result`. Token streaming is reconstructed after the turn commits.

Measured on this Linux host (Claude Code 2.1.233, operator profile):

| Receipt | What it showed |
| --- | --- |
| `evidence/linux-minified-noclear-cache-x86_64.json` | Path A minified, no `/clear`: T2 `cache_read` matches T1 write |
| `evidence/linux-pool-leased-sticky-x86_64.json` | Pool `Leased`: T1 write 1733 / T2 read 1733, same `s{slot}e{epoch}` |
| `evidence/linux-messages-sticky-eval-x86_64.json` | HTTP Messages sticky, cache hit above the ~1024-token floor |
| `evidence/linux-pi-agentic-subagent-x86_64.json` | Pi agentic + sequential + parallel subagents, all GREEN |

### Linux admission

`PROMOTED_PROFILES` still ships **one** cell: Claude Code 2.1.220 through
2.1.227 on macos/aarch64, transparent/sdk, pooled drain 1000 ms. Linux
2.1.233 is an **operator cell**, not a product promotion. An operator
invocation passes `--tested-claude-profile` with drain 250 ms (the linux
minified estimator, **not** a pooled promotion receipt). The triad
`promotion-2.1.233-linux-x86_64.json` /
`promoted-profile-<floor>-linux-x86_64.json` /
`pooled-transcript-drain-linux-x86_64.json` does not exist. Do not add a
linux `PromotedProfile` until that triad exists and
`promote_claude_version.py` can name a per-platform floor.

`evidence/linux-minified-post-answer-x86_64.json` is pinned as **not** a
promotion drain receipt (max 46 ms, recommend 250, versions 2.1.227 and
2.1.232). Shipping 250 as a "pooled" bound would be vacuous: both named
versions fit 250.

### Recommended Pi warm set

At the owner-set cap of 15:

```text
--path-b-pool-size 15
--path-b-warm claude-opus-5/medium=12
--path-b-warm claude-opus-5/xhigh=2
--path-b-warm claude-fable-5/xhigh=1
```

One cell per live Pi conversation (root + each live subagent). Agent end is
`/clear` (release), not remint. Spawn/steer/delete stay Pi orchestration.

Path A is not being deleted. Pool mint still goes
`start_session_pool` → `start_session_owned`.

---

## 3. What is done, and what is not

| Dimension | Status |
| --- | --- |
| Path B pool + `/clear` recycle | Shipped. `pmux run` / MCP `run_stateless`. |
| Sticky `Leased` + Messages facade | Shipped, opt-in, measured on linux/x86_64 with Pi. |
| Promoted cell without a flag | macos/aarch64 2.1.220..=2.1.227 only. |
| Linux 2.1.233 without a flag | **Not shipped.** Operator profile. |
| Path A deletion | **Not started, not planned in this landing.** |
| `native.rs` split / `step()` simplify | **Not done.** Idle-is-proof stays. |
| Gate A on this tree | Not re-run as a full capture. Targeted tests are the claim. |
| Gate B / Gate C | Unchanged from the archive. |

---

## 4. What a future change must not break

- Transcript is authority; the screen is a veto, never a vote.
- The caller of Path B names no resource. Messages may name a *conversation
  id*, which is a harness session token, not a Claude `SessionId` and not a
  filesystem path.
- Default daemon: owner-only UDS, no INET. Messages stays opt-in loopback.
- `/clear` only at lease end (or TTL), never after every HTTP request.
- `PROMOTED_PROFILES` entries require the receipt triad. Do not invent one.
- Do not delete `start_session_owned`. Do not split `native.rs` as a drive-by.

---

## 9. Design debt (mechanical contracts)

The long debt ledger is in the archive. Two headings below are **load-bearing**
for `scripts/path_b_done.py` criterion 4 and Gate A `test_run_gate.py`. Do not
rename them without co-editing those tools.

### 9.4 Post-commit findings still open (C6)

| # | file:line · defect · cost of leaving it · risk | Δ | Disposition |
|---:|---|---:|---|
| **C6** | `tools/linux-docker/gate-a-manifest.json` + `suite.sh:452-457` · Linux Docker lane still red on `linux_docker_self_tests` because the container manifest was not re-projected when the host manifest was trimmed · **KNOWN-REGRESSION** · **NEEDS-CARE** | ~7 cells | Open. Full finding and the two Gate C tests that block a drive-by repair are in `docs/archive/current-state-2026-08.md` §9.4. Closing C6 is a Gate C decision. |

### 9.5 REJECTED / NONCLAIM — advisory rows 42-56 (NEVER pre-v1)

Unchanged. See the archive.

### 9.29 THE BUG CLASS, instance thirty-three — a reordering "verified gate-equivalent" for one property, and a revision that was never a mutation counter

The ledger has now found thirty-three times. The Rust sites that spell that
sentence (`crates/protocol/src/v1.rs`, `crates/service/src/pool/mod.rs`,
`crates/service/tests/agent_resource.rs`) still agree with this heading.
The body of instance thirty-three is in the archive at the same heading.
