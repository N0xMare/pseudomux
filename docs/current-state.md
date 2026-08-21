# current-state.md

**Position of pmux, 2026-08-21.** This file is normative for *where the project
stands*. `spec.md` is normative for product behaviour. `testing.md` is
normative for test ownership. The product is the local API (Messages +
`run_stateless`) over a warm pool of constrained Claude cells.

The 3,618-line 2026-08 essay that previously lived here is archived at
`docs/archive/current-state-2026-08.md`. The historical commit-message ledger
remains `docs/defect-log.md`. Neither archive is a CURRENT Path B document.

---

## 1. What pmux is

A local token engine: a warm pool of **real interactive Claude Code TUIs**
inside a private rmux PTY sidecar. **Claude's project JSONL** is the sole
semantic authority. A harness such as Pi owns tools and context. pmux owns
the cells.

| Surface | Caller names | What happens |
| --- | --- | --- |
| **Messages harness contract** — the product | pin + release + class | One leased minified cell per conversation. `/clear` on release. `--messages-bind`. |
| **`pmux run` / MCP `run_stateless`** | `(model, effort, prompt)` only | A warm minified cell answers; `/clear` recycles it. |
| **Interactive sessions** | — | Not a product. Public wire refused. Thin CLI is `run` / `ping` / `doctor`. |

Internal engineering docs still say Path A / Path B for the session stack
and the pool. Those names are not the product surface.

Public **control** is an explicit owner-only Unix-domain socket. There is no
discovery and no client autostart. The Messages listener is off unless given,
loopback-only, and is not a general HTTP/TCP control plane.

Windows and print-mode Claude are unsupported.

---

## 2. Path B as a harness engine

A Path B cell is `SessionCell::Minified`: `--disallowedTools *`, private
config root, empty cwd, REPLACE system prompt (`The user message is the
entire instruction.` — displaces Claude Code's default agent prompt; not
consumer policy). The pool keys instances by
`(canonical model, effort argv)`. Membership in the idle set **is** the
emptiness proof. `/clear` is the recycle; remint happens only when
`turns_started` hits the recycle cap, at lease end, not mid-conversation.

### Sticky leases

`InstanceState::Leased` is a sixth live bucket. A Messages conversation pins
one instance: between turns the cell is not idle, not stealable by `pmux run`,
and not `/clear`ed. `x-pmux-conversation` is the pin; `x-pmux-cell` is
`s{slot}e{epoch}` (never a Claude `SessionId`). Release is
`POST /v1/conversations/{id}/release`; idle TTL is the backstop.
The pool clock owns that TTL (`idle_since_ms`); a replay is activity.
A book row with no pool cell is an orphan and does not occupy the lease cap.

`pmux doctor`'s pool layer reports `leased` and `conversation_leases`
(`conversation`, `cell`, `state`). Conversations holding a cell are
those rows: each is `Leased`, `CheckedOut`, or `Delivering`. Census
`leased` is only the between-turns bucket; `in_flight` is a turn in
progress. Do not compute occupancy as `live − idle`: `live` also
includes clearing, reserved, and tearing_down.

### Messages facade

`--messages-bind HOST:PORT` binds loopback only. Auth is
**presence-only**: any non-empty `x-api-key` or `Authorization` is accepted;
loopback is the trust boundary. A conversation pin
(`x-pmux-conversation`) is required; `--messages-allow-implicit`
is the single-session opt-in. `GET /v1/models` and `GET /v1/capabilities`
advertise the closed set. The first turn is flattened into a primer;
later turns type only the new suffix so Anthropic's prompt cache can hit.
Claude's tool surface stays denied; the harness runs tools and sends
`tool_result`. Token streaming is reconstructed after the turn commits.

Measured on this Linux host. Promoted linux cell is **2.1.227 through
2.1.236** (`evidence/promotion-2.1.236-linux-x86_64.json`). Operator-eval
`evidence/linux-operator-eval-2.1.236-x86_64.json` remains the pin-confirmation
receipt. Earlier 2.1.233 receipts remain historical:

| Receipt | What it showed |
| --- | --- |
| `evidence/linux-minified-noclear-cache-x86_64.json` | Path A minified, no `/clear`: T2 `cache_read` matches T1 write |
| `evidence/linux-pool-leased-sticky-x86_64.json` | Pool `Leased`: T1 write 1733 / T2 read 1733, same `s{slot}e{epoch}` |
| `evidence/linux-messages-sticky-eval-x86_64.json` | HTTP Messages sticky, cache hit above the ~1024-token floor |
| `evidence/linux-pi-agentic-subagent-x86_64.json` | Pi agentic + sequential + parallel subagents, all GREEN |
| `evidence/linux-minified-system-remainder-2.1.236-x86_64.json` | REPLACE displacer: TUI `pmux run` billed 199 cold / 288 after `/clear`, cache 0. Leftover envelope is hundreds (chars/4 bound 265 after `/clear`), not the 29k tool surface. Not a dump of the API body. |
| `evidence/linux-minified-system-body-2.1.236-x86_64.json` | Live `/v1/messages` body. Armed Sonnet turn: billing header, `You are Claude Code…` identity (not REPLACE), displacer, user `<system-reminder>` (`userEmail`, `currentDate`), `<total_tokens>` reminder. Tools/CLAUDE.md/git/cwd absent. |

### Linux admission

`PROMOTED_PROFILES` ships **two** cells: Claude Code 2.1.220 through
2.1.238 on macos/aarch64, pooled drain 1000 ms; and 2.1.227 through
2.1.236 on linux/x86_64, pooled drain 250 ms. Both transparent/sdk.
macos ceiling receipt is `evidence/promotion-2.1.238-macos-aarch64.json`;
pin-confirmation is `evidence/macos-operator-eval-2.1.238-aarch64.json`.
A linux PATH Claude newer than 2.1.236 still needs `--tested-claude-profile`.
macos PATH 2.1.238 does not.

The linux drain is `evidence/pooled-transcript-drain-linux-x86_64.json`:
191 reachable Path B arrivals over 2.1.227/2.1.232/2.1.233, max 118 ms,
estimator 250 ms. Every named version's own fit is also 250 ms because
118×2.0=236 sits inside the 250 ms rounding quantum — that is saturation,
not a one-version fit. The paid ceiling is
`evidence/promotion-2.1.236-linux-x86_64.json` (`pmux run` grades,
emptiness after `/clear`, 5 reachable arrivals at 2.1.236 max 46 ms).

`evidence/linux-minified-post-answer-x86_64.json` remains the fast-path
46 ms pin, **not** the promotion drain. Do not treat the macos
2.1.220..=2.1.238 range as covering Linux.

### Recommended Pi warm set

At the owner-set cap of 15:

```text
--pool-size 15
--pool-warm claude-opus-5/medium=12
--pool-warm claude-opus-5/xhigh=2
--pool-warm claude-fable-5/xhigh=1
```

One cell per live Pi conversation (root + each live subagent) when each
conversation is its own process. The shipped adapter holds one
`conversationId` per process. Agent end is `/clear` (release), not remint.
Spawn/steer/delete stay Pi orchestration.

The public session surface is refused. Pool mint still goes
`start_session_pool` → `start_session_owned` →
`start_session_owned_with_retention`. Public `start_session` is not the
mint.

---

## 3. What is done, and what is not

| Dimension | Status |
| --- | --- |
| Pool + `/clear` recycle | Shipped. `pmux run` / MCP `run_stateless`. |
| Sticky `Leased` + Messages harness | Shipped, opt-in. macos sticky pin `evidence/macos-operator-eval-2.1.238-aarch64.json`; linux/x86_64 also measured with Pi. |
| Promoted cell without a flag | macos/aarch64 2.1.220..=2.1.238 and linux/x86_64 2.1.227..=2.1.236. |
| Linux without a flag | **Shipped** for 2.1.227..=2.1.236. PATH 2.1.237 is outside the ceiling. |
| Interactive session product | **Removed.** Public wire refused. CLI is `run` / `ping` / `doctor`. Mint via `start_session_owned_with_retention` stays (`start_session_owned` is the pool wrapper). |
| `native.rs` split / `step()` simplify | **Not done.** Idle-is-proof stays. |
| `tools/dev/check.sh` | Living commit/push check. `--push` adds e2e + process blackbox. |
| `tools/dev/operator_eval.py` | Confirms a Claude binary on **this** OS (grades + Messages sticky). No pooled drain required. Does not edit `PROMOTED_PROFILES`. |
| `tools/dev/promote.py` | Drops `--tested-claude-profile` only when `evidence/pooled-transcript-drain-<os>-<arch>.json` already exists. linux/x86_64 and macos/aarch64 both have one. |
| Gate A | **Removed.** Living verification is `tools/dev/`. |
| Phase 0 / linux-docker / package-smoke | **Removed.** Historical freeze envelope, not a living pin. |

---

## 4. What a future change must not break

- Transcript is authority; the screen is a veto, never a vote.
- The caller of Path B names no resource. Messages may name a *conversation
  id*, which is a harness session token, not a Claude `SessionId` and not a
  filesystem path.
- Default daemon: owner-only UDS, no INET. Messages stays opt-in loopback.
- `/clear` only at lease end (or TTL), never after every HTTP request.
- `PROMOTED_PROFILES` entries require the receipt triad. Do not invent one.
- Do not delete `start_session_owned_with_retention` or
  `start_session_owned`. Do not split `native.rs` as a drive-by. Public
  `start_session` is not the mint.

---

## 9. Design debt (mechanical contracts)

The long debt ledger is in the archive. Headings below remain so old trees parse.
Do not rename them without co-editing `scripts/path_b_done.py`. Criterion 4 is
MET because `run_gate.py` is gone and `tools/dev/check.sh` exists; living
verification is `tools/dev/`.

### 9.4 Post-commit findings tombstone (C6)

| # | file:line · defect · cost of leaving it · risk | Δ | Disposition |
|---:|---|---:|---|
| **C6** | linux-docker lane · **TOMBSTONE** · Gate A candidate was already gone; the container lane is now deleted (`tools/linux-docker/` is not in the tree) | — | Lane deleted. Finding remains in `docs/archive/current-state-2026-08.md` §9.4. Not a living Linux pin. |

### 9.5 REJECTED / NONCLAIM — advisory rows 42-56 (NEVER pre-v1)

Unchanged. See the archive.

### 9.29 THE BUG CLASS, instance thirty-three — a reordering "verified gate-equivalent" for one property, and a revision that was never a mutation counter

The ledger has now found thirty-three times. The Rust sites that spell that
sentence (`crates/protocol/src/v1.rs`, `crates/service/src/pool/mod.rs`)
still agree with this heading.
The body of instance thirty-three is in the archive at the same heading.
