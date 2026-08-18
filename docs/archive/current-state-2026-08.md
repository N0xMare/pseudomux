# current-state.md (archived 2026-08-18)

The living position document is `docs/current-state.md`. This file is the
essay as of the Path B Messages landing. It is not edited to stay true.

---

# current-state.md

**The tracked state of pmux v1.** One file, read it and you know where the project stands: what it
is, what exists, what has been measured, what is owed, and what a future change must not break.

`spec.md` is normative for product behavior. `testing.md` is normative for test ownership and
release-gate commands. This file is normative for **position** — it claims nothing that is not
carried by a `file:line`, an exact count, or an exact recorded command result. The 30 pre-v1 review
reports it summarizes live under `.context/plans/review/`, which is **gitignored** (`.gitignore:20`);
nothing here depends on reading them.

---

## 1. What pmux is

A native, **owner-only Unix-domain-socket local API** (protocol v1, 4-byte big-endian framing, 8 MiB
payload bound) that drives the **real interactive `claude` TUI** inside a **private rmux 0.9.0 PTY
sidecar**, and uses **Claude's own project JSONL transcript as the semantic authority** for every
published fact.

- No HTTP, no TCP, no daemon autostart, no Windows, no print-mode driver. Everything fails closed.
- Eight release binaries: seven under `bin/` (`pmuxd`, `pmux`, `pmux-mcp`, `claude-p`, `pmux-rmuxd`,
  `pmux-launcher`, `pmux-hook`) plus `pmux-test-claude` (`crates/e2e/src/bin/pmux-test-claude.rs`),
  the deterministic fake Claude required by the hermetic full-stack and Docker lanes.
- Session state is **memory-only**: zero persistence anywhere in `crates/service/src`. Every durable
  fact lives in Claude's JSONL; resume is a new generation in a new pane. No schema, no migration,
  no corruption-recovery mode, no crash-consistency proof is owed.
- Three external clients (Rust `crates/client`, `clients/typescript`, `clients/python`), all with
  zero runtime dependencies, consuming one shared conformance corpus at `tests/conformance/v1/`.

Normative behavior is in `spec.md` (§3 topology, §4 launch, §5 turn execution, §6 transcript
authority and completion, §7 protocol v1, §8 attach, §9 integration contracts, §10 operations and
security, §13 non-goals). This file does not restate it.

---

## 2. The architecture decision that defines the product

**This is the thing most likely to be broken by a future reviewer, so it is stated precisely.**

pmux has exactly one semantic authority and one liveness gate, and they are different things.

| | Transcript | Terminal screen |
|---|---|---|
| Role | **AUTHORITY** — decides *what is true* | **INDEPENDENTLY REQUIRED LIVENESS GATE** — decides *whether it is safe to act* |
| Power | Sole and total authority over every published fact | Can only ever say "not yet". A veto, never a vote |
| Encoded at | `CompletionAuthority` is a **single-variant enum** — `Transcript` only (`crates/protocol/src/v1.rs:1288-1292`) | one of **nine** `CompletionFactor`s (`crates/service/tests/completion_gate.rs:23-33`) |
| Failure mode of a wrong constant | wrong answer | **total unavailability, never a wrong answer** |

`CompletionAuthority` having one variant makes "the screen became the semantic authority"
**unrepresentable in the wire type**. That is deliberate. Do not add a second variant.

The nine independently-required completion factors (`completion_gate.rs:23-33`):
`PromptAcknowledgement`, `TerminalCandidate`, `StableCursor`, `AtEof`, `NoPartialLine`,
`DrainElapsed`, `ReadyPrompt`, `Quiet`, `ModalAbsent`. The test starts from `all_satisfied()`, blocks
exactly one, drives a real turn through the production `SessionRegistry`, and asserts no turn was
stored — with the message `"{factor:?} was not independently required"`. `ReadyPrompt` and
`ModalAbsent` are the screen's two factors; blocking either blocks completion.

**Why this is the right shape.** The old VTE design put the heuristic in the **answer** position;
this one puts it in the **admission** position. A wrong terminal-geometry constant means
`classify_terminal_snapshot` (`crates/service/src/driver_io.rs:70`) returns `Unknown` forever and
every prompt fails closed with `prompt_not_acknowledged` after the 15 s input gate. That is 100%
unavailability: loud, immediately diagnosable, recoverable by a code change. The old failure was a
plausible string missing a paragraph: silent and unrecoverable.

**Quantified.** Answer extraction went from **1,140 lines** of chrome-stripping heuristics
(`content_filter.rs` 752 + `service/response.rs::strip_prompt_echo` 298 + ~90 lines of row-dedup in
`content_buffer.rs`, all on `origin/main`) to **~10 lines** of `ContentBlock::Text` concatenation at
`crates/claude/src/engine.rs:825-843`:

```rust
for block in &message.blocks {
    if let ContentBlock::Text { text } = block { final_text_blocks.push(text.clone()); }
}
let final_text = final_text_blocks.concat();
```

Every one of those 1,140 deleted lines was a guess about someone else's renderer, and the denylist
grew with every Claude release. `content_filter.rs:257` on `origin/main` literally carried the entry
`"or ~/.claude/skills/ for skills that work in any project"`.

**Corollaries a reviewer must not undo:**

- `spec.md:63-68` states the liveness-gate contract in normative text. It previously said the
  terminal "only corroborates"; that sentence was false and load-bearing, and was corrected in
  `5442bd7`. If it ever reverts, the next competent reviewer reads the gate as redundant with the
  transcript, deletes it, and converts a 100%-unavailability failure mode into a wrong-output one.
- `crates/service/src/hybrid_hooks.rs` buys one boolean and one warning string today and is **not**
  one of the nine factors. **It stays.** It is the only signal in the product independent of *both*
  the transcript and the screen, fired by Claude itself at exactly the moment `ready_prompt` infers
  from pixels — i.e. it is the designated replacement liveness factor if the TUI geometry ever
  fails. `spec.md:878-887` says so.
- The rewrite did not reduce difficulty. It moved difficulty out of an untyped, silent,
  version-coupled domain (`vte` cell extraction) into a typed, loud, structurally checkable one
  (`SchemaDrift`, `TerminalMessageMissingText`, `DuplicateToolResult`). Line counts are not the win.

---

## 3. Where the rewrite stands

| Dimension | Status | Basis |
|---|---|---|
| **Designed** | ~100% | `spec.md` §13 enumerates the non-goals; every one is a decision, not a hole |
| **Built** | ~100% | **zero** `TODO`/`FIXME`/`unimplemented!`/`todo!` in `crates/*/src` + `bin/*/src` (grep, 0 hits); **zero** bare `.unwrap()` in any production path (44 `src/*.rs` files scanned to the first top-level `#[cfg(...test...)] mod`, 0 hits); 8 binaries resolve and hash |
| **Deterministically validated** | ~100% of what deterministic testing can establish | **60 test targets, each passing in isolation, one `cargo test` invocation each.** The single aggregate number this row used to carry (**580 passed / 0 failed / 17 ignored**, after 519 → 544) is **deliberately no longer quoted**: it is not a stable figure on this host, and the per-binary harness that produces it now enumerates targets from `cargo metadata` cross-checked against the root manifest's `members`. It previously enumerated a HAND-WRITTEN array of six packages against a workspace of thirteen — every `bin/` package was absent, so "every one of the N test targets passed" was a true sentence about a set excluding `pmux`, `pmuxd`, `pmux-mcp`, `claude-p`, `pmux-hook`, `pmux-launcher` and `pmux-rmuxd`. **33 targets became 60.** A short list now REFUSES rather than reports |
| **Attested** | **Gate A, on one host** | the receipt of record is `.context/gate-a/receipt-8b59cbf.json`, sha256 `db5bacdeaaaeaaacf633c09a290add1933ee66e307eeb12059a38297a1e4e2d3`, **75 planned / 75 executed / 75 passed / 0 failed**, `source_unchanged: true`. It attests the tree of the commit whose subject is *"C9: a pre-connect regression hung the gate command instead of failing it"* — **not HEAD** — and it **supersedes all three `receipt-20260727*.json` receipts** (§7.1). `.context/` is gitignored (`.gitignore:20`), so **the receipt does not travel with a push**. This is a deterministic-gate attestation on `macOS-15.7.7-arm64` and nothing more — see §7.1 for the four things it does *not* establish |
| **Externally promoted** | **0%** | no promotable Gate B coverage, no Gate C run (§7). **Do not confuse this row with `PROMOTED_PROFILES`** — the word "promoted" is now overloaded and the two senses are unrelated. This row is about *external gate evidence*. The compatibility cell shipped in `compatibility::PROMOTED_PROFILES` (§6.2.1, §10 item 6) is promoted on a **transcript-corpus drain measurement that spends no ledger ordinal**, and it does not move this row |
| **Conceptual / unbuilt** | 0% | no subsystem is missing; 0 open matrix rows need new tests; 0 need product changes |

### Old (`origin/main`, VTE/adapters) vs new (this tree)

Measured with one script — implementation is everything in `*/src/*.rs` before the first top-level
`#[cfg(test)] mod`, the rest is test.

| | OLD | NEW | ratio |
|---|---:|---:|---:|
| Product implementation (Rust) | 9,360 | **22,079** | 2.36× |
| **test : implementation** | **0.43 : 1** | **1.88 : 1** | 4.4× |
| Screen-model implementation | 2,043 (`crates/core/src/vte/`) | **382** (`driver_io.rs` screen paths) | 0.19× |
| Answer extraction / chrome stripping | 1,140 | **~10** (`engine.rs:825-843`) | 0.009× |
| Screen-derived output states | **29** (7 `AgentState` + 13 `SemanticEvent` + 9 `WatchEvent`) | **3** (`TerminalScreenState` — `Ready`, `NeedsInput`, `Unknown` — `driver_io.rs:45-49`) | 0.10× |
| Semantic-authority implementation | 0 | 2,822 (`crates/claude/src`) | new |
| Wire DTOs | 251 | 1,801 (`crates/protocol/src`) | 7.2× |

Read in order, the migration explains itself: **implementation grew 2.36×, the test corpus grew
10.4×.** Of ~50,000 added lines, 25% is implementation and 75% is test. This is not a codebase that
got bigger; it is a codebase that got **pinned**. The screen did not disappear — it shrank 5.3× and
changed job, from screen *semantics* to screen *evidence*.

*(The 22,079 / 1.88:1 figures are the pre-commit measurement in the design advisory. Re-derived on
today's tree with the same rule: **21,966** implementation lines, 8,129 in-file test lines, 34,566
out-of-file test lines across 41 files. The ratio is 1.94:1 today. The conclusion is unchanged.)*

---

## 4. Functionality — what exists and what proves it

| Capability | Owning code path | Owning test |
|---|---|---|
| **Protocol v1 surface** — **12** methods / **12** results / 14 events / 34 error codes, **plus all 23 nested plain-string enums** pinned under `value_enums`. The two methods added since this row last read 10/10/…/18 are `diagnose` and `run_stateless` | `crates/protocol/src/v1.rs` | `tests/conformance/v1/manifest.json` (counts re-verified 2026-08-06 by reading the file: `methods` 12, `results` 12, `events` 14, `error_codes` 34, `value_enums` 23); `crates/protocol/tests/v1_conformance_vectors.rs::{shared_manifest_matches_the_closed_v1_surface,shared_manifest_value_enums_match_the_rust_string_enums}` plus both-direction assertions in TS and Python (testing.md `P-09`). **That first test used to compare the manifest against three string literals**, so it passed with the manifest two methods short of the surface — the "closed v1 surface" it checked was a copy of the manifest in a different syntax. Both lists are now derived from `Request`, `ResponseResult` and `EventPayload` through a wildcard-free match |
| **Path B — the stateless pool** — `Request::RunStateless` reaches `Pool::run`; classes keyed on the argv a process was launched with; the idle set IS the emptiness proof; per-cell private config root and per-instance cwd; recycle, warm floor, TTL sweep, quarantine retention and teardown. `pmux ask` and MCP `run_stateless` in front of it. Reachable with **no** `--tested-claude-profile` on a supported host | `crates/service/src/pool/`, `crates/service/src/stateless.rs`, `crates/service/src/native.rs`, `bin/pmux`, `bin/pmux-mcp` | `crates/service/tests/path_b_pool.rs`; `crates/e2e/tests/pool_concurrency.rs` (13 deterministic waves at 2/5/8/15 concurrent against a real daemon and a real sidecar, plus three real-Claude waves behind `PMUX_POOL_REAL_CLAUDE`); `crates/e2e/tests/cross_cell_contamination.rs`. `docs/path-b.md` §12 is the description of record |
| **Framing** — 4-byte BE, 8 MiB payload bound, allocation-free, shared with the production reader | `v1.rs::admit_native_frame_header`, `NativeFrameAccumulator`; consumed at `bin/pmuxd/src/handler.rs` and `crates/client/src/lib.rs` | `crates/protocol/tests/v1_wire.rs` (split-at-every-offset) |
| **Strict/additive split** — `deny_unknown_fields` on requests, hand-written `Deserialize` closing serde's internally-tagged unit-variant hole; `safe_u64` fences serialize *and* deserialize | `crates/protocol/src/v1.rs` | `v1_wire.rs`, `v1_golden.rs` (decode **and** byte-equal re-serialize), per-object-boundary additive injection |
| **Transcript authority** — locator, cursor, strict parser, message graph, 8-cell stop matrix with API-error precedence, ordered dedup tool correlation, usage-overflow rejection. Complete-line JSONL framing is **type-enforced**, not merely tested (`CompleteLine` → `JsonlParser` → engine) | `crates/claude/src` (2,822 lines) | `crates/claude/tests/transcript_engine.rs`, `cursor_and_parser.rs`, `transcript_properties.rs` (9 proptests against a differential `ReferenceFramer` oracle) |
| **Session actor** — single-owner serialization, one active turn, one immutable dual wall/monotonic deadline rechecked at commit | `crates/service/src/v1/actor.rs` | `crates/service/tests/v1_actor.rs`, `actor_model.rs` (3 proptests driving the **production** registry), `deadline_idempotency.rs` |
| **Generation fencing** — every generation-targeted path is type-fenced | `crates/service/src/v1/registry.rs` | `actor_model.rs`, `v1_actor.rs` |
| **TurnId idempotency** — no-eviction, byte-exact replay, capacity checked *before* any mutation | `crates/service/src/v1/actor.rs` | `deadline_idempotency.rs`, `v1_actor.rs` |
| **Bounded event ring** — 256 events / 16 MiB, surfaces a structured `ReplayGap` **with a snapshot** rather than silently skipping; page maximality proven by refetch | `actor.rs:1885` (`ReplayGap` struct), `crates/protocol/src/v1.rs:1528` (`EventBatch.replay_gap`) | `actor_model.rs` page-maximality property; `crates/client/tests/fake_uds.rs` |
| **Secure launch** — `LaunchBroker` (one-use 30 s token, consumed *before* the expiry check) + `pmux-launcher` doing `env_clear()` + `execve`; `EnvironmentSnapshot` is an **exact replacement** environment built as `allowlist(snapshot) - unset + set - policy_removals + profile_changes` (`claude_launch.rs::build_environment`), so an inherited name reaches Claude only if the allowlist admits it or the caller's `set` states it; secrets are 0600 inline files in a 0700 tempdir, never argv | `crates/service/src/{launch_broker,sensitive_launch,claude_launch}.rs`, `bin/pmux-launcher` | `bin/pmux-launcher/tests/process_blackbox.rs`; child-side argv/env attestation in `crates/e2e/tests/full_stack.rs` |
| **Launch-environment allowlist** — the inherited snapshot term is filtered `unknown-means-denied` before `unset`/`set`/policy/profile; case-sensitive exact names and prefixes; auth-policy aware (provider routing survives `Inherit`, denied under `Subscription`); `set` is the deliberate bypass; every dropped name is reported for audit | `crates/service/src/claude_launch.rs::{INHERITED_EXACT_KEYS,INHERITED_PREFIXES,PROVIDER_ROUTING_PREFIXES,inherited_from_snapshot,build_environment}` | six in-module tests (`an_unknown_inherited_name_is_denied_by_construction`, `the_allowlist_denies_nested_claude_markers_without_help_from_the_denylist`, `every_allowlisted_name_survives_the_snapshot_filter`, `allowlist_prefix_and_exact_matching_are_case_sensitive`, `caller_supplied_set_bypasses_the_allowlist_entirely`, `documented_environment_order_is_allowlist_then_unset_then_set_then_removals`) plus `inherit_retains_provider_routing_and_subscription_denies_it` and `removed_environment_keys_reports_allowlist_drops_and_nothing_else` (testing.md `S-25`, `S-26`) |
| **Prompt admission** — two-gate cursor-correlated editor fence, **exactly one paste**, changed-stable-proven render, **at most one Enter**, **no write retry on ambiguity** | `crates/service/src/driver_io.rs:698-802` | 11 in-module tests asserting exact `(paste_count, enter_count)` pairs: `(0,0)` for every pre-paste failure, `(1,0)` for every post-paste ambiguity, `(1,1)` **only** on a proven render |
| **Needs-input classification** — six-way modal table, structurally subordinate to editor detection; screen contents must not escape the classifier | `driver_io.rs:1440` `blocking_screen`, reached via `classify_terminal_snapshot:69` | `driver_io.rs:1896-1931` (incl. the `"screen contents must not escape the classifier"` leak invariant) |
| **Cancellation + interrupt recovery** — the empty-editor definition doubles as the cancel-recovery proof; a composer left dirty by an aborted admission cannot be mistaken for a recovered session | `driver_io.rs`, `actor.rs` | `crates/service/tests/lifecycle_faults.rs`, `v1_actor.rs`, `full_stack.rs` restart cell |
| **Attach** — one-use reservation, constant-time token compare including length; read-only attach rejected | `crates/service/src/attach.rs` | `crates/rmux/tests/attach_fragmentation.rs`; `bin/pmux-rmuxd/tests/process_blackbox.rs::real_attach_half_close_delivers_the_final_complete_frame_exactly_once` |
| **Close with process-boundary proof** — `getsid(pid)==pid` capture, transitive ppid fixpoint, sticky escape flag, birth-token recycle fence, `Ok(!escaped)` only on an empty member set, re-verified before every `SIGKILL` | `crates/rmux/src/process_boundary.rs` (`is_recycled:363`, `member_identity_still_proven:436`, call site `:411`) | `bin/pmux/tests/process_boundary.rs`; `full_stack.rs::public_close_retry_never_claims_an_observed_escaped_descendant_was_reaped` |
| **Hybrid hooks** — additive-only, identity-fenced; the designated fallback liveness authority | `crates/service/src/hybrid_hooks.rs`, `bin/pmux-hook` | `crates/service/tests/hybrid_hooks.rs`; `bin/pmux-hook/tests/process_blackbox.rs` |
| **Compatibility gate** — a small **promoted** set (`PROMOTED_PROFILES`, one cell: 2.1.220 **through 2.1.227** / macos / aarch64 / transparent / sdk, pooled drain bound 1000 ms) plus the operator's `--tested-claude-profile` cells, which are searched FIRST and therefore override; nothing else is admitted; the range never spans a minor and overlapping cells are refused at boot; `validate_v1_terminal_support` runs first, before any side effect; five named `RepromotionTrigger`s retract a range, each bound to the code that detects it | `crates/service/src/compatibility.rs`, `native.rs`, `evidence/pooled-transcript-drain-macos-aarch64.json`, `evidence/promoted-profile-2.1.220-macos-aarch64.json`, `tools/promotion/measure_transcript_drain.py`, `docs/version-drift.md` | `full_stack.rs::actual_daemon_empty_profile_registry_rejects_without_launching_a_child`; `compatibility.rs::{every_promoted_profile_passes_the_admission_an_operator_profile_must,a_promoted_cell_admits_this_platform_with_no_operator_profile,an_operator_profile_overrides_the_promoted_one_for_the_same_identity,every_promoted_drain_is_the_one_its_receipt_recommends}`; `pool_concurrency.rs::a_promoted_profile_serves_a_real_turn_with_no_operator_flag` |
| **CLI** — 10 commands (`Ping`, `Run`, `Start`, `Turn`, `Inspect`, `Cancel`, `Close`, `Attach`, `Doctor`, `Probe`; `bin/pmux/src/cli.rs:46-117`) × text/json/ndjson through one `emit()` path; exit 2 (clap) / exit 1 (runtime); NDJSON commit marker withheld until `result.process_reaped` | `bin/pmux/src` | `bin/pmux/tests/cli_contract_matrix.rs`, `native_cli.rs`, `process_boundary.rs`, `candidate_binding.rs` |
| **MCP** — exactly 8 strict tools with `additionalProperties:false`, `structuredContent` emitted once, no `pseudomux-rmux` dependency so it physically cannot open the attach byte stream | `bin/pmux-mcp/src/tools.rs:168-219` (8 `tool(` calls) | `bin/pmux-mcp/tests/stdio_blackbox.rs` |
| **`claude-p` facade** — the whole `claude -p` migration path; one `run_once` call, hardcoded empty `extra_args`, print-shaped flags rejected not forwarded, provenance labeled `pmux_interactive_transcript_reconstruction` | `bin/claude-p/src` | `bin/claude-p/tests/facade_blackbox.rs` |
| **Permission bypass, typed and audited** — `PermissionMode::DangerouslySkipPermissions` (wire `dangerously_skip_permissions`, `crates/protocol/src/v1.rs:769-780`) is the one variant whose argv is a **single flag** rather than a `--permission-mode` value; the mapping is a wildcard-free `PermissionModeArgv::{Pair,Single}` enum (`crates/service/src/claude_launch.rs::permission_mode_argv`) so a future variant is a compile error. Every turn of such a session republishes the `dangerous_permission_bypass` warning (`actor.rs:3113-3122`) on the completed **and** cancelled paths | `crates/service/src/claude_launch.rs`, `crates/service/src/v1/actor.rs` | `claude_launch.rs::{dangerously_skip_permissions_is_one_flag_and_no_other_mode_emits_it,dangerous_permission_bypass_has_a_stable_snake_case_wire_value}`; `actor.rs::permission_bypass_is_a_per_turn_result_warning_only_for_bypass_sessions` (testing.md `S-23`, `S-24`) |
| **Client-side profiles** — one JSON document expanded into launch fields **before the request is framed**; `--profile NAME` / `--profile-file PATH` with `PMUX_PROFILE` / `PMUX_PROFILE_FILE` fallbacks (RENAMED from `--agent*`/`PMUX_AGENT*`, whose old spellings are now refused by name because `--agent` means the stored server agent). Scalars replace / absent inherits, lists append parent-first, `extends` bounded at depth 4 with cycle detection, JSON `null` and unknown/duplicate/per-invocation keys rejected. **No `profile_name` wire field, no `cwd`, no discovery** (`spec.md` §4.8.4). There IS now a server-side agent registry (`spec.md` §4.8, amended); a profile authors one via `pmux agent create --from-profile` | `crates/client/src/agent_profile.rs` (605 code lines of 717), `bin/pmux/src/cli.rs:154-163`, `:489-530` | `crates/client/tests/agent_profile.rs` (16 tests); `bin/pmux/src/cli.rs::tests::{an_agent_profile_expands_into_the_start_dto_before_the_request_is_framed,explicit_flags_override_profile_scalars_loudly_and_append_to_profile_lists,agent_selection_is_explicit_on_both_sides}` (testing.md `CLI-12`, `CL-08`, `CL-09`) |
| **Rust client** | `crates/client/src/lib.rs` (1,287 lines) — 9 typed methods 1:1 with the protocol, one `request()` sharing production framing, cross-field result validator, sequence-validating reconnecting event stream; `pub mod agent_profile` re-exported at `:35-40` | `crates/client/tests/fake_uds.rs`, `v1_golden.rs` |
| **TypeScript client** — zero runtime deps | `clients/typescript/src/{client,protocol,smithers}.ts` | `clients/typescript/tests/{client,golden-conformance,dist-stage}.test.mjs` |
| **Python client** — `dependencies = []` | `clients/python/pmux_client/` | `clients/python/tests/{test_client,test_golden_conformance}.py` |
| **Daemon hardening** | `bin/pmuxd/src`: `umask(0o077)` bind guard, chmod 0600 after uid/type recheck, `ensure_private_directory` bails on `mode & 0o077`, `remove_if_same_socket` refuses on dev/ino/uid/type change, `SIGHUP` ignored, `bounded_log.rs` with reserved-capacity records and one-backup rotation | `bin/pmuxd/tests/process_blackbox.rs` |
| **Vendored rmux patch provenance** | `vendor/rmux-{client,server}` | `crates/rmux/tests/vendor_patch.rs`, `vendor_server_patch.rs` — mechanically reconstructs upstream via 15 anchored inverse replacements and verifies a 596-file published-tree hash |

---

## 5. Verified state — exact measurements

Everything in this section is a command that was run, with its exact result.

### Rust

| Command | Result |
|---|---|
| `cargo test --locked --workspace --all-targets --all-features` | **STALE AS A NUMBER — read §3 first.** The aggregate is no longer quoted as a position claim: **61** test targets now pass in isolation, one `cargo test … -- --include-ignored` invocation each, and the aggregate is not a stable figure on this host. **The per-binary harness had the scope defect a THIRD time, latent, and it is fixed (2026-08-06).** Its kind-to-selector mapping ended in a bare `continue`, so a `cargo metadata` target kind nobody had classified — an `example`, a `bench`, a `proc-macro` crate's library — was dropped from the set whose footer then said *"every one of the N test targets passed in isolation"*. `cargo test --all-targets` in the gate would run those; this would not. The mapping is now a table with a reason per entry and **no default**, and an unclassified kind REFUSES. It changes nothing today — this workspace is 47 `test` + 8 `bin` + 6 `lib` targets and the count is the same 61 — which is exactly why it was invisible. Proven by mutation: with the refusal replaced by `continue`, a fabricated `wasm-thing` target is silently dropped and the run exits 0 over the narrower set; with it, the run exits 1 naming the target and the kind (`tools/screen-corpus/per_binary_tests.sh`). **Running it also surfaced a second thing about the same script, and it is the opposite failure: a report that could never say the true sentence.** MEASURED: `1 of 61 targets failed in isolation: pseudomux-e2e/full_stack`, `3 passed; 7 failed`, every failure `PMUX_E2E_BIN_DIR must identify the exact candidate directory` (`crates/e2e/tests/full_stack.rs:3943`) — a variable the script never supplied, so one target was permanently red on a healthy tree and the coverage claim was structurally unreachable. The candidate directory is now **derived** from `cargo metadata`'s own `target_directory`, and the executables that must be in it are **derived** from the workspace's `bin` targets rather than listed (a literal eight here would be the same defect a third time). The second precondition, `PMUX_E2E_TYPESCRIPT_DIST_DIR`, is a staged artifact with a 0700 mode contract and **cannot** be derived — so the script now names it, and what it costs, **before** the table instead of leaving an operator to discover it as a red target forty minutes in. With both supplied, that target is `10 passed; 0 failed` in isolation (108.8 s) and the script prints its own claim for the first time: **`every one of the 61 test targets passed in isolation: 1031 test cases ran, 0 ignored`**, exit 0. Quote that sentence rather than an aggregate `cargo test --workspace` figure — it is the one this workspace can defend. Two operational facts from the per-binary harness are worth keeping: it **builds every target once before the loop** (MEASURED: the first eight targets took fifty minutes and the whole workspace then built in two minutes forty, because each per-target invocation was linking and first-executing a fresh binary and macOS stalls in dyld the first time it runs one), and `cargo test -p pseudomux-e2e` **does not rebuild `pmuxd`** — it is another package's bin target, which made a mutation campaign lie (§10). The historical reading, retained for the delta history: **580 passed, 0 failed, 17 ignored**, re-run to completion (exit 0, 54 `test result:` lines summed). The **544** this row carried until now was the 2026-07-27 agent-profile figure and had gone **36 stale**; **519** was the figure before that work: **+25**, of which 16 are `crates/client/tests/agent_profile.rs`, 4 are the new `bin/pmux/src/cli.rs` profile tests, 2 are the `claude_launch.rs` bypass-argv tests, 1 is `actor.rs::permission_bypass_is_a_per_turn_result_warning_only_for_bypass_sessions`, and the rest are the `value_enums` exhaustiveness assertions. The 519 figure also predates the §7.2 cursor regression, which added one test to `crates/claude` (`cursor_and_parser.rs` 13 → 14). **No per-change breakdown of the 544 → 580 step is recorded, so do not infer one from this table** — it accumulated across every change after the agent-profile work (§13 rows 3b onward). Re-executed green inside the Gate A capture as cell `rust_tests` — but see §7.1: the receipt stores only a 4,096-byte head and tail of that cell's 54,126-byte output, so **the receipt does not itself carry a total** |
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` | **0 errors** |
| `RUSTDOCFLAGS='-D warnings' cargo doc …` | **0 warnings** |
| Full-stack e2e, `--include-ignored --test-threads=1` | **STALE AS WRITTEN: "8 passed" is one TARGET of the crate's three.** `cargo test --locked -p pseudomux-e2e --all-targets -- --include-ignored --test-threads=1` — which is what Gate A cell `release_full_stack_e2e` runs — is `cross_cell_contamination` **9**, `full_stack` **10**, `pool_concurrency` **21**, plus two targets with no cases: **40 passed, 0 failed, 0 ignored**. MEASURED 2026-08-06 over **5 consecutive reproductions**, 528.3-539.0 s each, 5/5 green; see §7.1 |
| Vendored `rmux-client` clippy / rustdoc, `rmux-server` clippy / rustdoc — **four lanes** | **0 errors**, first execution 2026-07-27 |
| Vendored `rmux-server` `pane_io::tests` | **80 passed, 0 failed**, including all **14** patch-owned EOF regressions (empty set-difference against the named list) |

The 17 `#[ignore]`d tests are principled, not a graveyard: each carries a human-readable reason
naming what it spawns and asserting credential-freedom (real processes, real PTYs, must be
serialized).

**The full-stack suite, first executed 2026-07-26** — `crates/e2e/tests/full_stack.rs`, all 8 green:

```
active_public_turn_sidecar_loss_is_typed_and_reaps_the_process_boundary            ok
actual_daemon_empty_profile_registry_rejects_without_launching_a_child             ok
all_v1_methods_use_the_real_public_and_private_process_boundaries                  ok
daemon_restart_requires_explicit_resume_without_prompt_reinjection                 ok
external_typescript_stage_contract_rejects_invalid_roots_membership_modes_and_aliases  ok
public_close_retry_never_claims_an_observed_escaped_descendant_was_reaped          ok
result_observer_budget_is_the_semantic_deadline_plus_fixed_grace                   ok
runtime_identity_resolves_the_effective_interpreter_behind_a_launcher_shim         ok
```

`EXPECTED_CLAUDE_LAUNCHES = 42` (`crates/e2e/tests/full_stack.rs:61`, asserted `:3946-3951` with
the message *"an unexpected or missing Claude process launch escaped per-session accounting"`*)
**held on the first run.** It is hand-counted, so a miscount would have been indistinguishable from
a product bug. Every one of 42 real launches across 27 cells was accounted for: this is a genuine
end-to-end process-accounting proof and it had never been observed before. ~1,580 lines of
integration code executed for the first time, including the release-CLI attach block and the
`assert_rich_launch` / `assert_rich_result` cells.

Two traps that cost a cycle on the first run and will cost one again:
1. **`umask 077` is mandatory** for the TypeScript stage — `testing.md:393` requires it of *every*
   gate command, stated in prose two pages above the fenced block. Under a default `umask 022`,
   `tsc` emits `0644` and `readStablePrivateFile` fails with `client.d.ts mode must be 0600` against
   a correct tree. **The first Gate A driver forgot this and lost all three TypeScript cells to it**
   (§7.2 defect 4). *(An earlier revision of this file cited `testing.md:104`; that line is a
   coverage-layer table row. The mandate is at `:393`.)*
2. **The staging root must be canonical.** On macOS `/tmp` is a symlink to `/private/tmp` and
   `canonicalPrivateRoot` (`clients/typescript/tests/dist-stage.mjs:46`) rejects it.
   `npm run build` must **not** be used — it emits `clients/typescript/dist` into the source tree
   and the contract requires source `dist` to be absent. The flow is `prepare` → `tsc --outDir` →
   `verify`. Stage digest `5fcc793f29359fc96b34a9c194a375d00d0e6f94b4292dcce650441d5dc239b9`,
   reproduced byte-identically by two independent runs.

### Non-Rust

| Lane | Result |
|---|---|
| TypeScript `node --test` conformance | **49 / 49**, 0 failed, 0 skipped (was 48; +1 is `shared manifest value enums match the TypeScript unions`). Measured in Gate A cell `typescript_tests` |
| TypeScript `tsc --noEmit` | clean |
| Python client `unittest` | **34 / 34**, 0 skipped (was 32; +2 is `ValueEnumConformanceTest`). Measured in Gate A cell `python_client` |
| `tools/phase0` | **243 / 243**. The **87** this row carried was stale by 156. MEASURED 2026-08-06 across **16 consecutive isolated runs, 16/16 green** — 10 under Python 3.13 at ~185 s each and 6 under the driver's own pyenv 3.12.4 at 213-241 s — after the §7.2 defect 8 fix; the same cell was **2 of 12 red** before it |
| `tools/package-smoke` | **35** collected, 1 documented skip |
| `tools/gate-a-candidate` | **20 / 20** |
| `tools/evidence_common` | **48** collected, 1 documented Linux-only skip. The **46** this row carried was stale by 2 |
| `tools/gate-a` | driver self-tests green, including the one-cell end-to-end receipt they emit |
| `tools/linux-docker` | **110** collected, **1 failure**, and the failure is deliberate: `test_runner.py::test_linux_manifest_is_the_exact_ordered_candidate_projection`, debt row **C6**. It is the one red cell in the 80/81 Gate A receipt — **and that receipt is stale: two more `gate_f` cells are red at `26c258f` on this host, measured 2026-08-07 in a pristine worktree (see the note under §7.1). The honest verdict is 78/81.** The **109** an earlier note carried is now 110 — §7.2 defect 8's regression test |
| `ruff check` + `ruff format --check` over the tool trees | clean. Seven trees, not six: the executed cells cover `clients/python`, `tools/{evidence_common,package-smoke,phase0,linux-docker,gate-a,gate-a-candidate}`. `testing.md:546-547` had omitted `tools/gate-a` since the driver landed and was corrected against the executed argv, not the other way round |
| `bash -n` + `shellcheck` on `scripts/*.sh` | clean |

### Residue

Across **11** separate commands spawning real `pmuxd` + real `pmux-rmuxd` + real PTYs + fake-Claude
children: **zero** owned processes surviving (`pgrep -fal 'pmuxd|pmux-rmuxd|rmux|claude'`), **zero**
new `pmux*`/`rmux*`/`*claude*` entries under `TMPDIR`, source tree byte-unchanged, no
`proptest-regressions` artifacts created despite 4096/2048-case property runs. The product's own
residue contract (`testing.md` §6) holds under real execution.

### 5.1 The launch-environment allowlist, and its measured blast radius

**What changed.** The inherited environment term of the launch formula was inverted from a denylist
to an allowlist. `spec.md` §4.5 is now normative for
`effective = allowlist(snapshot) - unset + set - policy_removals + profile_changes`
(`crates/service/src/claude_launch.rs::build_environment`), with the filter
`::inherited_from_snapshot` over `::{INHERITED_EXACT_KEYS,INHERITED_PREFIXES,PROVIDER_ROUTING_PREFIXES,
PROVIDER_ROUTING_EXACT_KEYS}`. Matching is case-sensitive in both the exact and the prefix form; `set` bypasses the
filter and is the deliberate extension channel; the filter is auth-policy aware, so provider routing
survives `Inherit` and is denied under `Subscription`.

**Why, in one paragraph, because it is the part a future reviewer will want to revert.** The
denylist accumulated four nested-Claude markers — `CLAUDECODE`, `CLAUDE_CODE_ENTRYPOINT`,
`CLAUDE_CODE_REMOTE`, `CLAUDE_CODE_CHILD_SESSION` — and every one of them was added only *after* a
live failure, because the deterministic fake Claude does not read any of them. The fourth is the
argument: with `CLAUDE_CODE_CHILD_SESSION` inherited, the child Claude never wrote its own project
transcript, so the transcript authority (§2) had nothing to read and **every turn hung at
`awaiting_prompt_ack`** until the deadline. That is the same shape as the `content_filter.rs`
denylist this rewrite already deleted (§3): a list of other people's strings that grows with every
upstream release and is never finished. A denylist cannot be completed. `unknown-means-denied` can.

**The honest blast radius, measured against one real macOS developer environment:**

| | count |
|---|---:|
| Variables in the caller snapshot | **78** |
| Kept | **10** |
| Dropped | **68** (87%) |
| Of those, previously removed by the denylist | **5** |

**63 variables that used to reach Claude no longer do.** That is not a rounding error and it is not
hidden here: the allowlist is deliberately generous with infrastructure (`PATH`, `HOME`, TLS trust,
proxies, `XDG_*`, Node, `SSH_AUTH_SOCK`, `CLAUDE_CONFIG_DIR`, `LC_*`, `PMUX_*`) and denies
everything else, so the failure mode moves from *silent behavioral drift inside Claude* to *a named
variable is missing*. The second is loud, locally diagnosable, and has a one-flag remedy;
the first cost a gate attempt and was invisible to every one of the 544 tests that existed then
(**580** at HEAD, §5). The escape hatch is
`--env-passthrough KEY`, and `pmux probe` lists the dropped names (values are never serialized) —
`testing.md` `CLI-13`, `CLI-14`.

**The residual risk this creates, stated plainly.** The 87% number is one host. A caller in an
environment this repository cannot test — a corporate proxy variable nobody here has heard of, a
language runtime that keys off its own name — loses it silently at launch and gets a Claude that
behaves subtly differently. That is the cost side of the trade, it is real, and the mitigation is
exactly two things: the audit surface (`probe`) and the escape hatch (`--env-passthrough`). Both are
release-blocking `OPEN-L3` rows today (§8).

---

## 6. Performance properties

**pmux's own per-turn overhead has now been measured end to end, and it is 41 ms at p50.** What is
*claimed* is still boundedness and algorithmic linearity: the measurement is a recorded diagnostic,
not a threshold, and no latency target is gated anywhere. Those two sentences are not in tension —
§6.1 is the number, §6.4 is why it is not a gate.

`crates/service/tests/performance_diagnostics.rs:41` is literally named
`records_release_diagnostics_without_host_speed_thresholds`, emits
`"policy": "host-sensitive-diagnostic-only"` (`:56`), and its assertions are `!samples.is_empty()`
plus correctness checks. Its doc comment (`:38-40`) states the rule outright: *"Records
host-sensitive throughput and latency without making workstation speed a release gate."*

`crates/claude/tests/size_scaling.rs` asserts **exact affine work** via a counter, not timing:
`assert_affine_work` (`:61-82`) derives the slope from `work(3) − work(2)` and then requires
`work(n) == work(2) + slope·(n−2)` **exactly** at n=512 and n=4096, across three graph shapes
(`main_chain`, `sidechain`, `reversed_parent_chain` — two of them adversarial). It is
self-calibrating, so it fails only on non-affinity. **Do not weaken it to an upper bound**: O(n log
n) from 512→4096 is a factor of ~10.6 and is invisible under any maintainable constant bound.

### 6.1 Measured per-turn decomposition, 2026-07-27

Measured against `pmux-test-claude`, the deterministic fake Claude, which has **zero model latency**.
Every number below is therefore pmux's own machinery and nothing else. Schema
`pmux-performance-diagnostics-v1`, policy `host-sensitive-diagnostic-only`; the reporter is
`tools/gate-a/perf_report.py`. Run configured with `transcript_drain_ms = 150`, actor poll 20 ms,
9 turns. **It is still a recorder, never a threshold gate.**

| phase | n | min | p50 | p95 | max | boundary |
|---|---:|---:|---:|---:|---:|---|
| launch | 5 | 37.1 | 37.6 | 479.1 | 479.1 | `PrivateRuntime::start` → pane ready |
| admission | 9 | 0.0 | 0.0 | 0.0 | 0.0 | `submitted_at_ms` → `prompt_acknowledged_at_ms` |
| execution | 9 | 20.0 | 21.0 | 22.0 | 22.0 | ack → terminal candidate |
| completion | 9 | 150.0 | 170.0 | 171.0 | 171.0 | candidate → completed |
| close | 5 | 30.1 | 32.4 | 32.8 | 32.8 | close → process absence proven |
| **turn total** | 9 | **171.0** | **191.0** | **193.0** | **193.0** | `submitted_at_ms` → `completed_at_ms` |

**The headline: non-drain pmux overhead is 41 ms at p50** (191 total − 150 configured drain).

Real-world per-turn cost is therefore ≈ **41 ms + a 500 ms editor-fence floor + the configured
drain**. The 500 ms floor is two 250 ms stability windows (`driver_io.rs:610`) at 25 ms poll
granularity (`:40`). With the 2,000 ms untested fallback drain that is **≈ 2.54 s**; against real
Claude, where the observed drain was 2,354 ms mean (§6.3), it is **≈ 3.1 s**, which agrees with the
independently measured live figure. **pmux's own machinery is on the order of 1.5% of a real turn.
The remaining ~98.5% is deliberate, calibratable safety margin.**

#### What this diagnostic explicitly declares NOT observable

Four boundaries are unmeasured, and the report says so rather than interpolating them. Do not quote
the table above without them:

1. **paste → Enter.** No timestamp is published between the two admission gates, so the interval
   inside the paste/Enter sequence is not observable at all.
2. **The compatibility probe.** `validate_v1_terminal_support` emits no timestamp, so its cost is
   absent from every row above.
3. **The real editor fence.** `TerminalControl` is a double in this harness, so
   `RmuxTerminalControl::submit_prompt` is never exercised. Its 500 ms floor above is
   **constants-derived, not measured** — which is exactly why `admission` reads 0.0 ms here and
   757 ms against real Claude (§6.3).
4. **`execution` is transcript-poll latency, not model latency.** With a zero-latency fake, the
   20-22 ms is the polling interval, and it says nothing about how long a model takes.

Supporting throughput from the same lane: parser **166.6 MB/s** and **579,287 rows/s** at median over
4,096-row samples; actor lifecycle **4,688 ns/actor** startup and **16,399 ns/actor** cleanup in
batches of 32.

#### 6.1.1 The same decomposition through the REAL fence and the REAL socket, 2026-08-06

The table above runs `TerminalControl` as a double, which is why its `admission` row reads 0.0 ms and
why item 3 of the list above says the 500 ms editor fence is constants-derived. **That row is now
measured**, through the shipped `pmux` binary against a real `pmuxd`, a real private rmux sidecar and
a real pane — the only thing still faked is the model. `tools/promotion/measure_turn_latency.py`,
receipts `evidence/turn-latency-double-macos-aarch64.json` and
`evidence/turn-latency-2.1.220-macos-aarch64.json`. Distributions, not point estimates; percentiles
are nearest-rank, so every figure was observed.

| leg | double, drain 250, n=60 | real 2.1.220 sonnet/low, drain 1000, n=20 |
|---|---:|---:|
| `submitted` → `prompt_acknowledged` (the real editor fence) | 620 / **646** / 675 ms | 646 / **675** / 703 ms |
| `prompt_acknowledged` → `terminal_candidate` (generation) | 0 / **0** / 23 ms | 1,729 / **2,326** / 3,729 ms |
| `terminal_candidate` → `completed` (the commit gate) | 526 / **555** / 603 ms | 526 / **552** / 580 ms |
| **turn total, server side** | 1,150 / **1,204** / 1,257 ms | 2,957 / **3,574** / 4,983 ms |
| `pmux turn` process wall clock | 1,160 / **1,213** / 1,318 ms | 2,966 / **3,583** / 4,995 ms |

Four things this changes, each of which corrects something above rather than adding to it:

1. **The editor fence is ~646 ms, not "a 500 ms floor" — and ~91 ms of that 646 ms is the test
   double, not pmux.** Two 250 ms windows at 25 ms poll granularity was the arithmetic, and the
   observed value is ~150 ms above it. The clause that used to close this item — *"and is the same
   on both drivers, so it is pmux's, not the model's"* — was true of the SIZE and false of the
   CAUSE. §6.1.2 measures why: `crates/e2e/src/bin/pmux-test-claude.rs`'s `ensure_no_queued_input`
   calls `libc::poll(&mut descriptor, 1, 100)` between reading Enter and writing the typed `user`
   row, and pays that 100 ms in full on every passing turn. A/B with the timeout at 0, n=30 each:
   the Enter→ack leg falls **115.4 → 24.3 ms mean (Δ 91.1)** and the whole input gate falls from a
   642.0–650.0 ms median over four control runs to **556.0 ms**, with Gate 1, Gate 2 and the commit
   gate all staying inside the control spread. On the real driver the same slot is
   Claude echoing the prompt into the JSONL. **Neither is pmux's**, so pmux's own input-gate
   machinery is ~535 ms, and both the 646 and the 675 below carry a driver term that no change to
   pmux can move.
2. **pmux's own machinery on a warm real turn is ~1,227 ms**, not the ~41 ms of §6.1 and not the
   ~550 ms that `path-b.md` used to imply. §6.1's 41 ms is real but is *non-drain, non-fence*
   overhead measured against a doubled terminal; it is a component, not the total.
3. **The commit gate is ~555 ms and is NOT the drain at any value a real turn owes.** Re-running the
   double at `--drain-ms` 50 / 250 / 1000 gives turn totals of 1,219 / 1,204 / 2,062 ms median: the
   first two are within noise of each other, so below ~550 ms the drain is dominated by the
   screen-stability wait and contributes nothing. `graduated_drain_ms` lowers a real turn's
   requirement to 250 ms whenever the `turn_duration` marker is seen, which was 20 of 20 turns.
4. **The client clock costs 9-13 ms over the server's own view** — `pmux` process spawn plus the
   socket round trip. Small, and now stated rather than assumed to be zero.

Still a recorder, never a threshold: §6.4 is unchanged and normative.

#### 6.1.2 Both gates decomposed by measurement, 2026-08-07

§6.1.1 measures the two legs. This measures what is *inside* them, on the same host with the same
unmodified tool, and it corrects item 1 above, row R2 in §7, and the table in §6.2.

**Method, and the one thing that makes it checkable.** `tools/promotion/measure_turn_latency.py` is
imported VERBATIM — its `Sandbox`, its `Daemon`, its argv, its two-clock method. The only addition is
that the daemon is started with `PMUX_SCREEN_CORPUS_DIR` set, so the input gate's own production
reads are stamped and kept (`crates/service/src/screen_corpus.rs`; `gated_snapshot` records
`input_gate.pre_paste` / `input_gate.post_paste`). **Nothing was inserted into the daemon to take
these numbers**: the recorder is the production path's own instrument and is off unless that variable
is set. Perturbation control, because the instrument is on the measured path: corpus on vs off, n=30
each, back to back — input gate 645.5 vs 644.5 ms median. Harness `.context/latency/decompose.py`;
receipts `.context/latency/*.json` are workspace-local and gitignored, because the method travels and
the files do not.

##### The input gate splits exactly, and 521 of its ~645 ms is two waits

n=30 measured turns per run, 3 warm-ups discarded, `pmux-test-claude` (zero model latency), drain
250. The five segments are contiguous and telescope to the leg.

| # | segment | mean ms | reads | what it is |
|---|---|---:|---:|---|
| a | admit + `arm_at_eof` + terminal mutex | **1.0** | 0 | before any terminal call |
| b | **Gate 1** `prove_stable_empty_editor` + fence | **262–266** | 11 | `SCREEN_QUIET_FOR_MS` |
| c | bracketed paste RPC | **1.0** | 0 | `paste_once` |
| d | **Gate 2** `wait_for_stable_prompt_render` + fence | **262–267** | 11 | same constant, second meaning |
| e | Enter RPC → typed `user` row observed | **115** | 0 | `enter_once` + the actor's 20 ms poll |

**Everything pmux does in the input gate besides waiting costs 2 ms.** One turn's raw frames,
`[site, ms from submitted, screen revision]`:

```text
pre_paste  1 r8 · 29 r8 · 57 r8 · 85 r8 · 113 r8 · 141 r8 · 169 r8 · 197 r8 · 225 r8 · 253 r8 · 254 r8
post_paste 255 r9 · 283 r9 · 311 r9 · 339 r9 · 367 r9 · 395 r9 · 423 r9 · 450 r9 · 478 r9 · 507 r9 · 508 r9
```

30 of 30 turns, every run: `distinct_pre_revisions` = 1 (min = median = max),
`distinct_post_revisions` = 1, `revision_delta_across_paste` = 1. Gate 1 spends its whole window on a
screen that never changes, and Gate 2's *first* read — 1 ms after the paste RPC returns — already
carries the final revision, so its remaining ten reads over 253 ms observe nothing.

**That observation does not license spending the window, and §9.29 is why.** The obvious inference —
replace the eleven reads with a remembered `(revision, first_observed_at)` and discharge the window
in one read, worth up to 264 ms/turn — rests on `revision` being a mutation counter. It is not. The
daemon pmux ships assigns it *per capture*, and over these same 30 turns it advanced from **8 to 67**,
about two increments per turn, while the pane changed many times per turn. **Declined**, with the
derivation and the upstream ask in §9.29.

##### ~91 ms of the published 646 ms is the test double, not pmux

`crates/e2e/src/bin/pmux-test-claude.rs`'s `ensure_no_queued_input` calls
`libc::poll(&mut descriptor, 1, 100)` between reading Enter and writing the typed `user` row. It is
the double's own proof that pmux sent exactly one byte, and on every passing turn it waits the full
100 ms. A/B, n=30 each, same tree, only that timeout changed:

| | runs | leg `e` mean | input gate median | commit gate median | Gate 1 / Gate 2 mean |
|---|---:|---:|---:|---:|---:|
| poll 100 ms (shipped double) | 4 | 115.0–115.9 | 642.0–650.0 | 553.5–574.5 | 262–266 / 262–267 |
| poll 0 ms | 1 | **24.3** | **556.0** | 550.5 | 265.5 / 262.3 |

**Δ leg `e` = 91.1 ms** against the mean of the four control runs' means (115.4). The change is
isolated to leg `e`: both screen gates and the commit gate stay inside the control spread. §6.1.1
item 1 is corrected above. On real 2.1.220 the same slot is Claude echoing the prompt into the JSONL, so
**neither driver's share of that ~150 ms is pmux's**, and pmux's own input-gate machinery is ~535 ms
rather than 646. Of the residual 24 ms, ~6 ms is the actor's 20 ms poll quantization; the rest is the
Enter RPC, the pty and the double's own write.

The real-driver residue is **an estimate, not a measurement**: the 521 ms floor is the same code on
the same pane, so on the 675 ms leg the residue is ~154 ms of echo plus ≤20 ms of pmux detection. It
could not be decomposed on this host — `~/.local/share/claude/versions/` holds 2.1.221 and 2.1.222 as
0-byte files and 2.1.223 as the only real binary, so
`evidence/turn-latency-2.1.220-macos-aarch64.json` is not regenerable here.

##### The commit gate is two sampling periods, and the second one is the guarantee

Each turn-loop iteration awaits `completion_evidence`, which re-proves `SCREEN_QUIET_FOR_MS` of
screen quiet **from scratch** (`wait_for_snapshot_stability` restarts `stable_since` on entry), and
only then evaluates `batch.drain.satisfies(offered_drain_ms)` against a `batch` polled **before** that
wait. So the drain predicate is sampled once per ≈275 ms, and the first sample after the last byte
always reads ~0.

One knob per row; every row is its own n=30 run, and the shipped row is the range over all nine of
them rather than the one that reads best.

| knob | runs | commit gate median | reported `drain_ms` median |
|---|---:|---:|---:|
| shipped (`quiet_for` 250, drain 250) | 9 | **551.0–574.5** | 550.0–573.5 |
| `quiet_for` 125 | 1 | **469.0** | 468.0 |
| actor `poll_interval` 2 ms | 1 | **559.0** (unchanged) | 558.5 |
| drain decided from the confirming re-poll (the R2 reorder) | 2 | **255.0 / 277.0** | 254.5 / 276.5, **min 250** |

Two full periods are paid: one to reach the drain requirement, one to observe that it was reached.
§6.1.1 item 3's `--drain-ms` table is the same arithmetic from the other side.

##### The second period has a name now, a measured floor, and a compiler that enforces it

The quantity is *how late a transcript row may arrive and still be read before the turn commits*.
Until this pass it existed only as the arithmetic product of a screen constant and a poll interval,
neither of which knows it decides transcript truncation risk — and `quiet_for` alone serves **four**
quantities: Gate 1's window, Gate 2's window, the commit gate's screen-liveness window, and this.
`TURN_DURATION_DRAIN_FLOOR_MS`'s own docstring already warns that a later tuning of `quiet_for` could
silently delete the 250 ms floor. The same tuning silently narrows *this*, one level up, and nobody
had written it down.

`v1/backend.rs` states it now. `POST_MARKER_CATCH_WINDOW_FLOOR_MS` = **438 ms**, the largest
post-answer transcript arrival in the promotion campaign (456 turns across 189 real 2.1.220
transcripts, median 42, p90 120, p95 240, p99 344 — §6.2.1), with the **352 ms** ordinal-70 arrival
recorded beside it as the only one ever seen live through pmux. `post_marker_catch_window_ms(required,
period)` = `(required.div_ceil(period) + 1) * period` is the derivation — the requirement is only
observed on a sampling boundary, and the commit lands on the confirming re-poll one period later.

Three things make it more than a comment:

- **It predicts the measurement, and it is a FLOOR rather than an estimate — which is the property a
  refusal needs.** Nominal period 275 ms predicts a 550 ms window. Across **nine** independent n=30
  runs at the shipped constants the observed `drain_ms` medians were 550.0, 550.5, 553.0, 555.0,
  555.5, 555.5, 558.5, 558.5 and 573.5 — range 550.0–573.5, median-of-medians 555.5, and **every one
  at or above the derived 550**. Nominal period 150 ms (`quiet_for` 125) predicts 450 ms against a
  measured 468.0, likewise above. The excess is the per-read overhead the nominal period leaves out,
  so the refusal can only ever be wrong in the direction of refusing a configuration that would in
  fact have been safe. *(An earlier draft of this bullet said "550.0–558.5 over seven runs" — a range
  that excluded two of this pass's own runs, including its largest. It is recorded here rather than
  silently corrected, because a range chosen to fit is the defect §9.29 is about, committed by the
  same pass that wrote §9.29.)*
- **It is the band tests' own arithmetic.** It is the same expression `v1_actor.rs` spells as
  `catchable_window_ms`, and `the_bands_catchable_window_is_the_products_own_derivation` asserts the
  two agree at every cadence that suite samples. The six graduated-band tests assert the actor's
  published `drain_ms` equals `catchable_window_ms`; that new arm is what makes them assertions about
  a shipped guarantee rather than about a local helper.
- **A configuration that narrows it does not compile.** `SCREEN_QUIET_FOR` in `driver_io.rs`
  discharges two `assert!`s in its own initialiser. **The minified cell binds first**, because below
  one period the window is twice the period whatever the requirement is. MEASURED at the boundary by
  editing the constant and building: at 194 the minified window is exactly 438 and it compiles; at
  193 it is 436 and the build fails; at **125** — the value measured for latency above, which saves
  245 ms on the input gate and which no test in `pseudomux-service` notices even at 1 ms — the
  minified window is **300 ms, below the 352 ms row that really arrived**. Asserting only the
  graduated floor would have admitted that: it passes at 125 with a 450 ms window.

It saves **0 ms** and is not claimed to. Measured before and after, pooled n=60 each on the same
host: server total median 1,208 → 1,201 ms, mean 1,211.4 → 1,204.1, min 1,150 both, against a p10–p90
spread of ~76 ms; every segment above unchanged.

##### What is available, and what is not

| | ms | whose | available? |
|---|---:|---|---|
| Gate 1 `quiet_for` | 264 | pmux | **no** — the revision proof does not exist (§9.29) |
| Gate 2 `quiet_for` | 263 | pmux | not without measuring Ink's own render |
| paste, arm, both fences, admit | 2 | pmux | no |
| Enter → typed row observed | ~154 (real) / 115 (double) | **the driver's echo** (double: ~91 ms is its own guard) | ≤20 ms via the actor poll |
| commit gate, reaching the drain requirement | ~275 | pmux | no — the drain is empirical, §6.2.1 |
| commit gate, the catchable window | ~275 | pmux | **no — measured to cost a real 352 ms row** |

**The post-answer bound is EMPIRICAL, not structural**, and the tree already held two of the three
reasons: `TURN_DURATION_DRAIN_FLOOR_MS`'s doc records post-marker rows at +25 ms, and ordinal 70's
352 ms really happened. The third is new: Claude Code's own `turn_duration` row is constructed with
`pendingBackgroundAgentCount` / `pendingWorkflowCount` fields that its caller populates *precisely
when they are non-zero* — read from the 2.1.223 bundle, which is a later version than the promoted
cell and points in the conservative direction. The marker is not a last-write barrier. **The drain
stays, the graduated floor stays, and the commit gate's sampling period stays.**

**Two candidates remain, and each needs a measurement nobody has taken.** Gate 1 and Gate 2 are
250 ms each, unchanged since `405fccd`, sized by caution, and defended by nothing —
`cargo test -p pseudomux-service` is all-green with either at **1 ms**, because every `driver_io.rs`
test overrides them. That is not a licence; it is the finding. Gate 2 is the sharp one: rmux
acknowledges a pty write, not Ink consumption, and a bracketed paste still arriving when Enter lands
submits a truncated prompt. The experiment that would size it is a distribution of Claude's own
inter-chunk render gaps after a paste, taken through the corpus recorder against a real Claude. It
cannot be taken against the promoted 2.1.220 on this host, and evidence from 2.1.223 would be
evidence about a version that has no profile.

### 6.2 The per-turn overhead budget, which *is* known

All in `crates/service/src/driver_io.rs`. **The right-hand column is a NAME and not a line number,
and §9.29 records why**: this table cited six line numbers, every one of them correct at `405fccd`
and every one of them wrong by 2026-08-07, sitting in the document that records that exact defect
thirty-three times. A name is greppable; a line number is a claim nothing checks.

| Constant | Value | Name to grep |
|---|---:|---|
| Editor-stability window (Gate 1) | 250 ms | `SCREEN_QUIET_FOR_MS` |
| Post-paste render-stability window (Gate 2, same constant) | 250 ms | `SCREEN_QUIET_FOR_MS` |
| Commit-gate screen-liveness window (same constant, third meaning) | 250 ms | `SCREEN_QUIET_FOR_MS` |
| Terminal poll interval | 25 ms | `TERMINAL_POLL_INTERVAL_MS` |
| Commit-loop sampling period (the two above, multiplied out) | 275 ms | `COMMIT_LOOP_SAMPLING_PERIOD_MS` |
| Post-marker catch window (that period's product with the drain) | 550 ms | `post_marker_catch_window_ms`, `v1/backend.rs` |
| Evidence timeout | 400 ms | `evidence_timeout`, in `RmuxTerminalControl::new` |
| Input-gate cap | 15 s | `INPUT_GATE_MAX_DURATION` |
| Recovery timeout | 5 s | `recovery_timeout`, in `RmuxTerminalControl::new` |
| Max prompt bytes | 1 MiB | `MAX_PROMPT_BYTES` |

### 6.2.1 The drain is now MEASURED for one cell, and 1000 ms is the promoted value

> **SUPERSEDED IN ITS DERIVATION, NOT IN ITS NUMBER — 2026-08-09.** The shipped value is still
> **1000 ms**, and everything below about the partition, the reasons-per-exclusion and the
> fail-on-unclassified behaviour still holds. Two things changed:
>
> 1. **The cell is a RANGE, not a version.** `PROMOTED_PROFILES` ships floor **2.1.220** through
>    tested-ceiling **2.1.227**. Read `claude_version_floor` and `claude_version_tested_through`, not
>    a single `claude_version`; the struct has not had that field since P2 landed.
> 2. **The bound is POOLED, not a per-version fit.** `docs/version-drift.md` §P1 established that
>    fitting per version on a thin corpus produces a number that TRUNCATES answers — 2.1.223's own
>    free corpus fits **250 ms**, below the 438 ms arrival already observed at 2.1.220. What ships is
>    the same 438 ms maximum taken over **226 arrivals in 425 macos/aarch64 transcripts spanning
>    2.1.207 / 2.1.215 / 2.1.220 / 2.1.223**, doubled and rounded to a 250 ms step. The receipt of
>    record is now `evidence/pooled-transcript-drain-macos-aarch64.json`; the per-version receipt
>    named at the end of this subsection is the FLOOR's original and is retained as history.
>
> Each ceiling was driven for real: `tools/promotion/promote_claude_version.py` ran five
> minified-cell turns at 2.1.226 and measured 5 reachable arrivals, max **223 ms**, and five more at
> 2.1.227 for 5 arrivals, max **52 ms**. `system/api_error` has since
> been classified as retrospective rather than a post-answer arrival, with the tool failing the run
> if a retrospective kind is ever stamped after the candidate. See `docs/path-b.md` §12.4.

**This subsection supersedes the "the drain is the one tunable and nobody has calibrated it"
framing of §6.3 and §6.4 for exactly one compatibility cell.** For every other cell the untested
2,000 ms fallback and everything §6.3/§6.4 say about it are unchanged.

Promoted cell **2.1.220 / macos / aarch64 / transparent / sdk**, `transcript_drain_ms` **1000**.
The statistic is the **max of every post-answer transcript arrival a minified cell can produce**:
**438 ms over 456 turns in 189 real 2.1.220 transcripts** (1,195 files scanned), median 42, p90 120,
p95 240, p99 344. Margin 2.0, rounded up to a 250 ms grid, then held at 1000 — 2.28x the observed max
and half the untested fallback.

Three properties of that measurement are worth more than the number:

- **Every arrival was a structural end-of-turn row.** 182 `turn_duration` and 7 `stop_hook_summary`.
  **No semantic row ever followed an answer** in the whole corpus. An assistant row arriving after a
  turn's final assistant row is listed in the receipt's own `what_would_invalidate_it`.
- **The five unreachable arrivals are excluded WITH A REASON EACH, on the record**, not silently
  dropped: `queue-operation` (the task queue is a harness feature; a minified cell has no queue),
  `system/away_summary` (an interactive-session feature), and a post-answer `user` row (a harness
  injection such as a `<task-notification>`). Their max is 18,074,772 ms, so an analysis that folded
  them in would have produced a nonsense drain — which is why the partition is published and
  `partition_balances: true` is asserted.
- **The tool fails on a row kind nobody classified rather than defaulting**, and a unit test binds
  the shipped constant to the receipt's own recommendation so the two cannot drift
  (`compatibility.rs::every_promoted_drain_is_the_one_its_receipt_recommends`).

Receipt: `evidence/promoted-profile-2.1.220-macos-aarch64.json`. Regenerator:
`tools/promotion/measure_transcript_drain.py`. The corpus is host-local operator transcripts and is
**not committed** — it contains prompts — so the receipt is the durable artifact and re-running the
tool is how it is reproduced. Note that this makes the promoted drain **calibration evidence of a
different kind** from Gate B's: it is derived from transcripts that already existed, spends no
ledger ordinal, and calls no model.

### 6.3 The transcript drain, measured against real Claude

Over **24 real turns** carrying `authority=transcript` (the only live corpus that exists; see §7):

| Leg | min | max | mean |
|---|---:|---:|---:|
| Admission (`submitted_at_ms` → `prompt_acknowledged_at_ms`) | 690 ms | 1,026 ms | **757 ms** |
| Model work (`prompt_acknowledged_at_ms` → `terminal_candidate_at_ms`) | 1,630 ms | 48,122 ms | 7,640 ms |
| **Transcript drain** (`terminal_candidate_at_ms` → `completed_at_ms`) | **2,320 ms** | **2,479 ms** | **2,354 ms** |
| Whole turn (`wall_ms`) | 5,262 ms | 55,192 ms | 14,204 ms |

The configured `transcript_drain_ms` for that cell was **2,000** — the conservative untested fallback
— so the observed 2,320-2,479 ms is the 2,000 ms configured drain plus ~320-480 ms of poll and
observation slack. This distribution is recorded in `evidence/README.md` and is the enduring
analytical value of the 24 otherwise non-promotable turns.

**Conclusion, stated plainly.** Structural pmux overhead is **≈3.0-3.1 s per turn** (measured mean
757 ms admission + 2,354 ms drain = **3,111 ms**), of which the drain is **~76%**; against the
constants alone the floor is 2.5 s (250 + 250 + 2,000) with the drain at ~80%. **The drain is the
one tunable.** It is calibrated per compatibility cell, and **Gate B exists to produce a defensible
minimum** for it. Everything else in the budget is a stability window measured in hundreds of
milliseconds.

### 6.4 The standing decision: no latency target is defined or gated

**There is no defined latency target anywhere in `spec.md` or `testing.md`, and §6.1 does not create
one.** That remains deliberate. A per-turn latency claim is not defensible from one compatibility
cell on one host, and the project has explicitly chosen boundedness-and-linearity over a
host-sensitive number; a measurement recorded is not a threshold adopted (forward rule 2, §12: *the
receipt records; it does not gate*).

What §6.1 changes is that the open item is now **quantified rather than unknown**, and it identifies
the lever unambiguously: **the drain is the one thing worth tuning.** At the 2,000 ms fallback it is
~79% of the real-world per-turn cost, against 41 ms for everything pmux itself does. Calibrating it
to a defensible minimum against a real compatibility cell is **Gate B's job**, and the target, if one
is ever written, belongs in `spec.md` at that point.

---

## 7. Gate state

### 7.1 Gate A — deterministic, macOS

> **THE CELL COUNT, RECONCILED 2026-08-09.** Every figure below this box is historical, and the
> denominators in them (`75`, `81`, `83`) are all stale. Read the manifest, not this file:
> `tools/gate-a-candidate/phase-manifest.json` is **70 cells** — `gate_a` 28, `gate_b` 8, `gate_c` 4,
> `gate_d` 10, `gate_e` 10, `gate_f` 9, `residue` 1 — and `tools/gate-a/README.md` publishes that
> breakdown beside the driver. It went **83 → 70 at `d276b69`**, entirely in `gate_a` (41 → 28), when
> fourteen regression names that had been hand-copied into six places became one module the patch
> defines. Nothing was dropped from coverage; thirteen cells stopped being a hand-written duplicate
> of it.
>
> **"Gate A" as run is the six phases that are not `gate_b`: 62 cells.** `gate_b` is the fuzz and
> mutation lane and is invoked separately, with its own `--tool` placeholders and a 14,400 s per-cell
> timeout.
>
> **This host, this tree, 2026-08-09** — driver `sys.executable` = `~/.pyenv/versions/3.12.4/bin/python`:
>
>     {"planned": 62, "executed": 62, "passed": 60, "failed": 2,
>      "failed_ids": ["linux_docker_self_tests", "gate_a_residue"]}
>
> **60/62 as the receipt reads. 61/62 is the honest figure, and the difference is mine, not the
> tree's.** 2026-08-09T22:45:20Z, 35.2 minutes, `/private/tmp/gate-a-recon/receipt.json`.
>
> - `gate_f/linux_docker_self_tests` — **the deliberate red**, debt row C6, unchanged and out of
>   scope here.
> - `residue/gate_a_residue` — **not a finding about this tree.** It refused on
>   `/tmp/prd-tree-AJvL76` and `/tmp/prd-zero-timeout-PjlNdv`, two `tempfile::TempDir` roots that
>   `bin/pmux-rmuxd/tests/process_blackbox.rs::private_root` creates and drops. Their mtimes are
>   **22:42:30Z and 22:42:33Z — three minutes BEFORE this run started** — because I killed an earlier
>   gate attempt while its `rust_tests` cell was mid-flight, and a process group that takes SIGKILL
>   runs no destructors. Both PIDs in the leftover `pids` file were already gone; no process
>   survived. Removed, and `residue` re-run through the driver on its own: **1/1 pass**
>   (`/private/tmp/gate-a-residue2/receipt.json`), with `scripts/gate-a-residue.sh` standalone also
>   reporting `Gate A residue audit passed`. The audit did exactly its job — it caught residue an
>   operator left — and the correct reading of this cell is that **no cell of this run leaked
>   anything**.
>
> **Freshness, checked rather than assumed.** The receipt's `source_unchanged` is `true` and
> `source_digest_before == source_digest_after` at
> `3fab63956ebfdc8eb24a386967260b28d7df2236d7b936aeee7da4a9fca4b595` over **950 files**. That digest
> was then RE-COMPUTED with the driver's own `source_digest()` after the run and still matched, so
> the receipt attests this tree and not a neighbour of it. **The only edits made after the run are
> the documentation blocks in this reconciliation pass** — this box among them — and no gate cell
> reads `docs/` except `crates/service/tests/path_b_doc_citations.rs`, which is why that test was
> re-run green after every one of them.
>
> **One red cell was found that no previous run could have reported, because no previous run
> happened.** `gate_a/rustdoc` — `cargo doc --locked --workspace --all-features --no-deps` under
> `RUSTDOCFLAGS=-D warnings` — had been **failing since `20bf20f`**, which made
> `claude_launch::MINIFIED_CELL_FLAGS` `pub` with a doc comment carrying two intra-doc links to
> private items (`SAFE_EXTRA_FLAGS`, `MINIFIED_CELL_ENVIRONMENT`). `rustdoc` promotes that to
> `error: public documentation for ... links to private item ...`. **Neither `cargo test --workspace
> --all-targets` nor `cargo clippy --all-targets -- -D warnings` can see it** — only `cargo doc` runs
> the lint — so three consecutive sessions reported a green workspace over a red gate cell. Each of
> those sessions disclosed that it had not run Gate A; this is what that disclosure was worth. Fixed
> here by making the two references code spans rather than links.

**The 75-cell ordered manifest has executed end to end with every cell passing four times, on four
different trees** — three on 2026-07-27 and the one that is evidence today. §3 reads
`Gate A, on one host`.

> **Read this before the four receipts below.** The manifest is **no longer 75 cells**; it is **81**,
> and the current verdict is **80/81**, not 75/75. Six cells were added after the receipt of record
> (`gate_f` grew from 3 to 6 by restoring the five self-test cells `testing.md` requires, then to 9,
> and `residue` is unchanged), so the four receipts below attest a *smaller manifest* than the one
> that runs today and none of them attests this tree. Two full 81-cell runs were captured on
> **2026-08-06** on `macOS-15.7.7-arm64` (`Darwin 24.6.0`), each with `source_unchanged: true` over
> **926 files**:
>
> | Run | Window | Result | Note |
> |---|---|---|---|
> | 1 | 36.1 min | **79/81** — `release_full_stack_e2e`, `linux_docker_self_tests` | found §7.2 rows 6 and 7 |
> | 2 | 30.8 min | **80/81** — `linux_docker_self_tests` | after both fixes; the one red cell is debt row **C6** |
> | 3 | 30.5 min | **78/81** — `release_full_stack_e2e`, `phase0_self_tests`, `linux_docker_self_tests` | same tree as run 2 plus this note; the two extra cells are **intermittents**, now debt rows **C10** and **C11** |
>
> `gate_b` executed for the first time in this project's history in run 1 — all 6 cells, including
> `production_fuzz` — because the driver could not previously resolve `{cargo_fuzz}`.
>
> **Read run 3 carefully rather than averaging it with run 2.** Its two extra red cells are not
> regressions of runs 1–2 and not a re-appearance of §7.2 rows 6 and 7; both were chased to a
> measured rate and a named field:
>
> - `release_full_stack_e2e` failed on **one** deterministic pool test,
>   `fifteen_concurrent_callers_survive_children_killed_mid_clear` — `20 passed; 1 failed`, where run
>   2 was `21 passed`. Debt row **C10**: 2 failures in 7 whole-target sequences, 10/10 green in
>   isolation, and **4/4 green at `f73aae3`**, the commit before the cold-swap fix.
> - `phase0_self_tests` failed on **one** test erroring with `workspace revision changed across source
>   capture`. Debt row **C11**: an identity that carried the Git directory's mtime. Already a named
>   follow-up at `phase0_lib.py:1194-1215`; run 3 supplied the rate it lacked. **C11 is now FIXED**
>   — §7.2 defect 8.
>
> So the deterministic verdict is **80/81** and the only reliably red cell is **C6**, which is red on
> purpose (see its row in §9). Everything the four receipts below say about *provenance* still holds;
> only their cell count and verdict are stale.
>
> > **MEASURED 2026-08-07: "the only reliably red cell is C6" is FALSE on this host. Two more
> > `gate_f` cells are red at `26c258f` itself.** Measured in a pristine `git worktree` at
> > `26c258f`, so no uncommitted work is involved, and byte-identical to what the working tree
> > produces:
> >
> > * **`gate_f/phase0_self_tests` — 3 of 243 failing**, all in
> >   `test_verify_calibration.BannerCitationTests`. These are **line-range citations into product
> >   source that have rotted**: the banner cites `crates/service/src/driver_io.rs:2628-2631` for
> >   `state.last_change.elapsed()` and `crates/protocol/src/v1.rs:2163-2165` for "Absent on any turn
> >   where no Stop hook was observed", and neither range holds its text any more — `v1.rs`'s is ~500
> >   lines away. This is the bug class again, in the tool that checks for the bug class, and the
> >   suite is doing exactly its job by refusing.
> > * **`gate_f/package_smoke_self_tests` — 1 of 35 erroring**, and this one is ENVIRONMENTAL, not a
> >   defect: `importlib.metadata.PackageNotFoundError: No package metadata was found for setuptools`.
> >   Python 3.13 ships no `setuptools`, and the test asks for its distribution metadata. It would
> >   pass on a host that has it. Recorded because a gate cell that depends on an unpinned host
> >   package is a gate cell whose result is about the host.
> >
> > Neither is caused by the work in this commit, neither is fixed by it, and **both are out of its
> > scope** — they are recorded here because the sentence above them was measured false and a claim
> > nobody re-measures is what §9.25 is about. **The honest verdict on this host today is 78/81**,
> > with C6 red on purpose, phase0 red on citation drift, and package-smoke red on a missing host
> > package.
> >
> > > **BOTH CLOSED 2026-08-07, and each was wider than the cell that surfaced it — §9.26.**
> > > `phase0_self_tests` is **247 tests, 0 failing**: every product line number
> > > `verify_calibration.py` holds is now resolved from the cited file's own text at import by
> > > `cite(...)`, and a self-test refuses a hand-written one anywhere in that file. The two
> > > citations above were **not the only rotted ones**. Counted rather than estimated — every
> > > `path:line` into `crates/`, `bin/` or `clients/` in that file at `0d7f2ca`, bare
> > > `driver_io.rs:2714` shorthands included — the file held **22 product citations of which 16
> > > were wrong**: 5 in strings it PRINTS, 11 in comments and docstrings nothing graded. The gate
> > > cell refused on 4 of the 5 printed. The fifth was `actor.rs:83`, where the poll interval is on
> > > 85, and it was green under a test named for exactly that citation which was grading a comment.
> > > `package_smoke_self_tests` is **36 tests, 0 failing**, and the fix is a declared interpreter
> > > contract rather than a vendored dependency: `PYTHON_BUILD_SUPPORT_DISTRIBUTIONS` is stated
> > > once, `validate_python_tool_report` refuses a build-support tree by the name of what it is
> > > missing, and the fixture that materializes that tree out of the running interpreter checks the
> > > same tuple first and **skips naming the interpreter and the missing distribution**. The set was
> > > also hand-written three times in the validator and is now read from that one tuple.
> > >
> > > **And there was a THIRD red cell nobody had named: `gate_a/rustdoc`.** Measured in a pristine
> > > `git worktree` at `0d7f2ca` — `RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace
> > > --all-features --no-deps` exits nonzero on
> > > `error: public documentation for 'serialize' links to private item
> > > 'AGENT_SUPPLIED_START_PATHS'`. **13 doc links across four files** point at private items, so
> > > they resolve for nobody reading the published docs; the protocol one aborts the build before
> > > the other twelve are even reached, which is why it looks like one. `git log -S` dates them to
> > > `715f8c1` and `26c258f` — *after* the 2026-08-06 receipt this section is built on, which is
> > > exactly why that receipt does not mention it. Each is now a plain code span rather than a link
> > > that promises a resolution it cannot make. So the count at `0d7f2ca` was **77/83, not 78/81**,
> > > and the manifest has been 83 cells for two commits.
>
> **The manifest is now 83 cells, and no whole-manifest receipt attests them.** `gate_b` grew from 6
> to 8 with `cargo_mutants_version` and `mutation_score_agent_launch_pool_protocol` (§9.22).
>
> > **SUPERSEDED for the mutation cell on 2026-08-07: it has now been through the driver.** A whole
> > manifest run at `0d7f2ca` plus the §9.26 work came out **78/83**, and
> > `gate_b/mutation_score_agent_launch_pool_protocol` was green in it, in 5,285 s against a
> > `phase_timeouts_seconds.gate_b` of 14,400 — 2.7x headroom, measured rather than estimated. That
> > run is the BEFORE figure in §9.23. The five red cells were `linux_docker_self_tests` (debt row
> > C6, deliberate), `release_full_stack_e2e`, `cli_process`, `mcp_process` and the `gate_a_residue`
> > audit that follows them; none is in this work's scope and each is recorded where it belongs.
>
> > **THREE OF THOSE FIVE WERE ONE STALE `target/release`, AND NOT ONE OF THEM SAID SO.** Diagnosed
> > 2026-08-07 from the receipt at `/private/tmp/gate-full/receipt.json`. No cell in
> > `phase-manifest.json` builds `{release}`, and until this pass nothing in the driver checked its
> > age, so the whole release lane was a function of an out-of-band `cargo build --release` an
> > operator had last run at some earlier commit. The three failures read as three unrelated product
> > regressions in two phases:
> >
> > | Cell | Wall | What it said | What it was |
> > |---|---:|---|---|
> > | `gate_d/mcp_process` | 9.0 s | `tools/list` returned 9 tools, the test expected 13 | the release `pmux-mcp` predated `d310481`, which added the four agent tools |
> > | `gate_d/cli_process` | 62.6 s | `error: unexpected argument '--agent-version' found` | the release `pmux` predated `715f8c1`/`d310481`, which added the flag |
> > | `gate_a/release_full_stack_e2e` | 388.3 s | 14 failures, all `pool_concurrency.rs:263` | that cell's OWN staleness guard, firing correctly |
> >
> > The first reads as *"the agent resource was never wired to MCP"* and the second as *"the F7 fix
> > is wrong"*, and both readings are false: the source at `0d7f2ca` defines all thirteen tools
> > (`bin/pmux-mcp/src/tools.rs:203`) and carries `--agent-version` (`bin/pmux/src/cli.rs`), and with
> > the release directory rebuilt every one of the three passes unchanged. **The receipt already held
> > the proof and nobody read it:** `release.binaries[].sha256` for `pmux-mcp` was
> > `0c43d69e…` against `a8acfbc5…` on disk after a rebuild, and `pmux` `ab57e71b…` against
> > `e79d7115…`. A binary digest that moves without a source digest moving is a stale candidate,
> > full stop.
> >
> > **FIXED AS A PRECONDITION, NOT AS THREE CELL REPAIRS.** `run_gate.py::require_release_not_stale`
> > hoists `crates/e2e/tests/pool_concurrency.rs:225-269` verbatim — dependency set READ FROM
> > cargo's own `<binary>.d`, never guessed — and runs it beside `require_release_depinfo` before the
> > first cell. It covers all eight executables rather than the five that guard names, it names
> > EVERY stale binary and the source that makes each one stale in one refusal, and it exits 2
> > having run nothing and written no receipt. Measured against a deliberately backdated copy of
> > `target/release`:
> >
> > ```text
> > gate-a driver error: 2 release binaries are older than the source cargo says it is built
> > from, so the gate would measure a candidate that no longer matches this tree; run
> > `cargo build --locked --release --workspace` first:
> >   …/release/pmux is older than …/bin/pmux/src/cli.rs
> >   …/release/pmux-mcp is older than …/bin/pmux-mcp/src/tools.rs
> > ```
> >
> > Four driver self-tests own it, and each was proven by deleting the check and watching its target
> > fail. Three fail on the deleted call; the fourth,
> > `test_a_source_cargo_listed_and_that_no_longer_exists_is_not_stale`, is a no-false-positive test
> > and is **not** observable against that deletion — it is observable against deleting the
> > `except OSError: continue` arm, which is the deletion it was written for, and that is stated here
> > rather than folded into a claim of four.
>
> #### Both intermittents were chased to ground on 2026-08-06, and only one of them was a defect
>
> **`release_full_stack_e2e` is SATISFIABLE AS WRITTEN, and a report saying otherwise is stale.** A
> previous agent reported that the cell fails on an absent `PMUX_POOL_REAL_CLAUDE` *and* an absent
> staged `PMUX_E2E_TYPESCRIPT_DIST_DIR`, that both reproduce in a clean worktree, and that **"the
> gate cell as written sets neither env var — so as written it cannot pass on this host."** Checked
> against the manifest and the code rather than repeated:
>
> - The cell **does** set `PMUX_E2E_TYPESCRIPT_DIST_DIR`, to `{validation}/typescript-dist`
>   (`tools/gate-a-candidate/phase-manifest.json`, the `release_full_stack_e2e` cell), and three
>   `gate_a` cells that run *before* it — `typescript_stage_prepare`, `typescript_external_build`,
>   `typescript_stage_verify` — are what stage that directory. The variable is absent only when the
>   cell is run standalone outside the manifest, which the gate never does.
> - `PMUX_POOL_REAL_CLAUDE` **is** absent, and that was fatal — until §7.2 defect 6 fixed it in
>   `b6bbd4c`. Since then `Lane::real` skips loudly and returns `None`
>   (`crates/e2e/tests/pool_concurrency.rs`), which is exactly why the cell now reports
>   `21 passed; 0 failed; 0 ignored` instead of the `14 passed; 5 failed` run 1 recorded.
>
> **MEASURED, 5 consecutive reproductions of the cell exactly as the driver runs it** — the driver's
> `ENVIRONMENT_ALLOWLIST` as the base environment, the cell's two variables on top, `umask 077`,
> cwd = workspace, manifest argv verbatim, against a staged validation root — on a tree whose Rust
> is byte-identical to `3820dc5`:
>
> | Run | Wall | Result |
> |---|---:|---|
> | 1 | 538.3 s | `9 + 10 + 21 passed; 0 failed; 0 ignored` |
> | 2 | 528.3 s | same |
> | 3 | 530.7 s | same |
> | 4 | 531.8 s | same |
> | 5 | 539.0 s | same |
>
> **5/5 green.** So the cell's preconditions are established by the manifest, run 2 of the gate
> passed for exactly that reason rather than by luck, and the remaining intermittency is **C10** and
> nothing else. C10 did not reproduce in these five; its rate over every whole-target sequence
> anyone has run at this commit is now **2 in 12**, and 0/5 does not retire a 2-in-12 intermittent —
> see its row in §9.4 for what does.
>
> **`phase0_self_tests` was a real defect and is fixed.** 12 isolated runs of the cell's exact argv
> at `3820dc5`: **runs 2 and 7 red, 10 green** (~185-196 s each, 243 tests each), both failing on
> `test_phase0.py:1221::test_source_identity_is_byte_for_byte_canonical_linux_runner_digest` with
> the identical `Git repository control identity changed during capture`. The cause is §7.2
> defect 8 — a directory mtime recorded as identity, moved by an **external workspace poller** and
> not, as C11 previously claimed, by the capture's own queries, which run under
> `GIT_OPTIONAL_LOCKS=0` and write nothing. **Post-fix: 10 of 10 isolated runs green.**
>
> Two things about that batch worth stating rather than leaving to be assumed. It ran under the
> **framework Python 3.13**, whereas the driver's `{python}` is `sys.executable` — on this host the
> **pyenv 3.12.4** interpreter, which is the one that carries `ruff` and `setuptools`. The defect is
> a filesystem race inside `source_digest.py` and is interpreter-independent; the capture itself was
> measured directly at **1 abort in 20 pre-fix and 0 in 30 post-fix**, which is the instrument-free
> version of the same number, and a **further 6 of 6 isolated cell runs under the driver's own
> 3.12.4 came back green** (213-241 s each). And **the interpreter is not cosmetic elsewhere**:
> under 3.13, `package_smoke_self_tests` errors with
> `PackageNotFoundError: No package metadata was found for setuptools`, which is a host
> precondition and not a defect — under the driver's own 3.12.4 that suite is **35 collected, 1
> documented skip, 0 failures**. Anyone re-running a Gate F cell by hand must use the same
> interpreter the driver would, or they are measuring their `PATH`.

#### The receipt of record, and where it is not

**There are four receipts on disk and exactly one of them is evidence for this tree.**

| Receipt (all under `.context/gate-a/`) | Window (UTC) | Summary | Standing |
|---|---|---|---|
| `receipt-8b59cbf.json` | 2026-07-29T02:48:10Z → T03:01:07Z | 75/75/75/0, `source_unchanged: true` | **RECEIPT OF RECORD** |
| `receipt-20260727-env-allowlist.json` | 2026-07-27T22:44:26Z → T23:00:38Z | 75/75/75/0, `source_unchanged: true` | superseded |
| `receipt-20260727-agent-profiles.json` | 2026-07-27T19:42:00Z → T19:52:48Z | 75/75/75/0, `source_unchanged: true` | superseded |
| `receipt-20260727.json` | 2026-07-27T14:50:18Z → T15:01:03Z | 75/75/75/0, `source_unchanged: true` | superseded; retained for the defect history in §7.2/§7.3, not for its verdict |

All three `20260727` receipts are superseded because source changed after each was captured, so none
of them describes any tree that exists. **Earlier revisions of this document named
`receipt-20260727-agent-profiles.json` as "the current receipt" and cited all of them under
`evidence/gate-a/`. Both statements were false**: that receipt was already superseded, and
`evidence/gate-a/` has never existed.

Three things about the receipt of record that are easy to get wrong, stated before the table:

1. **It attests one commit, and that commit is not HEAD.** Its `source_digest_before` and
   `source_digest_after` are both `47ab4fb474578fb64eb35f82ea11a7485b750cdb71174a620145b885dafbdc39`
   over **877 files**, which is the tree of the commit whose subject is *"C9: a pre-connect
   regression hung the gate command instead of failing it"* — identified here by subject because the
   pre-push history rewrite changed that commit's id. **Do not assume the delta since is harmless;
   compute it.** `git diff --name-only <that commit>..HEAD` was docs-only at the moment this
   paragraph was written, and `docs/` is itself inside the driver's `SOURCE_ROOT_DIRS`
   (`tools/gate-a/run_gate.py:236-262`), so even a docs-only delta **moves the digest** — including this
   very edit. The pre-push fix round then landed changes under `clients/` and `tools/` as well, so
   **assume product and harness source after the attested commit is unattested until you have run
   that command yourself**. Either way the receipt does not attest the pushed tip and cannot be made
   to without a re-run.
2. **It does not travel with a push.** `.context/` is gitignored (`.gitignore:20`). A fresh clone has
   no receipt at all. Do not read its absence as evidence of anything: **request it, or regenerate it
   by re-running `tools/gate-a/run_gate.py`** — unlike `evidence/model-attempt-ledger.ndjson`, a Gate
   A receipt costs only machine time, so regenerating is always the cheaper answer than arguing about
   a missing file.
3. **Its sha256 is `db5bacdeaaaeaaacf633c09a290add1933ee66e307eeb12059a38297a1e4e2d3`** — check that
   before trusting a copy handed to you out of band.

| Fact | Receipt of record (`receipt-8b59cbf.json`) | Superseded (`receipt-20260727-agent-profiles.json`) |
|---|---|---|
| Ordered cell manifest | `tools/gate-a-candidate/phase-manifest.json` sha256 `e5b48f6b22…`, **75 cells**: `gate_a` 41, `gate_b` 6, `gate_c` 4, `gate_d` 10, `gate_e` 10, `gate_f` 3, `residue` 1 | same 75 cells |
| Result | **75 planned / 75 executed / 75 passed / 0 failed**, `failed_ids: []` | 75/75 |
| Receipt | `.context/gate-a/receipt-8b59cbf.json`, sha256 `db5bacdeaaaeaaacf633c09a290add1933ee66e307eeb12059a38297a1e4e2d3` | `.context/gate-a/receipt-20260727-agent-profiles.json`, sha256 `aeb39a9e73…` — **superseded** |
| Source integrity | `source_unchanged: true` — digest identical before and after the run. Algorithm `pmux-gate-a-source-v1-path-mode-size-content-sha256`, digest `47ab4fb474578fb6…`, **877 files** | digest `32519f39677b6d5b…`, 861 files |
| Host | `macOS-15.7.7-arm64`, Darwin kernel 24.6.0 | same |
| Release candidate | **8 binaries**, mode **0500**, sha256 each, in an owner-only validation root **outside the workspace**: `pmuxd a78e669c…`, `pmux c5fad60a…`, `pmux-mcp c45e3b98…`, `claude-p 9401736a…`, `pmux-hook d3bb16e4…`, `pmux-rmuxd 445f9049…`, `pmux-launcher 570b4cd6…`, `pmux-test-claude e3098117…` | `pmuxd 98a22d26…`, `pmux 00088aab…`, `pmux-mcp 16b817b7…`, `claude-p 4ee5b7ef…`, `pmux-hook 09ef6274…`, `pmux-rmuxd 938fb64e…`, `pmux-launcher 570b4cd6…`, `pmux-test-claude 0f7acd2b…` |
| Workspace tests | cell `rust_tests` exit 0 on `cargo test --locked --workspace --all-targets --all-features`, 246,266 ms. **The receipt records no total**: it stores a 4,096-byte head and tail of that cell's 54,126-byte output, and the counts live in the discarded middle. TypeScript **50/50** (`typescript_tests`), Python **35/35** (`python_client`) are recoverable, being short enough to survive. The count at HEAD is **580 / 0 / 17** (§5) | 544 / 0 / 17; TypeScript 49/49, Python 34/34 |
| Static lanes | `rust_fmt`, `rust_clippy`, `rustdoc`, `python_ruff`, `python_ruff_format` all exit 0 (`cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`) | same |
| Fuzz | **ran at spec: 50,000 runs × 3 targets** (`transcript_jsonl`, `transcript_cursor`, `native_frame`), **0 crashes, 0 artifacts** (`PMUX_FUZZ_RUNS=50000`, `Done 50000 runs` in cell `production_fuzz`; 0 artifacts is carried by the `residue` cell passing) | same |
| Driver | `tools/gate-a/run_gate.py` (533 lines), 26 self-tests in `tools/gate-a/tests/test_run_gate.py` (629 lines) | same |
| Wall time | 777,052 ms (12 m 57 s) | 648,416 ms (10 m 48 s) |

**Three of the eight binaries were bit-identical across the two 2026-07-27 candidates**
(`receipt-20260727.json` and `receipt-20260727-agent-profiles.json`) — `pmux-launcher
570b4cd6…`, `pmux-rmuxd 938fb64e…`, `pmux-test-claude 0f7acd2b…`. The five that changed are exactly
the five that link `pseudomux-protocol`, `pseudomux-service`, `pseudomux-client`, or `bin/pmux` —
the crates the feature touched. This is **not** a reproducibility claim; the disclaimer below still
stands in full. It is a narrower observation: **the build is deterministic for unchanged crates on
this host**,
which is the cheapest available evidence that the candidate is a function of the source rather than
of the moment it was built.

#### The 2026-07-27 agent-profile re-run took two attempts, and both failures were setup, not product

This subsection is the history of the **superseded** `receipt-20260727-agent-profiles.json` capture.
It is kept because the two failures are the argument, not the verdict; the verdict of record is the
2026-07-28 capture in the table above.

**Attempt 1 scored 73/75.** Neither failure was a product defect, and recording that distinction is
the point:

1. **`typescript_stage_prepare` failed** because the coordinator had pre-staged the TypeScript dist
   into the validation root, and the manifest's own `prepare` cell requires an empty root
   (`clients/typescript/tests/dist-stage.mjs:88`, `"prepare requires an empty root"`). The cell was
   correct; the operator had done its job for it.
2. **`gate_a_residue` failed on 9 findings**, all `__pycache__` / `.ruff_cache` directories left in
   the source tree by the implementation agents (`scripts/gate-a-residue.sh:119-131`).

**Record the second one as the gate working.** The residue cell exists to catch generated output
polluting the canonical source tree, and that is precisely what it caught. A gate that only ever
fires on product bugs is a gate nobody has tested. The standing prevention is already in the
manifest — `PYTHONDONTWRITEBYTECODE=1` on the Python cell and `--no-cache` on both ruff cells — and
it is now also written into the residue contract at `testing.md` §6.

Attempt 2, from a clean tree, scored 75/75 and produced `receipt-20260727-agent-profiles.json`. No
receipt was retained for attempt 1, so its source digest is not recorded here; what is recorded is
that both failures were in the environment the driver inherited, not in the product it measured, and
that neither cell's assertion was weakened to make attempt 2 pass.

**The same pattern repeated, harder, on the capture that produced the receipt of record.** It was run
in a standalone clone, alone, with nothing else in flight, and it took **four captures** to reach
75/75 — 69/75 because a fresh clone has no `node_modules`, so `tsc` is absent and the e2e lane
cascades off it, then 71/75 twice on the `typescript-dist` directory, which must **exist** and be
**empty**, and which `typescript_external_build` repopulates even after `stage_prepare` has already
failed, so a failed capture poisons the next one. Every one of those failures was setup. The
governing rule is the one worth carrying to Linux: **Gate A hashes the whole tree and must run alone
on a frozen one** — running it inside a live workspace is what produced the discarded
`source_unchanged: false` capture that is not among the four receipts above. (Source: the commit
message of *"Gate A 75/75 with a valid source identity, and the setup that took four runs"*, which is
the only place this run's per-attempt history is recorded.)

**The driver records rather than gates, and that is why a receipt exists at all.**
Continue-on-failure is its default (`test_continue_on_failure_is_the_default_and_may_be_stated`),
so every cell's outcome is captured whatever the outcome is. This is the entire reason it produced a
*diagnosis* on its first run where `tools/gate-a-candidate/candidate_envelope.py` structurally could
not: the envelope's `run_phase` raises on the first non-zero cell while its report is written only
after the loop completes, so a phase failing at cell 1 of 41 emits nothing. Forward rule 2 (§12) is
not a style preference — it is the difference between a 12/75 capture that reported all 63 of its
failures and a run that would have reported none of them.

#### What the pass establishes, and what it does not

Gate A is **closed as a deterministic gate on this host**. State it that way and no further.

It specifically does **not** establish:

- **It is not a reproducibility claim.** Nothing here is a clean-room bootstrap; the build was
  produced by this host's existing toolchain, and no second independent rebuild was compared.
- **It does not bind the transitive host toolchain closure.** Only the recorded *direct* tool
  versions are pinned. Everything those tools in turn depend on is unbound.
- **It does not resist a same-UID actor substituting inputs mid-run.** A process running as the owner
  can replace what the gate reads while it reads it.
- Consistent with the `testing.md:32-53` threat-model boundary, which is where that non-claim is
  normative. Gate A does not exceed D1, and it was never meant to.

### 7.2 The five defects the capture found

**This is the evidence that running the gate was worth doing rather than reasoning about.** Three
captures were needed: capture 1 scored **12/75**, capture 2 scored **70/75**, capture 3 scored
**75/75**. Every one of the five was invisible to inspection and none was hypothetical.

| # | Defect | Where | Disposition |
|---:|---|---|---|
| 1 | `{cargo}` resolved through the `~/.cargo/bin/cargo` symlink to the **rustup shim**, which dispatches on `argv[0]`; every cargo cell died with `error: unexpected argument '--all' found`. **Caused 59 of the 63 failures in capture 1.** | `tools/gate-a/run_gate.py` | **FIXED** — resolve the parent, preserve the invoked name. Regression `ToolNamePreservationTest` |
| 2 | libFuzzer `deadly signal` in `transcript_cursor` at execution **47,755** | `fuzz/fuzz_targets/transcript_cursor.rs` | **FIXED** — see §7.3; the *target* was wrong, not the product |
| 3 | `cargo-fuzz` creates `fuzz/artifacts/<target>/` and `fuzz/target` relative to **its own manifest**, regardless of `CARGO_TARGET_DIR`. The final `residue` cell correctly failed on them, making Gate A **structurally unpassable whenever the fuzz phase ran** | `scripts/gate-a-fuzz.sh` | **FIXED** — prune, but **fail loudly if non-empty** (`prune_empty_source_output`, `:205-221`), because a file there would be a crash written outside the evidence root |
| 4 | The driver never applied `umask 077` although `testing.md:393` mandates it for **every** cell; `tsc` emitted 0644 into the validation stage, `dist-stage.mjs verify` rejected the tree, and **all three TypeScript cells failed** | `tools/gate-a/run_gate.py` | **FIXED** — `os.umask(0o077)` at `:503` with the citation at `:498`. Regression `UmaskTest` |
| 5 | `owner_eof_reaps_a_hup_term_ignoring_pane_tree_and_surfaces_lease_loss` failed once with `private process boundary observation failed / could not run /bin/ps: No child processes (os error 10)` | `bin/pmux-rmuxd/tests/process_blackbox.rs` | **OPEN — debt row C8, §9.4** |

Defect 3 deserves a second read: it means the gate could never have passed in its previous shape, and
no amount of review would have surfaced it, because the failure is produced by a tool writing outside
the directory it was told to write to.

**Two more, found on 2026-08-06 by running all 81 cells again rather than reasoning about them.**
Neither was visible to the phase that introduced it: the first is a *test* file's precondition and no
Rust check reads it, the second passes under every umask but the one the gate itself sets. The re-run
scored **79/81**, then **80/81** after both were fixed.

| # | Defect | Where | Disposition |
|---:|---|---|---|
| 6 | The five real-Claude pool waves are documented as "`#[ignore]`d **AND** gated on `PMUX_POOL_REAL_CLAUDE`", and the code `.expect(..)`ed it instead — a message promising a gate over a predicate that was an assertion. `release_full_stack_e2e` runs the whole crate under `--include-ignored` and supplies only `PMUX_E2E_BIN_DIR` and `PMUX_E2E_TYPESCRIPT_DIST_DIR`, so the cell reported `14 passed; 5 failed` with five identical panics and **could not pass on any host** — nor could an operator opt in, since the variable is not in the driver's `ENVIRONMENT_ALLOWLIST` (`run_gate.py:85`) | `crates/e2e/tests/pool_concurrency.rs:432` | **FIXED** — skip loudly and return `None`, exactly as the sibling real lane at `cross_cell_contamination.rs:2258` already did. Nothing below the gate changed: with the variable set every wave runs as before. Regressions `the_real_lane_is_gated_on_its_variable_rather_than_asserting_it` (an *equality*, so neither "always panic" nor "always skip" survives) and `every_real_lane_test_names_and_reaches_its_gate` (derived from the file's own source) |
| 7 | `test_directory_mode_drift_changes_the_digest` `chmod(0o700)`'d a directory `setUp` had just created. Under `umask 077` — which `testing.md:124` requires of every gate command and `run_gate.py:914` sets — `mkdir` already yields 0700, so the chmod was a **no-op**, the digest did not move, and the test failed. Its verdict was decided by the ambient umask, not by the digest it names; it had never run correctly in the environment the docs mandate. This is defect 4 one level down, in a test rather than in the driver | `tools/linux-docker/tests/test_source_digest.py:191` | **FIXED** — derive the drifted mode from the directory's actual mode (`current ^ 0o070`) and assert the recorded mode equals the derived value. Proven by mutation: with directory mode removed from the aggregate the test now fails under **both** 0022 and 0077, where before it passed under 0022 and failed under 0077 regardless of the digest |

**One more, found on 2026-08-06 by chasing an intermittent to its mechanism rather than to its
rate.** It is the same shape as defects 4 and 7 — a verdict decided by something ambient rather than
by what the check names — one level further down again, this time inside the identity itself.

| # | Defect | Where | Disposition |
|---:|---|---|---|
| 8 | `_repository_control_snapshot` recorded a **directory's** `mtime_ns`/`ctime_ns` as part of *identity*. A directory's mtime records when its entry set last moved, which is not a property of the directory: every reader-shaped Git command creates `.git/index.lock` and unlinks it again even when it writes no index. MEASURED on this checkout: an external ~6 s workspace poller moved the Git directory's mtime **14 times in 30 s** with nothing of ours running, and `workspace_revision_capture` — whose bracket is ~380 ms — aborted **1 capture in 20** with `Git repository control identity changed during capture` on a tree whose `git status --porcelain` was byte-stable throughout. Through `phase0_lib.observe_source_identity`, which brackets a whole source manifest with two captures, that surfaced as `gate_f/phase0_self_tests` red in **2 of 12 isolated runs** — debt row **C11** | `tools/linux-docker/source_digest.py:897` | **FIXED** — a directory identity is `(device, inode)` plus mode, ownership and link count, and carries no timestamps; a regular file keeps them, and its bytes are bound by `sha256` besides. The validator's field set moves with the producer in both directions, so a producer that emitted one again is refused rather than silently reintroducing the abort. Regression `test_control_directory_identity_survives_its_own_entry_set_moving` — a **property** over a real Git directory (snapshot, add and remove an entry, snapshot, assert equal), not an assertion about a field list. Proven by mutation twice: reverting the producer fails it on `assertEqual(before, after)`, reverting the validator's directory field set fails it on the accept half. Post-fix: **0 of 30** captures moved, against 1 of 20 before |

**And one found by looking for the class rather than for a symptom**, in a guard that has reported
success on every gate run there has ever been.

| # | Defect | Where | Disposition |
|---:|---|---|---|
| 9 | `prepare_validation_root`'s docstring says it creates *"the documented validation tree owner-private, or refuse"*, and `VALIDATION_CHILDREN` was a hand-written `fuzz fuzz-evidence typescript-dist`. **Twenty-one `gate_a` cells set `CARGO_TARGET_DIR={validation}/cargo-target/<name>`**, and `docs/testing.md:391-396` documents that child in the same breath as the other three. So the one child every vendor build writes into was created by cargo under the ambient umask and **never mode-checked**: an operator who pre-created it 0755 got no refusal, and twenty-one builds wrote into a directory somebody else could read — which is *precisely* the case the docstring says the guard exists to catch, in the one child it never looked at. The message promised the documented tree; the predicate tested three quarters of it | `tools/gate-a/run_gate.py:82` | **FIXED** — `validation_children(manifest)` derives the set from every `{validation}/<name>` the manifest names, keeping the documented three as a **floor** that a broken derivation cannot drop below (it refuses instead). Only the first path component is taken, because `docs/testing.md:395-397` lets a cell select a named child *of* `cargo-target`, and it is `cargo-target` that must be owner-private. The manifest is now loaded before the tree is prepared; both still happen before any cell runs. Two regressions, both proven by mutation against a producer that returns the literal list: `test_every_validation_child_the_real_manifest_uses_is_prepared` (derives the reference set from the real manifest and fails naming `cargo-target`) and `test_a_group_readable_derived_validation_child_is_fatal_too` (the behavioural one — a pre-created 0755 `cargo-target`, with the root deliberately left 0700 so the refusal cannot come from the root instead) |

### 7.3 The fuzz finding, attributed carefully

**It would have been easy to fix the wrong thing here.** The crash was real; the defect was in the
test, not the product.

Crash input `crash-3f4d09ccb50dbe940c95532eb00a64f65f24f3da`:
`frae\x1dted\x1btrane` + 30×`\r` + `\n` + `then A re` + 21×`\r` + `\n` + `then A re\x11\x11\x11\x11e\n`.

The target asserted `!line.bytes.ends_with(b"\r")`. But `crates/claude/src/cursor.rs:188` strips
**exactly one** trailing CR — CRLF normalization and nothing more — so a line whose source ended in
several CRs legitimately still ends in `\r`.

Two independent sources confirm the **product** is correct:

- `crates/claude/tests/transcript_properties.rs:651` models the carriage return as a **boolean per
  record** — at most one, by construction.
- `spec.md`'s CRLF→LF normalization governs the **prompt** path, not the transcript path.

**The tracked fuzz target over-specified a contract the cursor never promised.** Changing
`cursor.rs` to strip greedily would have silently corrupted transcript lines that legitimately
contain CRs, to satisfy an assertion nobody had ever justified.

**It surfaced only because the lane ran twice.** The first run added 1,223 corpus units; the second
hit the assertion at execution 47,755. A single run would have missed it entirely. **Record that as a
property of corpus accumulation worth preserving: the corpus is a durable asset, and a fuzz lane that
starts from empty each time is a weaker lane than its run count suggests.**

Fixed three ways: the assertion corrected with the reasoning inline; the crash seeded as
`fuzz/corpus/transcript_cursor/regression-multi-cr-line`; and a minimized deterministic regression
`cursor_strips_exactly_one_trailing_carriage_return`
(`crates/claude/tests/cursor_and_parser.rs:75`) covering `a\n`, `a\r\n`, `a\r\r\n`, `a\r\r\r\n`,
`\r\n`, `\r\r\n` — taking that file from 13 to 14 tests. The fuzz finding is now held by a test that
runs in the normal suite, not only by a 50,000-run lane.

### 7.4 The frozen release directory, re-confirmed

The frozen release directory buys **zero additional test coverage** — verified three ways:
`grep -rl "PMUX_GATE_A" --include='*.rs' --include='*.py' .` returns **0 files**; the shared fence at
`tests/support/candidate_binary.rs` (260 lines) checks path canonicality, regular-file type, exec
bit, dev/inode, mode, length and SHA-256 but **never the build profile**; and every black-box suite
passed against `target/debug` **with the identity fence engaging and being satisfied**. It is a
provenance and attestation requirement, and it is a legitimate one. It is not a technical
prerequisite for any row.

### 7.5 Gate B — real Claude, macOS

| Fact | State |
|---|---|
| Authorized ceiling | **100** global attempts (decision D4) |
| Consumed | Not written here, and not written in `evidence/README.md` either: `python3 tools/phase0/phase0.py budget --ledger evidence/model-attempt-ledger.ndjson` counts it from the file, adding the ordinals that predate it (1-4, attested by the first record `{"global_attempt":5,"kind":"approved_prior_baseline"}`) and the **four detached reservations all numbered 31** (`evidence/README.md`, "Four detached reservations"). This row said **47** consumed with **53** remaining while the file had reached ordinal 81 — 85 and 15. A count in prose is stale the moment the next attempt reserves, exactly as a pinned digest is |
| **Remaining** | `remaining`, from the command above |
| Recorded turns | **24**, all `effort=low`, categories `{fresh: 21, persistent_seed: 1, same_process_warm: 2}`. The ledger now also carries **ten `effort=medium` reservations at ordinals 34-43** — the 2026-07-28 calibration campaign below, nine graded turns and one ordinal lost to a permission prompt |
| Compatibility cell, identical on all 24 of the 2026-07-19 turns | `{"claude_version":"2.1.215","os":"macos","arch":"aarch64","terminal_profile":"transparent","input_transport":"sdk","tested":false,"transcript_drain_ms":2000}` |
| Completion evidence, 24/24 | `authority=transcript`, `prompt_acknowledged=true`, `terminal_prompt_observed=true`, `terminal_quiet_observed=true`, `transcript_drained=true`, `lifecycle_hook_observed=false` |
| Promotable? | **No.** The 24 turns are bound to two non-reproducible source digests (`81ae72b2…`, `208103dc…`) on a tree that no longer exists; the `pmuxd` binary behind them (`5a6a0c5e…`) exists nowhere on disk. **Per decision D5 they are budget accounting only, and Gate B coverage restarts from zero** against one new frozen digest |
| Coverage still entirely untouched | **`replay` only.** Ordinals 44-55 (2026-07-29) added live coverage for `resume` (2), `persistent` (3), Unicode/wrapped **input** (3, with `--lifecycle hybrid`), `facade` (2) and `deadline` (1); `tools` came off this list on 2026-07-28. **`cancellation` and `attach/detach` are not on it either, for a different reason:** they are unreachable through the phase0 envelope -- `--scenario` accepts only `{one-shot, persistent, resume, claude-p-one-shot}` and `phase0.py matrix` lists direct rmux control, direct PTY input and `attached_stream` as `unsupported_by_envelope`, so `.context/campaigns/40-cancellation.py` and `50-attach-detach.py` refuse, spend nothing, and document what envelope change would unblock them. **`replay` was neither covered nor explained and is the one genuine remaining hole.** |

**What the 24 turns do still prove, and it matters:** the four hard-coded Claude-TUI geometry
constants in `driver_io.rs` are **not** unvalidated. `terminal_prompt_observed=true` and
`prompt_acknowledged=true` on 24/24 real turns means the admission path ran end to end against real
Claude — `active_editor` found the `❯` anchor, `prompt_glyph_col`'s all-whitespace-prefix test
passed, `empty_cursor_position` became true, the single bracketed paste landed, the changed-stable
render was proven, at most one Enter was sent, and the exact post-arm typed-user row appeared in the
transcript. The Ink-bordered-box hazard (a leading `│` U+2502 failing
`prefix.chars().all(char::is_whitespace)` at `driver_io.rs:172-183`) **did not occur** for this cell.
Corroborated 145 patch releases earlier by five real Claude Code v2.1.70 screens recoverable from
`origin/main:fixtures/claude_code_*.txt`, in which the `❯` is the first byte of its line, no border,
exactly 2 rows above the bottom.

So the TUI-constant risk is **regression on a known-good baseline**, not discovery. What remains
unvalidated and must stay in the claim language: other Claude versions (2.1.215 only), other
terminal profiles (`transparent` only), other input transports (`sdk` only), non-ASCII width (no CJK
or emoji prompt was ever sent), and any future Ink re-theme.

**The installed Claude has since drifted to 2.1.220** (`claude --version`, 2026-07-27) from the
historical 2.1.215 of all 24 turns. **Gate B therefore establishes a *new* compatibility cell, not a
continuation of the old one**, and the four TUI geometry constants have **never been observed against
2.1.220**. The paragraph above is a statement about 2.1.215 and does not transfer. Treat the first
2.1.220 turn as the discovery case it is: five patch releases is exactly the interval over which an
Ink re-theme lands without an announcement.

#### The 2026-07-28 live campaign — nine calibration grades against 2.1.220

Run from a **standalone clone outside this workspace**, for the reason `docs/testing.md` records
under *Environment preconditions*. Claude Code **2.1.220** (`claude.binary.path`
`…/versions/2.1.220`), `effort=medium`, `untested_transcript_drain_ms` **2,000**,
`turn_timeout_seconds` **600**, `terminal_profile` `transparent`, `input_transport` `sdk` — every one
of those read back from the campaign's own reservations in `evidence/model-attempt-ledger.ndjson`,
ordinals **34-43**, ten `effort=medium` records.

**Nine graded turns cost ten ordinals.** The ledger resolves each reservation's prompt by content
sha256 against `tools/phase0/prompts/`, and the mapping is exact: ordinals 34, 35, 37, 38, 39, 40,
41, 42, 43 are grades 01-09 in order. **Ordinal 36 is grade 03 run once with `permission_mode: null`
and no result**; the next campaign re-reserved grade 03 as ordinal 37 with
`permission_mode: dangerously-skip-permissions` and every later ordinal carries that mode. The
observed failure behind that switch is the permission-prompt case recorded as debt row **S3** (§9.9).

`gap` is the late-arrival gap in ms — the signed `last_transcript_activity_at_ms −
terminal_candidate_at_ms` of `compute_gap` (`tools/phase0/verify_calibration.py:377-395`, protocol
rule at `crates/protocol/src/v1.rs:1302-1327`); `out` is assistant output tokens.

| Grade | ordinal | gap (ms) | out (tokens) |
|---|---:|---:|---:|
| `01-baseline-trivial` | 34 | 0 | 4 |
| `02-poem-only-no-tool` | 35 | 0 | 130 |
| `03-poem-hash-single-tool` | 37 | 0 | 525 |
| `04-poem-hash-single-tool-variant` | 38 | 1 | 492 |
| `05-poem-hash-reverse-transform` | 39 | 0 | 2,052 |
| `06-poem-hash-triple-transform` | 40 | 1 | 2,385 |
| `07-long-poem-hash` | 41 | 1 | 2,182 |
| `08-unicode-poem-hash` | 42 | 0 | 1,048 |
| `09-long-unicode-poem-hash` | 43 | 0 | 1,443 |

Output spans **4 → 2,385 tokens (~600×)** and 0 → 3 sequential tool calls. Observed drain was
~2,350 ms against the configured 2,000 ms, and **the gap never exceeded 1 ms** on any of the nine.

**Seven of the nine grades were a hash oracle, and all seven reproduced.** The checker's
`expects_hash` is literally `"SHA256" in text` (`tools/phase0/verify_calibration.py:151`); grades
03/04/07/08/09 ask for a bare `SHA256: <hex>`, grade 05 for `SHA256(poem)` and `SHA256(reversed)`,
grade 06 for those plus `SHA256(upper)` — verified by reading the nine files. **7 requested, 7
independently reproduced from the poem text pmux's own `TurnResult` carried, 0 mismatches**,
including both transforms and both CJK+emoji grades. That is a checksum over the whole pipeline:
rmux PTY → Claude's JSONL → `crates/claude` cursor and parser → `final_text_blocks.concat()`
(`crates/claude/src/engine.rs:843`) → protocol → CLI stdout. One dropped, reordered or re-encoded
byte anywhere in it makes the digest disagree.

**Exactly which non-ASCII path this tested, stated precisely, because it is easy to state backwards.**
All nine prompt files are **pure ASCII English instructions** — verified, zero non-ASCII bytes in any
of them. `08-unicode-poem-hash.txt:1-2` asks Claude for a poem that "mixes Chinese or Japanese
characters with at least 2 emoji", and `09-long-unicode-poem-hash.txt:1-2` asks the same at ≥40
lines. So the campaign is the first test of the non-ASCII **response** path — transcript read, parse,
block concatenation, hashing, publication — and it is **not** a test of the non-ASCII **input** path,
the bracketed paste of non-ASCII text into the composer. §7.5's limitation above, "no CJK or emoji
prompt was ever sent", is still true verbatim, and **§11 item 2 remains open**: the exposure it names
is the all-whitespace-prefix test at `driver_io.rs:172-183`, which only the input side reaches.

**What this campaign cannot prove, stated before anyone quotes the table.** **n = 1 per grade** — nine
turns, not nine distributions; the checker labels every single-sample row
`(single sample: not a real distribution)` (`tools/phase0/verify_calibration.py:857`) precisely so
this cannot be misread. `effort=high` is **outside authorization**
(`APPROVED_EFFORTS = ("low", "medium")`, `tools/phase0/phase0_lib.py:97`, enforced at `:1368-1369`), so
the highest-latency configuration is unmeasured. One machine, one Claude version, one terminal
profile, one input transport. **This does not license cutting `transcript_drain_ms`.** The drain is a
bounded proof of *absence*, and nine turns of headroom on one host is not a lower bound on when a
late row can arrive. What it does establish, and all it establishes, is that **response structure
does not drive late arrival**: a 600× swing in output size, three sequential tool calls, and
non-ASCII output moved the gap by at most 1 ms.

**The receipt exists and is tracked: `evidence/gate-b-drain-calibration.json`.** It is where every
number in this section comes from, and it is what to check this prose against. It is the `--json`
output of `tools/phase0/verify_calibration.py` — the second, standalone verifier that recomputes the
late-arrival gap and each reported hash from the published attempt tree rather than calling
`phase0_lib.py` — with two documented deviations, both recorded in the file's own
`provenance.producer`: every `attempts[].attempt_dir` key is dropped because it is an absolute host
path, and a `provenance` block the verifier does not emit is appended by hand.

**What the receipt carries:** `attempts_discovered` **17** over **13 distinct ordinals** — ordinal 31
appears five times and 32-43 once each; the receipt does not itself explain the duplicate, the
explanation is in `evidence/README.md`. `attempts_by_bucket` is successful **10**, failed **7**,
incomplete 0, fatal 0, unreadable 0, with `attempts_partition_balances: true`; `attempts_failed` is
not a count but a **7-element list naming each failure and its error** (six `pmux run exited with 1`,
one `frozen source changed during the campaign`). Then: the per-grade block for all nine grades keyed
by `grade_order`; `overall_gap_distribution` — count 10, min 0, median 0, **max 1**,
`late_row_attempts` **0**, `noise_band_ms` 20; `hash_tally_overall` — **match 7**, `not_applicable`
3, `no_result` 7; `configured_transcript_drain_ms` **2000** with the note *"constant across every
successful attempt"*; `effort_tally` low 1 / medium 9; `grade_source_tally` `prompt_sha256` **17**,
i.e. every attempt was graded by prompt content hash, not argv position; `failing_conditions: []`;
and a 17-element `attempts` array of `{attempt_id, global_attempt_ordinal, prompt_suite_index, grade,
grade_source, effort, status, error, fatal_error, gap_ms, gap_uncomputable_reason, hash_overall,
hash_checks, notes}`.

**Provenance is in the `provenance` block, and it is hand-added, not measured.** The verifier's own
output carries **no** Claude version, source digest, commit, platform, or tool identity; the block
supplies them from the ledger and says so in each `note`. It has `claude` (the **2.1.220** binding,
whose only authority is `evidence/model-attempt-ledger.ndjson`
`claude.version_output.normalized_version` — *not* this receipt), `source_revision` (digest
`e0b94fa0…`, `git_head` `d87fb69`), `candidate.binaries_sha256` for the release tree the ten
successful attempts exercised, `evidence_root` (`tracked: false`), `platform`, `producer` (command,
tool, tool sha256, post-processing), `reserved_at_utc`, and `ledger_cross_reference` (13 rows
matched, 4 not in the ledger — the four detached ordinal-31 reservations). **Read the notes, not just
the values**: this block is an assertion about where each fact came from, and it is only as good as
`evidence/README.md` and the ledger it points at.

**What the receipt does not carry, and you must not infer:** `headroom_ms` is `null`
**deliberately**; and there is no transcript, poem text, or `TurnTimings` — those live in the
per-attempt artifacts, which are **not in this repo**. Every reservation records an
`artifact_directory` under `<HOME>/pmux-validation-20260728-104907/gate-b-evidence/`
(`evidence/model-attempt-ledger.ndjson`), i.e. inside the standalone clone. So the receipt is a
**reduction you can read and re-read, not an input you can re-derive**: re-running
`verify_calibration.py --evidence-root …` requires that directory, and it is gone from any fresh
clone. `evidence/` holds three tracked files — the ledger, this receipt, and `README.md`.

The receipt closes the condition this paragraph used to state as open. This campaign is still
**calibration input, not promotable coverage**, but for the reasons above it — n = 1 per grade,
`effort=high` unauthorized, one host, one Claude version — and no longer for want of a published
receipt.

### 7.6 Gate C — Docker, Linux

**Never built, never run.** The lane is deferred (debt rows 23/24) and is **currently red**:
`tools/linux-docker/tests/test_runner.py:277::test_linux_manifest_is_the_exact_ordered_candidate_projection`
fails because `gate-a-manifest.json` was not re-projected when `phase-manifest.json` was trimmed
82 → 75 cells (debt row **C6**), on top of 12 pre-existing host-Git ownership failures that
decision **D6** de-scopes. No reviewed multiarch base-image index digest is recorded anywhere in the
repo, so Gate C currently has no runnable invocation.

---

## 8. Coverage matrix pointer

`testing.md` §4 is **116 rows**, re-derived 2026-08-06 by parsing the section rather than by
carrying a number forward. **The `95` this paragraph carried was 19 stale**, and the `COVERED` `32`
below was stale by the same 19 — the same defect as the `544` test count in §5 and from the same
cause: a total that is retyped instead of re-read. Two of the 116 are this change's (`S-46` one-answer
deadline reclassification, `S-47` no retained rmux handle); the other 19 accumulated unattributed
across the work after the launch-environment allowlist, and **no per-change breakdown exists, so do
not infer one from this table.** Reproduce the count and the census rather than trusting them:

```bash
python3 - <<'PY'
import re, pathlib, collections
lines = pathlib.Path('docs/testing.md').read_text().split('\n')
start = next(i for i, l in enumerate(lines) if l.startswith('## 4.'))
end = next(i for i, l in enumerate(lines) if i > start and l.startswith('## 5'))
rows = re.findall(r'^\| ([A-Z]+-\d+) \|.*\| ([A-Z-]+(?:-L\d)?) \|\s*$',
                  '\n'.join(lines[start:end]), re.M)
print(len(rows), collections.Counter(status for _, status in rows))
PY
```

The historical additions are retained for the delta history: the agent-profile work added six, all
`COVERED` (`P-09`, `S-23`, `S-24`, `CLI-12`, `CL-08`, `CL-09`), and the launch-environment allowlist
added four (`S-25` `COVERED`; `S-26`, `CLI-13`, `CLI-14` `OPEN-L3`).

| Status | Count (derived 2026-08-06) | as recorded before | was |
|---|---:|---:|---:|
| `COVERED` | 53 | 32 | 31 |
| `OPEN-L3` | 44 | 44 | 41 |
| `OPEN-L4` | 1 | 1 | 1 |
| `OPEN-L5` | 9 | 9 | 9 |
| `EXTERNAL` | 3 | 3 | 3 |
| `OUT-OF-SCOPE` | 6 | 6 | 6 |

**No open row moved.** All ten of the historical additions were additions, not re-scorings, and so
are `S-46` and `S-47`: nothing that was open closed, and both new rows are `COVERED` on
deterministic in-repo checks. The release-blocking deterministic set grew from 50 to **53** — the
three new `OPEN-L3` rows are new surface (an auth-policy branch, a new CLI flag, and a new probe
field), not a re-scoring of anything that was previously green.

**The open deterministic rows are `OPEN-L3` + `OPEN-L5` = 44 + 9 = 53 at the derived census above**
(the paragraph below was written when that sum was 41 + 9 = 50 and its `~32`/`~18` split is scoped
to those 50; the three later `OPEN-L3` additions are named in the paragraph above and are not part
of either bucket). Honestly scored:

- **~32 were attestation-pending** — every cited test observed green against `target/debug`, waiting
  only on a frozen release directory and a receipt. **Both now exist** (§7.1): the frozen candidate
  was built into an owner-only validation root and the 75/75 receipt is published. What remains for
  these rows is the *bookkeeping* of re-scoring them against it — hours of transcription, not
  engineering, and the input is now a file rather than a plan.
- **~18 rode on `crates/e2e/tests/full_stack.rs`, which has now executed** (P-08, T-11, S-01, S-02,
  S-07, S-10, S-11, S-12, S-13, S-17, S-18, S-19, S-20, CLI-07, CLI-08, CL-07, PLAT-04, BIN-07) —
  and has since executed **against the frozen candidate**, as Gate A cell `release_full_stack_e2e`,
  8 passed / 0 failed. The receipt they lacked exists.
- **0 open rows need a new test. 0 need a product change.**
- The single `OPEN-L4` row (PKG-01) is **circular, not pending**: `package_smoke.py:1109-1113`
  unconditionally requires five `PMUX_PACKAGE_SMOKE_*` environment anchors that have **no producer
  anywhere in the repo**, so both published Gate A package commands refuse to start under any
  driver. There is no packaging defect in the loop — the mechanics pass 35/35.

**Read `testing.md` §1's `AUTHORED` definition (`testing.md:76-94`) before interpreting any row.** It
is the vocabulary word that separates coverage from attestation: *"`AUTHORED` is a statement about
attestation, not about coverage,"* and *"where a row says 'exact release Gate D/E rerun pending,'
that means **receipt** pending, not **coverage** pending."* That column fusion is what allowed
bookkeeping to block the project for a week; §4 is not restructured before the freeze, and
`testing.md:76-94` is the correction of record until it is (debt row 35).

One thin spot remains in the `COVERED` set, recorded rather than hidden, and one is now closed:

- **CLOSED — the enum drift channel.** P-02 claimed "every enum discriminant" is pinned by the shared
  manifest; that was true only for the four manifest unions, leaving **17 nested string enums**
  hand-duplicated in `clients/typescript/src/protocol.ts` and `clients/python/pmux_client/protocol.py`
  with nothing pinning them. All 17 are now pinned under `manifest.json` `value_enums`, with
  **both-direction** exhaustiveness assertions in all three languages (new row `P-09`;
  `crates/protocol/tests/v1_conformance_vectors.rs::shared_manifest_value_enums_match_the_rust_string_enums`
  asserts the key set *and* every value list). The re-sourcing also removed the duplicated inline
  literals in both clients — each had carried a local `SESSION_STATES` plus nine inline arrays — so
  the two runtime validators (`clients/typescript/src/client.ts` `requireEnumField`,
  `clients/python/pmux_client/client.py:862-866` `_values`) are now **transitively manifest-pinned**
  rather than independently hand-maintained. This was debt row 34; see §9.3.
- **STILL OPEN — 29 rows carry no `path::name`-qualified citation** (20 of them deterministic;
  worst: S-11, S-18, S-20, S-10, NF-04, DEP-01). A row citing only "redaction tests" cannot be
  invalidated by deleting a test (debt rows 35/36). Spot-checking S-18 found 17 real redaction tests
  — the citation is thin, not the tests. The six new rows all carry qualified citations, so the
  count of uncited rows did not grow; `S-46` and `S-47` carry qualified citations too. **The
  denominator "of 91" is deleted rather than updated: 29 was counted against a 91-row matrix and
  §8 now derives 116, so the ratio is not recoverable without re-auditing every row's citation,
  which is debt row 36 and not a drive-by. The numerator is the number that was actually measured.**

---

## 9. Design debt and non-optimal aspects

### 9.0 The size of the gap

Taking every non-vetoed structural proposal from six analysts and two critics, the entire remaining
structural distance between what exists and the best shape anyone could describe is:

> **−659 lines of product implementation (3.0% of 22,079), one process boundary, and six
> documentation sentences — with zero user-visible behavior change.**

There is no subsystem to redesign, no layering inversion, no missing abstraction, and no abstraction
that should not exist. A mechanical scan of every `pub` item in `crates/*/src` + `bin/*/src` for zero
non-defining references returned **six items**, and more than half of the 3.0% is deferred behind
work that has now happened.

The two known **HIGH**-severity product defects were **fixed** in `ce62bcc`:

- **D1** — `complete_active` / `timeout_completed_at_commit` silently wedged the session when the
  commit-time transition failed (cleared `active`, stored no terminal record, emitted no terminal
  event, held the writable-attach reservation forever). Now routed into
  `poison_after_unpublishable_terminal` like every sibling path.
- **D2** — `OwnedProcessBoundary::tracked_pids` was unbounded and unfenced against PID reuse, giving
  both a permanent `Ok(false)` liveness wedge and a path to `SIGKILL` an unrelated recycled session
  leader. Now fenced by a birth token (`process_boundary.rs:363` `is_recycled`, `:436`
  `member_identity_still_proven`, `:411` call site).

**C2-C7 are the residuals of those fixes**, found by independent review of `ce62bcc` and recorded
here rather than repaired on freeze eve. They are in the table in §9.3.

### 9.1 The rule (D9)

> **D9 — design admissibility and freeze.**
>
> **A design change is admissible before v1 if and only if it satisfies at least one of:**
>
> **(a)** it fixes a defect reachable by a **non-adversarial caller through the public v1 surface**,
> or by an ordinary accident (PID reuse, a clock step, a crashed client, a full disk);
> **(b)** it **deletes** code, documentation, or on-disk artifacts with **no observable behavior
> change at the v1 surface**; or
> **(c)** it is a **documentation edit that makes an existing sentence true.**
>
> **Everything else — every reshape, every consolidation, every "the shape is wrong even though
> every line works" — is recorded in this file as
> `file:line · one-line defect · one-line cost of leaving it · SAFE/NEEDS-CARE/RISKY`, and is
> NOT DONE.**
>
> **The design is FROZEN when this file exists and every recommendation in the pre-v1 design review
> has been either applied under (a)/(b)/(c) or written into it.**
>
> **After the freeze, a design finding may reopen it only if it is accompanied by an *observation* —
> a failing test, a live-run artifact, a real caller report, a crash. An argument, however good, is
> not an observation. A design finding supported only by reasoning is a complete and sufficient
> response when it is appended here, and no receipt is invalidated by one.**

**Appending a line to this file is a complete and sufficient response to any design finding.**

And the objective, so that "optimal" has a referent:

> **v1 objective: minimize time to a defensible release claim, subject to zero known defects
> reachable by a non-adversarial caller through the public v1 surface.**
> Line count is not in the objective. Test-to-source ratio is not in the objective. Process count is
> not in the objective. Provenance strength beyond `testing.md:32-53` is not in the objective.

D9 is the design-side analogue of D1 (`testing.md:32-53`, the evidence threat-model boundary), which
converts an unbounded class of adversarial findings into recorded nonclaims. Both give a legitimate
way to say "no" that is not a judgement about the finding's merit.

### 9.2 Why this file exists (the empirical justification)

Six competent analysts read the same tree and returned **opposite verdicts on identical code**:

- Two reports read the *same 73 lines* of `crates/protocol/tests/v1_golden.rs:1211-1283` and
  returned `DELETE / SAFE` and `KEEP / RISKY`.
- Two reports read the same **seven producerless `ErrorCode` variants** and returned `DELETE / SAFE`
  and `KEEP / RISKY`.
- Two reports read the same **13 test doubles** and returned `SIMPLIFY` and `KEEP`.

That is not analyst error. It is six people optimizing six different objective functions —
conceptual coherence, process-topology minimality, wire-surface closure, line count, failure
isolation, instrument ratio — over one artifact. Every one is defensible; none is *the* objective,
because **nobody had written the objective down**. Optimality is defined only against an objective
function, and this project never had one. A 28th report would find a fifth thing to delete and a
sixth thing to keep, forever. That is why the rule, and this file, exist.

One structural aggravator, named without blame: this is a solo project with an adversarial
self-review discipline and no external reviewer. Adversarial review has no natural stopping point
when the reviewer and the author are the same person, because there is always another hypothetical.
The missing artifact is not rigor. It is a **written scope boundary someone is allowed to invoke to
close a finding.**

### 9.3 DEFERRED — advisory rows 22-41 and review rows R1-R4 (AFTER-FREEZE)

Good ideas, wrong time. Format: `file:line · defect · cost of leaving it · SAFE|NEEDS-CARE|RISKY`.
Rows **22-41** are the design advisory's Q3 numbers so the two documents cross-reference. Rows
**R1-R4** came from the 2026-07-29 pre-push review and have **no advisory counterpart**; they are
lettered rather than numbered because the advisory's 42-56 are already spoken for by §9.5. Each is
recorded here rather than fixed because none satisfies the D9 stopping rule (§9.1): they are
optimisations or availability edges, never a wrong answer, and the pre-push window is not when to
take them.

| # | file:line · defect · cost of leaving it · risk | Δ lines | Why it waits |
|---:|---|---:|---|
| 22 | `tools/gate-a-candidate/candidate_envelope.py` (4,279 lines + tests) · envelope checker the driver replaces · dead harness outweighs product · **SAFE** | −5,366 | Only after a driver has produced one receipt. **Salvage ~125 lines first, above all the Cargo target-escape guard near `:2406`** — it refuses a phase cargo command whose `CARGO_TARGET_DIR` resolves into the frozen candidate (accident-class: a one-line manifest edit silently mutating the release binaries mid-run) |
| 23 | `tools/gate-a-candidate/` + `tools/linux-docker/` → `tools/_deferred/` · unmaintained lanes counted as live · harness:product ratio stays 1.85:1 instead of ~1.0:1 · **NEEDS-CARE** | 18,744 moved | A `git mv`, not a deletion; do it once the receipt exists |
| 24 | `tools/linux-docker/source_digest.py` (2,026 lines; the host-Git apparatus is ~1,214 of them) + ~450 test lines, ~45 in `run.sh` · host-Git provenance far beyond `testing.md:32-53` · 1,664 lines and 12 red tests carried · **SAFE** | −1,664 | **D6: scheduled, not debt.** De-scope to `rev-parse HEAD` + `status --porcelain` (~25 lines) as the *first act of whenever Gate C is picked up*; editing a deferred lane now is exactly the churn to avoid, and under row 14 the 12 red tests stop blocking at zero cost |
| 25 | `crates/e2e/tests/full_stack.rs:568-1165` · 597-line monolith → 16 tests + `Stack::boot`/`finish` + `OnceLock` candidates + per-test launch counts · one failure blanks the whole capture · **NEEDS-CARE** | +280 net | **Only now admissible — the monolith has run green once.** Row 27 remains a hard precondition. Had this been done first, the clean single-shot green baseline would not exist and every subsequent failure would have been ambiguous between a product defect and a decomposition defect |
| 26 | `CrossClientAssets::from_workspace()` in the `full_stack.rs` prelude · TS dist breakage blanks 14 integration cells · coupled failure domains · **SAFE** | ~10 | Real durable decoupling, but the dist stage is a proven 5-minute build, so not an unblocker. Take it **with** row 25 |
| 27 | `crates/e2e/tests/full_stack.rs:3978-4032` `assert_process_boundary_absent` · same PID-reuse blind spot as product defect D2, in the harness that certifies the product · numeric pgid/sid collisions once tests run parallel · **SAFE** | ~10 | Reuse `process_start_identity` (`:4035`). **Hard precondition of row 25** |
| 28 | `bin/pmux/tests/candidate_binding.rs` (150 lines) + `bin/pmux/tests/support/mod.rs:164-420` · duplicated binary-fence machinery reimplementing `tests/support/candidate_binary.rs` (260 lines, `#[path]`-included by seven other targets) · shared 8-binary fence stays at 2 negative tests instead of 8 · **NEEDS-CARE** | −257 | Retarget `candidate_binding.rs` at the shared fence, **then** delete the fork. Deleting `candidate_binding.rs` outright would take the fence underwriting every "the exact release binary was used" claim from 8 negative invariants to 2 |
| 29 | `bin/pmux-hook` · standalone binary that could be a hidden `pmuxd hook` subcommand; 1,943 lines across 4 files and a shipped release binary produce one boolean and one warning string · one extra release binary and its fence rows · **SAFE** | −666 | Mechanically safe (`pmuxd` already requires a subcommand and already links the service crate), but it renames a binary named in `full_stack.rs`, `scripts/gate-a-residue.sh`, `tools/linux-docker/evidence.py`, `tools/phase0/phase0.py`. Do **not** fold into `pmux` instead. **The hook *mechanism* stays regardless** (§2) |
| 30 | `crates/claude/src/engine.rs` `const RECORD_WORK: bool` (55 mentions, 25 `record_work::` call sites) · const-generic doubles the monomorphization so `size_scaling.rs` measures a path that is not literally production · measurement/production divergence · **NEEDS-CARE** | −90 | Keep `TranscriptAnalysisWork` and the exact-affine assert; delete only the const generic — `analyze()` and `analyze_with_work()` collapse to one monomorphization, so the measured path becomes literally the production path (net safety gain). Cost: one `saturating_add` per element visit |
| 31 | `crates/service/src/driver_io.rs:53-68`, `:1314-1327` `classify_terminal_screen` / `has_ready_prompt` / `is_prompt_input_line` · test-only fossil wrapper · it is the **only** entry point by which tests reach `blocking_screen`'s six-way table (`:1328`) and the "screen contents must not escape the classifier" leak invariant · **NEEDS-CARE** | −28 src, +~20 test | DELETE **only with the mandatory replacement test**: retarget `:1808-1843` at `blocking_screen` and `:1800-1806` at `prompt_glyph_col`. Only the `Ready` arm at `:78-80` is `#[cfg(test)]`; the cursor-less `blocking_screen` call at `:74-76` is **production and must stay**. Cheap substitute already available: one comment line at `:52` saying real rmux always supplies a cursor. **(Path corrected: earlier records said `crates/rmux/src/driver_io.rs`; the file is and has only ever been `crates/service/src/driver_io.rs`.)** |
| 32 | `clients/typescript/tests/dist-stage.mjs` adversarial third · defends the adversary D1 concedes; `full_stack.rs:112` `external_typescript_stage_contract_rejects_invalid_roots_membership_modes_and_aliases` re-checks the same six mutations as an independent consumer fence · 55 duplicate lines · **SAFE** | −55 | It produces the artifact the full-stack suite consumes — freeze it, do not iterate on it |
| 33 | `bin/pmux/src/cli.rs:115-116` `probe --keep` (+ `bin/pmux/src/main.rs:221`, `:259`) · `pmux start` plus a redacted echo; sole reason the code reasons about ownership transfer of a half-inspected session · one surplus CLI mode and its ownership logic · **NEEDS-CARE** | −25 | Smallest item on the board; do it with the CLI matrix so the cells stay exhaustive. Covered today by `bin/pmux/tests/process_boundary.rs:1354` |
| 34 | ~~`tests/conformance/v1/manifest.json` `value_enums` + exhaustive fences in Rust/TS/Python · 7 hand-maintained duplicate enum lists, incl. two **runtime validators** (`clients/typescript/src/client.ts` `requireEnumField`, and the Python equivalent) that would hard-reject a conformant server the day `SessionState` grows a variant · a drift *channel*, not a present drift · **SAFE**~~ | +345 / −95 estimated | **DONE 2026-07-27 — no longer debt.** Row retained, not dropped, because the estimate is the argument: the advisory called this "the highest coverage-per-line change available" and it landed as scoped. All **17** nested value enums are pinned under `value_enums`, with both-direction assertions in Rust (`v1_conformance_vectors.rs::shared_manifest_value_enums_match_the_rust_string_enums`), TypeScript (`golden-conformance.test.mjs`) and Python (`ValueEnumConformanceTest`, which additionally asserts that *every* Python `Literal` alias except `PmuxErrorCode` is pinned). The two runtime validators now source `V1_VALUE_ENUMS` (`clients/typescript/src/protocol.ts:557`, `clients/python/pmux_client/protocol.py:489`) instead of local literals, so they are transitively pinned. Coverage row `P-09`; §8's first thin spot is closed |
| 35 | `testing.md` §4 · Status column fuses coverage with attestation; "exact release Gate D rerun pending" appears in 50 rows and reads as *coverage* pending · the release document keeps overstating its own evidential position · **SAFE** | ~130 rows | 5 columns → 6 (`Layers \| Owners \| Coverage \| Attestation`), add `AUTHORED` to the rows, add `path::name` citations to ~20 rows, split S-11/S-20 into algorithm and calibration legs. 2-3 hours of hand work — **do it while writing the receipt**. The §1 `AUTHORED` paragraph (`testing.md:76-94`) is the 10-minute version and is already in place |
| 36 | `crates/e2e/tests/matrix_citations.rs` · the 157/157 citation-resolution figure is a described script nobody has · unexecutable claim · **SAFE** | +40 | Citation lint. Pairs with row 35 |
| 37 | `crates/service/src/native.rs:516` + `crates/service/src/v1/registry.rs:34` · two session maps both keyed by `SessionId`, both generation-checked, mutated in coordinated order at four sites (the D3/D4 seam); one `SessionResources` on `ActorInit` kills it structurally · seam keeps producing defects · **NEEDS-CARE** | +30 / −120 | Fix D3 first; collapse only if the seam keeps producing defects. `SessionMetadata` is six fields, of which one `Arc` the registry already holds and two are pure RAII drop-guards. Preserve the rmux-agnostic layering that makes 3,689 fake-driven test lines possible |
| 38 | `tools/package-smoke/package_smoke.py:1109-1113` · five-anchor gate refuses to start; the five `PMUX_PACKAGE_SMOKE_*` anchors have **no producer anywhere in the repo** · 2 `gate_a` cells fail 100% under any driver · **SAFE** | −510 | Self-derived fallback. The full version of the pre-freeze trim that instead drops the 2 cells and writes the PKG-01 nonclaim |
| 39 | ~~`tools/gate-a/run_gate.py` (174 lines, does not exist today) + tests · no driver executes the manifest · manifest is a description, not an executable · **SAFE**~~ | +1,162 actual | **DONE 2026-07-27 — no longer debt.** Row retained, not dropped, because the outcome is the argument: the driver shipped at **533 lines + 629 test lines**, not the estimated 174, and it produced receipt #1 on its third capture (§7.1). The row's own advice — write a small driver over the existing correct manifest rather than fix the 4,279-line envelope — is what worked. Rows **22** and **23**, both of which read "only after a driver has produced one receipt", have had their precondition satisfied |
| 40 | Client "no daemon autostart" behavioral test × 3 languages · S-19 is a stated public contract held **by absence of machinery**, not by assertion · a future convenience autostart breaks it silently · **SAFE** | +45 | Cheap; just not an unblocker |
| 41 | `crates/service/src/native.rs:607-614` + `crates/service/src/v1/actor.rs:742-755` · `expire_idle` gates on `Ready\|NeedsInput` (`actor.rs:747`) but a failed `close(Force)` leaves `Closing` forever, so no later tick retries · idle session leaks permanently when a close fails · **SAFE** | ~40 | **D3: scheduled, not debt.** Confirmed structural. **Do not delete the reaper** — it solves "no caller is watching" |
| **R1** | `crates/service/src/v1/actor.rs:2490` · the turn loop calls `engine.analyze()` in full on **every** poll of the 20 ms cadence (`actor.rs:80`, `poll_interval: Duration::from_millis(20)`), including polls that ingested **zero** rows · pure recomputation of an unchanged result, ~50×/s for the whole turn · **SAFE** | ~+60 cache | Recorded, not fixed. `analyze()` is a pure function of the ingested rows and deep-clones as it goes (`crates/claude/src/engine.rs:661`, `:740`, `:769`), so the cost is real and grows with transcript length; **memoising on "rows ingested since last analyze" is verified safe but is not D9 (a), (b) or (c)** — it changes no observable v1 behaviour and fixes no defect, so it is not admissible pre-push. **Cost, stated with its provenance:** the pre-push review first claimed ~15 ms at 4,096 rows and its validator corrected that **2.4× down to ~6.4 ms**, with the poll-cadence break at **~12,000 rows, not 4,096**, and **~4% of a core** at an ordinary 500-row turn. Use the corrected figures, and re-measure on a quiet host before quoting either. **The in-tree number is not the same quantity and must not be substituted for it:** `crates/claude/tests/size_scaling.rs` (`SMALL_ROWS` 512 / `LARGE_ROWS` 4,096 at `:9-10`) prints `pmux_transcript_scaling … large_ns=…` at `:50`, but what `median_elapsed` (`:84-94`) times is `assert_analysis` (`:96-111`) — a fresh engine, **ingest of every row**, and *then* one `analyze()`. That is an **upper bound** on the per-poll `analyze()` cost, not the cost itself; it read ~15 ms at 4,096 rows on this workspace's host under six-agent load, four consecutive runs, which is consistent with a ~6.4 ms `analyze()` and says nothing against it. Isolating `analyze()` alone needs a harness that does not exist in this tree |
| **R2** | `wait_for_snapshot_stability` in `crates/service/src/driver_io.rs` + the terminal-candidate branch of `TurnWorker::run` in `crates/service/src/v1/actor.rs` · the drain gate is effectively **sampled at ~275 ms granularity**, not tested continuously · explains the live drain overshoot and makes `transcript_drain_ms` read tighter than it is · **SAFE** | do not reorder | Recorded, and now understood. Each turn-loop iteration awaits `completion_evidence`, which re-proves `SCREEN_QUIET_FOR_MS` = 250 ms of terminal quiet **from scratch** — `wait_for_snapshot_stability` restarts `stable_since` on entry and polls at `TERMINAL_POLL_INTERVAL_MS` = 25 ms — and only then is the conjunction `ready_prompt && quiet && batch.drain.satisfies(…)` evaluated against a `batch` polled **before** that wait. So the drain predicate (`TranscriptDrainEvidence::satisfies`) is re-tested about every 275 ms. This is the arithmetic behind §6.3's observed **2,320-2,479 ms against a configured 2,000 ms**. **CORRECTED 2026-08-07: reordering to test the drain first is NOT gate-equivalent, and the claim that it was *verified* to be is instance thirty-three of the bug class (§9.29).** It is equivalent for *never commits before the drain elapsed* — measured, in the strongest form: with the drain decided from the confirming re-poll, `drain_ms` over n=30 has **min 250**, exactly the configured value, never under. It is NOT equivalent for *still catches a row that landed 352 ms after the marker*: the reorder collapses the loop's sampling period to ~1 ms and the catchable window with it, from ~550 ms to **276 ms median**, below both the 438 ms campaign max and the 352 ms arrival that really happened on ordinal 70. Six tests in `crates/service/tests/v1_actor.rs` go red on observed behaviour. Measured saving **274 ms median** — the largest single number in the whole decomposition — and **NOT AVAILABLE**: it is exactly the trade §6.1.2 rules out. The stale citations this row carried (`driver_io.rs:564`, `:559`, `:40`, `:796-802`, `actor.rs:2582`, `:2597-2601`, `backend.rs:206-210`) are replaced by names for the reason §6.2 now gives |
| **R3** | `crates/service/src/driver_io.rs:1117-1127` → `crates/service/src/v1/actor.rs:2336` → `:2818-2819` · a **transient** unterminated last line in the transcript at arm time kills the whole session, non-retryably · a background metadata flush colliding with the next turn's arm escalates a millisecond-transient state to session death · **NEEDS-CARE** | ~25 | Recorded, not fixed. Arm refuses on a final byte that is not `\n` and returns `SchemaDrift` / `unterminated_record`; the arm call site routes that straight to `fail_driver` (`actor.rs:2336`), which routes to `fail()` (`:2818`) and `force_reap_terminal` (`:2819`). `DriverFailure::new` hardcodes `retryable: false` (`crates/service/src/v1/backend.rs:32`, constructor `:28-35`), and the construction at `driver_io.rs:1118-1126` never calls the `.retryable(true)` builder (`backend.rs:38`) although neighbouring driver failures in the same file do (`driver_io.rs:804`, `:765`) — so the caller is told *do not retry* for a condition that clears itself in milliseconds. **Availability edge, never a wrong answer** — refusing to arm at a partial record is exactly what keeps a torn line out of a committed turn. The fix is a bounded re-check of the newline boundary under the already-held deadline, not weakening the boundary check; D9 explicitly permits recording instead, and that is what this row is |
| **R4** | `crates/service/tests/performance_diagnostics.rs:697-702`, gaps at `:744-763`, double at `:851-861` · the Gate A performance receipt's **`completion` phase is measured against a zero-latency `completion_evidence` double**, and none of the four `gaps` entries says so · the phase's own basis string at `:700` reads *"production completion gate: drain + ready/quiet evidence + confirmation re-poll + commit"*, which is what a stranger will believe · **SAFE** | +1 gaps entry | Recorded, not fixed. The double returns `ready_prompt: true, quiet: true` immediately (`:856-859`), so the measured `completion` never pays the real 250 ms quiet window (`driver_io.rs:637`, awaited at `:796-802`). The four existing gaps entries cover paste→Enter, the compatibility probe, the admission editor fence and model latency — **none names the completion side**. The fifth entry to add says: *the completion phase excludes the real terminal-quiet evidence; production adds ~135 ms mean (~270 ms worst) of drain-sampling quantization on top of `transcript_drain_ms`* (see R2). **Write it that way and no other.** The 250 ms is **not additive to the drain** — it runs *inside* the drain wait, and a review draft that presented it as an extra 250 ms term was wrong. §6.1's numbers are unaffected either way; what is wrong is only that the receipt does not disclose the substitution |

### 9.4 Post-commit findings (C1-C9) and one reclassification

Found by independent review of `ce62bcc`, by the coordinator, and — for **C8** — by the Gate A
capture itself, recorded under D9 rather than repaired on freeze eve.

| # | file:line · defect · cost of leaving it · risk | Δ | Disposition |
|---:|---|---:|---|
| **C1** | `tools/phase0/phase0_lib.py:4503` and `:4889` · `CampaignInterrupted` is raised at two sites and asserted by no test · an interrupted live campaign — the exact path that protects **53 irreplaceable attempts** from a partial run — has no regression · **SAFE** | +~40 test | Found while clearing a ruff `F401`. The unused import at `tools/phase0/tests/test_phase0.py:40` was a deliberate half-written-test breadcrumb; it is retained with `# noqa: F401` and a comment at `:37-39` pointing here rather than deleted, so the missing coverage stays visible. Remove the import only together with this entry. Note `phase0_lib.py:3920` also branches on `isinstance(error.__cause__, CampaignInterrupted)` |
| **C2** | `crates/rmux/src/process_boundary.rs:436-450`, `:534-540` · an **unreadable** birth token is permissive for the signal decision — `member_identity_still_proven` returns "proven" when either token is `None`, because `is_recycled` (`:363-368`) needs **both** `Some`; so on any target that is not macOS/Linux (`:534-540` returns `None` unconditionally), or whenever the token read fails, `SIGKILL` selection falls back to the pre-fix `getsid`-only proof · the D2 safety guarantee is platform-conditional, not universal · **NEEDS-CARE** | ~15 | v1 ships macOS-only and Gate C is Linux-only, so **no supported platform is affected today**; but the fallback should be conservative (refuse to signal on an unreadable token) rather than permissive. Fixing it now risks turning a working cleanup path into a permanent unconfirmed-close on any transient `proc_pidinfo` failure — needs a deliberate decision, not a freeze-eve edit |
| **C3** | `crates/rmux/src/process_boundary.rs:300-304`, `:336`, `:411`, `:436-450` · `session_id_recycled` is recomputed per snapshot and needs a **live** process-table row at the leader PID, so once a recycling stranger-leader exits, its orphaned same-session children become admissible members again · residual PID-reuse hazard, strictly narrower than pre-fix · **NEEDS-CARE** | ~20 | Requires an adversarially precise coincidence: pid-space wrap onto exactly the leader PID, plus `setsid`, plus `fork`, plus `exit`, all inside one 25 ms poll gap of an open cleanup window. Not an ordinary accident, so **not** a D9(a) reopener. Recorded as a residual per the reviewer's own recommendation |
| **C4** | `crates/rmux/src/process_boundary.rs:338-342` · comment claims membership "refreshes the token retained for it", but `entry().or_insert` never overwrites — a PID first recorded with a `None` token keeps `None` forever and stays permanently unfenced against reuse · comment is false; behavior is conservative · **SAFE** | ~3 | Either implement the refresh or correct the comment. Pairs with **C2** |
| **C5** | `spec.md:758-759` · "The observer retains every PID it sees in or below the boundary across teardown polls" predates D2 — the observer now retains `(PID, birth token)` pairs and **drops** proven-recycled PIDs; the birth-token fence is nowhere in the spec · doc understates a real safety property · **SAFE** | ~4 | D9(c)-admissible whenever `spec.md` is next touched |
| **C6** | `tools/linux-docker/gate-a-manifest.json` + `suite.sh:452-457` · the Linux manifest was **not** updated when `phase-manifest.json` was trimmed 82 → 75 cells, so the two are divergent: the container would still run the five removed `gate_f` harness-self-test cells as a release gate and the two unsatisfiable `*_package_artifact` cells (whose five `PMUX_PACKAGE_SMOKE_*` anchors have **no producer anywhere in the repo**, `suite.sh:452-457`) · **KNOWN-REGRESSION, introduced 2026-07-26** · **NEEDS-CARE** | ~7 cells | **This is a regression introduced during the freeze and not fixed.** Observable as exactly one new failure, `tools/linux-docker/tests/test_runner.py:277::test_linux_manifest_is_the_exact_ordered_candidate_projection`, taking that lane from 12 → 13 red cells (the other 12 are pre-existing `test_docker_ownership` host-Git failures). Deliberately not repaired: the lane is deferred by rows 23/24, cannot execute without Docker, is already red, and row 24's own guidance is that editing a deferred lane now is exactly the churn to avoid — **and under D6 `source_digest.py` is about to lose ~1,664 lines, which rewrites this manifest's inputs anyway.** Re-project the Linux manifest from `phase-manifest.json` as the **first act** of picking Gate C back up, before anything else in that lane. **Updated 2026-08-06:** the detector no longer reports this as arithmetic. `test_runner.py:284` carried a literal `{gate_a: 42, gate_d: 11, ...}` and `:350` a literal `len(observed) == 97`, so the test failed on a stale COUNT and never reached the projection; those three literals are now derived (phase membership from the candidate, projection size from the candidate's cells plus a declared 15-name container-only set), and the failure now names the seven drifted cells one by one. The repair was rehearsed mechanically on a scratch copy: it makes this test pass at 96 gates and then fails **two other tests in the same file** — `:651` `test_package_framing_property_and_shellcheck_gates_are_exact`, whose hand-written `required` list demands the two unsatisfiable `*_package_artifact` cells and the old `candidate_envelope_tests` name, and `:821`, whose gate ordering forbids `release_full_stack_e2e` in phase A because the container has no release binaries until D. Both are Gate C decisions, not drive-by edits, which is why C6 remains open. `tools/linux-docker/tests` now HAS a `gate_f` cell (`linux_docker_self_tests`), so this debt is recorded in the Gate A receipt as one named red cell rather than as a lane the gate never looked at. **Measured 2026-08-06 through the driver:** that cell ran `109 tests` and reported `FAILED (failures=2)` — this row, and the umask-dependent `test_source_digest.py` defect now recorded as §7.2 row 7. With row 7 fixed the cell reports `FAILED (failures=1)` and C6 is the **only** red cell in an otherwise 80/81 receipt. Both blockers above were re-confirmed by reading the two tests rather than by repeating the claim: `test_runner.py:638-650` is a literal `required` list asserting `typescript_package_artifact`, `python_package_artifact` and `candidate_envelope_tests` are present in the Linux manifest, and `:808-821` asserts an ordering in which `release_full_stack_e2e` follows `release_build`. Closing C6 means editing both, which is a Gate C decision and not a drive-by |
| **C7** | `tools/gate-a-candidate/phase-manifest.json` · advisory row 15 asked for the integration cell at ordered position **17**; it landed at **36** of 75 (moved out of `gate_d`, into `gate_a`) · fails earlier than the original 53 but later than intended · **SAFE** | reorder | The exact position was an advisory suggestion, not an invariant, and moving it earlier risks preceding the release build it depends on. **A 2026-07-26 coordinator summary incorrectly stated "53 → 17"; the committed value is 36.** Corrected here for the record |
| **C8** | `bin/pmux-rmuxd/tests/process_blackbox.rs:431::owner_eof_reaps_a_hup_term_ignoring_pane_tree_and_surfaces_lease_loss` · failed once inside the Gate A capture with `private process boundary observation failed` / `could not run /bin/ps: No child processes (os error 10)` — **`ECHILD`, not a timing bound** — and passes **3/3 in isolation** at ~15.8 s each · **a nondeterministic test in the lane that certifies the process-boundary guarantee** · **HIGH severity, NEEDS-CARE** | ~investigation | **CLOSED 2026-07-28 by disposition 3 — an explicitly unsupported boundary.** Nothing was repaired; the nonclaim is written down instead, and it is stated in full in the note below this table. **This row is not and never was a wall-clock bound**, so no host measurement bears on it: `ECHILD` out of `/bin/ps` (`crates/rmux/src/process_boundary.rs:374`, surfaced with its context at `bin/pmux-rmuxd/src/main.rs:268`) is the signature of an **inherited `SIGCHLD` disposition that auto-reaps children**, which removes the children out from under the observation before it can make one. The Gate A driver runs every cell with `start_new_session=True` (`tools/gate-a/run_gate.py:467`) and touches `signal` only at the import (`:27`) and the SIGTERM/SIGKILL escalation (`:445`), so it does not set that disposition itself — the interaction is between the test and the environment it inherited |
| **C9** | `bin/pmux-hook/tests/process_blackbox.rs::stalled_relay_is_bounded_and_does_not_echo_private_input` · failed 1 of 3 consecutive runs on 2026-07-27 with `did not respect the process bound`; the assertion **was** an *upper* bound on wall clock, `elapsed < PROCESS_TIMEOUT`, so it gated on the machine rather than on the product · **a nondeterministic test in a process-boundary lane** · **HIGH severity, NEEDS-CARE** | ~investigation | **Disposition 1 taken — being made deterministic, landing 2026-07-28 in a separate change.** Unlike C8 this **is** a wall-clock bound and the mechanism is now confirmed as host contention (see the note below this table). The change removes the upper bound and keeps a **lower** bound plus a recorded observation: load can delay an exit but cannot hurry one, so the lower direction is not load-sensitive and still catches a hook that reports a timeout it never waited out (`bin/pmux-hook/tests/process_blackbox.rs:319-324`, elapsed printed as an observation at `:306-307`, reasoning at `:259-278`). That is the general rule applied — **a gate must gate exactly the claim it protects**; "a busy host" was never part of "the relay is bounded", so widening the gate past the claim could only add false failures without adding protection. **Do not instead widen the bound**, here or anywhere: a bound widened to survive contention asserts nothing |
| **C10** | `crates/e2e/tests/pool_concurrency.rs:920::fifteen_concurrent_callers_survive_children_killed_mid_clear` · fails intermittently **since the cold-swap admission fix `170e3e0`** · **a fault-recovery regression, not a flaky assertion** · **HIGH severity, NEEDS-CARE** | ~investigation | **OPEN — measured 2026-08-06, not repaired here.** MEASURED: **2 failures in 7 whole-target sequences** at HEAD (1 of 3 full Gate A runs, 1 of 4 direct runs of `-p pseudomux-e2e --test pool_concurrency -- --include-ignored --test-threads=1`) and **10/10 green in isolation**, so it is load- and sequence-sensitive and only appears after the other twenty tests have run in the same process. **Not pre-existing:** a scratch worktree at `f73aae3` — the commit before the pool fix — passed it **4/4** under the identical sequence, while failing `eight_concurrent_callers_against_three_slots_cold_swap_rather_than_starve` 3/4, which is the defect `170e3e0` fixed. The failure is a **non-recovery**, not a refusal: after the mid-clear kill the census reads `registered_instances: 13` against `instance_terminals_present: 9` with `idle: 13`, `clearing: 0`, the daemon reports `Faulted/Fail`, and two callers get a permanent `DaemonLost / private rmux lease was lost during prompt submission` that survives retry — i.e. **four idle instances whose sidecar is gone stay registered and keep being handed out**. The plausible mechanism is the one `170e3e0`'s own report names: with every slot now used, every instance meets the dead sidecar, where before callers were refused at the cap. **Deliberately not repaired by the agent that found it**: the proximate cause is NOT the new admission wait (`clearing: 0` at the moment of failure, so nothing was being waited on), a fix belongs in the destroy/reap path, and guessing at a pool fault-recovery path is how a 2-in-7 intermittent becomes a permanent one. Reproduce with the whole-target sequence, not the single test. **Updated 2026-08-06:** five more whole-target sequences at this commit, reproduced as the driver runs the cell (allowlist base environment, the cell's two variables, `umask 077`, manifest argv verbatim, staged validation root), came back **5/5 green** at 528-539 s, `9 + 10 + 21 passed; 0 failed; 0 ignored` every time. That takes the measured rate to **2 in 12** and does NOT retire the row: 0 of 5 is consistent with a 17% intermittent, and the mechanism recorded above -- four idle instances whose sidecar is gone staying registered and being handed out -- was never a timing artifact. What the five runs DO retire is the claim that the cell is unsatisfiable as written; see §7.1. Whoever picks this up should reproduce with the whole-target sequence and expect to need roughly ten of them, not five. **Updated 2026-08-06 (transport layer (b), §9.11) — A/B MEASURED, AND THE RESULT IS A NULL THAT MUST NOT BE READ AS AN ALL-CLEAR.** Same protocol on both arms, eight sequences each, `PMUX_E2E_BIN_DIR` set and `cargo build --workspace --tests` first: `cargo test -p pseudomux-e2e --test pool_concurrency -- --include-ignored --test-threads=1`. **With layer (b): 2 red in 8.** **Without it (`crates/rmux/src/backend.rs` restored byte-exact from `4f17d5f`, sha256 `fe21a374…`): 0 red in 8.** Both red runs were this row's test with this row's exact census — `registered_instances: 13`, `instance_terminals_present: 9`, `idle: 13`, `clearing: 0`, `Faulted/Fail`, two callers on `DaemonLost / private rmux lease was lost during prompt submission`. **2/8 versus 0/8 is not a significant difference** (Fisher two-tailed p ≈ 0.47), and the pre-fix arm's own 0/8 is not consistent with this row's recorded **2 in 12** at the same commit either, so neither arm measures a stable baseline: pooled, the pre-fix rate is 2 in 20 against 2 in 8 (p ≈ 0.28). **The change is therefore neither exonerated nor convicted, and it is shipped with that stated rather than hidden.** The mechanism worth checking first, because it is the only one this change plausibly opens: a detached write now opens its own `UnixStream` *inside* the FIFO permit (`RmuxTerminal::write_pane`), so a write against a dying sidecar can hold that permit for a connect *and* a request instead of failing instantly on an already-latched connection — and `TerminalSession::close` waits on the same permit. `close` is still bounded (`timeout(cleanup_timeout, lock_owned())` then proceeds regardless), which is why this is a hypothesis and not a finding. **The experiment that would settle it is ~30 sequences per arm, not eight**; do that before either retiring this row or blaming layer (b) for it. |
| **C11** | `tools/linux-docker/source_digest.py:897` · `_control_node_identity` recorded a **directory's** `mtime_ns`/`ctime_ns` as part of *identity*, so a capture aborted with `Git repository control identity changed during capture` on an entry-set change that told it nothing · **an identity that includes something that is not identity** · **MEDIUM severity** | ~small, inside a de-scoped component | **CLOSED 2026-08-06 — fixed, with the cause corrected on the way.** This row previously said *"`workspace_revision_capture`'s own Git queries move them … a change **it caused itself**"*. **That attribution was wrong**, and it mattered: the capture runs every Git query under `GIT_OPTIONAL_LOCKS=0` (`source_digest.py:769`), which is precisely what stops `git status` writing `.git/index.lock`, so its own queries move nothing. MEASURED by watching the Git directory's entry set at 2 ms with nothing of ours running: an **external ~6 s workspace poller** adds and removes `index.lock` in two bursts ~130 ms apart, moving the directory's mtime **14 times in 30 s**. Against a ~380 ms capture bracket that is ~1 in 20 (**MEASURED 1/20**), and `phase0_lib.observe_source_identity` brackets a whole source manifest with two captures, which is why `gate_f/phase0_self_tests` came out red in **2 of 12** isolated runs (runs 2 and 7 of 12, both `test_phase0.py:1221`, both with the identical message). D6 was the stated reason not to repair it and D6 has slipped, so the row's own fallback was taken: a directory identity is now `(device, inode)` plus mode, ownership and link count, and carries no timestamps; a regular file keeps them and is bound by `sha256` besides. Producer and validator move together in both directions. §7.2 defect 8 has the disposition and the mutation proofs. **Post-fix: 0/30 captures moved, 10/10 isolated `phase0_self_tests` runs green under Python 3.13 and a further 6/6 under the driver's own 3.12.4.** The wider window `phase0_lib.py:1194-1215` narrowed by hand has the same cause and is now redundant rather than load-bearing; leave it, it costs nothing |
| **7** | `crates/protocol/src/v1.rs:671` `EventPayload::ReplayGap` · the event variant has no producer, while the *live* surface is the `SubscribeEventsResult.replay_gap` **field** (`v1.rs:1528`) — a consumer can wait forever for an event that only ever arrives as a field · **NEEDS-CARE** | −35 / 8 files | **RECLASSIFIED from BEFORE-FREEZE under D9(b).** The advisory rated this SAFE, but it fails "no observable v1 behavior change": `replay_gap` is a live member of the closed 14-event union in `tests/conformance/v1/manifest.json`, asserted by `crates/protocol/tests/v1_conformance_vectors.rs::shared_manifest_matches_the_closed_v1_surface`; it appears 11× in `cases.json`, 2× in `golden.json`, and is a handled event case in `clients/typescript/src/client.ts` and `smithers.ts`. Deleting it is an 8-file, three-language wire-contract change days before a freeze. **Pre-v1 disposition: document, do not delete** — one `spec.md` sentence stating the gap is delivered as a result field and the event variant is reserved-not-emitted (mirroring the treatment of the six reserved `ErrorCode`s). **Trap for whoever does delete it:** the `ReplayGap` name imported at `crates/service/src/v1/actor.rs:16` is the *struct*, still live at `:1885`; only the mapping arm at `:2181` goes |

#### C8 is not closed by the Gate A pass, and this is not an ordinary flake

**Do not read any of the four 75/75 receipts as covering this.** Each receipt covers a run **in which
this test happened to pass**. That is a different statement from "this test passes", and the
difference is the whole content of the defect.

**Neither the 2026-07-27 re-runs nor the 2026-07-28 receipt of record changes this by one word.** The
cell passed again every time, on newer trees. That is now four green receipts since the one failure,
plus 3/3 in isolation, and it is still not a fix: nothing was diagnosed, nothing was
changed, and the `ECHILD` interaction
that produced the single observed failure is exactly as unexplained as it was. Accumulating green
runs is the failure mode this entry exists to name, not a route out of it.

It matters more than a normal flake because a flaky command is not a passing command under this
project's own rule: `testing.md:378-380` states that a Gate A command which is *"unavailable, skipped
without an applicable documented platform exclusion, **flaky**, or dependent on an untracked oracle
fails the gate."* By that rule the honest position is that one cell of the manifest is currently
**known-nondeterministic**, and quarantining it is not a release pass.

**Exactly three dispositions are admissible.** There is no fourth, and "it passed 3/3 in isolation"
is not one of them:

1. **Make the timing/state deterministic** — find and fix the inherited `SIGCHLD` interaction so the
   test observes the process boundary reliably under `start_new_session=True`.
2. **Narrow the platform claim** — establish the boundary within which it *is* deterministic and
   publish that narrower claim.
3. **Retain an explicitly unsupported boundary** — write the nonclaim down, in the same register as
   the `testing.md:32-53` D1 non-claims, so a reader knows the guarantee has a documented hole.

Doing none of these and shipping on the green receipt is the one outcome this entry exists to
prevent.

#### The disposition taken for C8 (2026-07-28) — option 3, stated as a nonclaim

**Option 3 is taken, and the row closes.** Everything above stands: no green run closed it, nothing
was diagnosed further, and no assertion was weakened. What changed is that the hole is now written
down instead of carried as an unexplained flake, which is exactly what option 3 is.

**What IS claimed.** `owner_eof_reaps_a_hup_term_ignoring_pane_tree_and_surfaces_lease_loss`
observes the private process boundary deterministically on a host where the process running it has
an ordinary `SIGCHLD` disposition — the default, with no `SIG_IGN` and no `SA_NOCLDWAIT` inherited
from an ancestor. Every observation of this test to date, 3/3 in isolation and every gate cell since,
was made under that condition.

**What is NOT claimed, and this is the hole.** Nothing whatever about a test process that inherits a
`SIGCHLD` disposition which auto-reaps children. Under that disposition the kernel reaps the
children before `/bin/ps` can be asked about them, `ps` returns `ECHILD`, and the observation fails
with `could not run /bin/ps: No child processes (os error 10)`
(`crates/rmux/src/process_boundary.rs:374` → `bin/pmux-rmuxd/src/main.rs:268`). pmux is not claimed
to be observable-by-this-harness in that environment, and a red cell there is a **documented
nonclaim, not a product signal**. The nonclaim is registered next to the gate rule it qualifies, in
`docs/testing.md` under *A red process-boundary timing failure is a claim about the machine first*.

**What this does not cover.** It is not a claim that the product's boundary proof is weaker in that
environment — `OwnedProcessBoundary` never claims to observe a process the kernel has already
reaped — and it is not permission to widen the claim back by accumulating green runs. Closing this
row buys exactly one thing: a reader who hits `ECHILD` here can tell, without re-deriving it, that
they are outside the supported boundary rather than looking at a defect. If someone needs the
guarantee **inside** that environment, this row reopens under D9 with an observation, and options 1
and 2 are still on the table.

#### What the 2026-07-28 host measurement does and does not settle

**Three different tests get confused here, and only one of them was measured.** Naming which binary
each number came from is the whole point of this note.

**The test that was measured is in the launcher, and it is neither C8 nor C9.**
`bin/pmux-launcher/tests/process_blackbox.rs:414`
`socket_and_token_validation_fail_before_broker_use_and_are_bounded` asserts
`elapsed < PRE_BROKER_REFUSAL_BOUND` (2 s). For two days this host carried **16 orphaned processes
from an unrelated project, reparented to init and hot-spinning at 415% aggregate CPU (4.1 cores)**;
under that load the assertion failed, and **after the orphans were killed the test passed 10/10**.
This test is not carried as debt anywhere and is not being added to §9.4.

**The 600× headroom this note used to claim was never what that assertion measured, and the
correction is 2026-08-08.** ~3.3 ms is the launcher's refusal; the timed region also held
`launcher_binary()` and `assert_candidate_unchanged()`, each of which sha256s the whole candidate.
Measured here at HEAD: **350 ms** per iteration for the 4.3 MB debug binary — 174 ms per hash, twice
— against a 2 s bound, so **5.7×**, and the launcher's own share of it **4 ms**. The `sample` stack
that found it sat in `sha2::sha256::soft::compress`, not in the product. `gate_d/launcher_process`
passed throughout only because `PMUX_TEST_BIN_DIR` points it at the 1.2 MB release binary — the same
test, differing in nothing but what it hashed. Under 60 bounded spinners (load average ~60) the
assertion failed 3/3 at HEAD and passes 3/3 with the hashing hoisted out (`timed_refusal`, `:406`),
where the region reads 4 ms. A quiet host was necessary to pass, but the host was not the whole
mechanism, and a note asserting 600× headroom is what made the harness's own share of the stopwatch
look like nothing worth measuring.

**For C9 that is the confirmed mechanism.** C9's failing assertion was the same shape — an upper
bound on wall clock — and the contention observation demonstrates the failure class directly on this
host in the same window. It does not by itself retire the row; what retires it is disposition 1,
already in flight.

**For C8 nothing was measured, because there is nothing here to measure.** C8's failure was `ECHILD`,
not elapsed time, so a wall-clock measurement on another binary says nothing about it. Its only
connection to the orphan contention is that both fall inside the same two-day window. C8 closes on
the nonclaim above, not on this.

**"Fix the host" is not a fourth disposition — it is a precondition of measuring.** What the
measurement establishes is a method: to tell whether a wall-clock assertion or the machine is broken,
measure the actual operation — **and measure the region between `Instant::now()` and `elapsed()`
separately, because they are not always the same thing.** For the launcher test the operation is
4 ms and the region was 350 ms; the gap was the harness's sha256 of the candidate, and only one of
those two numbers was ever written down here.

**DO NOT RELAX THE LAUNCHER BOUND EITHER.** The 2 s at `bin/pmux-launcher/tests/process_blackbox.rs:440`
exists to prove that socket and token validation returns **without waiting out the broker deadline** —
the very next test, `stalled_broker_read_uses_the_shipped_ten_second_deadline_and_redacts_token`
(`:456`), is the 10 s deadline it distinguishes itself from, asserting `elapsed >= Duration::from_secs(9)`
at `:471`. Widening 2 s toward 10 s makes the assertion **unable to fail for the reason it was
written**: a regression that made validation block on the broker would pass. Since 2026-08-08 that
regression is caught twice — the case that could reach a broker points at a live socket nobody
answers, so it would pay the 10 s deadline, and the listener is asked afterwards whether anything
ever connected (`:449`).

### 9.5 REJECTED / NONCLAIM — advisory rows 42-56 (NEVER pre-v1)

**These are decisions, not debt.** They were proposed, adjudicated, and declined. Do not re-open them
without an *observation* (D9). They are not scheduled and they are not owed.

| # | Proposal | Adjudication (one clause) |
|---:|---|---|
| 42 | Delete 7 producerless `ErrorCode` variants (−55) | **VETOED on fact:** `ErrorCode` is a **closed deserialize union in three languages** (Rust `#[derive(Deserialize)]` with no `#[serde(other)]`; `client.ts` `requireEnumField`; the Python client raises), and `spec.md:1103-1111` already publishes six of the seven in the normative caller-policy table — so **removing one is exactly as breaking as adding one**, and deleting is the irreversible move. Disposition: 2 doc lines marking them reserved-not-yet-emitted. Only `persistence_disabled` is absent from the spec and it is not worth a client edit |
| 43 | Delete `crates/protocol/tests/v1_golden.rs:1211-1283` hand-rolled SHA-1/UUIDv5 vectors (−73) | **VETOED on fact:** the premise "a scheme the product does not ship" is false — it **does** ship, in `clients/typescript/src/smithers.ts` and `clients/python/pmux_client/smithers.py`, documented at `spec.md:1333-1335`, with 5 adversarial vectors in `golden.json` (embedded NUL, `café/任务/attempt-α`); Rust (`v1_golden.rs:1198` `rust_recomputes_every_shared_durable_uuid_v5`) is the **third independent witness** |
| 44 | Add a `"weekly limit"` arm to `blocking_screen` (+6) | **VETOED, but the gap is real:** `classify_terminal_snapshot:69-96` tests `active_editor` **before** `blocking_screen` and the motivating fixture has an *empty* composer, so with a real cursor the arm never fires; reordering is strictly worse (it reopens the bug the current order closes, and that fixture's first row is a user prompt containing the words "rate limit"). See OPEN QUESTIONS |
| 45 | Delete `full_stack.rs:1167-1214 exercise_public_connection_capacity` (−50) | **VETOED on fact:** it covers **64-way mixed valid/strict-invalid saturation with recovery** (every 4th connection sends `"unknown_execution_field"` expecting `InvalidConfig`, all 64 complete in 10 s, a 65th succeeds), which `concurrency_backpressure.rs` does **not** — `grep -c InvalidConfig crates/service/tests/concurrency_backpressure.rs` returns **0** |
| 46 | Consolidate the 13 `TerminalControl`/`TranscriptSource` doubles (−600/−900) | **KEEP:** each double encodes a precise fault-injection script; a generic scriptable double becomes a mini-DSL, and a divergent generic double makes a test *silently* vacuous rather than loudly broken. Forward rule adopted instead (§10.2) |
| 47 | Consolidate the 4 poll-until-stable loops (~180) | **RISKY:** they differ in budget source, failure semantics, and stability key; this refactors the path that admits every prompt, days before a freeze, to save ~60 lines. Document the asymmetry instead |
| 48 | Merge `crates/service/tests/process_support/actual_daemon.rs` with `full_stack.rs`'s `Sandbox`/`DaemonGuard` | ~1,500 lines of surgery on the two harnesses everything rests on. Forward rule instead: **the two `start()` functions are a matched pair** |
| 49 | Delete `scripts/pmuxd-run.sh` (−171) | **NEVER:** deleting a working dev tool to close a matrix row is optimizing the matrix; move it to `contrib/` with an `OUT-OF-SCOPE` row if it bothers you |
| 50 | Re-add `unicode-width` | **NEVER:** a second width model next to rmux's is worse than the failure it prevents, and the one char/cell mix is on a required-all-whitespace prefix that fails **closed**. See OPEN QUESTIONS |
| 51 | Loosen `tools/phase0/phase0_lib.py` `require_success=True` | **VETOED as unnecessary:** the ordinal advances at *reservation* so failures burn budget regardless; failed-attempt accounting is independently fenced; and **D5 already provides the escape hatch** (a new `campaign_id` against a new frozen digest is unbricked by construction). Do not spend a loosening of a fail-closed rule inside the tool guarding 53 irreplaceable attempts |
| 52 | Delete/replace `tools/evidence_common/` (4,524 + 2,325 lines) | **NEVER — FREEZE instead:** two real consumers the driver does not replace (phase0 against the irreplaceable budget; package-smoke bounding npm/pip children), and replacing 4,524 green lines reopens the exact promote→revoke loop D1 exists to close. Record observed hashes as *diagnostics*, drop the inbound pins, keep it out of `gate_f`, route future findings here |
| 53 | Centralize the digest pins into one `AUTHORITIES.json` + generator | **NEVER:** it keeps the fail-closed cascade and only reduces the number of literals a human retypes — it looks like a fix and is not one |
| 54 | Extract shared golden-vector loaders across `crates/client` + `crates/protocol` (−150/−200) | Creates cross-crate test-target coupling to save 0.5%; a line-level diff shows only 78 byte-identical lines, so it is parallel authorship, not a copy |
| 55 | Multicall / hardlink collapse of the 8 binaries; splitting `pmux-test-claude` out of the release dir | Blocked by `full_stack.rs:3303` (8 distinct `(device, inode)` pairs), and relaxing that weakens a real identity fence — **ship 8** (decision D2), with the carve-out sentence `spec.md` already gives `pmux-mcp` |
| 56 | Embed `rmux-server` as a library; replace `LaunchBroker` with a 0600 spec file | **NEVER:** `vendor/rmux-server/src/daemon.rs:323` calls `SignalWatcher::install()` unconditionally inside `bind()`, installing `sigaction` for **seven** signals incl. `SIGCHLD`/`SIGHUP` with `mem::forget`'d wake-pipe fds and **no uninstall path** — embedding silently deletes `pmuxd`'s own `SIGHUP→SIG_IGN` and puts a foreign `SIGCHLD` under Tokio's process driver. Removing the install is patch generation 4 and breaks all 15 anchored inverse replacements in `crates/rmux/tests/vendor_server_patch.rs`. The spec-file alternative saves ~310 lines and forfeits secrets-never-at-rest |

### 9.6 Losses from the old design, recorded as decisions

Three capabilities were dropped with real operability cost and no successor. **Record them; build
neither before v1.** Each is ~40 lines post-v1 and each re-opens a surface `spec.md` §13
deliberately closed.

| Loss | Cost |
|---|---|
| **No per-session structured event log** — `crates/service` has **zero** logging call sites, which is exactly why redaction is a bounded problem | Good for redaction, bad for post-hoc incident analysis |
| **No raw read / scrollback / screen text** — you cannot ask a live daemon what a wedged session looks like. Attach is the substitute and it is one-use | Real operability cost |
| **No modal answering (`pmux confirm`)** — zero modal answer bytes is now an asserted invariant | The sharpest usability loss: for an autonomous pilot an unanticipated modal is terminal for the turn |

### 9.7 DESIGN-DEBT — sandboxing the child is DEFERRED ENTIRELY

**Row S1. `crates/service/src/claude_launch.rs` · pmux launches Claude as an ordinary child process
inside the caller's own uid, with no microVM, container, or seccomp/sandbox layer of its own · a
compromised or misbehaving Claude has exactly the authority the caller already had · SAFE (nothing
to undo) · NOT DONE, and not scheduled for v1 or for the work immediately after it.**

This is recorded here rather than left implicit because "put the child in a microVM" is the first
thing a reader reaches for after reading §5.1, and it is the wrong lever. Four reasons, in
decreasing order of how badly it goes:

1. **It does not address this failure class at all.** The four nested-Claude markers are an
   *inheritance* bug, not an *isolation* bug. A microVM handed the parent's environment reproduces
   the identical hang: `CLAUDE_CODE_CHILD_SESSION` crosses the VM boundary as a string in the
   process environment, the child Claude writes no transcript, and every turn parks at
   `awaiting_prompt_ack` exactly as before. Whatever a sandbox is for, it is not for this. The fix
   for this was the allowlist, and it is already done.
2. **It would put transcript authority behind a virtio-fs mount.** §2 is the whole architecture:
   Claude's own JSONL is the sole semantic authority, and `crates/claude` reads it with
   type-enforced complete-line framing, a monotonic cursor, and fail-closed truncation/replacement
   detection (`T-02`). Moving that file across a guest filesystem inserts a caching, coherence, and
   partial-write layer between the authority and its reader — the exact class of ambiguity
   `CompleteLine` exists to make unrepresentable. The 9 differential proptests in
   `crates/claude/tests/transcript_properties.rs` would then be testing a model of a filesystem the
   product no longer uses.
3. **It would put the PTY behind a proxy layer.** The terminal is the independently-required
   liveness gate (§2), and its four hard-coded Claude-TUI geometry constants are validated by
   exactly 24 real turns on one compatibility cell (§7.5). Interposing a guest console between rmux
   and Claude changes what those constants measure, and there is no budget to re-validate them: the
   entire remaining live allowance is 53 attempts, already committed to Gate B coverage that has
   never run.
4. **It would weaken the strongest proof in the product.** Close today returns success only on a
   proven process-boundary reaping — `getsid(pid)==pid` capture, transitive ppid fixpoint, sticky
   escape flag, birth-token recycle fence, re-verified before every `SIGKILL`
   (`crates/rmux/src/process_boundary.rs`, invariant 5 in §10). Inside a VM that becomes an
   *assertion about the VM's lifecycle*: "the microVM exited, therefore its processes are gone." It
   reads stronger and is strictly weaker, because it is unverifiable from the host with the
   process-table evidence the current proof actually collects.

**The correct isolation story is the opposite direction: run the whole stack inside a sandbox, not
the child inside one.** Daemon, sidecar, PTY, transcript, and Claude all land on the same side of
the boundary, every property above is preserved unchanged, and the isolation is enforced by
something that is not pmux. `tools/linux-docker/` already demonstrates exactly this shape — the
full gate suite executing inside a container — which is why the honest disposition is "deferred,"
not "missing." Picking that lane back up is Gate C (§7.6), with its own preconditions.

**On microsandbox specifically:** it is beta, and macOS support is Apple-Silicon-only. v1 ships
macOS-only and Gate C is Linux-only, so adopting it would add a beta dependency that covers neither
platform completely, in exchange for none of the four properties above.

**What `StartSessionRequest::config_isolation` changed here, and what it did not.** It changed what a
deny-by-default profile can *express*. Without it the child needs read+write across
`$HOME/.claude/**` and `$HOME/.claude.json`, and that hole is not narrowable, because transcripts,
caches and configuration are interleaved in one tree: granting it necessarily grants
`history.jsonl` — every prompt the operator has ever typed on that machine, across all projects —
plus `CLAUDE.md`, `agents/`, `commands/`, `plugins/`, `skills`, a `settings.json` whose `hooks` are
arbitrary command execution the cell did not ask for, every other caller's transcripts under
`projects/**`, and the machine-wide trust table. Under a private root each of those becomes
deniable. It changed **nothing** about Row S1's disposition: pmux still sandboxes nothing, keychain
access is still required and undeniable, and none of the four objections above move. It is a
precondition that makes a narrow profile writable, not the profile.

### 9.8 DESIGN-DEBT — the completion fast path: the Stop-hook measurement is void, and it is re-anchored to `turn_duration`

**Row S2. `crates/protocol/src/v1.rs:1334-1365` · `TurnTimings::stop_hook_at_ms` is published beside
`last_transcript_activity_at_ms` (`:1302-1333`) but nothing reads it, and **that pair cannot decide
the fast path**: it was unpublishable at all while `--lifecycle hybrid` failed every turn (§9.10),
and once publishable its sign is **predetermined negative for bookkeeping reasons** (below) · the
drain stays at its untested 2,000 ms fallback and dominates ~76% of structural per-turn overhead
(§6.3) · SAFE (the fields are pure measurement; `CompletionAuthority` is untouched) · NOT DONE, and
the measurement a build must wait on is now the `turn_duration` one, not the hook one.**

**What the field was supposed to decide.** Completion today waits for the transcript to stop growing.
A fast path would complete as soon as Claude's `Stop` lifecycle hook arrives. That is sound if and
only if Claude flushes the transcript **before** it fires `Stop`, and the deciding quantity is the
signed difference
`stop_hook_at_ms − last_transcript_activity_at_ms` (`v1.rs:1345-1350`). **Consistently positive**
means the hook arrived after the final write and completing on it could only ever be faster.
**A single negative observation** means `Stop` can precede the last write, so the fast path would
commit a **truncated turn** — a wrong answer — and it must never be built. The field publishes the
instant, not the pre-subtracted difference, precisely because the sign is the entire answer and a
duration type would clamp exactly the negative case it exists to catch (`v1.rs:1352-1355`).

**It composes as a disjunction, which is why it is safe to pursue at all.** The gate would become
`stop_hook_observed || drain.satisfies(transcript_drain_ms)`, never `stop_hook_observed` alone. The
drain remains the fallback for every turn with no hook — including every session without the Hybrid
lifecycle hook installed (`v1.rs:1357-1359`) — so **the hook can only ever make completion faster and
can never decide it**, and the worst case of the whole change is today's behaviour. That is also why
this is not a second semantic authority: nothing in §2 moves, `CompletionAuthority` keeps its single
variant, and the hook stays what `hybrid_hooks.rs` already is — a signal independent of both the
transcript and the screen.

**Even a partially-sound hook still pays.** If the sign turns out to be unreliable, the hook is still
worth having for a weaker claim: it tells pmux **when to start waiting**. A stability window
*bracketed* by an observed `Stop` can be far shorter than the unbracketed 2,000 ms one, because the
question changes from "has anything more ever going to arrive" to "has anything arrived since the
hook". Do not discard the field on a mixed result; re-scope the claim to that narrower one.

**CORRECTION (2026-07-29): "the measurement is free" was wrong in both directions, and this row
previously said it.** D9(c) — the sentence is being made true rather than the code being changed.
Two independent defects in the claim:

1. **The pair could not be published at all.** Publishing `stop_hook_at_ms` requires the hybrid
   lifecycle, and installing pmux's `Stop` hook is exactly what made every hybrid turn fail with
   `$.subtype` drift (§9.10, ordinal 49). A measurement that only accrues on turns that cannot
   complete accrues on no turns. It was never "a by-product of Gate B".
2. **Once publishable, the sign is predetermined — and predetermined *negative*.**
   `last_transcript_activity_at_ms` is derived at commit as
   `completed_at_ms − drain_stable_for_ms` (`crates/service/src/v1/actor.rs:3009-3011`, in
   `build_turn_result`), so it marks
   the last **file write of any kind**, not the last *semantic* write. But installing the hook
   *causes* writes after the hook fires: the `stop_hook_summary` row carries the hook's own
   `hookInfos[].durationMs`, so it can only be written **after** the hook completes, and
   `turn_duration` then chains off it. The one real observation says exactly that — final assistant
   row `…04.364Z` → hook ran 14 ms → summary `…04.414Z` → `turn_duration` `…04.415Z`. So
   `stop_hook_at_ms` sits **before** the last write on essentially every turn, the signed difference
   reads negative, and **S2's own rule ("one negative is decisive against") would close the question
   in the safe direction on an artifact of the instrument.** The instrument perturbs what it measures.

**RE-ANCHORED: measure `turn_duration`, not the hook.** A zero-ordinal scan of the transcripts
already on disk (`~/.claude/projects/*/*.jsonl`, re-run 2026-07-29) establishes:

- **82** `turn_duration` rows across **four** Claude versions (2.1.177, 2.1.207, 2.1.215, 2.1.220).
- **Zero** model-generated semantic rows follow `turn_duration` inside its turn. Across all 82, the
  only rows appearing between `turn_duration` and the next typed prompt are non-semantic bookkeeping
  (`file-history-snapshot` 17, `last-prompt` 11, `ai-title` 9, `mode` 9, `queue-operation` 7,
  `bridge-session` 7, `permission-mode` 2) plus `system`/`away_summary` 13. **No** assistant row, no
  tool result, no attachment.
- **Presence: 82 of 82** turns that reached an `end_turn` assistant answer carry a `turn_duration`
  (100% on this corpus; a looser denominator that also counts interrupted and queued prompts gives
  82/103 = 80%, and an earlier scan's figure was 96% — the discrepancy is the denominator, not the
  marker).

This retires the premise this row used to open with — *"Claude appends incrementally with no
end-of-stream marker"*, a sentence deleted from the paragraph above because it is **false for the
CLI**. There is a marker. The candidate fast path is therefore

```text
(turn_duration_seen && at_eof && !has_partial_line) || drain.satisfies(transcript_drain_ms)
```

with `turn_duration_seen` already computed on every analyze (`crates/claude/src/engine.rs:227-243`,
`TranscriptAnalysis::turn_duration_seen`) and the drain predicate unchanged
(`crates/service/src/v1/backend.rs:208-210`, `TranscriptDrainEvidence::satisfies`). **Its provenance is strictly better than the hook's** on
five counts: it is **in-band in the authority channel** (the transcript is the sole semantic
authority, so this adds no second source of truth); it needs **no settings mutation** — no
`~/.claude/settings.json` composition, no caller-hook merge; it needs **no relay** — no socket, no
`pmux-hook` process, no clock domain crossing; it **does not contaminate its own measurement**, unlike
the hook, which causes the writes it is being compared against; and the blocked-stop case is
**refused by a payload proof** (`prove_stop_hook_summary_inert`, §9.10) rather than raced against a
2,354 ms window.

**The limits, stated plainly, because each one is a way this could still be wrong.**

- ~~This is **file-write order, not observed arrival order.**~~ **MEASURED 2026-08-06, and pmux was
  already the instrument.** The scan above reads finished files, but `TurnTimings` ships
  `turn_duration_observed_at_ms` and `post_turn_duration_row_observed_at_ms`
  (`crates/protocol/src/v1.rs`), which are stamped against reads pmux performs anyway and are
  *arrival* instants by construction — the second is defined as the first analysis-changing row
  arriving **strictly after** the batch that carried the marker. **n=20 real warm sonnet/low turns
  through the shipped `pmux` binary: 20/20 carried a marker; 0/20 published a post-marker row.**
  Receipt: `evidence/turn-latency-2.1.220-macos-aarch64.json`, regenerated by
  `tools/promotion/measure_turn_latency.py`. An independent instrument agrees — tailing the live
  transcript from a byte offset every ~2.9 ms across 6 turns saw `assistant`, then
  `system/turn_duration`, then nothing until the next prompt 1.28-3.5 s later. **The prize, now that
  it is a number: the commit gate spends 552 ms median (526-580) after the marker has already
  arrived, ~15% of a warm turn.** What this is NOT: 20 turns of one prompt shape on one host is
  evidence, not a proof that the marker is always last, and building the path is still a decision
  somebody has to take on that evidence.
- The **absence case is safe by construction.** Turns with no `turn_duration` (interrupted, aborted,
  and 4-20% of prompts depending on denominator) fall through the **disjunction** to today's drain.
  That costs latency and can never cost correctness — the same asymmetry that governs the rest of
  this document.
- `turn_duration` is **CLI-only**, measured: all **48** session files carrying one have
  `entrypoint: "cli"`, and **none** of the **22** SDK-entrypoint files (12 `sdk-cli`, 10 `sdk-ts`)
  contains a single `turn_duration` row. That is acceptable *because
  pmux drives the interactive CLI*, and it is one more reason the gate must stay a disjunction rather
  than a replacement.
- It is **not observed for non-`Completed` outcomes**, because no such turn exists on disk to observe.
  Nothing here says anything about what a failed, cancelled, or timed-out turn's tail looks like.

**Still S2, still NOT DONE — but the stated blocker is gone, so say what the new one is.** This entry
re-anchored a *measurement*, and as of 2026-08-06 the arrival-order question it was blocked on has
been answered by observation (first bullet above). It is still **not** a licence to change the drain.
What remains between here and a build is a **decision**, not another instrument: whether 20 observed
turns of one prompt shape on one host and one version is enough evidence that the marker is last, and
whose call that is. The composition rules carry over unchanged: disjunction only,
`CompletionAuthority` untouched, and a mixed result gets the claim re-scoped rather than the gate
loosened. Anyone taking the decision should widen the sample first — the tool that produced it takes
`--turns` and costs nothing but wall clock.

### 9.9 DESIGN-DEBT — `NeedsInput` should fail fast when no input channel exists

**Row S3. `crates/service/src/v1/actor.rs:2597-2602` + `crates/service/src/driver_io.rs:838-849`,
`:807-815` · a mid-turn `NeedsInput` is correctly classified and then treated as ordinary negative
liveness evidence, so an unattended turn polls to its full deadline instead of failing on a state
already known to be terminal · one irreplaceable ordinal per occurrence, plus the timing sample that
attempt would have produced · NEEDS-CARE · NOT DONE.**

**Observed live, 2026-07-28, and it cost an ordinal.** A permission prompt was raised mid-turn and
correctly classified `NeedsInput(Permission)` — the classifier works, and the wire mapping
`NeedsInputKind::Permission → ErrorCode::NeedsPermission` is right there
(`driver_io.rs:379-387`). But `completion_evidence` deliberately returns a **default
`TerminalEvidence`** on both `NeedsInput` branches (`driver_io.rs:838-849`, `:807-815`) — that is
negative liveness evidence, `ready_prompt: false`, `quiet: false`, not a failure — so the drain
predicate at `actor.rs:2597-2602` is never satisfied, the actor keeps polling, and the turn consumes
its whole `turn_timeout_seconds` before failing. That was **600 s** for the campaign in §7.5, and
`evidence/model-attempt-ledger.ndjson` carries the receipt: **ordinal 36 is grade 03 with
`permission_mode: null`**, and the immediately following campaign re-reserved the same grade as
ordinal 37 with `dangerously-skip-permissions`. **For an unattended run a permission prompt is
terminal** — nobody will answer it — so the wait converts a known-terminal state into a timeout and
destroys the timing sample the attempt was bought for.

**The tension, stated so nobody resolves it the wrong way.** pmux must **not** auto-answer the
prompt. `docs/spec.md:1109` puts `needs_trust`, `needs_login`, `needs_permission`, `needs_update` and
`needs_input` in one class whose caller policy is *"Obtain explicit authorized human action outside
the turn"*, and answering a modal automatically would be a security change wearing a robustness
change's clothes. Zero modal answer bytes is an asserted invariant (§9.6). **The proposal is only to
stop WAITING** once the state is known terminal *and* no input channel is attached — the turn still
fails with the same `NeedsPermission` code the caller would have received at the deadline, just
without burning the intervening ten minutes. The gate must gate exactly that claim: it is a
statement about whether anyone can answer, not about whether the answer would be granted.

**"No input channel" has to be decided precisely, because getting it wrong breaks interactive use.**
The only sound reading is **an operator can currently reach this session's terminal**, and pmux
already tracks exactly that: the attach reservation in `crates/service/src/attach.rs` is one-use, is
held by the session, and is the *sole* route by which a human's bytes can reach the pane. So the
condition is "no live writable attach is held for this session at the moment the terminal
`NeedsInput` is observed", re-checked at the moment of the decision rather than latched at turn
start, so a caller who attaches *during* a stalled turn keeps today's behaviour. What must **not** be
used: the absence of a TTY on the *client* (a client may be a daemon on behalf of a human), the
scenario or permission-mode configuration (a caller may attach to a `default`-mode session on
purpose), or a timeout heuristic (that is the very thing being removed). Two further constraints:
the check applies to a **mid-turn** `NeedsInput` only, since `initial_needs_input` at startup already
fails fast (`crates/service/src/v1/actor.rs:448`, via `crates/service/src/v1/registry.rs:77` and
`crates/service/src/native.rs:997-1003`); and **`NeedsInputKind::Trust` keeps its current behaviour
regardless of how this is decided** — a folder-trust screen is answered by a human, once, outside
pmux, and nothing here changes that.

### 9.10 RECORD — the Claude `system` subtype taxonomy, and `api_error` as the next ordinal-killer

**Why this section exists.** On 2026-07-29 a live campaign cell with `--lifecycle hybrid` failed on
its **first** turn and spent **ordinal 49** of `evidence/model-attempt-ledger.ndjson`
(`global_attempt_ordinal: 49`, `cell.lifecycle: "hybrid"`, `scenario: "one-shot"`, Claude 2.1.220).
It was the file's last line when this section was written and is not any more; ordinals are
spent, not amended.
The receipt is verbatim in that attempt's artifact directory
(`.../30-nonascii-input/evidence/attempt-97ab6f6d-…/pmux-run.stderr.redacted.txt`):

```text
pmux: turn failed code=SchemaDrift message="Claude transcript schema drift at $.subtype
(row f517343d-d00e-419c-b005-9cc8c5a464be): unsupported active system subtype
Some(\"stop_hook_summary\")" retryable=false
```

Installing the `Stop` hook (which `--lifecycle hybrid` does, via `bin/pmux-hook`) makes Claude write
a **main-chain** `system` row with subtype `stop_hook_summary`, and
`TranscriptEngine::validate_strict_active_path` (`crates/claude/src/engine.rs:883`, rejection at
`:914-923`) refused any active-chain `System` row whose subtype was not exactly `turn_duration`. **pmux
refused rather than guessed, which is the architecture working** — unavailability, not a wrong answer.
But `hybrid` is a documented public lifecycle mode in which **every** turn failed, so this was a
defect reachable by a non-adversarial caller — D9(a), and fixed in-tree the same day by the
payload-proof route described below. The next such failure should be a **lookup**, not a research
project. Hence the table.

**The wild population on this machine, Claude 2.1.156 → 2.1.220** (counted over
`~/.claude/projects/*/*.jsonl`; every row below is `isSidechain: false`, i.e. main-chain):

| subtype | count | chain shape | what the payload records | status in pmux |
|---|---:|---|---|---|
| `api_error` | **114** | parent set; **114/114 have a child** | `error`, `retryInMs`, `retryAttempt`, `maxRetries` — a **mid-turn transport retry** | classified, unimplemented → **rejects the turn today** |
| `turn_duration` | 82 | parent set (chains *through* other system rows) | `durationMs`, `messageCount`; payload-proved to carry no `message`/`content`/`attachment` (`JsonlParser::prove_turn_duration_inert`) | **allowlisted** |
| `compact_boundary` | 25 | **`parentUuid: null` + `logicalParentUuid`** (25/25) | `compactMetadata` (trigger, `preTokens`, `preservedSegment`) — a deliberate chain break at a context compaction | classified, unimplemented → rejects |
| `model_refusal_fallback` | 15 | parent set; only 2/15 have a child, so it is usually the **leaf** | `originalModel` → `fallbackModel`, `trigger: refusal`, `apiRefusalCategory`, `retractedMessageUuids` — a **mid-turn model substitution plus row retraction** | classified, unimplemented → rejects |
| `away_summary` | 14 | parent set | free `content` summary written **after** `turn_duration` | classified, unimplemented → rejects |
| `local_command` | 5 | parent set | `content` with `<command-name>` (e.g. `/model`) — a slash command the operator typed | classified, unimplemented → rejects |
| `stop_hook_summary` | **1** | parent set; `turn_duration` chains *from* it | `hookCount`, `hookInfos[].durationMs`, `hookErrors`, `hookAdditionalContext`, **`preventedContinuation`**, `stopReason`, `hasOutput` | **allowlisted 2026-07-29 by payload proof** (`JsonlParser::prove_stop_hook_summary_inert`); this row is the one that spent ordinal 49 |

"Classified, unimplemented" is exact, and the allowlist is **earned per subtype, not declared**.
`JsonlParser::parse_system` keeps the subtype string for **every** `type: "system"` row and produces
`RowKind::System`; membership of `SystemRow::is_proven_inert_marker`
(`crates/claude/src/model.rs`) is granted only after that subtype's own payload proof runs, and the
active-chain validator asks nothing but that predicate. So the two things a future subtype must
supply are (1) **its own payload proof** — the shared `reject_semantic_payload` check is not
sufficient on its own, since four of the six subtypes above carry a `content` field and
`compact_boundary` additionally carries a `logicalParentUuid` that means "the chain deliberately
broke here" — and (2) **its own leaf semantics**, because `turn_status` treats *any* `System` leaf as
terminal-compatible (`crates/claude/src/engine.rs:797-800`, `leaf_allows_terminal`). Allowlisting a
subtype without (2) grants it that generosity silently.

**Why reject-by-default stays.** This population is not bookkeeping. `compact_boundary` breaks the
parent chain on purpose; `model_refusal_fallback` records that a *different model* answered and that
rows were retracted; `api_error` records mid-turn retries. Those are rows a completion authority must
not ignore, and the safe default for a row whose meaning is unknown is to refuse the turn.

**Row S4. `crates/claude/src/engine.rs:914-923` (rejection) + `SystemRow::is_proven_inert_marker`
(`crates/claude/src/model.rs`) · a `system`/`api_error` row on the active chain
fails the turn with the same `$.subtype` drift that spent ordinal 49, so an ordinary rate-limit or
5xx retry — 114 wild instances here, the most common system subtype on this machine, and mid-turn by
construction — kills a live turn TODAY · one irreplaceable ordinal per occurrence · NEEDS-CARE ·
NOT DONE, recorded under D9.** This is a **D9(a) candidate in its own right**: reachable by a caller
who does nothing wrong except be rate-limited. Two constraints on any fix. First, `api_error` must
**not** simply inherit the `System`-leaf terminal generosity at `engine.rs:797-800`: at the instant
it lands it *is* the leaf, the latest logical message may already read `end_turn` from an earlier
fragment, and the row's own payload (`retryAttempt`, `maxRetries`, `retryInMs`) says a **retry is
pending** — so its leaf semantics are the opposite of terminal, and admitting it without deciding
that is the truncation class this architecture exists to forbid. Second, the assistant-side surface
is already handled and must not be conflated with this one: `isApiErrorMessage` is parsed into
`AssistantFragment::is_api_error` (`crates/claude/src/parser.rs`, `parse_assistant`). **This also bears on §11
question 1**, which asks whether a rate-limited turn returns `TurnTimeout` or a typed `ApiError`: the
on-disk evidence says the likely answer is a **third** one — `SchemaDrift`, non-retryable — and that
is now a reading-derived expectation, not an observation.

**The trap that makes the obvious `stop_hook_summary` patch unsound.** The row is **not**
unconditionally inert: its payload carries `preventedContinuation`. Claude's documented hook contract
lets a `Stop` hook **block** the stop (`decision: "block"`), after which Claude **continues the
turn**. A naive allowlist-the-subtype patch therefore creates a real truncation race: assistant
writes `end_turn` → a blocking `Stop` hook fires → Claude writes
`stop_hook_summary(preventedContinuation: true)` → Claude continues. In the window before the
continuation's first row lands — plausibly longer than the ~2,354 ms observed drain (§6.3), since it
includes a fresh model call's first-token latency — the chain leaf is a `System` row, the latest
logical message says `end_turn`, and the screen shows a ready prompt. All three legs of
`actor.rs:2597-2602` hold and **pmux would commit a truncated turn**. That path is reachable without
an adversary: `compose_caller_settings` merges caller hooks **additively**
(`crates/service/src/hybrid_hooks.rs:536-550`, array append at `:577-600`, asserted at `:804-817`),
so a caller's own blocking `Stop` hook coexists with pmux's. **What the applied fix does instead** is
allowlist the subtype *and* gate on the payload: `prove_stop_hook_summary_inert` refuses the row —
`"stop_hook_summary blocked the stop, so the turn may still continue"` — when `preventedContinuation`
is anything but `false`, and refuses it again when `hookErrors` or `hookAdditionalContext` is a
non-empty array, since hook feedback is something Claude can act on. So the blocked-stop case is
**refused by a payload proof rather than raced against the drain**, which is why allowlisting the
subtype alone was never admissible. The row also cannot be demoted off-graph, because `turn_duration`
chains **through** it (`engine.rs:361-376` requires a resolvable parent for every active main-chain
row).

**Detected *and* rendered, deliberately.** `TranscriptAnalysis::stop_hook_summary_seen` is not a flag
that only the parser consults: `build_turn_result` (`crates/service/src/v1/actor.rs`, the
`caller_stop_hook_observed` warning) surfaces it in default output **when pmux installed no hook of
its own** — transcript lifecycle — because there the row is the only evidence that a *caller's* Stop
hook ran inside a pmux turn. Under `hybrid` it is suppressed, since `lifecycle_hook_observed` already
names that same fact. This avoids the silent-detection shape on purpose — code that notices something
and never says so in *default* output — which is how earlier defects in this tree stayed hidden. Note
also that
`turn_duration_seen` — computed on every analyze, `crates/claude/src/engine.rs:227-243` — has **no**
consumer outside tests and appears nowhere on the wire; §9.8 is what would give it one.

**What is still unverified about `hybrid`.** The drift that spent ordinal 49 is fixed in-tree and
covered deterministically, but **that a `hybrid` turn now completes against real Claude is
UNVERIFIED** — establishing it costs an ordinal, and no ordinal has been spent since. Do not write
"hybrid works" anywhere until a live receipt says so.

**The honest thinness: n = 1.** Exactly **one** `stop_hook_summary` row exists anywhere on this
machine — `~/.claude/projects/-Users-<USER>-dev-pmux-phase12-cwd/1aa963e5-ad99-47ee-9c32-cf67854cdea2.jsonl`
line 16, uuid `f517343d-…`, version 2.1.220 — and it has `preventedContinuation: false`,
`hookCount: 1`, `hookErrors: []`, `hasOutput: false`. **`preventedContinuation: true` has never been
observed here.** The blocking semantics above come from Claude's documented hook contract and from
the field names, **not** from a captured transcript. What would settle it, and it **costs no
ordinal**: a local, non-pmux Claude CLI session with a deliberately blocking `Stop` hook, then read
the resulting JSONL tail — specifically whether `preventedContinuation: true` appears, what the
continuation's first row is, and how long after the summary row it is written. Until that exists,
treat the blocking case as **assumed reachable** (the safe direction) and do not build a fix whose
correctness depends on it being impossible.

---

### 9.11 RECORD — transport layer (b) landed, and most of what it was specified to be was declined

The transport-cancellation work shipped as four layers. (a) made the rmux call sites
cancellation-safe, (c) gave every session its own transports, (d) is the layered health tree. **(b) —
"recoverable control plane" — was the last one open**, and it was specified twice: once as a
`ControlPlane` with epochs, watch channels and rebuild budgets, and then rescoped by the design of
record to per-session rebind, a `matches_pid` fix, a sidecar death latch, and an fd-budget
follow-up. It has now been decided item by item against the tree that (a)/(c)/(d) actually produced,
rather than implemented because it was written down.

**Implemented — one thing, and it was a live defect.** After (c), every read minted its own
throwaway connection, but writes rode one `Pane` captured at `create` for the terminal's whole life.
rmux-sdk binds a handle to its `TransportClient` at construction and
`TransportState::set_terminal_failure` is write-once and never cleared (transport/state.rs:39-44),
so **one aborted write left `paste`, `enter` and `interrupt` failing on that terminal forever**
while `map_terminal_error` went on answering `DaemonLost` with `retryable: true` — a retry no caller
could win. Nothing had to be abandoned by pmux to reach it: the SDK's own `operation_timeout` firing
against a stalled sidecar is enough. **REPRODUCED** at `4f17d5f` by
`crates/service/tests/private_runtime.rs::private_terminal_write_recovers_after_the_sdk_aborts_its_write_transport`,
which `SIGSTOP`s the exact private sidecar, awaits one `paste` to completion (`ControlPlaneLost`
after the 1,500 ms deadline), `SIGCONT`s, and then writes again:
`the same terminal must write again once the daemon answers: "ControlPlaneLost"`. Writes now mint
their handle per write from the same retained lazy facade reads use
(`RmuxTerminal::write_pane` / `write_window`, `crates/rmux/src/backend.rs`), inside the spawned task
and under the FIFO permit — outside it, a write abandoned on its first poll would have taken the
`connect` with it and never been issued at all, silently converting "may or may not have landed"
into "definitely did not". The regression proves the pane, not the return code: the recovered
`paste` is found by `wait_visible_text` and the recovered `interrupt` by the fixture's own `SIGINT`
trap.

**Declined, with the evidence:**

| (b) as specified | Disposition |
|---|---|
| `ControlPlane` with epochs, watch channels, rebuild budgets | **Obsolete.** It was designed for the one process-wide transport (c) deleted. With nothing daemon-wide left to rebuild, "recover the control plane" and "mint the next handle" are the same operation, and the second needs no state. The design of record had already collapsed this; the collapse is now total |
| Per-session rebind at operation boundaries, validating identity with `pane_by_id` once | **Half shipped, half rejected.** The rebind is the per-operation mint above. The `pane_by_id` validation is **not** taken: on a miss `current_pane_ref_for_id` fans out `list-sessions` plus one `list-panes` per session, twice, per call, which is the exact hazard `operation_pane`'s own doc forbids. Every handle in `backend.rs` addresses the slot form, from one `PRIVATE_PANE_SLOT` constant |
| `matches_pid` must compare `ProcessStartIdentity`, not a bare pid — *review blocker* | **Already closed.** There is no `matches_pid` in the tree; `crates/rmux/src/process_boundary.rs` compares `ProcessStartIdentity` through `is_recycled` at `:290`, `:301` and `:446`. Verified by reading, not assumed |
| Never set `process_reaped = true` on control-plane loss | **Already holds.** `process_reaped` is only ever assigned from a positive reap (`actor.rs:1507`, `:1875`, `:2004`); a `ControlPlaneLost` close takes the `Err` arm to `Failed` and assigns nothing |
| Sidecar death latch: `try_wait()` → doom everything, refuse `StartSession`, no restart | **Superseded by (d), and unsound as specified.** `try_wait()` answers "has the child been reaped", not "is the daemon serving": a `SIGSTOP`ped sidecar passes it and serves nothing, and that is the case `ControlPlaneFault::Unresponsive` exists for — recorded there against a real sidecar by the (d) work, not re-measured here. The health tree's `probe_request_path` detects both, on the request path, for one round trip whatever the pool size. It would also buy no behaviour: a killed sidecar answers `ConnectionRefused` immediately (same inherited measurement, recorded on `ControlPlaneFault::Unreachable` as 0 ms), `Pool::mint` does not loop on failure (`pool/mod.rs:823` → `abandon_mint` → destroy, verified by reading) and `Pool::admit` waits on a bounded poll with a ceiling, so nothing spins. Adding a weaker signal beside a stronger one is not a layer |
| fd budget: raise `RLIMIT_NOFILE` in both processes, cap concurrent sessions, map `EMFILE` to a refusal | **Still open, and it is the honest remaining follow-up.** There is still no `setrlimit`, no session cap, and no exhaustion detection anywhere. Unchanged by this work, which is a transport-shape change and not a deployment change |

**The fd numbers moved, so they were re-measured rather than re-quoted.** MEASURED on this host with
a real sidecar, `exact_open_fd_count` sampled at rest after each of four sessions, before and after
the change:

| | owner-side | sidecar-side |
|---|---:|---:|
| `4f17d5f` (retained write handle) | **3.00**/session | **4.00**/session |
| after (b) | **2.00**/session | **3.00**/session |

**One thing this change was measured against and did NOT come out clean of, recorded here and not only in the
debt table: debt row C10.** Eight whole-target `pool_concurrency` sequences with layer (b) came back **2 red**;
eight with `backend.rs` restored byte-exact from `4f17d5f` came back **0 red**. Both reds are C10's own test with
C10's own census. At n=8 per arm that difference is not significant, and the pre-fix arm's 0/8 does not reproduce
C10's recorded 2-in-12 at the same commit either — so this is a null result over an unstable baseline, not a
clearance. The full numbers, the one mechanism this change plausibly opens (a detached write now holds the FIFO
permit across a `connect` as well as a request), and the experiment that would settle it are in the C10 row.

Both fd figures were exactly linear across 1-4 sessions, from a baseline of 16 owner-side and 17 sidecar-side.
The saving is the retained write connection and its accepted peer; a terminal now holds two
long-lived connections (the owned session and its lease heartbeat) and mints a transient third per
operation. **The `~80 concurrent sessions` ceiling recorded by the (c) verifier is NOT restated
here**: it does not follow from 4/session and a 256 soft limit by any arithmetic in this file, it was
not re-derived, and the per-session costs above are the part that was actually observed. The binding
side is still the sidecar.

---

### 9.12 THE BUG CLASS, instance nineteen — a census that said *every* over six of seven

Found while working the pool's refusal surface, and it had been green the whole time.
`crates/service/src/pool/refusal.rs` exposes **seven** refusal constructors. Its own regression,
`every_pool_refusal_uses_a_code_both_shipped_clients_already_know`, built a list of **six** by hand
and checked those against a hand-written four-code set. `sidechain_rows_not_counted` was the seventh
and nothing anywhere tested its code.

**PROVEN blind, not inferred:** with that function changed to answer `ErrorCode::Internal` — a code
the module's own doc says it never adds, and which both shipped clients would have to already know
for the frame to survive — `cargo test -p pseudomux-service --lib -- pool::refusal::tests` reported
**`test result: ok. 12 passed; 0 failed`**. The e2e wave check would not have caught it either:
`pool_concurrency.rs::claim_every_refusal_is_a_known_code` admits ten codes, and `Internal` is one
of them.

The set is now derived from the module's own source. `REFUSAL_CENSUS` names the seven, and it is
checked from two directions so neither can quietly narrow: one test parses `include_str!("refusal.rs")`
for every `pub fn` and asserts the census equals that set, and the code check asserts a constructed
body exists for every censused name before checking any code. Mutation-proved both ways — dropping
the seventh name reddens both tests naming it, and the `Internal` probe now fails with
`sidechain_rows_not_counted answers Internal, which is not one of the codes this module committed to`.

Two prose defects went with it, both the same shape: `pool_concurrency.rs`'s comment said the pool
"commits to exactly these four" directly above a ten-element list (four are the module's, six are
fault codes from the session and driver layers — now labelled as such), and this file's §8 said the
matrix was "**95 rows**, re-parsed today" when parsing it returns 116.

### 9.13 THE BUG CLASS, instance twenty — SEVEN advertised values the daemon refuses or ignores

Found by reading `bin/pmux/src/cli.rs` and `bin/pmux-mcp/src/tools.rs` end to end as a user meets
them, and confirmed **live over a real socket against Claude 2.1.223** rather than by reading. The
highest-traffic instance of this class turned out to be `--help` itself, which nothing tested.

| Surface | What it advertised | What the daemon does | Authority |
|---|---|---|---|
| `--terminal-profile rmux-standard` | a `[possible values:]` entry | `unsupported_feature`, "reserved and is not implemented in protocol v1" | `compatibility.rs:383` |
| `--input-transport attached-stream` | a `[possible values:]` entry | `unsupported_feature`, same sentence | `compatibility.rs:389` |
| `--retention one-shot` | a `[possible values:]` entry | `unsupported_feature` on every CLI path (`run`, `start` and `probe --launch` all call `start_session`); `run_once` **overwrites** the field | `native.rs:3061`, `native.rs:1487` |
| `--on-disconnect cancel-turn` / `close-session` | two of three `[possible values:]` | `unsupported_feature`, "disconnect actions and heartbeat leases require a future leased connection API" | `native.rs:2383`, `v1/actor.rs:1255` |
| `--heartbeat-timeout-ms` | an ordinary option | `unsupported_feature`, same sentence, for any value | same |
| `attach --read-only` | an ordinary flag | `unsupported_feature` on **every** session; with the minified cell's writable refusal this means a minified cell cannot be attached at all | `native.rs:1737`, `v1/actor.rs:188` |
| `close --policy graceful\|force` | a `[default: graceful]` choice | accepted by both and changes nothing: **every** `TerminalControl::close` in the tree takes `_policy` and discards it | `driver_io.rs:1778` |

Nine subcommands and twenty-three arguments also rendered with **no description at all** —
`ping`, `inspect`, `cancel`, `close` and `attach` were bare names in `pmux --help` — and the one
`--output` string, which is `global = true`, told all twelve subcommands that "NDJSON includes turn
events" when only `run` and `turn` publish any.

**Nothing is withdrawn from the wire.** The daemon owns the verdict, a client that refuses locally
can drift from a daemon that later implements the feature, and `pmux probe` must keep building the
exact DTO. What changed is that every one of these now says so in the help that offers it, and four
derived tests hold it there: `every_subcommand_and_argument_a_user_can_type_carries_help_text` and
`every_subcommand_says_which_path_it_is_on` walk clap's own command tree,
`every_value_this_cli_offers_that_the_daemon_refuses_says_so_in_its_own_help` checks the census from
two directions against `get_possible_values()`, and
`every_prompt_this_cli_takes_is_also_takeable_from_a_file` derives the file-form rule from argument
names so a future `--review-prompt` is held to it without anyone remembering.

The MCP schema carried the same defect in its purest form: `claude_config_schema`'s
`permission_mode` listed **six** of `PermissionMode`'s **seven** variants, so every agent caller was
told `dangerously_skip_permissions` did not exist while `pmux --permission-mode
dangerously-skip-permissions` offered it and the daemon ran it. `additionalProperties: false` does
not police an enum's contents and nothing else did.
`every_enum_in_every_tool_schema_names_exactly_its_protocol_variants` now WALKS every schema for
`"enum"` arrays and checks each against the variants parsed out of `crates/protocol/src/v1.rs` —
both directions, so a schema enum with no census entry is red rather than unchecked.

### 9.14 RECORD — three refusals that named a violation and not the answer

The same read found three places where the daemon knew the answer and the operator never saw it.

1. **`ClientError::Server`'s `Display` drops `details`.** `pmux ask --model no-such-model` printed
   the violation while the same `ErrorBody` carried `admitted_models`, derived from `MODEL_TABLE`.
   The first repair printed `details` verbatim and **`cli_contract_matrix.rs` caught it inside one
   sweep**: `details` also carries attach capability tokens. The shipped repair is a contract, not a
   key allowlist — `recommendation` is the advice channel, refusals write advice there, and
   `bin/pmux/src/main.rs` renders that key and no other. `pool::class` now writes the derived model
   list into it.
2. **A modal-blocked turn rendered `{:?}` of the state.** `run_turn` answered "session is not ready:
   NeedsInput" while `self.needs_input` held `kind: trust` and Claude's own words for it, and
   `pmux inspect` on the same session one call later printed both. It now names the modal and
   publishes a per-kind recommendation, exhaustive over `NeedsInputKind`.
3. **`path_b_not_enabled` did not name `--path-b-parent`.** The health tree's answer for the same
   condition already did; the path a caller actually hits did not.

Three operator boot refusals were held to the same bar (`--path-b-claude`'s absence, a warm set
larger than the pool, an RSS budget below what the pool needs), and `--path-b-warm`'s tier list is
now rendered from `EffortLevel::as_str` instead of a hand-written string that had twenty-two spaces
of Rust indentation folded into the middle of it. The absent-parent guard's set of ten flag names is
derived from clap's `serve` arguments and proven one boot per flag.

---

### 9.15 THE BUG CLASS, instance twenty-one — eleven of twelve methods, behind three copies of a number

`tests/conformance/v1/README.md` promised `golden.json` held "one complete request/result pair for
every method". MEASURED against the manifest it is pinned to, it held **eleven of twelve**:
`run_stateless` — the whole of Path B, the method `pmux ask` reaches, and the only producer of
`stateless_result` — had no pair in any of the three languages, while both shipped clients implement
it and both validate its result against no shared vector.

The guard compared the corpus to a **number**, in three hand-written copies of `11`, none derived
from `manifest.methods`:

| Where | What it said |
|---|---|
| `crates/protocol/tests/v1_golden.rs:520` | `assert_eq!(golden.requests_and_results.len(), 11);` |
| `crates/protocol/tests/v1_golden.rs:553-554` | `assert_eq!(methods.len(), 11); / results.len(), 11);` |
| `clients/typescript/tests/golden-conformance.test.mjs:214` | `assert.equal(...length, 11);` |
| `clients/python/tests/test_golden_conformance.py:224` | `self.assertEqual(len(...), 11)` |

A literal freezes the corpus at the size it had the day it was typed: **deleting an entry reddens
it; failing to ADD one does not**, which is exactly how an *appended* method slips through — and
`run_stateless` was appended. This is the same defect `shared_manifest_matches_the_closed_v1_surface`
already fixed for `manifest.json` with an exhaustive `match`
(`v1_conformance_vectors.rs:126-135` records that history); the fix was applied to one checker in
that directory and not to the other.

The count is now derived in all three languages and compared **by name**, so a failure says which
method is uncovered. The per-corpus inventories (`client_required_field_deletions.results`,
`strict_request_object_pointers`) are derived from the corpus too, so a method appended with no
inventory of its own reddens rather than passing by having no cases.

**And the pair found a defect on arrival.** `stateless_result` carries an optional `stop_reason`, and
the corpus's required-field inventory deletes `stop_reason/kind` from every result that carries one.
The TypeScript and Python `run_stateless` validators were the only two of the three that read such a
field and never checked it: they accepted the mutilated frame. That gap survived precisely because
this was the one method with no golden pair.

### 9.16 THE BUG CLASS, instance twenty-two — a check whose message named the defect it could not catch

Found by the delete-the-check discipline, in a check written for the agent resource and in the same
commit that added it. `redaction_hides_values_while_the_digest_still_identifies_the_configuration`
asserted that two specs differing only in a hidden environment value stay distinguishable, and its
message read *"a digest computed over the redacted spec would collide here"*.

MEASURED by deleting the check and taking the digest over `redact_agent_spec(spec)`: **it does not
collide.** Two different values digest to two different digests, so the assertion passed over the
very defect its own message named. A second attempt — comparing the store's reported digest to
`config_digest(&spec)` — failed the same way for a different reason: a mutated digest function moves
both sides of that comparison identically.

The check now computes `sha256` over the canonical serialization **itself**, which is what a caller
in any of the three languages would do, and the mutation reddens it. The lesson is narrower than
"derive your lists": *a check that asks the production function for its own expected value cannot
catch a change to that function*.

### 9.17 THE BUG CLASS, instance twenty-three — three refusals that named a field and a transport that named nothing

`StartSessionRequest`'s decoder composes an exact sentence for a start that names both an agent and
a launch field, for one that names neither, and for `version: 0`. MEASURED live over a real socket,
the wire answered all three with:

```text
code   : invalid_config
message: request does not match protocol v1
```

`bin/pmux-mcp`'s `start_session` description promised "refused with `invalid_config` naming the
colliding field"; `docs/spec.md` §4.8.1 said "refused by name"; and writing the both-modes rule over
PRESENCE rather than over equality-to-default was done *so that* the caller would learn which field
collided. Three claims, one predicate, and the predicate stopped at the decoder.

The flattening was not itself a bug. Forwarding a decoder's rendered text wholesale returns the
caller's own values: MEASURED, `{"environment":{"set":{"SECRET":42}}}` renders as ``invalid type:
integer `42`, expected a string``, and a start frame carries environment values, inline settings and
MCP documents, and system prompts. `DECODE_REFUSAL_MARKER` now prefixes every `de::Error::custom`
the protocol crate writes and `caller_actionable_decode_refusal` returns only the span between it
and serde's own position suffix — so what reaches a caller is text pmux composed out of field paths,
never a value the caller sent. Both directions are pinned by one test, and the second half is the one
that would catch a future "just forward the decoder's message".

### 9.18 THE BUG CLASS, instance twenty-four — a comment that described a mechanism the code did not have, and a store that corrupted itself

`crates/service/src/agent.rs`'s `write_version` doc said, in bold: *"An existing `<version>.json` is
**never** opened for write: `create_new(true)` fails rather than truncating, so a bug that tried to
mutate a version a caller pinned is a refusal instead of a silent rewrite."*
`grep -n create_new crates/service/src/agent.rs` returned **one hit: that comment.** The actual
immutability guard was `if path.exists()` — a check-then-act with the whole write in the window —
followed by a `std::fs::rename` that silently overwrites.

It was reachable by two ordinary callers: `bin/pmuxd/src/handler.rs` serves up to 64 connections
concurrently, and `AgentStore::update` took no lock between reading `head` and writing the next
version, so two callers holding the same `expected_version` both passed the fence. A third mechanism
compounded it: the temporary file's name was `path.with_extension("json.tmp")` — a **pure function of
the destination** — so both writers opened one file, and `truncate(true)` let the second cut the
first's bytes off mid-write.

MEASURED, 25 rounds of two concurrent `update_agent` calls on one fence at the library level:

```text
bricked=7 divergent=13 clean=5
head UNREADABLE: agent store …/2.json is not a readable agent version:
                 trailing characters at line 1 column 1497
list FAILED:     …the same record, for the whole store…
```

`head` said `2`; `2.json` was another writer's tail glued to this one's body. The agent could not be
read at head, launched at head, or repaired through the API — and in the 13 divergent rounds the
caller got `rc=0` with a `config_digest` the store did not hold, which is a wrong answer rather than
a refusal.

Publication is now atomic AND exclusive, and the two properties have two mechanisms: the bytes are
written and `fsync`ed under a name no other writer shares, and `link(2)` gives the finished inode its
real name and fails with `EEXIST` rather than replacing. Naming the file and refusing to overwrite
one are the SAME syscall, which is what a fence has to be. The comment and the code now say the same
thing.

**The same commit found a second one in the same file, of the same kind:**
`admit_agent_containment`'s doc named `containment_can_only_refuse_more_never_admit_more` as the test
that "proves" the composition direction — *"THE WHOLE RULE"*. That identifier's only occurrence in
the repository was that comment. The test now exists, in `native.rs`'s test module, because
`admit_bound_resources` is private there and the composition is only testable where both halves are
visible.

### 9.19 THE BUG CLASS, instance twenty-five — one bad record took the whole listing down, and the recommendation for it named the command that failed

`AgentStore::list` propagated `?` per entry, so a single unreadable record answered the entire
listing with that record's refusal. Reproduced three ways: a widened `head`, a torn version file, and
a UUID directory with no `head` at all — the last of which **`create` itself could leave**, because
it made the agent directory and then wrote into it.

The compounding half is the shape this repository keeps finding: `no agent <id>` answered with
`"recommendation": "list the stored agents with 'pmux agent list'"` — **unreachable in precisely the
state it is offered**, since the record that made the id unresolvable is the one that took the
listing down. MEASURED live, `agent list` failed naming a *different* agent than the one asked about.

`AgentList` now carries `unreadable`: each such record by id, with the refusal `get_agent` would have
given for it, omitted from the wire when empty so an ordinary listing's bytes are unchanged. Dropping
the bad record instead would have been worse — a stored agent silently ceasing to exist is the
accepted-and-ignored shape this design refuses. `create` is now assembled under a staging name that
is not a UUID and published in one `rename(2)`, so the half-made state it used to leave is
unconstructible rather than tolerated.

### 9.20 THE BUG CLASS, instance twenty-six — a guard that walked five of the nine paths its own list supplies

`StartSessionRequest`'s `Serialize` hand-wrote **five** `emit_policy` calls where
`agent_supplied_start_paths()` supplies all **nine**, 130 lines above in the same file. The comment
justifying the omission said `claude`, `environment.set`/`unset` and `cell` "need no arm here …
a present one is sent and refused by name" — refused by name in `Deserialize`, **which no in-process
caller runs**: `validate_v1_serializable` only serializes. MEASURED:

```text
serializer REFUSES beside an agent: ["auth_policy","terminal","lifecycle","retention","compatibility"]
serializer ACCEPTS beside an agent: ["claude","environment.set","environment.unset","cell"]
  cell:            embedder sent "minified"  -> resolved carried the agent's `full`
  environment.set: embedder sent {"LEAK":"1"} -> resolved carried the agent's set
```

The wire was always safe. This was the `pub` Rust surface the codebase defends explicitly elsewhere,
and `agent.rs` rested a written claim on it: *"nothing a caller wrote is discarded."*

Both presence tables now return `[(&'static str, bool); AGENT_SUPPLIED_START_PATHS.len()]`, so a path
added to the derived list stops both of them compiling until each says whether the request in front
of it carries that path. The count is a type, not a review note.

**Three smaller instances landed with them.** The golden corpus's EVENT coverage was still a
hand-written `14` — in the same file and the same commit that derived the METHOD count (§9.15), and
neither client asserted event coverage at all; appending `"future_event"` to `manifest.events` left
all eight Rust golden tests green. `pmux start --agent` refused by naming `--model` when the value
came from `PMUX_MODEL` in the caller's shell rc, a flag they never typed, which locked that shell out
of `--agent` entirely. And `run_once` set `retention = OneShot` and then agent resolution replaced it
with the stored agent's `Persistent` — a value pmux wrote and pmux discarded, inside pmux's own path.

### 9.21 THE BUG CLASS, instance twenty-seven — a crash-safety direction nothing measured, inside the fix for the bug class

`AgentStore::update` published a version file and then moved the `head` pointer, and the comment
between the two lines said:

> A crash between the two leaves a published version the pointer has not reached yet, which reads as
> "the update did not land" — the safe direction.

`docs/spec.md` §4.8.2 repeated it. **Nothing tested it**, and it was the same author's second false
measured claim in consecutive commits — the first being §9.20's "refused by name", refused in a
decoder no in-process caller runs.

MEASURED with a SIGKILL harness — a child updating one agent in a loop, killed at an offset jittered
uniformly across one *measured* update cycle — **19 of 45 trials wedged**. It did not read as "the
update did not land"; it read as *this agent can never be updated again*. `update` always recomputed
`head.next()`, so it always targeted the number already on disk, and `link(2)` always refused it:

```text
head=4  published_max=5
  retry@4 -> id_conflict: agent 8f830f27-… is at version 5, not the expected version 4
  retry@5 -> id_conflict: agent 8f830f27-… is at version 4, not the expected version 5
  get(None) -> 4   list -> 4   unreadable=0
```

Consecutive attempts on one fence were told it was stale in **opposite directions**, and `list_agents`
reported the record healthy at the older version with nothing unreadable, so no surface said a word.

The harness's own first version found **zero** wedges in 40 trials against that store, because it took
the kill offset from `subsec_nanos() % 4000` and so sampled one phase of a 20ms cycle. A crash-safety
property confirmed by a harness that samples one phase is the same defect one level up.

**The fix is in the reader, because it cannot be in the window**: publication and the pointer are two
files and cannot be one syscall. `head` is now documented and used as a durable LOWER BOUND, and every
read walks forward from it over every version NAME that exists — a loop, not a one-step lookahead,
because `advance_head` writes an absolute value with no lock and a descheduled writer can make the
pointer regress. The step predicate is `link(2)`'s, exactly: any name, not "a readable version", so
`update` mints a number no name is taken for and the wedge is unconstructible rather than recovered
from.

**ADOPTED, not discarded**, and the caller-visible consequence is written down: an update interrupted
by a crash MAY have landed. No ordering avoids that — the same crash one line later would have moved
the pointer — and it is exactly the case the fence documentation already prescribed `get_agent` +
`config_digest` for. Adoption makes that recovery truthful; discarding would have required unlinking a
published file, which makes `missing_version`'s "a version is never removed" false for a version a
session may have pinned, and no reader can tell a crashed writer's orphan from a live writer's version
published microseconds ago.

After: **45 trials, 0 broken, 18 of them landing in exactly that window**; and 40 crash-and-restart
cycles on one store, 185 versions published, 0 whose bytes changed and 0 that stopped reading.

### 9.22 THE BUG CLASS, instance twenty-eight — twenty-six hand-run mutation campaigns, and the space they were sampling

Every campaign in this repository's recent history mutated by hand: delete the guard, run the target,
confirm red, restore, verify byte-exact. Thirty-one mutants on the pool, twenty-five on the
adversarial fixes, twenty-two on the agent resource, fifteen on the health encoding, twelve on the
waves. Each one was a **sample**, chosen by the same reading that wrote the code, of a space a tool
enumerates exhaustively — which makes the whole practice one more instance of the class: a
set-of-things-to-check assembled by hand where it could be derived.

`cargo-mutants 27.1.0` is now pinned under the workspace beside `cargo-fuzz`, and
`scripts/gate-a-mutants.sh` is the enumeration. The first run was scoped to every first-party file
the campaigns had touched — 1,588 mutants — and was stopped at 623 of them. 145 of those 623 did not
compile, and `unviable` is excluded from both sides of the score, so what it measured is **69
survivors in 478 decided mutants, 85%** (`.context/gate-a-mutants/part1.out/outcomes.json`). The 623
is the number of mutants that got an outcome, not a denominator; it is written out here because the
first draft of this paragraph used it as one.

The survivors are not a long tail of equivalents. Named, because each is the same sentence the
counter above tracks:

* **`claude_launch::reject_team_markers_reaching_child` deletes entirely with the suite green.** Its
  own doc says "a future policy change that stops stripping one re-arms the refusal automatically" —
  a claim about a function no test had ever called. Every case exercised its SIBLING, the one that
  refuses a marker in `environment.set` before resolution.
* **`agent::supplied_start_paths` returns `["xyzzy"]` with the suite green.** Its doc: "re-exported so
  callers that must name one in a message read the protocol's list and never a copy." Nothing
  compared the two.
* **`agent::read_private_file`'s `ELOOP` arm is deletable.** `O_NOFOLLOW` was opened with, a refusal
  was written naming the hazard exactly — "a link's own mode says nothing about what it points at" —
  and nothing in the tree had ever put a symbolic link in the store. A link to another agent's
  version file WOULD have been served.
* **`agent::value_digest` returns a constant with the suite green.** Every assertion checked that the
  plaintext was GONE, which a constant satisfies exactly as well as a digest — while making two
  different secrets indistinguishable on the surface whose claim is that it identifies the
  configuration exactly.
* **Six boundary predicates flip `>` to `>=` with the suite green**, on the agent label and
  description limits and on the opaque-JSON safe-integer range. In each the refusal names the exact
  number and no case had ever sat on it, so `MAX_SAFE_JSON_INTEGER` itself had never been shown to
  survive the gate it is the boundary of.
* **`default_hook_timeout_ms` returns `0` with the suite green** — a value
  `agent::validate_agent_spec` refuses by name, so the mutant turns every omitted hybrid timeout into
  a stored agent nobody can create.

**Two smaller instances landed with the tooling.** `tools/gate-a/tests/test_run_gate.py`'s
placeholder fixture hand-listed six tool names where `run_gate.TOOL_EXECUTABLES` supplies them, and
broke the moment a second pinned tool was added; `tools/gate-a-candidate/tests/test_candidate_envelope.py`
bound `cargo_fuzz` to a literal path and answered a literal `cargo-fuzz 0.13.2` — a fake that would
have kept passing after the pin moved, which is the one thing `gate_b/cargo_fuzz_version` exists to
catch. Both now derive: the first from the driver's own table, the second by READING the manifest's
`<tool>_version` cell.

**And one in this work's own scaffolding, caught by the compiler rather than by a reviewer.**
`scripts/gate-a-mutants.sh` first ran the pinned cargo with `RUSTC` unset. Cargo then invoked `rustc`
from `PATH` — rustup's proxy, which resolves the toolchain from the CURRENT DIRECTORY — so every
workspace crate compiled under 1.88.0 from `rust-toolchain.toml` and every registry crate, compiled
from `~/.cargo/registry`, got the host default of 1.97.1. `error[E0514]: found crate rmux_proto
compiled by an incompatible version of rustc`, 1853 errors deep, before a mutant ran. The script now
derives `RUSTC` as cargo's sibling and refuses unless the two report the same release, which is
`scripts/gate-a-fuzz.sh`'s rule applied to the stable toolchain.

### 9.23 The measured mutation baseline, and what the number is not

`scripts/gate-a-mutants.sh` at `PMUX_MUTANTS_SCOPE=gate` enumerates **702** mutants over
`crates/service/src/{agent.rs,claude_launch.rs,pool/**}` and `crates/protocol/src/**`. The score is
`caught / (caught + missed)`; `unviable` is excluded from both sides because a mutant the compiler
rejects was never a test of the tests, and `timeout` counts as caught.

> **The scope value was called `admission` and the cell
> `mutation_score_service_admission_and_protocol`.** Both were renamed once the label was read back
> against the globs: `native.rs` is not in them, and `native.rs` is where `admit_bound_resources`,
> `admit_config_root`, `admit_cwd`, `claim_reaches` and `effective_config_root` are declared. The
> guards this repository calls "admission" are held by §9.24's differential test, not by this number.

> **The 88.67% this section first reported was never one run.** It is the arithmetic composition of
> two partial runs — `validation/out` for the four non-pool files and `poolonly` for `pool/**` — and
> the composition is exactly right (702 = 441 − 13 + 274, and the sums reproduce), which is why it
> went unnoticed. It was also already stale when it was written: `v1.rs:204 is_zero_u64 -> true` is
> MISSED in `validation/out` and CAUGHT on the same tree once the test that closes it landed. The
> figure below replaces it, and is careful to say which half of the scope is complete.

**THE WHOLE SCOPE IS NOW MEASURED IN ONE RUN, TWICE.** Both are at the cell's own settings
(`PMUX_MUTANTS_SCOPE=gate`, `PMUX_MUTANTS_JOBS=4`, pinned 1.88.0) on an idle machine, and both are
COMPLETE in the sense that distinguishes a measurement from a composition: `end_time` is non-null and
`caught + timeout + missed + unviable` sums to `total_mutants`, which is `enumerated_mutants` from the
same run's metadata. (`outcomes.json` counts the mutants that got an OUTCOME, so a run stopped at 623
of 1,588 writes `total_mutants: 623` and sums perfectly against itself — the enumeration is the second
check and `end_time` is the first.)

* **BEFORE, at `0d7f2ca` plus §9.26's work — the gate cell's own run, 5,285 s:** 702 enumerated, 102
  unviable, 600 decided, 561 caught, **39 missed — 93.50%.** Per file: `v1.rs` 91%,
  `launch_environment.rs` 100%, `agent.rs` 97%, `claude_launch.rs` 97%; `pool/class.rs` 100%,
  `config.rs` 95%, `host.rs` **0% (0 of 1)**, `instance.rs` 100%, `machine.rs` 100%, `mod.rs` 85%,
  `refusal.rs` 100%. `pool/**` alone: 233 decided, 214 caught, **19 missed — 91.85%**, which retires
  the 84.5% figure below. **All eighteen closures the four derived tests claimed are CAUGHT in it**,
  checked site by site against its own `caught.txt` rather than counted; the nineteenth survivor is
  `mod.rs:1130`, which nobody had claimed and which the pool-only run had scored CAUGHT (§9.27).
* **AFTER, this commit — same script, same settings, 5,099 s:** 702 enumerated, 102 unviable, 600
  decided, 573 caught, **27 missed — 95.50%**, exit 0 against the new floor of 94. Per file:
  `v1.rs` 90%, `launch_environment.rs` 100%, `agent.rs` 96%, `claude_launch.rs` 97%;
  `pool/class.rs`, `config.rs`, `host.rs`, `instance.rs`, `machine.rs` and `refusal.rs` all
  **100%**, `pool/mod.rs` 96%. **`pool/**` alone: 233 decided, 229 caught, 4 missed — 98.28%**, up
  from 91.85%, and the four are the four equivalent mutants named below.
  **Every one of the 27 survivors falls in a class named in this section**: 16 in the
  `serialize_struct` field-count accumulator (class 2), 4 in `pool/mod.rs` (equivalent, below), 2
  `#[cfg(not(unix))]` twins (class 1), `v1.rs:287` (class 4), `v1.rs:1565` (class 3),
  `agent.rs:1383` (class 5), `agent.rs:1432` and `agent.rs:1209` (the two confirmations below).
  There is no unclassified gap left in this scope.

The earlier per-half figures are kept here because they are what the floor was defended from and
because one of them was stale: **428 enumerated, 61 unviable, 367 decided, 344 caught, 23 missed —
93.73%** for the four non-`pool` files, and **197 of 233 decided — 84.5%** for `pool/**` from a
pool-only run that predated the four derived tests. Neither is the number any more.

**Five classes of survivor cannot be closed by a test in this tree, and each is named with its
reason rather than counted as a gap.**

1. **`cfg`-guarded bodies.** `cargo-mutants` does not evaluate `cfg`, so every
   `#[cfg(not(unix))]` item contributes a survivor that is not compiled on this host at all: its
   mutant is byte-identical to the baseline, and no test on any Unix run can kill it. Both are in
   `claude_launch.rs` — `resource_key` at :964 and `is_executable` at :1216 — and the proof that the
   host's own code IS covered is that every mutant of the `#[cfg(unix)]` twin was caught:
   `resource_key` at :957 reddened 26 tests, `is_executable -> false` at :1209 reddened 24 and
   `-> true` reddened 2, plus all three operator mutants at :1211. (This entry said "five tests" for
   both until the counts were read out of `.context/gate-a-mutants/out/mutants.out/log/`.)
2. **Field counts serde_json ignores.** **Sixteen of the eighteen** `crates/protocol/src/v1.rs`
   survivors of the AFTER run are the `fields` accumulator at `v1.rs:1785–1791`, whose only consumer
   is `serialize_struct`'s length HINT. JSON is self-describing and this tree has no other
   serializer, so every one of them is equivalent under every format the product uses. Classes 3 and
   4 take the other two, so **`v1.rs` has no real gap left**: 190 decided, **172 caught — 90%**,
   against 34 survivors in 190 decided (82%) on the first run, before this branch's tests.
   (This entry cited `v1.rs:1777–1783`, which was right when written and wrong two edits later; it
   said `1780–1787` for the BEFORE run of this very section and `1785–1791` is where the same
   sixteen sit after the four lines added to a doc comment 260 lines above them. §9.27's fourth
   item is this line, and the lesson is that a range in prose is stale on arrival — the survivors
   are `missed.txt`, which is copied into the evidence directory on every run.)
3. **Compile-time devices.** `agent_supplied_start_paths::exhaustive` is an `#[allow(unused)]`
   `..`-free destructuring whose checker is rustc, not a test. Mutation testing measures the test
   suite; this construct is answered by `gate_a/rust_check`.
4. **A bound a preceding arm has already decided.** `v1.rs:287 > -> >=` in `validate_opaque_json` is
   an EQUIVALENT MUTANT, and the argument is short enough to check: line 287 is in the `as_u64` arm,
   reached only when `as_i64()` returned `None`, which for a JSON integer means the value exceeds
   `i64::MAX` = 9_223_372_036_854_775_807. `MAX_SAFE_JSON_INTEGER` is 9_007_199_254_740_991, which is
   smaller — so every value reaching that line is already strictly greater than the bound, and `>`
   and `>=` agree on all of them. **The tool found this by refusing a claim.**
   `the_opaque_json_integer_bound_admits_the_exact_edge_and_refuses_one_past` was documented as
   closing `:287` and `:298`; re-running showed `:298` MISSED-then-CAUGHT and `:287` MISSED in both
   runs. The test's doc now states the equivalence instead of the closure.
5. **A syscall failure this tree cannot inject.** `agent.rs:1383` replaces the `AlreadyExists` match
   guard in `publish_version_exclusively` with `true`, reporting every `hard_link(2)` failure as
   `IdConflict`. Killing it needs `hard_link` to fail with something OTHER than `EEXIST` — and by
   that point `create_new_private_file` has ALREADY succeeded in the same directory, so the
   directory is writable and the remaining failure modes are `ENOSPC`, `EDQUOT`, `EMLINK`, `EIO` and
   a filesystem without hard links. None is stageable from a unit test, and this tree has no
   syscall-level fault injection. **This is a real defect the mutant describes** — a full disk would
   be reported to an operator as "a stored version is immutable and is never rewritten" — and it is
   listed here as untestable rather than as equivalent, because those are different things. It is
   the sibling of `agent.rs:969`, which looked equally untestable and was not: see §9.25.

**And one survivor is the crash-safety claim, confirmed.** `AgentStore::advance_head -> Ok(())`
survives — the head pointer is never moved after `create` writes it. That is not a gap: `advance_head`
has exactly one call site (`update`, `agent.rs:931`), `create` writes the pointer itself into the
staging directory it renames into place, and §9.21's whole fix is that `head` is a durable LOWER BOUND
which `published_head` walks forward from. A tool that deletes the pointer update entirely cannot make
a test fail, which is the strongest available confirmation that the lower-bound claim is true rather
than asserted.

**`sync_parent_directory -> ()` survives too, and its own doc already said why:** against process
death the directory entry is visible to every later reader, and the call matters only against power
loss, which no test on this host can stage.

**THE SAME TREE SCORED DIFFERENTLY UNDER LOAD, AND THE ERROR RAN THE UNSAFE WAY.** This scope was
run twice over one tree: once with a Python suite running beside it, and then again on a quiet
machine, which is the run of record above. They disagreed on **three** mutants, every one of them in
the direction that makes the gate PASS:

* `v1.rs:1779 += -> *=` and `+= -> -=` — two lines of the `fields` accumulator serde ignores — came
  back CAUGHT under load and MISSED quiet. Neither timed out, so the mechanism is not the `timeout`
  rule at all: it is any test that goes flaky under load, whose failure is then attributed to the
  mutant.
* `agent.rs:1432 sync_parent_directory -> ()` came back TIMEOUT under load and MISSED quiet. **A
  mutant that DELETES an `fsync` cannot make anything hang**, which is what makes this one
  unambiguous rather than arguable.

MISSED is the true answer in all three. The figure of record is the quiet run, and the property is
written into `docs/testing.md` beside the score.

**THREE MORE DISAGREED BETWEEN RUNS THAT WERE ALL QUIET — AND THE MECHANISM IS NOW NAMED RATHER
THAN CALLED "LOAD".** The bullets above end at "any test that goes flaky under load, whose failure is
then attributed to the mutant"; that sentence is true and it stops one step short of the three tests
that do it. Each flip below was resolved by opening the log of the run that said CAUGHT and reading
which test failed, which takes a minute and settles it:

| mutant | said CAUGHT in | because this test failed | and it failed on |
| --- | --- | --- | --- |
| `v1.rs:1561 agent_supplied_start_paths::exhaustive -> ()` | the whole-scope BEFORE run | `bounded_soak.rs::repeated_real_rmux_cycles_remain_resource_bounded_and_leave_no_residue` | `cycle 13 retained a session, attach socket, or launch artifact`: `rmux.sock` still present |
| `agent.rs:1209 advance_head -> Ok(())` | the targeted verification run | `driver_io.rs::tests::a_preamble_that_lands_after_the_anchor_still_rebinds` | `preamble_not_settled` after `waited_ms: 802` |
| `pool/mod.rs:1130 abandon_unpublishable -> ()` | the pool-only run | `private_runtime.rs::a_terminal_resize_after_creation_is_delivered_and_not_silently_clamped` | `private rmux sidecar exited unsuccessfully: exit status: 1` |

**THE MAGNITUDE IS MEASURED, NOT GUESSED.** The two complete whole-scope runs above differ on
**exactly three mutants**, all outside `pool/**` and all CAUGHT in one run and MISSED in the other:
`v1.rs:1565`, `agent.rs:1383`, and one line of the `serialize_struct` accumulator. Fifteen of the
eighteen-mutant delta between them is this commit's work; three is noise; and the noise is worth 0.5
points on a 600-mutant denominator.

**All three flips are wall-clock-bounded tests that spawn real processes, and none of the mutants can
affect any of them** — `exhaustive` is a destructuring rustc checks, `advance_head` is a pointer
write no test reads, and `abandon_unpublishable` is unreachable without a planted state. So MISSED is
the true answer in all three, and the mechanism is not the machine being busy: it is that the
mutation loop runs four full suites in parallel, and three tests in those suites are budgeted against
a wall clock and a real sidecar. **The error is bounded and one-directional** — a spurious FAILURE is
always read as a CAUGHT mutant, never the reverse — so every score here is an over-estimate by at
most a few mutants, which is one of the two reasons the floor is set below the measurement rather
than at it.

**Two mutants are caught by NON-TERMINATION, and that is a real detection rather than a slow host.**
`AgentStore::version_name_is_taken -> true` and its `!= -> ==` sibling (`agent.rs:1090` and `:1092`)
make `update`'s version-minting loop reject every candidate name forever. They are recorded as
`timeout` — which this score counts as caught — and they are **the same two mutants, in all three
independent runs, under three different machine loads.** That is what distinguishes a genuine hang
from the load-induced kind the `timeout` rule is otherwise exposed to.

The rest were real, and the ones closed in this pass are listed with the mutant they close in
`crates/service/tests/agent_resource.rs`, `crates/service/src/claude_launch.rs::tests`,
`crates/protocol/tests/v1_wire.rs` and `crates/service/src/pool/{mod,refusal,class,machine}.rs::tests`
— each doc comment names the surviving mutant by file and line, so a reader can re-run exactly that
one.

**FOUR OF THE TWENTY-TWO CLOSURE CLAIMS WERE FALSE, AND THE TOOL CAUGHT ALL FOUR.** This is the
whole argument for the cell in one paragraph: every claim of the form "SURVIVING MUTANT CLOSED: X"
is a checkable statement, and re-running is what checks it.

* **`v1.rs:151 - -> +` in `NativeFrameAccumulator::push` was claimed closed and was not.** The
  mutant needs `filled > 0` AND an input longer than the payload has left; the test's "two frames in
  one push" case starts its payload at `filled == 0` and its split-payload case ends with exactly the
  bytes it needs, so `+` and `-` agreed in both. A third case — a part-filled payload finished by a
  push that overshoots the frame boundary — closes it, and mutating the line by hand now panics
  `range end index 18 out of range for slice of length 10`.
* **`v1.rs:287 > -> >=`** was claimed closed and is an equivalent mutant (class 4 above).
* **`claude_launch.rs is_executable -> false`** was claimed closed and had never survived: the
  `#[cfg(unix)]` twin's five mutants were all caught by the pre-existing suite, and the survivor is
  the `#[cfg(not(unix))]` one, which no test on this host can kill. That test closes nothing and now
  says so.
* **`agent.rs:969`, the `NotFound` match guard, was claimed closed by a VACUOUS TEST.**
  `an_agent_directory_that_cannot_be_inspected_is_not_reported_as_a_missing_agent` closed the store's
  parent directory to `0o300` and expected `stat` to fail — but `stat(2)` needs SEARCH permission on
  each parent, which is the EXECUTE bit, and `0o300` is `-wx`: it grants exactly that. So
  `symlink_metadata` succeeded, `get` returned `Ok`, and the test took an unguarded
  `Ok(_) => return` escape hatch and asserted nothing at all. It passed identically with the guard
  deleted. MEASURED on this host: a parent at `0o300` or `0o100` lets `lstat` through; `0o600` and
  `0o400` give `EACCES`. The mode is `0o600` now, the escape hatch is gone, and the fixture's own
  premise — that `stat` really did fail with `PermissionDenied` — is asserted before the assertions
  that depend on it. With the guard mutated the test now reddens: *"an unreadable store must not be
  reported as an absent agent: no agent 90dc69c3-…"*.

The other eighteen verified: each names a mutant that appears in a `missed.txt` before the test and
in a `caught.txt` after it.

**The pool's survivors were the largest untriaged block, and eighteen of the thirty-six were closed
by four derived tests.** Each of the four was also proven by hand in the house idiom — the guard
deleted, the target run red, the file restored byte-identical — rather than resting on the next run
alone. **All eighteen claims are now confirmed by re-running the mutants, twice**: every one is in
the `caught.txt` of the complete whole-scope BEFORE run and again in the targeted verification
(`.context/gate-a-mutants/verify/`, 75 mutants over every site ever recorded as surviving). Checked
site by site rather than counted — the first draft of this sentence said "seventeen of the eighteen",
having attributed `mod.rs:1130` to a claim nobody made.

* **Eight `Display` impls rendered the empty string with the suite green** —
  `PoolInvariantViolation`, `TransitionRefusal`, `ConfigRefusal`, `InvariantViolation`,
  `InstanceClass`, `InstanceState`, `Transition`, `IllegalTransition`. `fmt -> Ok(Default::default())`
  writes nothing and reports success, and these are the types that put a reason in front of an
  operator when the pool halts. `every_display_this_pool_implements_renders_a_non_empty_reason`
  SCANS `crates/service/src/pool/*.rs` for `impl … Display for X` and requires its sample table to
  name exactly that set, so a type that grows one tomorrow fails by name.
* **Six accessor mutants on `BucketCounts`** (`idle`, `reserved`, `tearing_down`, each to `0` and to
  `1`). Every case reached them through a rendered refusal whose interesting bucket happened to hold
  zero or one instance, which cannot tell an accessor from a constant. Each bucket now carries a
  distinct count of at least two.
* **Two on `admitted_model_list`** (`-> "xyzzy"`, `-> String::new()`). Its doc claimed the whole
  property — "derived from `MODEL_TABLE`, so a model added to the table is offered by every refusal"
  — and nothing compared the two. This is `agent::supplied_start_paths` again, in the same shape.
* **Two on `InstanceState`** (`owns_a_root -> true`, `is_terminal -> true`). `is_terminal` is now
  DERIVED from the edge table rather than restated, since its doc — "absorbing, no transition leaves
  it" — is a property `EDGES` already answers.

**THE POOL'S NINETEEN SURVIVORS ARE NOW FIFTEEN CLOSED AND FOUR EQUIVALENT, AND
THE FOUR ARE EQUIVALENT WITH A TEST BEHIND THE ARGUMENT.** Nineteen and not
eighteen: the complete whole-scope run adds `mod.rs:1130`, which the pool-only
run had recorded as CAUGHT (§9.27). The previous pass left the eighteen open
with one sentence — "every one of them needs a live pool actor under `tokio`
with slots in specific states, which is a harness this pass did not build" — and
that sentence was wrong about thirteen of them.
`crates/service/tests/path_b_pool.rs` IS a live pool actor under `tokio` with a
deterministic host, a driven clock and a queueing spawner, and it had been for
the whole branch. What those thirteen needed was not a harness: it was a test
that observes something the existing ones do not. Every closure below was
verified BY RUNNING THE MUTANT, not by reading the test — 75 mutants over every
site ever recorded as surviving, `.context/gate-a-mutants/verify/`.

**Fifteen closed, by what each one costs:**

* **`mod.rs:520 Pool::check_invariants -> Ok(())`** — a `pub` checker with
  dozens of callers and no test that could fail, because every call site asserts
  it returns `Ok`, including forty `assert_invariants` calls inside one mixed
  sequence. THIS one did need a new harness, and a different one from the
  integration double: the states it answers for are states the pool refuses to
  enter, so they have to be PLANTED, and `PoolState` and `Pool::state` are
  private to `crate::pool`. `pool/mod.rs::tests::live` is that harness — a real
  `Pool` over a minting host, with the module's own state reachable — and
  `every_pool_invariant_this_module_names_is_refused_when_it_is_planted` plants
  one state per variant, with the variant set READ OUT OF THE ENUM DECLARATION
  rather than listed, and a well-formed pool at the end so the table cannot be
  satisfied by a checker that refuses everything.
* **`mod.rs:690 && -> ||`, `mod.rs:690 < -> <=`, `mod.rs:691 && -> ||`** — the
  three clauses of `should_rewarm`. The existing test asserted
  `spawner.pending() >= 1` under the sentence "a checkout that emptied a class's
  idle set queues a re-warm", and **the post-answer clear alone satisfies that
  bound**: the assertion could not tell a queued re-warm from no re-warm at all,
  let alone a spurious one. Three cases with EXACT counts — dry class beside a
  free slot, dry class at the budget, and a class still holding an idle
  instance — separate all three mutants, and the third also mints, so it is
  caught twice over.
* **`mod.rs:876 Pool::abandon_mint with ()`** — no test in this tree had ever
  made a mint fail. `Script::mint_failures` was READ by the double's `mint` and
  written by nothing, so the entire compensation path for a launch that did not
  happen was untested and deleted clean. With it deleted the instance stays
  `Reserved` forever: a pool of two whose Claude is misconfigured is permanently
  full after two requests. Both arms are now covered — proven-reaped releases
  the slot and erases the tree, may-be-live leaks the slot and KEEPS it.
* **`mod.rs:899 && -> ||`** — a re-warm that lands after the pool stopped
  minting. The conjunction is the two reasons a pool stops (`shutting_down`, and
  a halt), and `||` makes either alone sufficient to mint. The window is the
  ordinary order, not an exotic one: the re-warm is queued during admission and
  the halt is raised by the clear that runs behind it.
* **`mod.rs:1130 Pool::abandon_unpublishable with ()`** — the teardown for an
  instance that could not enter the idle set, which `publish_idle_locked`'s own
  doc calls "a capacity leak with no diagnostic": the instance is neither
  serviceable nor being destroyed and holds its slot for the daemon's life. It
  is reachable only when a proof-carrying transition would leave an instance
  breaking its own invariant — the configuration-reload case that doc names —
  so it needs a PLANTED state and therefore the in-module harness, which is the
  second of the two mutants that justify building it.
* **`mod.rs:1098 delete !`, `mod.rs:1109 == -> !=`, `mod.rs:1286 == -> !=`** —
  retention. The positive case was proven and the negative was not, and all
  three mutants live in the negative: `1286` marks every instance quarantined,
  so a healthy recycled instance's whole config root is moved into the
  operator's evidence directory and kept forever instead of erased; `1098`
  reclassifies a clear that positively typed nothing as one that may have typed;
  and `1109` drops `BeginDestroy` out of the quarantine path, leaving the
  instance stuck in `Quarantined` holding its slot, with `Reaped` refused as an
  illegal edge out of it. `retention_keeps_a_quarantine_and_nothing_else` reads
  zero, zero and one from the same retention directory in one test.
* **`mod.rs:1504` (the `NotFound` guard replaced with `true`)** — every `stat`
  failure reported as "already gone". `Pool::destroy` reads that `Ok` as "the
  root is erased", takes the `Reaped` edge and releases the slot, so a tree pmux
  could not even look at is recorded as destroyed with `leaked` still 0. The
  new fixture closes the SLOT directory to `0o600` — **not `0o300`, which grants
  the search bit `stat` needs**, which is the trap §9.25 records — and asserts
  that premise before anything depends on it.
* **`mod.rs:1507 || -> &&`** — `symlink_metadata` never follows, so a symlink's
  own `is_dir()` is always false and the two clauses agree about every symlink,
  which is the only shape the existing cases build. A REGULAR FILE where the
  epoch directory belongs is the shape `&&` lets through into `remove_dir_all`.
* **`config.rs:262 > -> >=`** — the existing case declares 3 against a pool of
  2, and `3 > 2` and `3 >= 2` agree about that. The one value that separates
  them is a warm set the size of the pool, which is the most natural
  configuration anybody would write and was the value never tried.
* **`config.rs:318 ^= -> |=`** — the fingerprint test asserted only that two
  prompts differ and that one prompt is stable, and `|=` satisfies both while
  not being FNV-1a at all. Known-answer vectors close it.
* **`host.rs:246 TrackedSpawner::spawn with ()`** — every pool test substitutes
  a queueing spawner, which is what makes "the caller never waits on the clear"
  observable, so the one `Spawner` the daemon installs had no test and a `spawn`
  that dropped its future passed the whole suite. What it drops is every
  post-answer `/clear` and every background re-warm.

**Four are EQUIVALENT MUTANTS, and each is equivalent because of a stated
premise that is now a test rather than an argument:**

* **`mod.rs:702 < -> <=`, `mod.rs:903 < -> <=` and `mod.rs:903` (match guard →
  `true`).** All three widen a capacity test that is conjoined, in the same
  expression, with `free_slot(..).is_some()`. `free_slot` skips exactly the
  slots `capacity` subtracts — the occupied ones and the leaked ones — so
  `free_slot` answering `Some` already implies `live() < capacity()`.
  `a_free_slot_is_never_offered_while_the_pool_is_at_its_budget` enumerates every
  pool state for `pool_size` 1 through 4 and asserts the implication, so the day
  it stops holding these three become real. The asymmetry confirms the reading:
  the same guard replaced with `false` is CAUGHT, and `<` replaced with `>` or
  `==` is CAUGHT.
* **`mod.rs:1308 && -> ||`** — the unpublish condition in `transition_locked`.
  Under `||` the extra cases are `Idle -> Idle`, which the machine has no edge
  for, and any transition between two non-`Idle` states, where the slot is not
  in an idle set to begin with so `remove_from_idle` is a no-op.
  `the_machine_has_no_edge_from_idle_back_to_idle` is the first half written out;
  the second is `PoolInvariantViolation::IdleSetHoldsNonIdle`, which the test
  above now plants and proves is refused.

**`native.rs` and `driver_io.rs` are measured out of band and are not in the gate cell.** The partial
runs over them are the most useful thing the campaign produced about coverage SHAPE rather than
coverage AMOUNT, and the shape is one boundary rather than a scatter.

`native.rs` was stopped after 90 mutants; 40 were unviable, so **26 of 50 decided survived**
(`.context/gate-a-mutants/native-partial.out/`). **17 of the 26 fall in methods on `NativeService`
itself, across twelve of them** — `start_pool`, `pool`, `start_idle_reaper`, `reap_idle_sessions`,
`reap_pending_startup_cleanup`, `resolve_agent_reference`, `seed_config_isolation_root`,
`clear_session`, `clear_boundary`, `clear_timeout_ms`, `wait_for_turn` and
`start_session_owned_with_retention`. The other **nine are not**, and an earlier draft of this
paragraph said "all but three" and listed the two `Drop` impls as though they were `NativeService`
methods: they are `SessionMetadata::shutdown`, `PendingStartupCleanup::close_controlled_terminal`
(two mutants) and `::shutdown`, `record_lifecycle_stop_instant` (three), and
`Drop for SessionLifecycle` and `Drop for IdleReaper`. What all 26 share is not a receiver, it is a
PRECONDITION — every one needs a live service or a live session actor to run at all — and that is
the same boundary §4's S-36 row already recorded as a RESIDUAL in prose ("that one call site needs a
live `NativeService` and is therefore only exercised by `#[ignore]`d tests"), now with a number
attached to it.

`driver_io.rs`'s **15 survivors in 111 decided** are 13 boolean-operator flips and two arithmetic
ones, and **nine of the 13 sat in `rendered_prompt_is_proven` and `active_editor`**, where the
clauses are only ever exercised together. The remaining four are single flips in `prompt_glyph_col`,
`validate_prompt`, `validate_turn_deadline_domain` and `prove_stable_empty_editor`.

**That measurement is at `09f5f41` and the first of those two functions no longer exists under that
name.** It was `rendered_prompt_head_is_proven` and is `rendered_prompt_is_proven` again; it carries
a text clause the geometry never had, that clause has its own tests, and three of its geometric
clauses were separately found to survive mutation on 2026-08-10 — two now have tests and the third
was deleted as unreachable. The survivor count above was never re-measured against any of that, so
read the nine as a statement about a predicate this tree does not contain rather than as a current
number.

### 9.24 The differential entry-path test, and the two experiments that prove it can fail

Leaks 1, 2 and 3 were each the same sentence — **this path lacks the guard** — and each was found by
reproducing one entry path after the guard had been written for another. `start_session`, `run_once`,
a stored agent and the pool each reach admission by their own route, and each resolves the
configuration root by a different mechanism: a caller's `environment.set`, the same value carried
inside a `RunOnceRequest`, a stored agent's own `set`, and the pool's `config_isolation` root. **A
test that drives one of them says nothing about the other three**, which is why six alias-proof
identity tests all passed while leak 7 let eight shapes into a live cell.

`native::tests::every_entry_path_that_reaches_admission_answers_the_alias_family_identically` drives
ONE logical operation — "start against a directory a live minified cell holds, spelled like this" —
through every route and asserts the four answers are the SAME VALUE. It never asserts a particular
answer per path: a rule that refuses on three paths and admits on the fourth is the shape of every
leak in the family, and only a comparison can see it. The alias family is the one the leaks taught:
identity, trailing slash, `..` through a MISSING component, a terminal symlink, a path inside the
live cell's subtree, and the APFS firmlink (`/System/Volumes/Data/...`) that `canonicalize` does not
collapse. Each spelling's premise is asserted before it is used, so a row that stops aliasing fails
as a broken fixture rather than passing as a rule that held.

**The route list is DERIVED**, by three scans whose union is the answer: every `Request` variant whose
`dispatch` arm reaches the admission closure, every function outside `native.rs` that calls one of
that closure's externally visible doors, and every function anywhere in the crate that builds a
`StartSessionRequest` literal. `ADMISSION_ROUTES` must classify every derived route as one the test
DRIVES or one that carries no start, with the reason, and the check runs in BOTH directions — a
derived route with no row fails, and a row naming a route the derivation no longer reports fails too.

**It discriminates, and that is measured rather than argued.** The guard was removed from one path at
a time; both edits were reverted and both files verified byte-identical afterwards.

1. Deleting `claim.cell == SessionCell::Minified ||` from `claim_reaches` leaves the containment rule
   reaching only MINIFIED applicants — the pool and nothing else. The test reddens naming the route,
   the role and the spelling: `Admitted(Write)` for `["agent_start", "caller_start", "run_once_start"]`
   against `Refused(InvalidConfig)` for `["pool_start"]`, on a configuration root spelled *inside the
   live cell's subtree*.
2. Making `agent::resolve_agent_start` carry the caller's `set` instead of the stored agent's takes
   the configuration root out of that one route's request. Every HELD spelling still agreed — both
   answers are `Refused(InvalidConfig)`, for two different reasons — so it was caught only by the
   unheld-pair control row, which exists precisely so the test cannot pass by refusing everything.

**The second experiment found a defect in the test's own harness.** `disagreement` reported every
route whose answer differed from the alphabetically FIRST one, so with `agent_start` the only route
escaping, the failure named `["caller_start", "pool_start", "run_once_start"]` — the three that were
RIGHT. The truth was recoverable from the map printed beside it, and that is exactly the standard
this tree does not accept. It partitions now: every answer given, with the routes that gave it, and
no privileged route.

### 9.25 THE BUG CLASS, instance twenty-nine — seven claims written BY the pass that installed the tool to find them

§9.22 recorded the practice as instance twenty-eight: twenty-six hand-run mutation campaigns, each a
sample of a space a tool enumerates. Installing the tool produced instance twenty-nine in the same
week, in the same shape, and — as with the two before it — **written by the agent fixing the previous
one.** Every one of these was caught by asking the artifact whether it was true, and none by reading.

1. **Four "SURVIVING MUTANT CLOSED" comments named mutants they did not close** (§9.23). A closure
   claim is a checkable statement and nobody had checked one; re-running checks all twenty-two at
   once. `v1.rs:151` needed a condition the test never constructed, `v1.rs:287` is unkillable,
   `is_executable -> false` had never survived, and `agent.rs:969` was claimed by a test that
   chmod'd a directory to `0o300` believing that blocked `stat` -- it grants the search bit `stat`
   actually needs -- and then returned early through an unguarded `Ok(_)` arm, asserting nothing.
2. **The gate cell was named for a concept it excludes.**
   `mutation_score_service_admission_and_protocol`, at `PMUX_MUTANTS_SCOPE=admission`, mutates no
   file containing an admission guard: `admit_bound_resources`, `admit_config_root`, `admit_cwd`,
   `claim_reaches` and `effective_config_root` are all in `native.rs`, which the scope leaves out for
   wall-time reasons. A cell whose NAME promises more than its predicate measures is the definition
   of the class. It is `mutation_score_agent_launch_pool_protocol` at `PMUX_MUTANTS_SCOPE=gate` now,
   and the script prints its globs and its two exclusions beside the score on every run.
3. **`Cargo.toml` cited a gate cell that does not exist.** The comment on `[profile.mutants]` ended
   "`gate_a/mutation_profile_is_dev_without_debuginfo` … asserts that shape from this file". There is
   no such cell in any phase. The guard is real — `assert_profile_is_dev_without_debuginfo` in
   `scripts/gate-a-mutants.sh`, running as the first step of the mutation cell — but the citation was
   invented, and a reader checking the claim would have concluded the opposite of the truth. The
   repair then cited that function by FILE AND LINE, and the line moved inside the same session; it
   names the function only now, which is the most precise thing here a reader can still resolve.
4. **The differential test's own harness named the wrong routes** (§9.24). It reported dissent
   against the alphabetically first route, so a lone deviant that sorted first was reported as three
   deviants that were correct.
5. **Three measured numbers in this document were false**, and each was found by opening the file it
   claimed to summarise: "69 survivors in 623 decided" (623 was the count of mutants that got an
   outcome; 145 of them were unviable and the denominator is 478), "all but three are methods on
   `NativeService`" (nine of twenty-six are not, and two of the nine are the `Drop` impls the sentence
   listed as though they were methods), and "watching five tests go red" for the `cfg` twins (26, 24
   and 2).
6. **A composite of two runs was presented as a measurement.** The 88.67% baseline is
   arithmetically exact — `validation/out` for the non-pool files plus `poolonly` for `pool/**`,
   702 = 441 − 13 + 274 — which is why it read as a single figure and why it survived review. It was
   stale on the day it was written: a mutant it lists as MISSED was already CAUGHT on the same tree.
   **No single complete run of the script had ever finished** when the number was published.

7. **Two more citations in the script's own header were false, and one derivation finds both.**
   The header said only the test targets of `PMUX_MUTANTS_TEST_PACKAGES` run — an environment
   variable nothing in this tree reads or sets; the real thing is a fixed `TEST_PACKAGES` array, and
   "configurable" is a different claim from "three names fixed in the script". The tell is
   mechanical: every `PMUX_MUTANTS_*` name that is real occurs at least twice in the file (a
   declaration and a use), and that one occurred exactly once. The same header called `vendor/`
   "75% of the Rust in the tree", which is neither of its two true values — **84.4% by file (643 of
   762) and 70.7% by line (311,685 of 440,778)**, both from `git ls-files '*.rs'`.

The counter-measure for all seven is the same one this repository keeps arriving at, and it is now a
gate cell rather than a practice: **the claim and the check must be the same artifact.** A closure
claim that is not re-run is prose; a scope named for a concept is prose; a citation nobody resolves
is prose. `gate_b/mutation_score_agent_launch_pool_protocol` re-runs all of it on every gate.

### 9.26 THE BUG CLASS, instance thirty — a self-test named for the citation it did not grade, and a precondition nobody declared

§9.25 ended on "a citation nobody resolves is prose". `tools/phase0/verify_calibration.py` held
**22 product-source line numbers, and 16 of them were wrong** — a rate nobody had ever measured,
because the only thing that ever looked at any of them looked at 4.

That 16 is counted, not estimated, and the counting is the point: the first draft of this section
said "eight", which was the number of *sites* a reader notices rather than the number of citations
in them, and it was written into four files before it was checked. Re-derive it with the audit at
the end of this section.

**What was actually wrong.** `gate_f/phase0_self_tests` was red at 3 of 243 because
`NEGATIVE_HEADROOM_BANNER` cited `driver_io.rs:2628-2631` for `state.last_change.elapsed()` and
`STOP_HOOK_UNCOMPUTABLE_REASONS` cited `v1.rs:2163-2165` for "Absent on any turn where no Stop hook
was observed" — both true when written, both pointing at unrelated code after Path B moved
`driver_io.rs` by ~2,000 lines and `v1.rs` by ~500. That is the ordinary shape. The instance is what
was *around* them:

1. **The tool PRINTED a wrong line number that a test named for it was passing over.**
   `crates/service/src/v1/actor.rs:83` appears in the default text output beside every noise-band
   figure. `poll_interval: Duration::from_millis(20)` is on **85**; 83 is
   `replay_byte_capacity`. `test_the_noise_band_line_cites_the_actor_poll_interval` was green
   throughout, because it resolved its subject with `module_lines_naming("poll_interval")` — the
   tool's source lines containing that exact token — and the printed line says "one actor poll
   interval," with a space. So the test graded the copy in a comment two hundred lines away, which
   happened to say 85, and never touched the string it is named for. A test whose NAME promises the
   emitted line and whose PREDICATE reads a comment is the class exactly, and it is the same shape
   as §9.25's gate cell named for a concept it excludes.
2. **11 of the 16 were in comments and docstrings, where nothing looked at all.** The only
   surviving drift guard walked uppercase module constants and asserted the ranges were merely
   *in bounds*, so a citation pointing at the wrong function in the right file passed it. All 11:
   `v1.rs:1269-1436`, `v1.rs:1334-1365`, `v1.rs:1166-1189`, `engine.rs:865`, `v1.rs:1305-1313`,
   `actor.rs:2897-2900`, `actor.rs:2908-2909`, `actor.rs:3007-3018`, `v1.rs:1345-1350`, and
   `v1.rs:1315-1323` twice. The 5 printed ones were `v1.rs:2163-2165`,
   `driver_io.rs:2628-2631`, `driver_io.rs:2714`, `driver_io.rs:2661-2663` and `actor.rs:83`;
   the 6 that were still exact were all `backend.rs` or `actor.rs:85`.
3. **The package-smoke validator hand-wrote its own tool set three times.**
   `set(module_files) != {"pip", "setuptools"}`, `expected_distributions = [["pip", …],
   ["setuptools", …]]`, and a two-name unpack, none derived from the other two.

**The fix is the one §10.3 already arrived at for `driver_io.rs`: the number is derived or it is not
written down.** `cite(path, anchor, after=…, through=…)` resolves each citation at import by
searching the cited file for the text the sentence is about, and **refuses rather than guesses** —
absent file, absent anchor, or an anchor matching more than once all render with no `path:<digits>`
at all and the reason in the text. Ambiguity is not hypothetical:
`state.last_change = Instant::now();` occurs twice in `driver_io.rs`, byte-identical, once at an arm
boundary and once inside `read_observed_range`, and the banner's whole argument rests on the second;
`after="fn read_observed_range("` is what distinguishes them, and deleting it turns the citation into
a refusal rather than into a plausible wrong line. Citations that had no anchor worth resolving —
comments and docstrings pointing at a struct or a doc paragraph — name the SYMBOL and drop the
number, which is what §10.3 did and for the same reason.
`test_no_product_line_number_is_written_down_anywhere_in_the_tool` then refuses any hand-written
product line number in that file, comments included; citations into `tools/phase0/` itself are
exempt and every one of them was still exact when this was measured.

**The interpreter contract is the same defect with no line numbers in it.**
`gate_f/package_smoke_self_tests` was red at 1 of 35 with
`importlib.metadata.PackageNotFoundError: No package metadata was found for setuptools`, three frames
below anything naming a package. `package_smoke.build_python_package` never touches the ambient
interpreter — it takes a declared, hashed `python_build_support_tree` — but the self-test's FIXTURE
materializes that tree out of whatever the running interpreter has installed, so it required of the
host exactly what the product requires of a declared input, and required it without saying so. Python
3.12 stopped shipping `setuptools` through `ensurepip`, which turned a silent assumption into a red
cell on a stock 3.13.

Of the three available repairs, **declare and check** is the only one that is not a different
project. *Vendoring* setuptools adds a large third-party tree to a repo that hashes 926 source files
per gate run, forever, so one fixture can be built. *Stopping the dependency* means changing
`clients/python/pyproject.toml`'s `build-backend = "setuptools.build_meta"`, which is the published
build contract of `pmux-client` and changes what a user's `pip install` does — a product decision,
not a gate repair. And the gate ALREADY requires a `{python}` with `ruff` importable, for
`gate_a/python_ruff`, so an interpreter contract is not a new kind of requirement here; it was
just an undeclared one. So: `PYTHON_BUILD_SUPPORT_DISTRIBUTIONS` is stated once and the validator
reads it, `validate_python_tool_report` refuses a tree by the NAME of what it lacks instead of
"is not exact", and the fixture checks the same tuple before it materializes anything and skips
naming the interpreter, the missing distribution and the remedy. On an interpreter that has
setuptools the real flow runs exactly as before — verified at Python 3.12.4, `36 tests, OK`.

**Recorded and NOT fixed: `tools/phase0/phase0_lib.py` and `tools/phase0/README.md` carry the same
rot, measured with the same audit.** `phase0_lib.py` holds **15** product citations of which **10 are
wrong** — lines 81, 82, 96, 119, 121, 126, 137, 150, 1739 (the `cli.rs` half; the `main.rs` half on
the same line is exact) and 4635, where `:81` is the identical wrong actor poll interval fixed above
— and the README holds **8** of which **4 are wrong**: lines 260, 264, 272 and 491. They are named
rather than repaired because a hand-corrected line number resets the clock instead of stopping it,
and doing to those two files what was done to `verify_calibration.py` is a sitting of its own.
Reproduce with:

```bash
python3 -c "
import re, pathlib
BARE = {'v1.rs': 'crates/protocol/src/v1.rs', 'engine.rs': 'crates/claude/src/engine.rs',
        'actor.rs': 'crates/service/src/v1/actor.rs', 'backend.rs': 'crates/service/src/v1/backend.rs',
        'driver_io.rs': 'crates/service/src/driver_io.rs'}
CIT = re.compile(r'((?:(?:crates|bin|clients)/[\w./-]+|[\w-]+)\.rs):(\d+)(?:-(\d+))?')
for rel in ('tools/phase0/verify_calibration.py', 'tools/phase0/phase0_lib.py',
            'tools/phase0/README.md'):
    for i, line in enumerate(pathlib.Path(rel).read_text().splitlines(), 1):
        for m in CIT.finditer(line):
            path = m.group(1) if '/' in m.group(1) else BARE.get(m.group(1))
            if path is None: continue
            body = pathlib.Path(path).read_text().splitlines()
            a, b = int(m.group(2)), int(m.group(3) or m.group(2))
            print(rel, i, m.group(0)); print('   says:', line.strip()[:100])
            print('   is  :', ' | '.join(x.strip() for x in body[a-1:b])[:100])"
```

Run against `0d7f2ca` it prints 22 citations for `verify_calibration.py`; run against this commit it
prints none, because there is nothing left in that file to print.

### 9.27 THE BUG CLASS, instance thirty-one — eighteen survivors left open under a reason nobody checked

§9.23 closed with a sentence, and the sentence was the defect:

> **The remaining eighteen pool survivors are REAL GAPS and are left open, named, rather than filed
> as equivalents.** … Every one of them needs a live pool actor under `tokio` with slots in specific
> states, which is a harness this pass did not build. **They are the next agent's work and they are
> the reason the floor is not higher.**

**`crates/service/tests/path_b_pool.rs` is a live pool actor under `tokio`**, with a deterministic
`InstanceHost`, a driven `Clock`, a queueing `Spawner` and a real filesystem, and it had been in the
tree for the whole branch — 35 tests, 2,057 lines, cited three rows above in §4. Thirteen of the
eighteen needed no harness at all; they needed a test that **observes something the existing ones do
not**, and each one is now four to thirty lines in that same file. A reason for not doing work is a
claim about the tree, it is checkable exactly like a closure claim, and nobody had checked this one.

The same reason set the floor. "They are the reason the floor is not higher" is how an unverified
sentence becomes an exit criterion: 85% was defended against a `pool/**` figure of 84.5% that was
itself stale. The completed BEFORE measurement is **91.85%** for `pool/**` and **93.50%** for
the whole scope, and closing the survivors that reason had parked took one commit.

Five smaller instances, all found by asking an artifact what it actually tests:

1. **A re-warm counted with a lower bound.** `path_b_pool.rs`'s
   `emptying_a_classes_idle_set_mints_a_replacement_immediately` asserted `spawner.pending() >= 1`
   under the sentence "a checkout that emptied a class's idle set queues a re-warm" — and on that
   path the pool queues TWO things, the post-answer clear and the re-warm, so **the clear alone
   satisfies the bound**. The predicate could not distinguish a queued re-warm from no re-warm, let
   alone from a spurious one, which is why all three `should_rewarm` mutants (`mod.rs:690` twice and
   `mod.rs:691`) survived it. The enclosing test is sound — it drains and counts mints afterwards —
   so this is a weak assertion inside a good test rather than a vacuous test, and it is recorded
   because the shape is the class exactly: the message says "a re-warm", the predicate says "at
   least one piece of background work". **The line is left exactly as it stands**, deliberately:
   the property it should have asserted is now asserted exactly, three configurations wide, in
   `a_re_warm_is_queued_only_when_a_checkout_leaves_the_class_dry_beside_a_free_slot`, and editing a
   test file after the measurement would make the score in §9.23 describe a tree nobody measured.
2. **A `pub` checker with no test that could fail.** `Pool::check_invariants`'s own doc says it is
   "exposed rather than private so a test can assert it after every step of an arbitrary command
   sequence: an invariant only the implementation can see is an invariant a test cannot prove". It
   had dozens of callers and every one of them asserts `Ok` — forty inside one mixed sequence — so
   `-> Ok(())` satisfied all of them. A doc promising a capability that nothing exercises is the
   same sentence one level up.
3. **A scripted failure nothing ever scripted.** `Script::mint_failures` was READ by the double's
   `mint` and written by nothing, in a file whose header says every hard-to-provoke edge "is
   exercised here by telling the double to produce it". No test in this tree had ever made a mint
   fail, which is why `Pool::abandon_mint` — the whole compensation path for a launch that did not
   happen — deleted clean.
4. **A citation that had already moved.** §9.23 placed the `serialize_struct` field-count
   accumulator at `v1.rs:1777–1783`; the complete run puts its survivors at **`v1.rs:1780–1787`**,
   and the count is **15 of 16** `v1.rs` survivors rather than "sixteen of the eighteen". Same file,
   same claim, two edits later. This is §9.26's finding inside §9.26's own repair pass.
5. **And one in this work's own scaffolding, caught by its floor rather than by a reviewer.** The
   new `declared_violations` scan reads `PoolInvariantViolation`'s variants out of the source so the
   planted-violation table cannot narrow. Its first run found **one** variant of six: `rustfmt`
   writes `Name { field: T }` with a space and `Name(T)` without, and the matcher tested
   `rest.starts_with('{')` against a leading space. It refused — `assert!(declared.len() >= 6)` — as
   a derivation that has stopped matching must, instead of reporting full coverage over a set of
   one. The floor is the only reason this is a paragraph and not a sixth wrong claim.

**And a sixth, which is the same class one level out: "any test that goes flaky under load" was as
far as anybody had looked.** §9.23 recorded three mutants flipping between a loaded host and a quiet
one and diagnosed them as *some* flaky test, unnamed. Three more flipped between runs that were all
quiet, and naming them took one minute each — open the log of the run that said CAUGHT and read
which test failed. They are
`bounded_soak.rs::repeated_real_rmux_cycles_remain_resource_bounded_and_leave_no_residue`,
`driver_io.rs::tests::a_preamble_that_lands_after_the_anchor_still_rebinds` and
`private_runtime.rs::a_terminal_resize_after_creation_is_delivered_and_not_silently_clamped`: all
three spawn real processes or hold a wall-clock budget, and none of the three mutants they convicted
can reach any of them. A diagnosis that stops at "flaky" is a diagnosis with no predicate under it,
and the table in §9.23 is what it becomes when somebody looks.

### 9.28 THE BUG CLASS, instance thirty-two — a docstring that named the property and code that checked its precondition

`run_gate.py::require_release_depinfo` opened with this, and it had been true of the docstring and
false of the function for as long as both existed:

> `crates/e2e/tests/pool_concurrency.rs:237` **proves the candidate is not stale** by reading
> `<binary>.d` — the depinfo cargo itself wrote — and refuses when it is absent…

`pool_concurrency.rs` reads that file and compares **mtimes**. The driver read the same sentence and
checked that the file **exists**. Depinfo presence is the *precondition* of the staleness proof, not
the proof: it says the directory is a cargo build and says nothing whatever about when. So the one
paragraph in the driver that talks about staleness shipped a check that cannot observe it, under a
citation to the code that can — the class exactly, and the same shape as §9.20's guard that walked
five of the nine paths its own list supplies.

**What it cost, measured.** One stale `target/release` produced **three** red cells in two phases of
the 2026-08-07 receipt — `gate_d/mcp_process` (9 tools where the source defines 13),
`gate_d/cli_process` (`unexpected argument '--agent-version'`) and `gate_a/release_full_stack_e2e`
(14 failures, 388 s) — and the first two read as brand-new product regressions in the two surfaces
this branch had just built. The only thing in the whole gate that could see the cause was
`pool_concurrency.rs`'s own guard, which covers five of the eight executables and reports as a
product failure six minutes into the longest cell in phase A. `require_release_not_stale` is that
guard hoisted to the preflight, over all eight, naming every stale binary in one refusal. §7's gate
block above carries the receipt evidence.

**A second instance in the same hour, in a document whose other counts are derived.**
`tools/gate-a/README.md` published "`tests/test_run_gate.py` is 35 tests, ~8 s" while the file held
38 taking 20 s — and four lines above it,
`test_the_real_manifest_cell_count_is_the_one_the_readme_publishes` reads the phase counts **out of
this same README** and refuses a drift of one. The counts a tripwire watches stayed right for
months; the count beside them, which nothing reads, went stale twice without anyone noticing.
`docs/testing.md` §F restated the same pair. Both now carry the measured figures **and say in the
line itself that nothing derives them**, which is the honest repair: a number that cannot be
tripwired should announce that it is a description rather than sit next to numbers that are pins and
borrow their authority.

**And the count of the class was itself wrong.** The brief for this pass opened "THIRTY-TWO KNOWN
INSTANCES" and said the counters read thirty-two in `crates/protocol/src/v1.rs`,
`crates/service/tests/agent_resource.rs` and `crates/service/src/pool/mod.rs`. All four counter
sites read **thirty-one**, and §9.27 was instance thirty-one. This section is thirty-two, and the
counters now say so — but that is the third time in this repository's history that a number restated
in three files has been wrong in at least one of them, and `agent_resource.rs`'s own header has said
so since instance twenty-nine while leaving the three to be kept in step by hand.
`test_run_gate.py::test_every_statement_of_the_bug_class_counter_spells_the_same_ordinal` now
compares them, against the ordinal spelled by the LAST `THE BUG CLASS, instance …` heading in this
file, because the document is what decides the count and the code should be reading it. Its floor
earned its keep on the first run: the scan matched `v1.rs` and lost the other three, because the
sentence ends "times." at one site and "times:" at two more, and the named-file floor said so rather
than reporting agreement over a set of one. On its second run it caught **this paragraph** — the
ordinal came back as ``…` `` because the last match in the file was the prose above quoting the
heading rather than a heading, so the truth is now read only from lines that begin `###`. A
derivation whose first two runs each find a real defect in itself is the argument for writing
derivations.

**It is in the gate driver's tests and not beside the counter, deliberately, and the third defect
this paragraph records is that it was written in the wrong place first.** It began as a `#[test]` at
the foot of `crates/service/tests/agent_resource.rs`, which is where the counter and the lament
about it live. Every test target of `pseudomux-service` runs **once per mutant** under
`scripts/gate-a-mutants.sh`, in a copy of the tree `cargo-mutants` makes — and whether that copy
carries `docs/` is a claim about a third-party tool that nothing in this repository had ever tested,
because no Rust test here reads a file outside `crates/` or `bin/`. A wrong guess aborts the
88-minute `gate_b` baseline. `tools/gate-a/tests` never runs under mutation, already scans the whole
repository for a second defect of this exact shape, and reaches the Markdown as easily as the Rust.
Two gate attempts were killed and restarted over this pass — one on a `clippy::double_ended_iterator_last`
that `cargo clippy` had been run before the edit that caused it, and one on this — which is the
argument for running the fast static cells against the exact tree you are about to spend two hours
on.

### 9.29 THE BUG CLASS, instance thirty-three — a reordering "verified gate-equivalent" for one property, and a revision that was never a mutation counter

Two claims, one pass, one shape: a sentence that names a property, checks a *different* property, and
reports the check under the first property's name. One of them is in this document. One of them is in
the decomposition that was written to find things like it.

**First: R2, in §7's residual table, about this exact change.** It closed

> Reordering to test the drain first is verified gate-equivalent, but it is an optimisation, not a
> defect: the gate never admits early, only late

That is true of *"never commits before the drain requirement elapsed"* — measured, and in the
strongest available form: with the drain decided from the confirming re-poll, `drain_ms` over n=30
has **min 250**, exactly the configured value, never under. It is **false** of *"still catches a row
that lands 352 ms after the marker"*. The reorder collapses the commit loop's sampling period from
~275 ms to ~1 ms, and the catchable window with it, from ~550 ms to 276 ms median (n=30) — under the
**438 ms** campaign max and under the **352 ms** arrival that really happened on ordinal 70. Six
tests in `crates/service/tests/v1_actor.rs` go red on observed behaviour when the stale term is
removed, including

```text
---- the_live_352ms_post_marker_arrival_is_still_caught stdout ----
assertion `left == right` failed: the row must have been served: pmux was still
polling 352ms after the marker, as it was on ordinal 70
  left: 1
 right: 2
```

`cargo test -p pseudomux-service --lib` stays green throughout: the loss is visible only where
somebody built a timeline for it. **"Verified" named a run that was made, and reported a property
that run does not decide.** R2 is rewritten above, the reorder is recorded as NOT AVAILABLE, and the
274 ms it would save is stated so nobody has to rediscover the size of the trade.

**Second, and it is why the 264 ms Gate 1 saving is not being taken: `TerminalSnapshot.revision` is
not a mutation counter, and the proposal that would have spent it said it was.** The proposal —
replace Gate 1's 250 ms accumulated poll with a revision-identity proof, remembering
`(revision, first_observed_at)` per session and discharging the window in one read — rests on this
premise, written as a conditional and never checked:

> If `TerminalSnapshot.revision` is monotone and bumps on every screen mutation, then "revision *R*
> observed at *T₁* and again at *T₂*" proves the screen was unchanged across the **whole** interval.

The daemon pmux ships and runs (`bin/pmux-rmuxd` embeds `rmux_server::ServerDaemon` from `vendor/`,
and both snapshot request paths funnel into one place) assigns the revision **per capture**, by
comparing this capture's fingerprint against the previous *capture's*:

```rust
// vendor/rmux-server/src/handler_pane/snapshot.rs, PaneSnapshotRevisionRegistry::revision_for
if state.fingerprint == fingerprint {
    return state.revision;
}
state.fingerprint = fingerprint;
state.revision = state.revision.saturating_add(1);
```

`revision_for` is reached only from `handle_pane_snapshot_inputs`, i.e. only when a capture RPC is
served. **The revision counts observed fingerprint transitions, not pane mutations.** Measured
through the corpus over 30 consecutive turns: it advanced from **8 to 67**, about two increments per
turn, while the pane's content changed many times per turn. That is the signature of a
transition-on-capture counter and not of a mutation counter, and it is directly visible in the frames
printed in §6.1.2.

The interval property the proposal needs *is* nevertheless true today, and the reason for that is the
finding. `compute_snapshot_fingerprint` hashes `output_sequence`, which `PaneTranscript` advances on
every non-empty `append_bytes`, so equal revisions at `T₁` and `T₂` imply equal `output_sequence` and
therefore that **no bytes were fed to the pane in between**. But `output_sequence` is `pub(crate)` in
`vendor/rmux-server/src/pane_transcript.rs`; it is not a field of `rmux_proto::PaneSnapshotResponse`,
not a field of `rmux_sdk::PaneSnapshot`, and not on the wire. pmux cannot read it, cannot assert it,
and could not detect its removal from the fingerprint — the revision would go on looking identical
and would quietly stop meaning what a 250 ms proof had been built on. The published contract
(`rmux-proto` `src/response/pane.rs`: *"changes whenever any observable field … changes"*;
`rmux-sdk` `src/snapshot.rs`: *"Equal revisions therefore mean 'nothing observable changed'"*)
compares **two captures** and says nothing about the interval between them. A pure content-hash
implementation satisfies every word of it, and under one, equal revisions would mean only what pmux's
existing `fence == baseline` snapshot comparison already checks for free.

And in the shipped system the gap is not hypothetical: between turns **nobody captures at all**, so
across the interval the anchor would span, the revision cannot have advanced for any reason, and
"revision *R* at `T₀` and again at `T₁`" reduces to "these two frames are equal" — plus two hidden
fields. The proposal would make an interval nobody observes load-bearing for the first time.

So the trade is not "264 ms for the same proof". It is **264 ms for a proof pmux states from its own
reads, exchanged for one it inherits from an invisible field of a vendored third-party daemon under a
sentence that does not say it.** **Declined.** The unblocking step is upstream and is drafted at
`docs/upstream-issues/03-rmux-snapshot-revision-contract.md`: ask rmux to say which of
the two readings it intends. Either answer is worth having; the present text is the only one that is
not, because it reads like the strong one and is implemented as the weak one plus a detail no
consumer can see.

**Third, found while writing the above, and it is the purest form of the class this document holds.**
§6.2's per-turn overhead budget is a table of constants, each cited by a line number in
`crates/service/src/driver_io.rs`. **All of them were wrong, and all of them were right at
`405fccd`, the initial commit:**

| §6.2 row | cited | actual at `d89a963` | at `405fccd` |
|---|---:|---:|---:|
| Editor-stability window (`quiet_for`) | `:554` | `:1329` | `:554` |
| Post-paste render window (same field) | `:554` | `:1329` | `:554` |
| `TERMINAL_POLL_INTERVAL` | `:40` | `:43` | `:40` |
| Evidence timeout | `:555` | `:1330` | `:555` |
| `INPUT_GATE_MAX_DURATION` | `:39` | `:42` | `:39` |
| Recovery timeout | `:556` | `:1331` | `:556` |
| `MAX_PROMPT_BYTES` | `:36` | `:39` | `:36` |

Nothing derived them, nothing checked them, and the file grew by seven thousand lines around them.
R2 four sections later carried seven more of the same. This is `verify_calibration.py`'s 16-of-22
(§9.26) in a document rather than in a tool. **Repaired the way §9.26 repaired its own: the column
carries the constant's NAME, which is greppable and cannot go stale** — and the three quantities that
had no name to carry (the screen quiet window, the poll interval in milliseconds, and the loop period
they multiply out to) were given one by this pass for the assertion in `SCREEN_QUIET_FOR`, and are
now what the table points at.

**And the same six are not only prose — they are PUBLISHED AS EVIDENCE.**
`crates/service/tests/performance_diagnostics.rs` writes a `product_constants.citations` map into the
Gate A performance receipt, and it carried exactly those six `driver_io.rs:<line>` strings: `:554`,
`:554`, `:40`, `:555`, `:39`, `:556`. A gate cell has therefore been emitting six wrong citations
into a signed evidence artefact for as long as the file has been growing, under a `source` field that
says *"transcribed … not imported: these are private consts"* — which names the mechanism of the
defect and does not treat it as one. That map now carries names, plus one `file` key, plus a
`derived` block publishing the commit-loop sampling period and the post-marker catch window beside
the constants they are the product of, so the completion phase's number no longer has to be
re-derived by whoever reads it. The gap entry at that cell already disclosed that its `completion`
phase runs against a zero-latency double (§7, row R4); what it did not disclose is that the constants
it offers instead were pointing at the wrong lines.

**And a fourth, at receipt level, recorded because it is the same shape.** Of the eight measurement
receipts this pass inherited, `r6-base-frames.json` is labelled `base-frames` and holds a commit-gate
median of **255 ms** — it is the *second* run of the R2 reorder, not a baseline. Its own `label`
field says `base-frames`; its numbers say otherwise, and only `r8-head-frames.json` is the baseline
it claims to be. A receipt whose label and content disagree will be quoted wrongly by everyone after
the person who took it.

**What was taken instead, and why it is smaller on purpose.** The catchable window now has a name, a
measured basis (438 ms campaign max, 352 ms live sample), a derivation that predicts the measurement
at two configurations, a compile-time refusal in `driver_io.rs` that the minified cell binds first,
and a test tying the band suite's private `catchable_window_ms` to the product's own
`post_marker_catch_window_ms`. It saves **0 ms** and is not claimed to save any. What it buys is that
the next person who shortens a screen constant for latency finds out **from the compiler** that they
are shortening a truncation-risk guarantee — which is the failure `TURN_DURATION_DRAIN_FLOOR_MS`'s
doc predicted one level down, and which nobody had written one level up.

## 10. Known-good invariants a future change must not break

A change that breaks any of these is not a refactor. Each is load-bearing and each has a named owner
in code.

0. **An agent may narrow what a session may name; it may never name a resource on its behalf**
   (`crates/service/src/agent.rs`, `docs/spec.md` §4.8.1). `cwd`, `config_isolation` and `identity`
   stay per-session; `environment.snapshot` stays per-session STRUCTURALLY, because
   `AgentEnvironmentSpec` has no such field. `AgentRef::version` is REQUIRED, a stored version is
   immutable, resolution is a pure function at the one start door, and the resolved digest is echoed
   — drop any one and `docs/spec.md` §4.4's argv-purity claim is false. An agent is NOT a security
   boundary: the daemon and its clients run as the same uid, so anything an agent would refuse the
   caller can send inline, and documenting it otherwise is the error §4.8 concedes in full.

1. **`CompletionAuthority` has exactly one variant** (`crates/protocol/src/v1.rs:1288-1292`).
   "The screen became the semantic authority" must remain unrepresentable in the wire type.
2. **Nine independently-required completion factors** (`crates/service/tests/completion_gate.rs:23-33`).
   Each is proven independently necessary by blocking exactly it and requiring no turn is stored.
   The screen holds two of them and holds a veto, never a vote.
3. **Exactly one paste and at most one Enter, with no retry on ambiguity**
   (`crates/service/src/driver_io.rs`: `paste_once` and `enter_once` are the only writers, each
   called once per `submit_prompt`/`type_control_command`, neither in a loop). `(1,1)` **only** on a
   proven changed-stable render. Held by the in-module tests that assert exact
   `(paste_count, enter_count)` pairs against the fake terminal — **28** of them over **42**
   assertion sites, derived rather than counted by hand, because the `11` this row carried had gone
   17 stale. Re-derive with:

   ```bash
   python3 -c "
   import re, pathlib
   s = pathlib.Path('crates/service/src/driver_io.rs').read_text()
   t = re.findall(r'async fn (\w+)\(\) \{(.*?)\n    \}\n', s, re.S)
   print(len([n for n, b in t if '.counts()' in b]))"
   ```

   The line range this row used to cite is deliberately gone: it drifted every time the file grew,
   and two function names cannot.
4. **Generation fencing on every generation-targeted path**
   (`crates/service/src/v1/registry.rs`). The one historical bypass, `current_actor`, had zero
   callers and was deleted precisely so that a future caller is forced to think about the fence.
5. **Close returns success only on proven process-boundary reaping**
   (`crates/rmux/src/process_boundary.rs`: `getsid` capture, transitive ppid fixpoint, sticky escape
   flag, birth-token recycle fence at `:363`/`:436`/`:411`, `Ok(!escaped)` only on an empty member
   set). An rmux kill acknowledgement is never cleanup proof; an observed escape permanently
   invalidates positive proof.
6. **The admitted compatibility set is exactly the operator's cells plus `PROMOTED_PROFILES`**
   (`crates/service/src/compatibility.rs`). One cell is promoted today — Claude Code 2.1.220 on
   macos/aarch64, transparent/sdk, `transcript_drain_ms: 1000` — and it exists so Path B is
   reachable without a flag: the registry used to be genuinely empty, which meant Path B worked
   for whoever passed `--tested-claude-profile` and refused for everyone else. Promotion widens
   the door by exactly one identity: a version one patch away, or the same version on another
   platform, is still refused before a Claude process is spawned. The promoted drain is measured,
   not chosen (max post-answer transcript arrival 438 ms over 456 turns in 189 real 2.1.220
   transcripts; 2.28x margin), its receipt is `evidence/promoted-profile-2.1.220-macos-aarch64.json`,
   and `tools/promotion/measure_transcript_drain.py` regenerates it. An operator profile for the
   same identity is searched first and wins.
7. **Owner-only UDS, no TCP, no HTTP, no daemon autostart** (`bin/pmuxd/src`: `umask(0o077)` bind
   guard, chmod 0600 after uid/type recheck, `ensure_private_directory` refusing `mode & 0o077`,
   `remove_if_same_socket` refusing on dev/ino/uid/type change). "No daemon autostart" is currently
   held **by absence of machinery**, not by assertion — see debt row 40.
8. **Memory-only session state.** Zero persistence in `crates/service/src`. Adding a store re-opens
   schema, migration, corruption recovery, and crash consistency — the single largest amount of
   complexity this architecture correctly did not build.
9. **`size_scaling.rs`'s exact-affine assertion stays exact.** Do not weaken it to an upper bound.
10. **The config-root and cwd rules are stated about the INCUMBENT, the RESOLVED resource, and
    CONTAINMENT** (`crates/service/src/native.rs::{admit_bound_resources,claim_reaches,
    admit_config_root,admit_cwd}`). Three successive versions of this rule were written as "if the
    REQUEST looks like X, check the root", and each was open to the next entry path that reached the
    same directory in a different shape — the one that got through named a live minified cell's root
    through `environment.set["CLAUDE_CONFIG_DIR"]` and carried no `config_isolation` at all. A rule
    of the form "if this ROOT is in use by a minified cell, refuse" is closed under entry paths that
    do not exist yet; a rule of the form "if this REQUEST says minified, check the root" is not. Do
    not re-gate either rule on `config_isolation.is_some()` or on `request.cell`: SEEDING is
    conditional on the caller having asked for a private root, ADMISSION is not.

    **And the relation is containment, not identity.** Six leaks were spellings of one directory and
    were closed by deciding on the resource; LEAK 7 was not a spelling. `R/sub` really is a different
    resource from `R`, so every alias-proof identity test answered "no incumbent" correctly and
    admitted eight measured shapes straight into a live cell's private root. The invariant that has
    to hold is **no directory a live minified cell binds may be reachable by any other session, in
    any role, at any depth**, and it is asked with `claude_launch::one_directory_contains_the_other`
    over the full cross-product of the directories both sides bind. Do not narrow it back to the
    matching role, to one direction, or to the incumbent's side alone: a minified applicant nesting
    inside a live ordinary session is the same leak one second later. Do NOT widen it to
    ordinary-versus-ordinary either — nesting is the ordinary shape of a filesystem, and that arm's
    identity answer is what keeps `SeedDisposition::Write` available to a private root sitting under
    a live session's cwd.

11. **A mutation campaign must prove it drove the binary it mutated.** `cargo test -p pseudomux-e2e`
    does **not** rebuild `pmuxd` — it is another package's bin target. MEASURED: a mutation making
    `Pool::commit` refuse EVERY turn was verified against the live MCP wave and **the wave PASSED**
    (1 passed, 0 failed), because it drove the *previous* daemon. **Thirteen green waves, and none of
    them could tell.** The freshness guard reads **cargo's own depinfo beside each binary**; a
    hand-rolled "newer than anything under `crates/`" rule is wrong in the direction that lies,
    because it marks `pmux-rmuxd` stale for an edit it does not link and cannot be cleared by
    rebuilding. Do not replace it with a timestamp heuristic.

12. **A predicate that sums remembered counters is not a census, and a claim about a directory that
    does not exist is not a claim.** Three instances found together with the above:
    `quiesced_census` promised a quiesced pool and summed four counters somebody remembered (now
    `live == idle`); `trees()` and `assert_pool_parent_drained` answered "empty" for a directory
    that **does not exist**, so every teardown claim in two files was satisfiable by looking in the
    wrong place; and a clause in `admissible_here` that no test could fail was deleted rather than
    kept for comfort. This is the same defect class as §10 item 10 and as `docs/path-b.md` §7's two
    refusal-message defects: **a statement whose predicate does not test what the statement says.**

13. **Requested terminal geometry must be the DELIVERED geometry.** MEASURED: every pane rendered
    **24x80** while `bin/pmux/src/cli.rs` requested **24x120**. `TerminalSession::resize` called
    `pane.resize`, which for a single-pane window becomes `resize-pane -x/-y` — a lone pane cannot
    exceed its window, so 120 collapsed to 80 **and the call returned success**. `create` had been
    fixed for this and the resize path was left behind, so every resize after creation was accepted
    and silently clamped. The resize now takes a window handle;
    `crates/service/tests/private_runtime.rs::a_terminal_is_created_at_the_requested_geometry_and_not_the_rmux_default`
    asserts the delivered snapshot against the request, starting from rmux's 24x80 default so the
    clamp is an upper bound the test must grow out of. This is load-bearing beyond cosmetics: every
    minified-cell screen predicate is calibrated against a real pane, so a requested geometry that is
    fiction is a trap for the next calibration.

14. **A private terminal retains NO rmux handle. Every connection it uses is minted per operation**
    (`crates/rmux/src/backend.rs`: `operation_pane`, `write_pane`, `write_window`; the struct holds
    a lazy `Arc<Rmux>` and no `Pane` or `Window`). rmux-sdk binds a handle to its `TransportClient`
    at construction and the poison latch is write-once, so a retained handle is a connection that
    dies permanently on its first aborted request — which is exactly how `paste`, `enter` and
    `interrupt` came to be dead-for-life on a terminal that read perfectly well (§9.11). The write
    mint must stay INSIDE the spawned task and under the FIFO permit:
    `private_abandoned_paste_reaches_the_pane_strictly_before_a_following_interrupt` abandons a
    `paste` on its first poll, and a `connect` awaited on the caller's task would swallow the write
    entirely. Owner: `private_terminal_write_recovers_after_the_sdk_aborts_its_write_transport`.

15. **One physical deadline gets one answer, and which clock expired is the BUDGET's question, not
    the call site's** (`crates/service/src/driver_io.rs`: `InputGateBudget::expiry`, read by
    `gated_snapshot`, `gated_styled_screen`, `paste_once`, `enter_once`). `cap` is
    `min(gate maximum, remaining turn)`, so a fired `tokio::time::timeout` means either "the turn is
    over" (`TurnTimeout`) or "this operation could not be proven inside the gate's own bound"
    (an ambiguity) — and the `Elapsed` it hands back says **nothing** about which, which is why
    somebody has to ask the budget. The reads asked; the two writes did not, so the
    same event reached callers under two different codes depending on nothing observable. On the
    `/clear` path it was not even a race: `DEFAULT_CLEAR_TIMEOUT_MS` and `INPUT_GATE_MAX_DURATION`
    are both 15,000 ms and the deadline is computed first, so the remaining turn binds on **every**
    clear. Any new site that races this budget must ask the same question, and
    `enter_once`'s answer must keep `mark_enter_attempted`: `clear_and_rebind` reads that one key to
    decide whether the bound transcript is suspect, and a deadline answer that dropped it published
    `clear_not_submitted: true` for a `/clear` whose Enter had already gone in.

---

## 11. OPEN QUESTIONS — UNVERIFIED, resolvable only by observation

The two items the design review could not settle by reading. Under D9 an **observation** is
required, and both are answerable inside Gate B's remaining budget — §7.5 says how to count it,
and this sentence said "53 of 100 attempts live" when 15 were (D4/D5).

1. **Rate-limited Claude: `TurnTimeout` or a typed transcript `ApiError`?** (advisory row 44,
   **UNVERIFIED**.) Resolvable by **exactly one Gate B scenario row** — "send one prompt while
   rate-limited; record `TurnResult.stop_reason`" — costing **one** of the remaining attempts.
   - If it returns a typed `ApiError`: nothing to do; the screen arm stays absent.
   - If it returns `TurnTimeout`: the fix is **~10 lines in the transcript path**, not the screen
     path — `crates/claude/src/parser.rs` already parses `isApiErrorMessage` into
     `TerminalOutcome::ApiError` and `crates/service/src/v1/actor.rs` maps it to `Failed`; it is
     simply untyped. That change also gives `ErrorCode::RateLimited` and `EventPayload::RateLimit`
     a real producer.
   - Do **not** "fix" this by reordering `classify_terminal_snapshot` (`driver_io.rs:70-97`).

2. **Wide-character width handling** (advisory row 50). `unicode-width` stays out. Send **one CJK and
   one emoji prompt during Gate B and write down the result.** The exposure is a char/cell mix on a
   required-all-whitespace prefix (`driver_io.rs:172-183`) and fails closed, so a negative result is
   a recorded limitation, not a repair.

3. **No defined per-turn latency target.** §6.4 explains why this stays deliberate. **This item is
   now quantified rather than unknown**: §6.1 measures pmux's own non-drain overhead at **41 ms
   p50** against a zero-latency model, which means a target, when written, is a statement about the
   *drain* and about a real compatibility cell — not about pmux's machinery. It becomes answerable
   once the drain is calibrated in Gate B, and it belongs in `spec.md` at that point rather than
   guessed now.

---

## 12. FORWARD RULES

Adopted in place of the consolidations the design review declined. They apply to new work from now
on; they do not require touching anything that exists.

1. **No new test double — extend `support::TestTerminal`.** (Advisory row 46.) The 13 existing
   `TerminalControl`/`TranscriptSource` doubles stay as they are; a divergent generic double makes a
   test silently vacuous, so growth goes into the shared one.
2. **The receipt records; it does not gate.** (Advisory §2.5 item 1.) Evidence tooling writes down
   what happened; it does not hold veto power over the experiment. **No new hand-typed digest pin,
   no new precondition that refuses to start, no new cell whose subject is the instrument's own
   health.** This one rule reverses every shape error in the evidence architecture.
3. **`crates/service/tests/process_support/actual_daemon.rs` and `full_stack.rs`'s
   `Sandbox`/`DaemonGuard` are a matched pair.** Change one, change the other, in the same commit.
4. **Run the experiment before sealing it.** Any first-ever execution happens outside a sealed
   candidate, where a surprise costs a rerun instead of a checkpoint. This is what the full-stack
   first run got right and what the envelope path got backwards.

---

## 13. Next steps

| # | Step | State | Notes |
|---:|---|---|---|
| 1 | **Repository restructure** | **DONE** | `apps/` → `bin/`, `spec.md`/`TESTING.md`/`DESIGN-DEBT.md` → `docs/`, ledger → `evidence/model-attempt-ledger.ndjson`, `.context/` gitignored |
| 2 | **Performance characterization** | **DONE** | §6.1: full per-turn decomposition against `pmux-test-claude` at zero model latency, 2026-07-27. **Non-drain pmux overhead is 41 ms p50.** Four boundaries recorded as explicitly NOT observable. §11 item 3 is now quantified rather than unknown; no target is defined or gated, and per §6.4 that stays deliberate until Gate B calibrates the drain |
| 3 | **Gate A capture** | **DONE ✅ — receipt superseded (see 3c)** | **75/75, driver exit 0**, receipt `.context/gate-a/receipt-20260727.json` sha256 `303d92a7…`, source digest unchanged across the run, fuzz at spec (50,000 × 3, 0 crashes). Driver `tools/gate-a/run_gate.py`. Took **three captures** — 12/75 → 70/75 → 75/75 — finding five defects, four fixed with regressions (§7.2). **Gate A is closed as a deterministic gate on this host only** (§7.1), and **defect 5 / debt row C8 is not closed by it** (§9.4) |
| 3a | **Agent profiles + typed permission bypass, and Gate A re-run** | **DONE ✅ — receipt superseded (see 3c)** | Shipped: `PermissionMode::DangerouslySkipPermissions` with its single-flag argv special case, the per-turn `dangerous_permission_bypass` warning, client-side agent profiles (`crates/client/src/agent_profile.rs`, `--agent`/`--agent-file`), and the 17-enum `value_enums` manifest pin that closes debt row 34. Gate A re-run **75/75, driver exit 0**, receipt `.context/gate-a/receipt-20260727-agent-profiles.json` sha256 `aeb39a9e…`, digest `32519f39…` over 861 files, unchanged across the run. Took **two attempts** — 73/75 then 75/75 — both failures setup rather than product (§7.1). **The previous receipt is superseded and is invalid as evidence for this tree** — and so, now, is this one. Workspace tests 519 → **544** (**580** at HEAD, §5); matrix 85 → **91** rows, all six additions `COVERED` |
| 3b | **Launch-environment allowlist** | **DONE ✅ — receipt debt paid by 3c** | The inherited snapshot term is now `unknown-means-denied` (§5.1), auth-policy aware, with `set` as the deliberate bypass. Measured blast radius on one real macOS environment: **78 in, 10 kept, 68 dropped, of which only 5 were previously denylisted**. `spec.md` §4.5 carries the new formula, the ordering, and the reason. Matrix 91 → **95** rows (`S-25` `COVERED`; `S-26`, `CLI-13`, `CLI-14` `OPEN-L3`). This row used to read *"the Gate A receipt of record predates this change and does not attest it"*; that stopped being true on 2026-07-27, when `receipt-20260727-env-allowlist.json` (75/75/75/0, `source_unchanged: true`, digest `0c61ae1e…` over 864 files) covered it, and again with the receipt of record in 3c |
| 3c | **Gate A re-capture at the C9 commit — the receipt of record** | **DONE ✅** | **75/75/75/0, `source_unchanged: true`**, receipt `.context/gate-a/receipt-8b59cbf.json` sha256 `db5bacdeaaaeaaacf633c09a290add1933ee66e307eeb12059a38297a1e4e2d3`, digest `47ab4fb4…` over **877 files**, 777,052 ms. Run in a standalone clone, alone; took **four captures** to reach 75/75 and every failure was setup (§7.1). It attests the tree of the commit *"C9: a pre-connect regression hung the gate command instead of failing it"* — **not HEAD**. The digest has moved since, `docs/` alone being enough to move it, and the pre-push fix round also touched `clients/` and `tools/`; §7.1 says how to check the delta rather than assume it. **`.context/` is gitignored, so this receipt does not travel with a push** — regenerate it with `tools/gate-a/run_gate.py` rather than treating its absence as a finding |
| 4 | **Gate B** | **DONE for every envelope-reachable scenario** | Ordinals 44-55 spent live: `persistent` 3 (turn 3 echoed turn 2's digest byte-identically, and that digest reproduces from turn 2's own poem text by independent `shasum`), `resume` 2 (same session id across a full process restart, pre-restart poem and digest recalled exactly), non-ASCII **input** 3 under `--lifecycle hybrid`, `facade` 2 through `require-tested` compatibility, `deadline` 1 which failed as designed with `code=TurnTimeout` -- the PRODUCT bounded itself and the envelope's +30s hard bound never intervened. Ordinal 49 died first on the `stop_hook_summary` SchemaDrift fixed in `2fb7c97`. What the ledger holds now, and what remains, is counted by `phase0.py budget` rather than restated here — this cell said "51 records, ordinals 5-55; ~45 attempts remain" long after both had moved. Still open: **`replay`** (see the coverage row above). |
| 5 | **Gate C** | **blocked — Docker API wedged** | The environment blocker is now the first one: `com.docker.backend` is running and the socket is present, but `/_ping` returns **HTTP 000** and `/version` **times out**, so no container starts. Beyond that the two original hard preconditions stand, in order: the **D6 de-scope** of `tools/linux-docker/source_digest.py` (debt row 24, −1,664 lines) and the **C6 manifest re-projection** from `phase-manifest.json` (debt row C6). Then a reviewed multiarch base-image index digest, which is recorded nowhere today |
| 6 | **After the receipt exists** | **unblocked** | The receipt now exists, satisfying the stated precondition of rows 22/23/39. Debt rows 22/23 (quarantine the 18,744-line deferred harness), row 25 (decompose the full-stack monolith, with row 27 first), rows 28/29/30/31/33 (−414 implementation lines), rows **35/36** (matrix restructure + citation lint). **Row 34 is done** (§9.3) |
| 7 | **Close C8 and C9** | **DONE / in flight** | **C8 is CLOSED** by disposition 3 — the `SIGCHLD`-disposition sensitivity is now an explicitly unsupported boundary, with the claim and the nonclaim written out in §9.4 and the nonclaim registered in `docs/testing.md`. Nothing was repaired and no green run closed it. **C9 took disposition 1** and is being made deterministic in a separate change landing 2026-07-28: the upper wall-clock bound is replaced by a lower bound plus a recorded observation. Neither row is closed by accumulating green runs, and the launcher's 2 s bound must not be widened either (§9.4) |
| 8 | **Exercise `--agent` against real Claude in Gate B** | with 4 | Profiles are pure client-side expansion and are fully deterministic-tested (`CLI-12`, `CL-08`, `CL-09`), so this costs **0 additional attempts**: run the Gate B scenarios through an `agents.json` rather than through flag lists, and the campaign gets a second witness that the expanded DTO is the whole truth about a launch. Do **not** spend an attempt on `dangerously_skip_permissions` itself — its argv and its warning are decided entirely inside pmux (`S-23`, `S-24`) and a live turn observes nothing new |

**One sequencing rule, learned the expensive way:** the envelope should record a passing suite, not
be the precondition for ever running one. Reversing those two steps is what produced a week-long
stall; the full-stack first run reversed it back, and `EXPECTED_CLAUDE_LAUNCHES = 42` held on the
first try precisely because it was checked outside a sealed candidate, where a recount would have
cost nothing.

**The Gate A capture settled it.** A 533-line recorder that continues on failure produced a complete
12/75 diagnosis on its first run and a 75/75 receipt on its third. The 4,279-line envelope, given the
identical tree, would have emitted **nothing** — it raises on the first failing cell, and the first
failing cell was cell 1. Five real defects were found, of which **three were in the gate apparatus
itself** and one (§7.2 defect 3) made the gate structurally unpassable whenever the fuzz phase ran.
None was reachable by reading. Whatever is built next, build the recorder first.
