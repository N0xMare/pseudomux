# current-state.md

**Position of pmux, 2026-09-01.** This file is normative for *where the project
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
2.1.257** (`evidence/promotion-2.1.257-linux-x86_64.json`). Operator-eval
`evidence/linux-operator-eval-2.1.257-x86_64.json` is the pin-confirmation
receipt and `evidence/linux-model-matrix-2.1.257-x86_64.json` is the
model/effort probe. Earlier 2.1.233 and 2.1.236 receipts remain historical:

| Receipt | What it showed |
| --- | --- |
| `evidence/linux-minified-noclear-cache-x86_64.json` | Path A minified, no `/clear`: T2 `cache_read` matches T1 write |
| `evidence/linux-pool-leased-sticky-x86_64.json` | Pool `Leased`: T1 write 1733 / T2 read 1733, same `s{slot}e{epoch}` |
| `evidence/linux-messages-sticky-eval-x86_64.json` | HTTP Messages sticky, cache hit above the ~1024-token floor |
| `evidence/linux-pi-agentic-subagent-x86_64.json` | Pi agentic + sequential + parallel subagents, all GREEN (2.1.233) |
| `evidence/linux-pi-agentic-subagent-2.1.257-x86_64.json` | Same three Pi scenarios on the promoted 2.1.257 cell with no operator flag, all GREEN; release returns the same pids to idle through `/clear` |
| `evidence/linux-minified-system-remainder-2.1.236-x86_64.json` | REPLACE displacer: TUI `pmux run` billed 199 cold / 288 after `/clear`, cache 0. Leftover envelope is hundreds (chars/4 bound 265 after `/clear`), not the 29k tool surface. Not a dump of the API body. |
| `evidence/linux-minified-system-body-2.1.236-x86_64.json` | Live `/v1/messages` body. Armed Sonnet turn: billing header, `You are Claude Code…` identity (not REPLACE), displacer, user `<system-reminder>` (`userEmail`, `currentDate`), `<total_tokens>` reminder. Tools/CLAUDE.md/git/cwd absent. |

### Linux admission

`PROMOTED_PROFILES` ships **two** cells: Claude Code 2.1.220 through
2.1.258 on macos/aarch64, pooled drain 1000 ms; and 2.1.227 through
2.1.257 on linux/x86_64, pooled drain 250 ms. Both transparent/sdk.
macos ceiling receipt is `evidence/promotion-2.1.258-macos-aarch64.json`;
pin-confirmation is `evidence/macos-operator-eval-2.1.258-aarch64.json`.
`evidence/promotion-2.1.238-macos-aarch64.json` and
`evidence/macos-operator-eval-2.1.238-aarch64.json` are the prior macos
ceiling and pin, and stay historical.
linux ceiling receipt is `evidence/promotion-2.1.257-linux-x86_64.json`;
pin-confirmation is `evidence/linux-operator-eval-2.1.257-x86_64.json`.
A macos PATH `claude` at 2.1.258 and a linux one at 2.1.257 are each inside
their own cell and need no flag. A PATH Claude newer than either ceiling
still needs `--tested-claude-profile`.

The linux drain is `evidence/pooled-transcript-drain-linux-x86_64.json`:
191 reachable Path B arrivals over 2.1.227/2.1.232/2.1.233, max 118 ms,
estimator 250 ms. Every named version's own fit is also 250 ms because
118×2.0=236 sits inside the 250 ms rounding quantum — that is saturation,
not a one-version fit. The paid ceiling is
`evidence/promotion-2.1.257-linux-x86_64.json` (`pmux run` grades,
emptiness after `/clear`, 5 reachable arrivals at 2.1.257, max 39 ms,
median 35). `evidence/promotion-2.1.236-linux-x86_64.json` is the prior
ceiling and stays historical.

`evidence/linux-minified-post-answer-x86_64.json` remains the fast-path
46 ms pin, **not** the promotion drain. Do not treat the macos
2.1.220..=2.1.258 range as covering Linux.

### 2.1.257 drift

Four changes between 2.1.236 and 2.1.257 needed code, not only a wider range.

1. **Transcript rows.** New attachment `remote_session_change`; new records
   `atis-latch` (launch preamble) and `cost-state` (after every turn). Until
   the parser admitted them, every `pmux run` failed with `SchemaDrift`.
2. **Slash-command menu geometry.** The menu now renders *above* the composer.
   The `/clear` selection proof knew only the 2.1.220 below-composer geometry
   and refused `menu_not_rendered`, so the post-turn `/clear` failed and the
   pool destroyed and re-minted a cell per turn. 2.1.236 fails the same way
   (measured), so the linux 2.1.227..=2.1.236 promotion was running per-turn
   remint, not `/clear`; the differing `pids_after_each_turn` in
   `evidence/promotion-2.1.236-linux-x86_64.json` was that symptom. The proof
   now accepts both geometries (`driver_io.rs`, corpus
   `crates/service/tests/corpus/claude-2.1.257-clear-menu.ndjson`), and
   `pool/mod.rs::finish_turn` emits a `tracing::warn!` (`a stateless instance
   failed to clear after its turn and will be replaced`) so a failed clear is
   visible in pmuxd's log. At 2.1.257 one claude pid served four consecutive
   `pmux run` turns.
3. **Remote Control.** Pool cells auto-started the claude.ai Remote Control
   bridge (`/rc active`, a `bridge-session` record, a `remote_session_change`
   attachment carrying the claude.ai session URL), about 200 extra input tokens
   per turn: 498 input with the bridge, 289 without, on the same one-line
   prompt. No env var or CLI flag disables it; pmux seeds
   `remoteControlAtStartup: false` and `disableRemoteControl: true` in the
   cell's private `settings.json` (`config_isolation.rs`).
4. **Model table.** `claude-fable-5` is replaced by `claude-fable-5-1`
   (aliases `fable`, `fable-5-1`, `fable-5.1`); 2.1.257's own `fable` alias
   resolves to `claude-fable-5-1`. Claude Code still knows `claude-fable-5` and
   adds `claude-mythos-5-1`; neither is in pmux's `MODEL_TABLE`.

The free 2.1.236 -> 2.1.257 launch-bundle A/B moved nothing: every launch flag
parses identically, the effort vocabulary is unchanged
(`low`/`medium`/`high`/`xhigh`/`max`), and no option was removed. The new
options are `--restricted`, `--system-prompt-snapshot`, and the
background-session commands.

### 2.1.258 (macos)

Measured on this MacBook Pro (macos/aarch64) on 2026-09-01. **Nothing Claude
Code ships changed shape for pmux.** 2.1.258 introduced no transcript row kind beyond the
four 2.1.257 already forced into the parser (`atis-latch`, `cost-state`,
`ai-title`, `remote_session_change`), and the slash-command menu has the same
above-composer geometry as 2.1.238 macos and 2.1.257 linux, so the `/clear`
selection proof needed no change. The launch-bundle A/B 2.1.251 -> 2.1.258
adds the `--system-prompt-snapshot` option that 2.1.257 already introduced
and rewords the background `--resume` help; every minified launch flag is
still accepted.

The promotion therefore only widened the macos cell from 2.1.238 to 2.1.258 on
the unchanged pooled 1000 ms drain: 5 reachable post-answer arrivals, max
42 ms, median 25, min 19; per-version fit 250 ms published and not shipped.
One claude pid served four consecutive same-class turns through real `/clear`
recycles, and the fifth turn (effort high, a different class) took a second
cell. `tools/dev/model_matrix.py` answered 48/48 rows with zero
`reported_model` mismatches — the first macos model-matrix receipt.

| Receipt | What it showed |
| --- | --- |
| `evidence/promotion-2.1.258-macos-aarch64.json` | Paid macos ceiling: verdict promotable, floor 2.1.220, tested through 2.1.258, 5 reachable arrivals max 42 ms against the pooled 1000 ms bound. |
| `evidence/macos-operator-eval-2.1.258-aarch64.json` | `GREEN_OPERATOR` pin confirmation: grades exact at sonnet-5 low and high, Messages sticky on the same cell `s0e0`, cache write 1914 / read 1914. |
| `evidence/macos-model-matrix-2.1.258-aarch64.json` | `GREEN_MATRIX`: 48/48 rows answered, 0 `reported_model` mismatches, pool never halted. |
| `evidence/macos-pi-agentic-subagent-2.1.258-aarch64.json` | Pi 0.84.4 + pi-subagents 0.63.0 through `examples/pi/pmux.ts` on the promoted 2.1.258 macos cell with no operator flag, against the recommended 15-cell warm set: agentic (read/write/bash, `AGENTIC_OK`), one sequential reviewer subagent (`MULTI_OK`), two parallel reviewer subagents (`PARALLEL_OK`, `in_flight_max` 3 on three distinct cells), all GREEN; cache hit on every post-first turn; every reviewer child exit 0 on `pmux/claude-opus-5-xhigh`; leases release within 2 s of Pi exit, the same 15 claude pids stay live, every cell at epoch 0, zero failed-clear warnings. 21 real turns (14 root, 7 child). Two facade defects this eval found and fixed before its final run are in §2 "2.1.258 (macos)". |

**Two Messages-facade defects the Pi eval exposed, both fixed here.** Neither
is a Claude Code change; both were latent and surfaced because the harness
side moved (pi-subagents 0.50.0 -> 0.63.0) and because a model turn happened
to take a shape the parser had never met.

1. **Response `model` was the canonical stem.** `POST /v1/messages` answered
   `"model":"claude-opus-5"` for a request that sent `claude-opus-5-xhigh`.
   pi-subagents 0.63 verifies the model a child reports against the launch
   candidate it configured (accepting the bare leaf id), so every reviewer
   child finished its review and then exited 1 with
   `model_verification_failed: expected 'pmux/claude-opus-5-xhigh' ... observed
   'claude-opus-5'`. The facade now echoes the request's `model` string byte
   for byte (`conversation.rs::LeaseTurn::requested_model`,
   `messages_http.rs::tests::the_response_model_is_the_requested_id_not_the_canonical_stem`);
   the canonical stem stays in `StatelessResult::model`. `docs/spec.md` §3.1
   states the rule.
2. **A tool call with raw newlines inside a JSON string fell through as text.**
   opus-5/medium emitted `<tool_call>{"name":"write",...,"content":"# Review\n\n..."}</tool_call>`
   with real newlines where `\n` escapes belong. Strict JSON refuses a control
   character inside a string, so the whole block reached Pi as literal text
   and `review.md` was never written. `parse_completion` now retries a payload
   that fails strict parsing after escaping control characters that sit inside
   string literals only (`messages_http.rs::escape_control_characters_in_strings`;
   `not-json` still stays text), and the `TOOLS:` primer now says the payload
   is strict JSON with `\n` escaped. Tests
   `a_tool_call_whose_string_carries_raw_newlines_is_still_a_tool_call` and
   `control_character_escaping_touches_only_string_interiors`.

The Pi harness for this receipt also had to gate on the children's own exit
codes: the scenario verdicts (files written, final token text) were GREEN on
the run whose every child had exited 1.

Hygiene the same change carried: `crates/rmux/tests/attach_fragmentation.rs`
stopped compiling on macOS after the linux clippy fix passed `&size` to
`openpty`, which takes `*mut winsize` on Apple's libc and `*const` on glibc;
it is now `&raw mut size`, which coerces to both.

### Recommended Pi warm set

At the owner-set cap of 15:

```text
--pool-size 15
--pool-warm claude-opus-5/medium=12
--pool-warm claude-opus-5/xhigh=2
--pool-warm claude-fable-5-1/xhigh=1
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
| Sticky `Leased` + Messages harness | Shipped, opt-in. macos sticky pin `evidence/macos-operator-eval-2.1.258-aarch64.json`; linux sticky pin `evidence/linux-operator-eval-2.1.257-x86_64.json`, both also measured with Pi. |
| Promoted cell without a flag | macos/aarch64 2.1.220..=2.1.258 and linux/x86_64 2.1.227..=2.1.257. |
| Linux without a flag | **Shipped** for 2.1.227..=2.1.257. This host's PATH `claude` is 2.1.257, inside the ceiling. |
| Interactive session product | **Removed.** Public wire refused. CLI is `run` / `ping` / `doctor`. Mint via `start_session_owned_with_retention` stays (`start_session_owned` is the pool wrapper). |
| `native.rs` split / `step()` simplify | **Not done.** Idle-is-proof stays. |
| `tools/dev/check.sh` | Living commit/push check. `--push` adds e2e + process blackbox. |
| `tools/dev/operator_eval.py` | Confirms a Claude binary on **this** OS (grades + Messages sticky). No pooled drain required. Does not edit `PROMOTED_PROFILES`. |
| `tools/dev/model_matrix.py` | One real turn per admitted `(model, effort)` cell on **this** OS. Gates nothing; edits neither `MODEL_TABLE` nor `PROMOTED_PROFILES`. |
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
