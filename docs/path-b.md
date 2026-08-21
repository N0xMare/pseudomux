# path-b.md

**This is not the product.** The product contract is [spec.md](spec.md) and the
root README. This file is the pool engineering essay (minified cells, recycle,
isolation). Operators integrating a harness should not start here.


**The Path B pool: a stateless, tool-less Claude Code cell, and the operational policy that runs a
pool of them.** This file was a design specification. **It is now mostly a description of a shipped
thing**, and where it still describes a design the tense says so explicitly.

**Implementation status (2026-08-06; linux flagless 2026-08-20; macos 2.1.238 2026-08-21).** The cell AND the pool are shipped. macos/aarch64 2.1.220..=2.1.238
and linux/x86_64 2.1.227..=2.1.236 are reachable without a flag.

| Shipped | Where |
|---|---|
| `SessionCell::Minified` on `start_session`; `clear_session` as a protocol-v1 method (§3.5, §3.6) | `crates/protocol/src/v1.rs` |
| The clear/rebind boundary and the assert-empty invariant (§3.2) | `crates/service/src/driver_io.rs` |
| The pool machine — classes, idle sets, checkout, recycle, warm floor, TTL sweep, teardown, quarantine retention (§3.1, §6, §7) | `crates/service/src/pool/` |
| The half that touches a child, a TUI, a transcript and the registry (§2.1) | `crates/service/src/stateless.rs` |
| `Request::RunStateless` / `StatelessResult`, and `pmux run` / MCP `run_stateless` in front of it | `crates/service/src/native.rs`, `bin/pmux`, `bin/pmux-mcp` |
| Sticky `Leased` instances and the opt-in loopback Messages facade (`--messages-bind`) | `crates/service/src/pool/`, `bin/pmuxd/src/conversation.rs`, `bin/pmuxd/src/messages_http.rs` |
| Per-cell private config root, containment admission, per-instance cwd (§4, §5) | `crates/service/src/{native.rs,config_isolation.rs,claude_launch.rs}` |
| Two promoted compatibility RANGES — macos/aarch64 2.1.220 through 2.1.238 and linux/x86_64 2.1.227 through 2.1.236 — so a supported host needs no `--tested-claude-profile` (§5.5, §12.4) | `crates/service/src/compatibility.rs`, `evidence/pooled-transcript-drain-macos-aarch64.json`, `evidence/pooled-transcript-drain-linux-x86_64.json`, `evidence/promotion-2.1.238-macos-aarch64.json`, `evidence/promotion-2.1.236-linux-x86_64.json` |

**Still design, and labelled as such in place:** admission by a measured memory budget (§7 — the
budget is a boot assertion, not a runtime gate), the retention/pruning policy of §8 above the two
hard invariants, and the §10 items still marked open.

`spec.md` is normative for product behavior. `current-state.md` is normative for position. This
file is normative for the **Path B pool**: what an instance is, how it is recycled, how many may
exist, what is pruned, and what remains unproven.

Every quantity below is one of three things and is always labelled:

- **MEASURED** — observed against Claude Code 2.1.220, macOS/arm64, on the development host.
- **DERIVED** — arithmetic over MEASURED numbers, shown in full.
- **CHOSEN** — a policy constant an implementer may tune; the reasoning is given so the tuning is
  informed rather than arbitrary.

**RETRACTED** is a fourth label, used below wherever a claim previously carried MEASURED and has
since been disproved. A retracted claim is never deleted. A reader who remembers it needs to see it
struck, and the reason a false measurement was believed is itself the durable finding — see §0.

---

## 0.0 THE PATH B READING ORDER — start here, and what each document is

A new contributor should read rows 1-4 in order and nothing else first. Rows 5-7 are **dated
receipts**: they record what one host measured on one day, they are never edited to stay true, and a
sentence in them written in the present tense means *the present of that date*. Rows 8-9 are
normative documents that contain Path B material and a great deal that is not Path B.

| # | document | status | read it for |
|---|---|---|---|
| 1 | `README.md` | CURRENT | The caller's surface. `pmux run`, the Messages facade, Pi, the model/effort table, and pool sizing. Twenty minutes. |
| 2 | `docs/path-b.md` | CURRENT | This file. What an instance IS (§2), how it is recycled (§3, §6), the private root (§5), and §0 — the probe rule, which is the most reusable thing here. |
| 3 | `docs/path-b-adversarial.md` | CURRENT | What a hostile caller can do to a pooled instance, and the three prompt shapes pmux used to admit and could not deliver. Read before touching `validate_prompt` or the composer. |
| 4 | `docs/version-drift.md` | CURRENT | What breaks when Claude Code moves, which constants are version-keyed, and the re-promotion triggers. Read before promoting a version. |
| 5 | `docs/2.1.226-compatibility.md` | DATED RECEIPT | 2026-08-09. The structural compatibility probe of 2.1.226 at 0 ordinals. Its §6 is a defect list and **§6.1 and §6.2 are the only two closed** — §6.3, §6.4 and §6.5 are still live and each carries a STILL OPEN banner saying so. |
| 6 | `docs/2.1.226-acceptance.md` | DATED RECEIPT | 2026-08-09. Ten real turns at 2.1.226. §6 (the SIGTERM window) and §9.1 (the launch bundle) are closed and say so; §9's remaining content is a list of what the session did NOT establish, which is not a defect list and does not close. |
| 7 | `docs/2.1.227-compatibility.md` | DATED RECEIPT | 2026-08-11. The A/B that promoted 2.1.227: every version-keyed instrument run at 2.1.226 and 2.1.227 within one hour, and **not one of them disagreed**. Read §2 for the derived list of version-keyed sites — 44 today, against the 16 the row above derived — and §9 for what one patch step does and does not establish. |
| 8 | `docs/spec.md` | PARTIAL | Normative for product behaviour. §4 operator daemon and allowlist, §5 compatibility, §6 transport. |
| 9 | `docs/current-state.md` | PARTIAL | Normative for position — Path B as a harness engine, Linux operator cell, gate stubs. The 2026-08 essay is `docs/archive/current-state-2026-08.md` and is not a Path B document. |

**The status vocabulary is exactly `CURRENT`, `DATED RECEIPT` and `PARTIAL`**, and this table is not
decoration: `crates/service/tests/path_b_doc_citations.rs` reads it to learn which documents are
Path B documents, and refuses if a row names a file that does not exist or a status outside the
three. A `CURRENT` or `DATED RECEIPT` row is a document whose every `path:line` citation that names
an identifier is checked against that identifier's real line, every `path_b_doc_citations` / `tools/dev/check.sh` run. A `PARTIAL` row
is not, and the reason is scope, not confidence — see §0.4.

---

## 0. How the false claims in this file got here, and the probe rule that follows

**Read this before designing a probe.** Two of this document's own MEASURED claims were false; a
third belief the code depended on was false and was never written down here at all. All three failed
the same way, and the failure is a property of the *probe*, not of the observer.

### 0.1 The confounded probe — the canonical example

§2.2 carried, under the heading *"Rejected, with the measured reason"*:

> | `CLAUDE_CONFIG_DIR` override **alone** | Same auth break. |

**That was FALSE**, and it was believed for months. The probe behind it changed **two variables at
once**: it pointed `CLAUDE_CONFIG_DIR` at a throwaway directory *and* ran with no API key. "Not
logged in" was therefore **over-determined** — both changes independently produce it, and the probe
could not attribute the result to either.

The real mechanism, read out of the 2.1.220 bundle and then confirmed live (§5.1): Claude namespaces
the **keychain service name** by `sha256(config_dir)[0:8]`, so a fresh root looks up a keychain item
that does not exist. `CLAUDE_SECURESTORAGE_CONFIG_DIR=""` pins the service name back to the
un-suffixed one, and the isolated child then authenticates against exactly the operator's own
credential. MEASURED on this host:

    CLAUDE_CONFIG_DIR=$T claude auth status                                  -> loggedIn:false
    CLAUDE_CONFIG_DIR=$T CLAUDE_SECURESTORAGE_CONFIG_DIR= claude auth status -> loggedIn:true, max

**What the false entry cost.** Per-cell private configuration roots looked *impossible*, so the
design was built around the wrong conclusion for months. What can be stated without inference: the
private root landed **after** admission had already been written, and every one of the nine
isolation leaks §5.6 records is a rule about a directory that admission had to learn afterwards.
Whether an earlier root would have prevented any specific one of them is not established here and is
not claimed.

### 0.2 The same shape twice more

- **"Every Path B session currently spawns MCP servers silently"** (§10, item E3). **RETRACTED.** It
  was an inference from *"pmux cannot pass `--strict-mcp-config`"*, never an observation of a
  process. A complete descendant inventory of the live `claude` PID, sampled every 50 ms across four
  cells, is exactly `security find-generic-password` and `caffeinate -i -t 300`. No node, no python,
  no npx, in any configuration. The private config root killed it, and a poisoned `.mcp.json` in the
  cwd blocks on an approval modal — a hang, not a covert spawn.

  **AND THIS BULLET IS ITSELF THE THIRD INSTANCE OF THE SHAPE — 2026-08-09.** The measurement above
  is real and stands. What was built on it did not follow: §2.2's row went on to retract
  `--strict-mcp-config` as "NO LONGER LOAD-BEARING" *because* no server process spawns. **An
  account-level remote MCP connector is an HTTP endpoint. It spawns nothing, so a
  descendant-process inventory is structurally incapable of observing one** — rule 5 of §0.3 read in
  reverse: the instrument would never have shown a presence, so the absence was not evidence about
  it. MEASURED at 2.1.226 in a pristine private root with an empty `.claude.json`, from the child's
  own `--debug-file`: `[claudeai-mcp] Fetching from `
  `https://api.anthropic.com/v1/mcp_servers?limit=1000` and `[mcp-registry] Loaded 294 official MCP
  URLs`, on a cell with `--disallowedTools '*'` and no `--mcp-config`. The flag is now passed
  (`claude_launch.rs::MINIFIED_CELL_FLAGS`) and the same log falls to `MCP configs resolved in 0ms`
  with nothing fetched. §2.1 and §2.2 are corrected.
- **"The `/clear` command menu is alphabetical."** Never written down here, but it was assumed by
  the code and by two reviews. MEASURED: the menu runs an undocumented fuzzy score that matches
  **descriptions** as well as names. At prefix `/c` the selected entry is `/cd` ("Move this session
  to a new working directory"); `/doctor` is a candidate at `/cl` because its description contains
  "Claude". The highlight is rendered in **foreground colour alone** (`fg=idx153` against `idx246`)
  — no glyph, no reverse video — so it was not merely hard to read, it was **absent from pmux's own
  data** until `terminal_snapshot`'s plain-text read was widened to carry the cell grid.

### 0.3 The rule

**A probe that changes two things establishes nothing about either.** Concretely, before this file
accepts a new MEASURED row:

1. **One variable per probe.** If two must move, run the third cell that moves only the second.
2. **A negative result needs a positive control.** "Not logged in" and "no such process" are the two
   answers this codebase has been fooled by; both are also what a broken harness returns.
3. **Prefer the mechanism to the outcome.** `--bare` (§2.2) is the counter-example that proves the
   rule: the *conclusion* was right and the *probe* was not, and what makes the row trustworthy
   today is that `rf()` is checked at the top of every OAuth accessor in the compiled bundle and
   bare mode deliberately ignores `claudeAiOauth`. Read the bundle; it does not lie about itself.
4. **Say which instrument you read.** `input_tokens` alone is not a turn's input — a 2,709-character
   prompt reported `input=2 cache_creation=1230`. A claim about context must name
   `input_tokens`, `cache_creation_input_tokens` and `cache_read_input_tokens` or it is not a claim
   about context.
5. **An absence is only evidence if something would have shown a presence.** This is the same rule
   §5.6's rule 2 states for the filesystem (`stat` reporting `NotFound` for a `..` spelling proves
   nothing) and §2.2 states for `stderr`: **pmux never reads the child's stderr**, so "the CLI did
   not complain" is not an observation pmux is capable of making.

### 0.4 The citation rule, and the fourth false claim — one this file caused

§0.1 and §0.2 are about probes. This one is about the *sentence*, and it is the one that cost the
most: a claim in this document became a product defect, by a mechanism nothing in §0.3 catches.

Line 187 of this file used to be the `--strict-mcp-config` row of §2.2, carrying the retraction §2.2
now retracts. Three product source files cited that line by number. Then `20bf20f` — the commit that
*fixed* the underlying defect — added rows above it, and line 187 became the replace-mode system
prompt row. **All four `docs/path-b.md:<line>` citations in the code tree pointed at the wrong claim
the moment the fix landed, and one of them was written by that same commit.** A reader following
`claude_launch.rs`'s citation to line 187 of this file would have found a paragraph about system
prompts where the sentence promised a measurement about MCP.

Line numbers are correct exactly once. The repair is not to renumber them:

1. **A Path B document is cited by SECTION, never by line — by product source and by another Path B
   document alike.** §2.2 is stable under insertion; this file's line 187 is not.
   `crates/service/tests/path_b_doc_citations.rs` refuses a `<path-b document>:<digits>` anywhere in
   a code tree or in one of these documents — in **any spelling a reader resolves identically**,
   matched by path-component suffix, because for two commits the ban searched for each document's
   fully-qualified path as a literal and the one live instance in the tree was written bare, in the
   sentence above, in the section that forbids it. It also checks that every section a comment names
   is a heading the document really has.
2. **EVERY `path:line` citation inside a Path B document must land on a line that holds something
   the sentence names.** Cite the thing you name: if the sentence says `FORBIDDEN_DRIVER_FLAGS`, the
   number is that constant's line, and a member of it is reached by naming the member. The same test
   checks this for every document row 1-6 of §0.0 marks `CURRENT` or `DATED RECEIPT`. **A citation
   that names nothing checkable is REFUSED, not skipped** — which is the whole of the difference
   between this rule and the one it replaced, and it is worth stating why. The rule used to grade a
   citation only when the sentence named an identifier the cited file holds, and to pass over the
   rest in silence: a table row that gives a path, a number and an English paraphrase has nothing a
   predicate can hold to. That was 70 of 132 citations, under a heading that said *every*. A
   citation that escapes the checker is worth less than no citation at all, because a reader takes a
   `path:line` in this repository to be one the build verifies.
3. **"Names" is four spellings and two kinds, and none of them is a list.** A quotation is marked
   with backticks, straight quotes, typographic quotes or markdown emphasis; what is inside it is an
   anchor either as an identifier the file holds — one it holds few enough times that landing on it
   says something — or as a phrase that occurs in the file verbatim, compared after both sides are
   read past comment markers, line wrapping and emphasis. The second kind is what makes a citation
   of a **comment** checkable, and a MEASURED comment is what half of these documents cite.
4. **No rule can be satisfied by editing a number.** All are derived: the document set comes from
   §0.0's table, the anchor comes from the sentence, and the line comes from the file.

This is the same rule a since-deleted Phase 0 verifier printed — *"a citation nobody re-measures has already
rotted"* — applied to the documents instead of to a banner. That tool computed its numbers from
anchors at import; a markdown file cannot, so the check is external and the anchor is the identifier
the prose was already naming.

**Measured before the rule existed**, by the predicate that now enforces it, run against the tree at
`b3c02d3` — counting only citations naming an identifier the cited file actually holds, which is the
subset where "wrong line" is decidable:

| document | rotted / gradable | rate |
|---|---:|---:|
| `docs/path-b.md` | 5 / 6 | **83%** |
| `docs/version-drift.md` | 5 / 7 | **71%** |
| `docs/2.1.226-compatibility.md` | 7 / 13 | **54%** |
| `docs/2.1.226-acceptance.md` | 3 / 6 | **50%** |
| `docs/path-b-adversarial.md` | 2 / 5 | **40%** |
| **all six** (`README.md` carries no `path:line` citation at all) | **22 / 37** | **59%** |

**Zero paths rotted and zero citations pointed past a file's end.** Every one of the 22 was the right
file and the wrong line, which is the failure mode a reader cannot detect — the file opens, the code
looks plausible, and nothing announces that the sentence and the line have parted company. It is
also why "the path resolves" was never evidence of anything. `docs/path-b-adversarial.md`, four days
old, was already 40%.

At this commit the same predicate reports **0 of 40**. The rate is not the finding; the finding is
that it was never going to stay at zero without something that recomputes it.

**Measured again when rule 2 became total**, over the whole 130 rather than the gradable subset: 60
of the 130 citations in the six linted documents failed it — 55 that named nothing a line could be
checked against, 5 abbreviated to a basename two or more scanned files share, and the rest rotted
outright. Nine of those were rotted rather than merely unanchored, and three of the nine had been
*invisible* for the same reason the other 55 were skipped: `docs/2.1.226-compatibility.md`'s two
rows for the composer's BOTTOM- and TOP-anchoring measurements pointed 126 lines above them, at a
closing brace and a function parameter in `crates/service/src/driver_io.rs`; `docs/version-drift.md`
pointed 168 lines above the `read_transcript` docstring it quotes in
`tools/promotion/measure_transcript_drain.py`. All 130 are graded now, and the count the run prints
is the coverage.

**Rule 1's scan is the whole repository at the same commit**, which found 47 line citations of a
linted document living in documents the scan had never opened — 37 of them into this file, from
`docs/archive/sandbox-spike.md` and `docs/archive/linux-handoff.md`, and most already pointing at unrelated
paragraphs. That hole had a cost beyond the wrong lines: the §0.4 repair before this one was written
to be *line-count neutral* so those 37 would not all move at once. **A rule that makes the document
it protects un-editable is not protecting it**, and this paragraph could not have been written under
the old one.

---

## 1. The asymmetry this design is resolved against

> **Returning before the work is done is UNACCEPTABLE. Refusing to return is merely bad.**

Path B introduces exactly one new failure mode (§3.4), and the whole design of the rebind, the
assert-empty invariant, and the admission policy exists to keep that mode on the *refuse to return*
side of the asymmetry. Path B's value is **statelessness and horizontal scale, NOT latency.**

**The projection this paragraph used to carry is RETRACTED, and so are BOTH of the numbers that
replaced it.** It read: *"it is projected to save ~200ms against Path A's MEASURED 571ms"*. It was a
**projection**, not a measurement. Its Path A anchor (**571 ms**) never agreed with the **535.5 ms**
§10.1 reported for the same quantity, and neither figure has an argv, a receipt or a commit behind
it anywhere in this repository — so neither could be defended and **neither is quoted here any
more**. They are not reconciled; they are replaced.

**Re-measured on this host, on this tree, 2026-08-06.** One Path A turn through pmux against the
zero-latency driver costs a **median 1,204 ms** server-side over n=60 (1,150 min, 1,242 p90, 1,257
max), of which the transcript drain is not the binding term at any value a real turn owes. The full
method, the per-leg distribution and what would invalidate it are in §10.1 and in
`evidence/turn-latency-double-macos-aarch64.json`; `tools/promotion/measure_turn_latency.py`
regenerates both. **Path B is not faster: 1,955 ms median against Path A's 1,213 ms on the same
clock, on the same daemon, in the same run.**

The clause worth keeping is the last one, and it is untouched by the arithmetic: **there was never a
200 ms saving available to take**, because the graduated drain floor already fires and is already
not the binding constraint. **No guarantee in this document may be traded for latency at all**,
which is a stronger and simpler rule than the one it replaces. Anywhere the design could be made
faster by weakening a check, it is not.

---

## 2. What a Path B instance is

A Path B instance is one `claude` TUI process in a private rmux PTY, launched with no tool surface,
in a private empty working directory, driven as a pure input -> output engine. It is **fungible**:
any instance can serve any Path B turn, because after `/clear` no instance carries any state that
distinguishes it from any other.

### 2.1 The launch bundle, as shipped

**This table is now the argv and environment `launch_request_for` actually produces**
(`crates/service/src/stateless.rs`), not a wish list. An earlier revision listed three flags it
called inexpressible (`--strict-mcp-config`, `--safe-mode`, `--setting-sources user`) as though they
were in the bundle, with §10 item 7 further down calling the bundle "INCOMPLETE and does not work as
written". Both are retired — but not identically, and the difference is the 2026-08-09 correction:
**`--strict-mcp-config` was never inexpressible, was needed after all, and is now emitted**; the
other two are not needed, for measured reasons given below, and the bundle works.

**The argv is no longer stated twice.** The complete flag list lives at
`crates/service/src/v1/minified.rs` and in `measure_transcript_drain.py`'s `MINIFIED_LAUNCH_FLAGS`,
and `stateless::tests::the_documented_minified_launch_bundle_is_the_argv_a_mint_emits` compares both,
element for element and in argv order, against the argv a real mint produces. A flag added to or
removed from the launch now turns those two lists red, which is exactly what did not happen while
three source files each carried their own sentence about it.

| What is delivered | Why it is in the bundle |
|---|---|
| `--disallowedTools "*"` | **MEASURED TOTAL.** Removes tools, subagents **and bundled skills** — not hides them. Denied cell `input_tokens` 182-229 with `cache_creation: 0`; control cell `cache_creation: 29,272`. ~29k tokens of tool surface **absent**. It is also what makes the sidechain guard meaningful: with no tool surface a `Task` subagent is structurally unreachable, so a sidechain row is evidence the denial failed. |
| `--permission-mode dontAsk` | No interactive permission surface to hang on. Nothing is attached to answer a modal. |
| `--strict-mcp-config` | **ADDED 2026-08-09, and MEASURED.** Without it a pristine minified cell at 2.1.226 reaches the CALLER'S ACCOUNT connector list over HTTP — `[claudeai-mcp] Fetching from https://api.anthropic.com/v1/mcp_servers?limit=1000`, plus a 294-entry official registry load, 6 MCP lines in the child's `--debug-file`. With it: **2 lines, `MCP configs resolved in 0ms`**, no fetch, `state: ready` either way. pmux passes no `--mcp-config`, so "only servers from `--mcp-config`" means none. §2.2 records why the earlier retraction did not follow from its own measurement. |
| `--model <class>` / `--effort <class>` | Launch-time argv, rendered from the **same** `ResolvedModelEffort` that produced the pool's class key, so the pool's model of a process cannot drift from the process (§9). |
| Replace-mode system prompt (`SystemPromptPolicy::Replace`) | REPLACE versus append for the agent-prompt *file*, and MEASURED to survive `/clear`. It is not the entire API `system` array: Claude Code still prepends its identity line. The **wording** is CHOSEN — see §2.3. |
| `SessionIdentity::New { session_id: None }` | **pmux picks the id.** An earlier revision said `--session-id <fresh uuid>`; that was the design. A caller-chosen id is one of the two ways a transcript that already served work gets admitted as a fresh cell, and the pool has no caller to take one from. |
| `<per-instance pre-trusted empty cwd>` | §4. |
| `config_isolation` (pmux-owned `CLAUDE_CONFIG_DIR` + service-computed securestorage pin) | §5. |
| `CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL=1` | §2.3. MEASURED necessary. |
| `RequireTested` + `cell: minified` | A minified cell runs only on a compatibility cell whose composer geometry has been measured (§5.5). |
| Empty caller environment; the DAEMON's own environment as the snapshot | An empty environment was tried first and MEASURED to fail: with no `HOME` and no `PATH` the first turn returned `needs_login`. The daemon's environment is daemon configuration in the same sense `--path-b-claude` is; nothing on the wire can put a byte in it, and it still passes the allowlist, the subscription-auth removals and the transparent denylist. |

### 2.2 Rejected and disqualified, with the reason and the instrument

Recording the reason is the point; a flag rejected without a reason gets re-proposed. The
**instrument** column is here because §0: a row whose instrument is "an inference" is not a
measurement.

| Rejected | Standing | Reason, and how it is known |
|---|---|---|
| `--bare` | **STILL TRUE, re-grounded** | Breaks subscription auth. The original probe was the confounded one of §0.1 and proved nothing; the conclusion survives on **bundle evidence**, which is stronger: `rf()` is checked at the top of every OAuth accessor and bare mode deliberately ignores `claudeAiOauth`. Right answer, wrong probe — do not cite the old probe. |
| `CLAUDE_CONFIG_DIR` override **alone** | **RETRACTED — the row was FALSE** | It read "Same auth break", a MEASURED claim from the confounded probe of §0.1. A private root works, and §5 is built on it. What is true is narrower: a config root **without** the securestorage pin gets a login screen, because the keychain service name is namespaced by `sha256(config_dir)[0:8]`. pmux computes the pin itself, so a caller never has to. |
| `--max-turns` | ~~STILL TRUE~~ **FALSE, and not because of 2.1.226 — MEASURED 2026-08-09.** The row read *"Does not exist in 2.1.220."* | **The flag exists and is parsed**, at 2.1.226 AND at 2.1.223. Non-executing sentinel probe (§0.3 rule 5's instrument, from `docs/2.1.226-compatibility.md` §1.1): `claude --max-turns 5 --pmux-probe-sentinel doctor` reports `unknown option '--pmux-probe-sentinel'` at both versions, i.e. `--max-turns` was accepted; the control `--definitely-not-a-flag` and the three near-misses `--max-turn`, `--max-turnss`, `--maxturns` each name themselves. It is a HIDDEN option — absent from `--help` at 2.1.226, exactly like `--system-prompt-file`. **Corroborated without a probe at all:** the Claude Code process that hosted the session which found this was itself launched with `--max-turns 1000` on the same 2.1.226 binary, so the flag is not merely parsed, it is in use on this host. **Why nobody caught it:** the 36-flag sweep's set is DERIVED from what pmux emits or forbids (`MINIFIED_LAUNCH_FLAGS`, `SAFE_EXTRA_FLAGS`, `FORBIDDEN_DRIVER_FLAGS`), and a flag this document merely *rejected in prose* is in none of them, so the one instrument that re-checks flag existence every version is structurally blind to every row in this table. **Blast radius is zero and that was checked, not assumed:** pmux does not pass it, and `validate_extra_args` (`crates/service/src/claude_launch.rs:179`) allowlists caller `extra_args` to `SAFE_EXTRA_FLAGS` — two spellings — so a caller cannot reach it either. It is absent from `FORBIDDEN_DRIVER_FLAGS` and does not need to be there. but the RECORDED REASON is false, and a reader deciding whether to propose it would be told it cannot be had. |
| `--tools ""` | STILL TRUE, one clause weakened | Cannot travel `push_value`, which bails on empty values by design. The clause "leaves 85 MCP tools" is a claim about a build with MCP servers configured; under the private root of §5 there are none (§0.2), so it is the `push_value` half that is load-bearing. |
| `--disable-slash-commands` | STILL TRUE | Would remove `/clear`, the entire statelessness mechanism. The caller-facing `/` escape is already closed in `driver_io.rs::validate_prompt` (mirrored in `bin/pmux/src/cli.rs`), so this flag buys nothing and costs the design. |
| `--no-session-persistence` | STILL TRUE, **and now enforced** | Inert in the TUI today, but would DELETE the transcript if ever honoured. It **is** in `FORBIDDEN_DRIVER_FLAGS` (`FORBIDDEN_DRIVER_FLAGS`, `crates/service/src/claude_launch.rs:32-50`, whose sole entry is that flag) — the §11 row that tracked this is closed. |
| `--strict-mcp-config` | ~~**NO LONGER LOAD-BEARING**~~ **THE RETRACTION IS RETRACTED — 2026-08-09. It is in the bundle (§2.1).** | The row read: *"MEASURED: no MCP server process is spawned in any configuration (§0.2), so there is nothing for it to stop."* **The measurement was right and the conclusion did not follow.** The predicate was a descendant-process inventory; an account-level remote connector is an HTTP endpoint that spawns no process, so that instrument could never have observed the case the row was about. MEASURED at 2.1.226, one variable moved, from the child's own `--debug-file`: **6 MCP lines including a fetch of `https://api.anthropic.com/v1/mcp_servers?limit=1000` without the flag, 2 lines and `resolved in 0ms` with it.** It was never inexpressible either — it is driver-owned argv, not a protocol field, and `MINIFIED_CELL_FLAGS` now emits it for every `cell: minified` launch on both paths. |
| `--safe-mode` (flag) | **REMOVED FROM THE BUNDLE, and NOT because it was measured harmful. Re-weighed 2026-08-09 and still out.** | It was in the bundle for "no CLAUDE.md, skills, plugins, or hooks", which the private root of §5 and `--disallowedTools "*"` already deliver — and `docs/2.1.226-compatibility.md` §4.2 measured user-scope skill discovery landing on the private root, with the operator's 77 `smithers-*` skills absent. **The flag and the env var are NOT independent, and this row used to imply they were**: 2.1.226's help says the flag *"Sets `CLAUDE_CODE_SAFE_MODE=1`"* in as many words. Both measurements stand — `CLAUDE_CODE_SAFE_MODE` in the LAUNCH ENVIRONMENT was MEASURED to BREAK the cell 5/5 (§2.3), and the flag was probed at §13 item 3 and breaks nothing — so what is measured is that the variable is fatal when pmux puts it in the child's environment and inert when the child sets it for itself after argv parsing. That is a real distinction and it is not the one the row asserted; "do not confuse them" was a sentence, not a finding. What is *still* unprobed is the only thing that would decide it: 2.1.226's help says it also disables "custom themes, keybindings, and more", and every screen constant Path B's fast path trusts was measured without it. §13 item 3's probe covered `ready` and one answered token; it covered no `/clear`. Adding an unneeded flag that moves an uncalibrated input is the trade this file exists to refuse. **What changed is that three source files no longer claim pmux passes it** — `minified.rs`, `measure_transcript_drain.py` and `tools/phase0/README.md`. |
| `--setting-sources user` | STILL NOT NEEDED — **but the recorded reason covered one source of three. Re-grounded 2026-08-09.** | The row read: *"Under a private root pmux seeds `<root>/settings.json` itself (§5.2), so the resolution is pinned by construction rather than by a flag."* 2.1.226's help names the sources: *"Comma-separated list of setting sources to load (**user, project, local**)"*. The private root pins the **user** source and says nothing about the other two — `project` and `local` resolve from the **cwd**, not from the config root, so that predicate could not have observed either. The conclusion survives on a DIFFERENT mechanism: §4's per-instance cwd is empty and pre-trusted, so there is no `.claude/settings.json` and no local override to load. That makes it contingent on §4, not on §5 — if a future change ever gave a cell a non-empty cwd, this row would be wrong and §5 would still be true. **A fourth source is outside both:** admin-managed (policy) settings, which 2.1.226's own `--safe-mode` help says *"still apply"*. They are outside the private root, outside this flag's vocabulary, and outside pmux — which is why the promotion tool's check 6 stays a refusal rather than a pass. |
| `--effort <unknown spelling>` | **RETRACTED as a CLI-level rejection, and CORRECTED again 2026-08-06** | It was believed that an unknown effort spelling is refused by the CLI. **MEASURED FALSE**: an unknown spelling does not fail — it warns on **stderr** and silently uses the default, **and pmux never reads the child's stderr**. Verbatim, 2.1.220: `Warning: Unknown --effort value 'nonsense-value' — ignoring it and using the default effort. Valid values: low, medium, high, xhigh, max.`, exit 0. **The correction: `ultracode` is NOT an unknown spelling and this row used to say it was.** Read directly from the child's stderr outside pmux, `--effort ultracode` produces **no warning at all** — it is a RECOGNISED value the warning's own list omits. Six spellings are accepted silently (`low`, `medium`, `high`, `xhigh`, `max`, `ultracode`) and the match is **case-insensitive** (`Low` and `ULTRACODE` are accepted too); everything else warns. pmux removed the extra variant on product grounds, not because the CLI refuses it, and the guard that enumerates the admitted spellings is now derived from the enum through `every_variant!` so a new variant cannot be invisible to it. |
| model/effort pair validation in pmux | **RETRACTED as a CLI-level fact** | **MEASURED: the CLI does not enforce model/effort pairs.** `haiku-4-5/xhigh` and `sonnet-4-6/max` both ran successfully with the requested model. The API-level pairing table is true of `output_config.effort`, not of the CLI path pmux drives. Any pair table pmux enforces is therefore **pmux's own product policy** and must be labelled CHOSEN, never justified as "the CLI would refuse it". |

#### 2.2.1 The retraction audit — every standing above, re-asked against its own predicate

**2026-08-09.** `--strict-mcp-config` cost a live isolation leak, so the whole table was re-read with
one question per row, the question §0.4 exists for: **what predicate established this standing, and
could that predicate have observed the case the sentence is about?** Not "is the claim true" — "is
the instrument capable of seeing it false". **Four** of the eleven rows failed that test; two of the
four still reach the right answer.

| row | predicate it rested on | could the predicate see the case? | outcome |
|---|---|---|---|
| `--bare` | the compiled bundle: `rf()` at the top of every OAuth accessor | **yes** — reading the bundle is direct, and the row already says the original probe was confounded and must not be cited | stands |
| `CLAUDE_CONFIG_DIR` alone | a working private root, §5 built on it | **yes** | stands, already retracted correctly |
| `--max-turns` | *"does not exist in 2.1.220"* — a version-keyed absence | **no.** Nothing re-checks it: the flag sweep's set is derived from what pmux emits or forbids, and this flag is in neither | **FALSE.** The flag is parsed at 2.1.223 and 2.1.226 |
| `--tools ""` | `push_value` bails on empty values | **yes** — a property of pmux's own argv builder | stands |
| `--disable-slash-commands` | it would remove `/clear` | **yes** — that is what the design needs | stands |
| `--no-session-persistence` | present in `FORBIDDEN_DRIVER_FLAGS` | **yes** — checked by a test | stands |
| `--strict-mcp-config` | a descendant-process inventory | **no.** A remote connector is an HTTP endpoint and spawns no process | **retraction retracted**; the flag ships |
| `--safe-mode` | flag ≠ env var | **no** — 2.1.226's help says the flag SETS the var | decision unchanged, **reason corrected** |
| `--setting-sources user` | pmux seeds the private root's `settings.json` | **no** — that is the `user` source; `project` and `local` come from the cwd | conclusion survives on §4's empty cwd, **not** on §5 |
| `--effort <unknown>` | the child's stderr, read outside pmux | **yes**, and the row says why pmux itself cannot make that observation | stands |
| model/effort pairs | five live turns with the requested model | **yes** | stands |

**Four of eleven rows rested on a predicate that could not observe their own case, and two of the
four had a wrong conclusion** — `--max-turns`, whose standing was simply false, and
`--strict-mcp-config`, whose retraction cost the leak. The other two, `--safe-mode` and
`--setting-sources user`, reach the right answer for a reason that does not hold. That ratio is the
point. A standing whose reason is unsound is
already a defect even while the answer is right, because the next reader reuses the reason: §2.2's
`--strict-mcp-config` row had a *correct measurement* attached to an unsound inference for weeks, and
what shipped the leak was somebody trusting the inference.

**The gap this audit found in the instruments, not in the rows.** `docs/2.1.226-compatibility.md`
§1.2 derives its flag set from `claude_launch.rs` and `sensitive_launch.rs` — which is right for
"does pmux's argv still parse" and is **structurally blind to this entire table**, because a flag
pmux rejected in prose appears in no Rust literal. Every row above whose reason is a claim about
Claude's CLI is therefore un-re-measured by default, and `--max-turns` is what that looks like after
six patch versions. Naming it is all that is done here; deriving the probe set from this table too is
a change with its own proof, not a paragraph.

### 2.3 The environment a minified cell is delivered, and the four names deliberately absent

**MEASURED, not reasoned** (`claude_launch.rs::MINIFIED_CELL_ENVIRONMENT`).

Every private configuration root pmux seeds downloads the **official plugin marketplace** from GCS on
first launch: **428 files, 6.2 MB, 39 plugin directories, 31 `SKILL.md` files and 8+ third-party
`.mcp.json`**, starting **11 s after launch** and finishing 53 s before the cell's first turn. A cell
whose whole claim is that it carries nothing from the caller before it cannot also carry a
third-party plugin tree it did not ask for, and the download is a **network dependency sitting
inside the readiness window**. It also puts 428 files into a root §5.6 requires to be pristine.

`CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL=1` suppresses it — **verified: no `plugins/`
directory appears** — with the cell still passing.

**The four names deliberately ABSENT are why this is a table and not a prefix.**
`CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC`, `DISABLE_TELEMETRY`, `DO_NOT_TRACK` and
`CLAUDE_CODE_SAFE_MODE` were each MEASURED to **BREAK the cell 5/5**, by rendering a persistent
notice that changes the screen shape and fails startup. Suppressing traffic is not the goal;
delivering an instance nothing distinguishes from any other is, and **a notice is a distinguishing
mark**. Anyone adding one of these back is trading the cell for a preference.

It is applied **after** the terminal profile's denylist, for the same reason step 6 is: every name
here is `CLAUDE_CODE_*`, and a future `CLAUDE_` prefix entry in `TRANSPARENT_PREFIXES` would
otherwise silently strip it and quietly restore the download. It is applied only for
`cell: minified` — an ordinary caller's plugins are the caller's business.

**The system prompt is CHOSEN, and must never be dressed as measured.** `DEFAULT_SYSTEM_PROMPT`
(`crates/service/src/pool/config.rs`) is verbatim:

> The user message is the entire instruction.

It displaces Claude Code's default agent prompt. It is not consumer policy (that lives in the typed
user message). Nothing measured that wording; `evidence/linux-minified-system-remainder-2.1.236-x86_64.json`
measures leftover *tokens* after REPLACE (chars/4 lower bound; after `/clear` on linux 2.1.236 the
bound is hundreds, not the 29k tool surface), not whether this sentence is a good sentence. What *is*
enforced is the bound: **512 bytes, CHOSEN**, refused at boot if empty, deliberately a byte bound
rather than a sentence counter, because a sentence counter rejects a correct prompt containing "e.g."
A future reviewer may change the wording freely and owes no measurement for it; they owe a
re-measurement of nothing, because the fingerprint of the configured prompt is an instance invariant
and a change simply invalidates every idle instance.

**What REPLACE does not own.** A live `/v1/messages` intercept of a minified TUI cell
(`evidence/linux-minified-system-body-2.1.236-x86_64.json`) shows the armed Sonnet turn's `system`
array is three text blocks: (1) `x-anthropic-billing-header:…` (Claude Code OAuth/client
attribution; first-party Anthropic is reported to strip it before the model), (2) `You are Claude
Code, Anthropic's official CLI for Claude.` (hardcoded for interactive sessions in 2.1.236; not the
REPLACE file), (3) the displacer. The first user message is prefixed with a `<system-reminder>`
carrying `# userEmail` and `# currentDate`. A further `messages[].role=system` part is the
`<total_tokens>` reminder. Tools, `CLAUDE.md`, git, and cwd were absent. `--exclude-dynamic-system-prompt-sections`
is ignored with `--system-prompt` (Claude's own `--help`). `--bare` is still refused: it sets
`CLAUDE_CODE_SIMPLE=1` and ignores OAuth. Consumer policy stays in the typed message / Messages
flatten. Do not lengthen the displacer to argue with block (2).

---

## 3. Instance lifecycle

    launch ──▶ ASSERT-EMPTY ──▶ IDLE ──▶ checkout ──▶ serve turn ──▶ deliver result
                    ▲                                                      │
                    │                                                      ▼
                    └────────── rebind ◀────────── /clear ◀────────────────┘
                                                       │ (recycle predicate, §5)
                                                       ▼
                                                   teardown ──▶ launch

An instance is **fresh two ways**: newly launched, or cleared. The assert-empty invariant (§3.2) is
written so that one predicate covers both — that is a requirement of the design, not a coincidence,
because an implementer who writes two predicates will eventually let them drift.

### 3.1 Checkout

Checkout removes an instance from the idle set and binds it to exactly one turn. An instance is
checkout-eligible only if it is IDLE **and** its assert-empty check passed **and** it is not past a
recycle predicate (§5). Checkout is a pure pool-bookkeeping operation: it performs no I/O against
the TUI, because any I/O here would be latency on the critical path with no guarantee bought.

**How that is not in tension with §3.2, and what SUPERSEDED the answer this section used to give.**
This section described an `Option<EmptinessProof>` carried on each instance, set when assert-empty
passes and re-checked at checkout. **The shipped machine is stronger and simpler: MEMBERSHIP IN THE
IDLE SET *IS* THE EMPTINESS PROOF.** `Idle` has exactly two inbound edges, `WarmProven` and
`ClearProven`, both proof-carrying; every other outcome quarantines. There is **no cached proof
re-checked at checkout**, because there is no state in which an instance is idle and unproven. The
implication is asserted over the **edge set** rather than over the two edges the author had in mind,
and `Instance::check_invariants` refuses an idle instance whose last transition carried no proof,
whose turns reached the cap, or whose system-prompt fingerprint no longer matches configuration
(`crates/service/src/pool/`). Checkout is still zero-I/O, so §3.1's no-I/O rule and §3.2's checkout
precondition both still hold — by a structural argument instead of a bookkeeping one.

Two defects were found in that machine by asking what a second task sees between locks, and both are
worth carrying forward as shapes rather than as history: the idle set used to be tidied by whoever
caused a departure and **two of four callers did not**, so a caller arriving mid-sweep found a slot
it could not check out and got `Internal` (unpublishing now happens inside `transition_locked`, the
one place a state can change); and `transition_locked` used to mutate and validate *afterwards*, so
a transition its own invariant rejected left the instance half-applied — neither serviceable nor
destroyable. It now builds a candidate, validates that, and commits only on success.

### 3.2 The assert-empty invariant

**Statement.** Every row of the transcript file the instance is bound to is *individually* proven
inert. `is_admitted_on_active_chain` is **one clause** of that predicate, not the whole of it.

An earlier revision of this section stated the invariant as "zero rows that
`is_admitted_on_active_chain` would admit", and that statement was wrong in three ways. It is
type-incorrect: `is_admitted_on_active_chain` is a method on `SystemRow`, so `Assistant`,
`TypedUser`, `UserToolResults` and `Attachment` are outside its domain and read as *pass*. It is
vacuous in the wrong direction: a transcript holding a complete prior exchange but no
`turn_duration` row -- a cancelled turn, an interrupted turn, a truncated write -- contains zero rows
it admits, so it would pass a file full of a prior caller's context. And it does not catch the
threat this check exists for: a mis-selected slash command writes the *same* `local_command` shape
`/clear` does, which that predicate admits in neither case, so it reports PASS for `/model`,
`/compact` or `/resume` alike.

The implemented predicate is structured over `RowKind` so a future variant is a compile error rather
than a silent pass (`crates/service/src/driver_io.rs`, `prove_transcript_inert`):

| `RowKind` | Disposition | Refusal reason |
|---|---|---|
| `Metadata { .. }` in the preamble allowlist | ACCEPT -- see below | — |
| `Metadata { .. }`, any other record type | REFUSE -- reject-by-default | `unexpected_metadata_record` |
| `System` admitted on the active chain | REFUSE -- a turn ended, or one is in flight | `turn_marker_present` |
| `System` with subtype `local_command` | ACCEPT | — |
| `System`, any other subtype | REFUSE -- reject-by-default; catches `compact_boundary` | `unexpected_system_subtype` |
| `UserOther` | CONDITIONAL -- see below | `unexpected_user_row` |
| `TypedUser` / `Assistant` / `UserToolResults` / `Attachment` | REFUSE -- leakage | `semantic_row_present` |
| `Unknown { .. }` | REFUSE -- `JsonlParser` returns this without erroring even in Strict mode | `unknown_row` |

**CORRECTION (2026-08-03): the metadata row used to read ACCEPT, unconditionally, and that was a
leak.** "Excluded from the active graph by construction" is a statement about what the completion
engine *reads*, not about what a row *carries*. `is_metadata_record`
(`crates/claude/src/parser.rs`) also covers `queue-operation` -- which is **queued user input**, and
MEASURED carries its `content` in 1,076 of 2,133 rows across 231 transcripts on the development host
(this repo's own post-`turn_duration` census counts 7 of them, `docs/archive/current-state-2026-08.md`) -- as well as
`ai-title` and `summary`, both of which carry text derived from a prior conversation. A clear
returned `rotated: true` over a preamble carrying all three. Metadata was the one accept-by-default
arm of an otherwise reject-by-default predicate, and it was the arm carrying text.

The allowlist is exactly the record types a real preamble contains, MEASURED:

| record type | Where it is measured | Identity |
|---|---|---|
| `mode` | row 0 of both preambles | `sessionId` required |
| `permission-mode` | launch preamble | `sessionId` required |
| `bridge-session` | launch preamble | `sessionId` required |
| `file-history-snapshot` | both preambles | none -- MEASURED absent on 289 of 289 rows |
| `last-prompt` | trailing row of a cleared preamble, `lastPrompt: null` | `sessionId` required |

Anything else -- `summary`, `ai-title`, `queue-operation`, `progress`, `pr-link`, and any record type
a future Claude adds -- is `unexpected_metadata_record`, naming the offending record type as a
bounded schema token. `last-prompt` additionally requires `lastPrompt` to be absent or null
(`metadata_prompt_present` otherwise): MEASURED it is null in the one post-`/clear` transcript of the
61-file corpus that served no work, and carries the prompt text verbatim in 2,337 of 2,365 rows
everywhere else. The row is admitted for what it says, not for its type.

**Identity applies to metadata here.** MEASURED over 231 transcripts: the four stamped record types
carry `sessionId` on 100% of 7,222 rows and it equals the transcript's own id on every one; no
metadata record type carries `cwd` at all. So a preamble row stamped with a *foreign* session id is
not drift to be tolerated -- it is the file saying it belongs to someone else. The turn path keeps
its own exemption deliberately: mid-turn, metadata is dropped from the analysis graph before it can
contribute to any completion proof (`ParsedRow::is_analysis_changing`), so a foreign-stamped metadata
row there changes no answer, while here the answer being given *is* about the file's contents.

**Stated limit.** `file-history-snapshot` is the one allowlisted row that proves nothing about whose
transcript it is, because MEASURED it carries no identity field to check. It is admitted on its
record type alone. What bounds the exposure is that it is one row inside a 16-row / 64 KiB budget in
a file whose *other* rows are all identity-checked, and that the locator has already corroborated
the file's id and cwd from its own rows before any of this runs. If a future Claude begins stamping
it, the check applies automatically -- a present `sessionId` is validated whether or not the record
type is one that is required to carry one.

Plus: a strict parse (a parse error is a refusal, not a skip), `validate_semantic_identity` against
the bound session id and the instance cwd for every identity-bound row kind, and a budget of 16 rows
/ 64 KiB checked before the parse so a leaked transcript is refused cheaply (`row_budget_exceeded`).

**The `UserOther` clause, and the composer proof.** At most two `UserOther` rows, each of which must
carry a string `message.content` that either opens with `<local-command-caveat>` or opens with
`<command-name>`. If it carries a `<command-name>` that is **not** `/clear`, the refusal reason is
**`wrong_local_command`** and it names the offending command.

**This clause, not the row-0 anchor, is what catches a mis-selected slash command.** Every observable
the rebind checks is identical whichever command ran: a new file appears, row 0 is a `mode` row, and
the id it carries is new and unseen. Row 3 is Claude's own authoritative record of which command the
fuzzy composer actually executed, read from the only source this codebase trusts, and it costs one
string comparison. See §10 for what it still does not protect against.

**What is read.** The bound transcript file only — the exact path
`<config_root>/projects/<slug(cwd)>/<bound-session-id>.jsonl`, built by
`TranscriptLocator::expected_candidates` (`crates/claude/src/locator.rs:128`) with a dash-collapsed
fallback slug from `collapse_dashes` at `crates/claude/src/locator.rs:136-138`. Nothing else on disk is consulted. No screen scrape is
involved: the screen is a liveness veto in this codebase, never a source of truth
(`docs/archive/current-state-2026-08.md` §2).

**What it therefore CANNOT prove, stated so it is not presented as the invariant.** It proves that
every row of one file is individually inert, and nothing more. It cannot see (i) any other file in
the root — `history.jsonl`, `paste-cache/`, sibling transcripts, `memory/`, all of which are per-ROOT
and are covered instead by §5.6; (ii) process memory — composer contents, the in-memory pending
history queue, or model-side context, which the transcript only proxies for (§10.8); (iii) the
screen, including a composer prefilled by up-arrow recall, whose text is invisible here until it has
already landed in a transcript, at which point the leak has happened; (iv) that the bound file is the
file being written — a hand-typed `/clear` under a writable attach rotates Claude's id underneath
pmux's bound one, after which this attests a file nothing is appending to. Channels (iii) and (iv)
are reachable only through a writable attachment, which §3.5 refuses on this cell. Presenting
assert-empty alone as the statelessness invariant is how `history.jsonl` went unnamed through two
adversarial passes; the invariant is the conjunction listed in §5.6.

**What makes it pass.** Concretely the file will contain only preamble:

- **Newly launched:** the launch preamble — MEASURED `mode`, `permission-mode`, `bridge-session`,
  `file-history-snapshot`. All parse as `RowKind::Metadata`.
- **Cleared:** the 5 rows `/clear` writes (MEASURED: 5 rows into the new file, **0 into the old**;
  a `last-prompt` metadata row follows once nothing else does). An earlier revision described these
  as "one of which is `{"type":"system","subtype":"local_command"}`", which is true but radically
  incomplete and would lead an implementer straight into refusing every clear. MEASURED over 61
  post-`/clear` transcripts in `~/.claude/projects/-private-tmp-clearprobe-cwd`, the five rows are:

  | # | shape | `RowKind` |
  |---|---|---|
  | 0 | `{"type":"mode","mode":"normal","sessionId":NEW}` | `Metadata` |
  | 1 | `{"type":"file-history-snapshot",…}` | `Metadata` |
  | 2 | `type:"user"`, `isMeta:true`, string content `<local-command-caveat>…` | **`UserOther`** |
  | 3 | `type:"user"`, `isSidechain:false`, **main scope**, string content `<command-name>/clear</command-name>…` | **`UserOther`** |
  | 4 | `{"type":"system","subtype":"local_command","content":"<local-command-stdout></local-command-stdout>"}` | `System` |

  **Two of the five are `type:"user"` rows and one of them is main-scope.** Both parse as
  `RowKind::UserOther` rather than `TypedUser`, because neither carries `promptSource:"typed"` and
  `message.content` is a string rather than an array (`crates/claude/src/parser.rs`). A real caller
  prompt *does* carry `promptSource:"typed"` and becomes `TypedUser`, which the table above refuses
  outright -- so `UserOther` is not a hole a prompt can travel through.

  MEASURED preamble size across all 61 files: min 1051 B, median 1890 B, max 1890 B.

  **The row count is five plus one.** The `last-prompt` row lands once nothing else does, so the
  fixture the shipped predicate is pinned against carries **six** rows
  (`driver_io.rs::tests::a_cleared_transcript_carrying_the_measured_preamble_rebinds` asserts
  exactly that). Read "the 5 rows `/clear` writes" as the five it writes *immediately*; a reader who
  budgets for five and refuses a sixth has re-introduced the bug the 16-row budget exists to avoid.

**One predicate, two callers.** The shared body is one function; each caller adds the one extra
boolean it is entitled to assert. The rebind caller requires the `/clear` echo to be present
(`clear_command_missing` otherwise -- a rotated transcript with no echo means something else opened
it). The launch caller requires its absence (`unexpected_clear_echo` -- a launch preamble carrying
one means the id resolution found the wrong file).

**Where the launch caller lives, and why it moved.** `TranscriptSource::assert_empty_at_launch` is a
trait method **whose default REFUSES**, and `SessionRegistry::register` demands it of every
`SessionCell::Minified` registration before an actor exists. It used to be an `if` in
`NativeService::start_session`, which meant the only thing that could reach it was an `#[ignore]`d
end-to-end test that builds real binaries and a real PTY: setting the guard to `if false` left the
entire default suite green. Registration is the boundary *every* route to a minified cell passes
through -- the registry is `pub`, and `SessionRegistry::register` is the only caller of
`SessionActorHandle::spawn_actor` -- and it is reachable from a test with no Claude process. The
require-tested rule lives at the same boundary and immediately before it, which is why the copy that
had been placed inside `SessionActor::spawn` was DELETED: it could not fire, and a guard that cannot
fire is not defence in depth, it is a second statement of a rule that no test can reach and that
keeps reading as enforcement after the reachable one is weakened.

The default refuses because a source that cannot prove the claim has not made it; an `Ok(())` default
would pass by omission for every source written afterwards, including an embedder's. That POLARITY
was undefended until 2026-08-03: replacing the refusing body with `Ok(())` left the whole workspace
green, because the only overriders are `FileTranscriptSource` and the test double and every other
implementor takes the default without ever being registered as minified.
`minified_cell.rs::a_transcript_source_that_cannot_prove_emptiness_may_not_back_a_minified_cell`
registers a source that deliberately does not override it, and fails on the flip.

The question is asked of the FILE, not of the request: `SessionIdentity::Resume` names a transcript
that already holds a prior caller's context, and a caller-chosen `New` id can collide with one.
Absence of the transcript is a pass -- Claude creates it lazily and a session admitted before it
appears has served nothing.

**On failure.** The instance is QUARANTINED, never returned to the idle set, and torn down. It is
not "cleared again and retried": a failed assert-empty means the pool's model of that instance is
wrong, and re-clearing an instance you do not understand is exactly the move that returns a wrong
answer. Refusing the instance costs one instance; trusting it costs a guarantee.

**All three are now implemented.** This paragraph read "the first two are implemented and the third
is not"; teardown on quarantine ships (§12.2), the quarantined instance's tree is retained under
`--path-b-retain-dir` before the slot is released, and a publish refusal following a clear that may
already have been typed is itself a quarantine rather than a stranded slot. An assert-empty refusal on the rebind path
returns `ErrorCode::SchemaDrift` with `violation: "assert_empty_refused"` and a `reason`
discriminator, and the actor's `poison_after_failed_rebind` forces `SessionState::Tainted` even when
the event cannot be published. A Tainted session refuses every turn (`RecoveryFailed`,
non-retryable) and refuses a second clear non-retryably, so "clear it and try once more" is
unrepresentable through the actor.

**Teardown is a POOL OBLIGATION, and it is not free.** `SessionActor::expire_idle` reaps only
`Ready | NeedsInput`, so a Tainted session is **never** auto-reaped: it holds its rmux sidecar and
its Claude process -- MEASURED 486 MB at turn 15 (§7) -- until the caller explicitly calls
`close_session`. For a single held Path A session that is defensible, because an operator may want
to attach and look. For a pool it converts one quarantine into permanent capacity loss. The pool
owner must tear down on quarantine; `expire_idle`'s state gate is deliberately left alone. **The
pool discharges that obligation** — and it declines the generic idle reaper at its own enumeration
rather than relying on `expire_idle` refusing, because "the call I made was rejected" and "I declined
to make it" are different statements and only the second survives a second reaper.

**Failure disposition, and why it is separate from a failed clear.** A rebind failure means pmux does
not know what state the instance is in; an assert-empty refusal means pmux knows exactly what
happened and it was not a clean clear. They must not share a diagnostic:

| `reason` | Means | Operator action |
|---|---|---|
| `wrong_local_command` | **The composer executed a different slash command.** | **Page.** Menu geometry drifted; pin the Claude version and halt Path B. |
| `turn_marker_present` | A `turn_duration`/`stop_hook_summary`/`api_error` row in a transcript that served no work. | The model of `/clear` is wrong; re-run the clear probe. |
| `semantic_row_present` | A prompt, reply, tool result or attachment survived. | Same, higher severity: this is leakage. |
| `unexpected_system_subtype` | e.g. `compact_boundary` — a compaction happened. | Re-derive §3.4. |
| `unexpected_metadata_record` | A metadata record type outside the measured preamble allowlist — `queue-operation` (queued user input), `ai-title`, `summary`, `progress`, `pr-link`, or a type Claude added. | Read the file. A queued-input row is leakage; a new type must be classified before it is admitted. |
| `metadata_prompt_present` | A `last-prompt` row whose `lastPrompt` names a prompt. | The cell ran a turn. Same severity as `semantic_row_present`. |
| `unexpected_user_row` | A `UserOther` row that is neither caveat nor command echo. | Schema drift in the preamble. |
| `unknown_row` | `RowKind::Unknown` on a supposedly clean file. | Claude added a row type; classify it. |
| `row_budget_exceeded` | > 16 rows or > 64 KiB. | Almost certainly leakage; read the file. |
| `unparseable_row` | Strict parse failed. | Schema drift. |
| `clear_command_missing` | The rotated file never carried a `/clear` echo. | Something else opened this transcript. |
| `preamble_not_settled` | The clean rows carry the echo but a trailing record was never terminated. | The writer stalled; inspect the file. |
| `unexpected_clear_echo` | A launch preamble carries a `/clear` echo. | Id resolution found the wrong file. |

Diagnostics carry no transcript content: only `reason`, `row_kind`, `line`, `rows`, and -- for
`wrong_local_command` and `unexpected_system_subtype` -- the offending Claude schema token, itself
length- and charset-bounded before it is reproduced.

### 3.3 Serve turn

Unchanged from Path A: prompt injection, transcript-authoritative completion, terminal liveness gate.
Path B changes *what the cell can do*, not *how a turn is decided to be over*. `CompletionAuthority`
remains single-variant `Transcript`.

### 3.4 `/clear`, and the failure mode it introduces

`/clear` MEASURED behavior on 2.1.220:

- Creates a **NEW** transcript file; it does **not** append to the old one.
- **ROTATES the session_id** every time.
- The new file appears **+39ms** after Enter, **immediately** — not lazily.
- Its **ROW 0** is `{"type":"mode","sessionId":"<NEW-UUID>"}`. **That row is the rebind anchor.**
- `turn_duration` **survives** `/clear` (60/60 turns in a soak).
- The `--system-prompt` replacement **survives** (verified with a sentinel token).
- Context is genuinely cleared, **and the residue is now quantified rather than asserted** — see
  §3.4.1, which replaces the bare word "genuinely".
- Real cost **~30ms, FLAT** across 60 clears, 61 accumulated files, and 16-way contention.
- Relaunching instead costs **~4.4s** (TUI). That 145x gap is the entire reason `/clear` is in this
  design.

#### 3.4.1 What a cleared instance carries, MEASURED in tokens

The claim "context is genuinely cleared" used to rest on a recall prompt answering nothing. It now
rests on a token count, which is the instrument that can be wrong in the safe direction.

**Context across `/clear` is CONSTANT, not accumulating.** Six real sonnet/low turns on one
instance: **171 tokens cold, then 326 after one, two, three and five clears.** Fifteen of fifteen
cleared turns read **326** across four separate runs.

**The 132-token step is the `/clear` preamble itself**, and it does not accumulate because a rotated
transcript carries at most one caller prompt per file. The residue is three messages, **420
characters** in total, read off the instance's own transcripts:

| message | length |
|---|---:|
| `<local-command-caveat>…` | 245 chars |
| `<command-name>/clear</command-name>…` | 130 chars |
| `<local-command-stdout></local-command-stdout>` | 45 chars |

The hypothesis under test named the first two and **missed the third**; the arithmetic is what found
it. Note the instrument caveat of §0.3 item 4: `input_tokens` alone is **not** the turn's input — a
2,709-character filler prompt reported `input=2 cache_creation=1230`, which is what made an earlier
filler-prompt probe look anomalous.

**`history.jsonl` never reaches model context. MEASURED.** With **40k tokens seeded** into an
instance's own `history.jsonl`, the next turn's `input_tokens` was **unchanged at 186**, and a
leading post-`/clear` question about the seeded content was answered **NONE**. So `history.jsonl` is
**disk hygiene and an up-arrow recall channel — it is NOT the statelessness bound**, and §6's
recycle cap is capacity hygiene rather than a privacy bound. §5.6 keeps it on the closed-channel list
for the reason it was always there: a **writable attachment** can recall it into the composer, which
is a filesystem-and-keyboard path that never passes through model context at all. Do not read this
measurement as retiring item 2 or item 4 of the §5.6 conjunction; read it as naming exactly which
channel each one closes.

**The failure mode.** `/clear` **abandons** the old transcript rather than truncating it: inode
unchanged, length unchanged. So every existing fence stays green while pmux tails a file that will
never grow again.

**Why it fails closed.** `TurnStatus::Terminal` is unreachable without a prompt acknowledgement,
which requires ingesting a main-scope row **from the file pmux is actually reading**. An abandoned
file yields no such row, so the turn **TIMES OUT rather than returning a wrong answer**. That is the
asymmetry resolving correctly, by construction, without any new code.

**The diagnostic gap this design closes.** Today the operator would see a bare `TurnTimeout` with
no thread to pull. Path B must distinguish the rebind failure explicitly (§3.5).

### 3.5 Rebind

Rebind is how an instance stops reading the abandoned file and starts reading the new one.

**The seam already exists.** `trait TranscriptSource` already takes `session_id` as a **per-call**
parameter on both methods (`crates/service/src/v1/backend.rs:452-459` — `arm_at_eof(&self,
session_id)` and `poll(&self, session_id, position)`). `FileTranscriptSource` takes it on both
(`arm_at_eof` forwards to `arm_sync` at `driver_io.rs:3257`.
`poll` forwards to `poll_sync` at `driver_io.rs:3265`.
The `expected_session_id` field is at `driver_io.rs:2842`.
A `TranscriptLocator` is bound at construction at `driver_io.rs:2273`). Rebinding is therefore a
**wiring** change, not a new abstraction.

**Procedure.**

1. Record the projects-directory listing for the instance cwd **before** Enter is sent (this is
   cheap: a per-instance cwd holds only that instance's files, §4).
2. Type `/clear`, send Enter, record `t0`.
3. Poll the same directory for a `*.jsonl` that was not in the prior listing. MEASURED first
   appearance is `t0 + 39ms`.
4. Read **ROW 0**. Require `type == "mode"` and a parseable `sessionId` UUID. Row 0 being a
   `mode` row is MEASURED and is the anchor; a first row of any other type means the pool's model
   of `/clear` no longer matches the installed Claude, and the instance is QUARANTINED.
5. Resolve the rotation: the extracted UUID is the successor id. **This is where step 5 splits.**
5.5. Run assert-empty (§3.2) against that file, *before* it is bound. It runs here and not after
   the bind for two reasons: `arm_sync` sets `armed = true` and resets `TailState`, and an
   armed-then-tainted session is a state with two truths in it; and the wait for the preamble to
   settle is a bounded not-yet answer, so a half-written preamble must not quarantine a healthy
   instance. Every refusal *other* than a missing echo, an unterminated trailing row, or a file that
   is still growing is immediate, because waiting longer cannot unmake a semantic row or a wrong
   command name.

   **The settle wait must require QUIET, not merely a terminated last row (CHOSEN: 50ms).** Step 6's
   `arm_at_eof` reads the length with `stat`, opens, and refuses if the two disagree — and on this
   path that refusal is a quarantine. Every intermediate state of a preamble being written row by
   row is a complete, terminated, individually-inert set of rows, so a "last record is terminated"
   check cannot see it and the bind lands on a moving file. MEASURED: the five preamble rows carry
   timestamps spanning 3ms; 50ms is ~16x that, spent between turns inside the existing 2000ms
   deadline. This is a **heuristic, not a proof** — nothing observable can prove a writer has
   finished — and what makes it acceptable is that the residue fails closed at the arm.
6. Bind: `arm_at_eof` on the successor id re-resolves the locator, which accepts only a transcript
   whose own rows corroborate that id and this cwd. Only then return the instance to IDLE.

**Rebind deadline: 2000ms (CHOSEN).** MEASURED appearance is 39ms; 2000ms is ~50x headroom, which
absorbs 16-way contention and a loaded filesystem without ever being close to a real bound. On
expiry the instance is QUARANTINED and the failure surfaces as `TranscriptUnavailable` with
`violation: "clear_rebind_not_observed"`, carrying the abandoned session id, the elapsed time, and
the transcript counts before and after. It is **not** reported as `TurnTimeout`, which is the
requirement; a distinct `ClearRebindTimeout` **error code** is deliberately not added, because
`ErrorCode` is closed and both shipped clients hard-reject unknown values -- emitting one to a
client older than the release would make it reject the whole response frame rather than deliver a
worse message. See §11.

Rebind never runs on the turn's critical path. It happens after the result is delivered, while the
instance is not checkout-eligible. Slow rebind therefore costs pool capacity, never turn latency —
which is the correct place for it to cost anything.

**Quarantine is for clears that may have executed, and only those.** The actor's
`poison_after_failed_rebind` exists because a clear that landed leaves the bound transcript
abandoned, and a session that kept accepting turns against it would time each one out ten minutes
later with nothing to pull on. That argument does not reach a clear that was refused *before the
command was submitted*, and two such refusals need no malformed input to trigger: a `clear_session`
whose deadline has already passed, refused inside the input gate before a byte is written; and a
`clear_session` issued **before the session's first turn** — the natural order for a pool checking an
instance out — where `watch_rotation` finds no transcript to snapshot because Claude creates the file
lazily. Both used to end in a permanently `Tainted` cell holding a live Claude process that only
`close_session` reclaims.

The deadline case is not only the already-elapsed one. `DEFAULT_CLEAR_TIMEOUT_MS` and
`INPUT_GATE_MAX_DURATION` are both 15,000 ms and the clear's deadline is computed first, so on
**every** clear the remaining turn — not the gate maximum — is what bounds the gate. A deadline that
expires with the `/clear` paste still in flight is therefore a routine outcome rather than a corner,
and it answers `TurnTimeout` and marks `clear_not_submitted`, the same as an already-elapsed one.
Before 2026-08-06 it answered `PromptNotAcknowledged` instead, because `paste_once` did not ask which
of the budget's two clocks had run out; see `docs/archive/current-state-2026-08.md` §10 item 15. A deadline that
expires inside the **Enter** does not take this path at all and must not: Enter went in, so the
refusal keeps `enter_attempted` and the clear proceeds to the rotation authority.

`driver_io::clear_and_rebind` marks exactly those two refusals with `clear_not_submitted: true`, at
the two sites that own the fact, and the actor returns them without poisoning. The claim is
**positive**: a failure that does not make it is quarantined, so a refusal path added later fails
closed by default rather than by remembering to.

**There is no exactly-once retry window, and there deliberately never will be one derived from
session state.** `clear_session` refuses every fence that is not the currently bound transcript, with
`IdConflict` and `violation: "stale_transcript_fence"`, and `ClearSessionResult::rotated` is
therefore always `true`. The refusal is Step 0, ahead of the busy guard, and types nothing.

The answer that was removed said "your clear landed and the transcript it opened is the one you are
looking at" to a fence stale by exactly one rotation, on the reasoning that a retry of a lost
response is one rotation behind. That is true of the retry and useless as a rule, because
`ClearSessionRequest::expected_transcript_session_id` **starts equal to `session_id`**: after one
clear, the one-behind value is byte-identical to the fence a session begins with, which is what a
restarted client or a second caller that never saw the first clear presents. Two attempts to bound
the answer by session state both leaked, and both were reproduced over the real socket:

1. Keyed on the abandoned id, which nothing invalidated. `clear(S) -> N1`, a turn carrying a secret
   under `N1`, then `clear(S)` again was answered `rotated: false`, typed nothing, opened no
   transcript, and the next caller's turn landed in the first caller's transcript behind the secret.
2. Re-keyed on the actor's event sequence, justified as "`emit` is the single funnel every state
   change passes through". False for the one path whose whole purpose is to let a second party
   mutate the TUI: `reserve_writable_attach`, `release_writable_attach` and the success path of
   `finish_attach_reconciliation` all mutate the session and emit nothing, so the window stayed open
   across an entire attached session. Reproduced: a fresh clear under attach was `SessionBusy` with
   "a writable terminal attachment currently owns input" while the one-behind fence, answered ahead
   of that same guard, got `Ok(rotated: false, state: Ready)`.

The general fact is that the mutation channel a writable attachment opens **does not pass through
the actor at all** — the caller gets an rmux grant and the bytes go client → rmux socket → PTY — so
no actor-side state can encode "nothing has happened since the rotation". Every candidate is a
proxy, and the two proxies tried so far were both believed closed on the day they were written.

**Recovery is a read, not an inference.** `SessionSnapshot` publishes `transcript_session_id` and
`cell`, so a caller whose response was lost asks whether the fence moved. If it did, a clear landed;
if certainty about the cell's contents is wanted, clear again on the current fence, which is
semantically idempotent and MEASURED at ~30ms (§10.2). Nothing is lost relative to the old answer,
which never proved the landed clear was *yours*. Note also that Path B's only intended caller is
barred from the old behaviour by its own policy: §7 quarantines an instance on any doubt about a
clear's outcome rather than clearing it again and retrying.

If exactly-once clear ever becomes a *stated* requirement, it gets a caller-supplied idempotency
token and a stored result — the way `run_turn` already does (spec §5.1) — never an inference from
session state. The turn path never had this bug because it never inferred.

**A minified cell refuses writable terminal attachment.** `attach_session` with `read_only: false`
returns `UnsupportedFeature` with `violation: "writable_attach_forbidden_on_minified_cell"`, before
any rmux grant is minted, and `select_minified_cell` refuses to convert a session that holds one. It
is the same architectural fact as the leak above, from the other side: everything Path B promises is
stated in terms of what the actor can observe, and this is the one capability that moves the cell
without being one of those things. What it enables is open-ended, and the capability is one place —
composer text that PREFIXES the next caller's prompt, up-arrow recall out of the instance's own
`history.jsonl` (§5.6), and a hand-typed `/clear` or `/model` that rotates Claude's id underneath the
one pmux has bound, after which the emptiness proof attests a file nothing is writing to.

### 3.6 Issuing `/clear` at all

**Implemented.** `ControlCommand` is a private, payload-free, single-variant enum whose text is
selected at compile time in `driver_io.rs`; no caller byte can become the text typed there, and
`validate_prompt` is untouched and unrelaxed. The wire surface reaches it through
`Request::ClearSession`, which carries no text. There is still no caller-facing typed control API,
and this change must not become one by accident.

`validate_prompt` (`driver_io.rs`) refuses any caller prompt whose first character that is neither
whitespace nor an invisible Unicode format character is a **composer mode prefix**, with
`ErrorCode::UnsupportedFeature` and a message naming the character. The rule reads past invisibles
because JS `String.prototype.trim` strips U+FEFF and Claude Code is a Node/Ink TUI, so a leading BOM
used to carry `/clear` through a guard that only trimmed White_Space. That refusal is **correct and
must stay**.

This paragraph used to say the refused character was `/`, which was true of the guard and false of
the composer. `crates/claude/src/composer.rs` now owns the set — `COMPOSER_MODE_PREFIXES` is `/` and
`!` — because a prompt beginning `!` was MEASURED at 2.1.226 switching the composer into bash mode
and RUNNING THE REST AS A SHELL COMMAND on the host, six times out of six on a warm pooled instance.
`docs/path-b-adversarial.md` §4 carries the reproduction, the sweep the set is derived from, and the
render-proof weakness that let Enter be pressed on a buffer pmux had not read. Path B therefore needed an **internal-only** clear operation
that reaches the injection path without traversing `validate_prompt` and is **not reachable from the
caller surface** in any form. **That is what `ControlCommand` is, and it is built** — the trailing
sentence of this paragraph used to read "this is a change outside this document's file ownership and
is reported, not made", which stopped being true when `ControlCommand` landed. There is still no
design for a typed control API, and Path B must not become one by accident.

**The guard was proven by a real PTY, not by reading.** Nobody had tested a prompt containing
`\e[201~`. Both guards hold — `validate_prompt` refuses ESC/NUL/controls, and the wire encoder
refuses them again independently — so this is a **confirmed-mitigated risk and NOT a live defect**.
Proving it needed a live PTY: with both guards removed, a caller sending `"benign\e[201~/logout\r"`
gets `benign` pasted and **`/logout\n` delivered as KEYSTROKES**. `paste_injection.rs` holds that
shut against 55 hostile inputs and 512 generated ones. Note also the fourth anti-vacuity finding from
the same round: **nothing asserted the slash-command guard at all** — `/clear` carries no ESC, so it
survives both control-character guards and would be submitted EXACTLY, satisfying a
"refuse-or-submit-exactly" property while handing Claude Code a command.

---

## 4. Per-instance cwd, and why

**Every instance gets its own empty, pre-trusted working directory.** Claude derives the transcript
project directory from cwd, so a private cwd gives each instance a private
`<config_root>/projects/<slug>/` namespace.

**Why this matters more than it looks.** After `/clear`, the pool must answer "which transcript is
mine?" With a per-instance cwd, that directory contains only this instance's files, so the
"appeared since my pre-Enter listing" set has **exactly one** member. The answer is unambiguous
**BY CONSTRUCTION**.

The alternative — a shared cwd across instances — makes the answer a race: N instances clearing
concurrently drop N new files into one directory within a ~39ms window, and attribution then
requires either a lock around clear (serialising the one operation the design needs to be cheap and
parallel) or a content probe that is itself racy. `TranscriptLocator::locate` already has an
`Ambiguous` arm (`crates/claude/src/locator.rs:173`) precisely because multiple candidates is a
real state; a per-instance cwd means Path B never enters it.

Choosing a structural guarantee over a lock is also the cheaper option, but it would be the right
choice even if it were more expensive. See §8 for the honest caveat: the shared-cwd hazard is
argued, never demonstrated.

**Now enforced by the daemon, not merely allocated by the pool.** Sessions are keyed by `SessionId`
alone, so two live sessions sharing a cwd was representable and nothing refused it. A start whose
RESOLVED cwd OVERLAPS a directory already bound to a live session is refused when either side is a
minified cell — both directions of the cell, because "refuse a minified applicant whose cwd is
taken" would still admit an ordinary session into a live cell's cwd, and both directions of
containment, because a cwd inside a live cell's workspace reads the same files as the workspace
itself. The rule crosses ROLES as well: a cwd standing on a live cell's CONFIGURATION ROOT was
LEAK 7's third shape and a cwd-against-cwd comparison could never have seen it (§5.6). The resolved
cwd is the canonical path the child is actually launched with, so a second spelling of the same
directory is the same directory.

---

## 5. Private config root and trust pre-seeding

**Implemented.** `StartSessionRequest::config_isolation` names a pmux-owned Claude configuration
root; the daemon delivers it as `CLAUDE_CONFIG_DIR` and computes the credential pin itself. This is
the section that supersedes §2.2's `CLAUDE_CONFIG_DIR` rejection.

### 5.1 Why the config root and the credential store had to be separated

Verified read-only in the compiled bundle at `~/.local/share/claude/versions/2.1.220`, the same
version every other fact in this document is pinned to:

| Claim | Evidence |
|---|---|
| The keychain SERVICE NAME is namespaced by `sha256(config_dir)[0:8]` | `function oG(e=""){let t=process.env.CLAUDE_SECURESTORAGE_CONFIG_DIR,r=t!==void 0?!t:!process.env.CLAUDE_CONFIG_DIR,n=t!==void 0?t.normalize("NFC"):fn(),o=r?"":`-${createHash("sha256").update(n).digest("hex").substring(0,8)}`;return`Claude Code${Ds().OAUTH_FILE_SUFFIX}${e}${o}`}` |
| The file-backed store follows the same variable, not the config dir | `function gK(){let e=process.env.CLAUDE_SECURESTORAGE_CONFIG_DIR;if(e!==void 0)return(e\|\|join(homedir(),".claude")).normalize("NFC");return fn()}` |
| The EMPTY STRING is a first-class value | Claude's own env filter: `if(r===""&&t!=="CLAUDE_SECURESTORAGE_CONFIG_DIR")continue;` — every other name drops an empty value, this one is preserved deliberately |
| `.claude.json` relocates with the config dir | `lE=Vr(()=>{if(existsSync(join(fn(),".config.json")))return join(fn(),".config.json");return join(process.env.CLAUDE_CONFIG_DIR\|\|homedir(),`.claude${XUn()}.json`)})` with `fn()=(process.env.CLAUDE_CONFIG_DIR??join(homedir(),".claude")).normalize("NFC")`; `XUn()` is `""` for the production OAuth environment |
| `userSettings` resolves to `<config dir>/settings.json` | `_4r("userSettings",…)=path.resolve(fn())` and `mnt("userSettings",t)=join(_4r(…),"settings.json")` |
| Trust is inherited from ancestor directories | `function J3y(){…let e=Rt(),t=fbe();if(e.projects?.[t]?.hasTrustDialogAccepted)return!0;let n=VMe(xt());while(!0){if(e.projects?.[n]?.hasTrustDialogAccepted)return!0;let i=VMe(resolve(n,".."));if(i===n)break;n=i}return!1}` |

MEASURED on this host: `CLAUDE_CONFIG_DIR=$T claude auth status` → `loggedIn:false`;
`CLAUDE_CONFIG_DIR=$T CLAUDE_SECURESTORAGE_CONFIG_DIR= claude auth status` → `loggedIn:true, max`.

**The invariant, stated so it can be tested:** *the isolated child authenticates against exactly the
credential store the same request would have used without isolation.* pmux computes the pin as
`snapshot - unset + set` for `CLAUDE_CONFIG_DIR` and passes it **byte-for-byte** — Claude
NFC-normalizes it itself, and normalizing on the pmux side risks hashing to a different service
name than the operator's own un-isolated session. The root, by contrast, is delivered
**canonicalized**, because it must name the same directory pmux seeds and the transcript locator
walks. That asymmetry is pinned by
`claude_launch.rs::tests::the_pin_is_byte_exact_while_the_root_is_canonical`.

The caller cannot supply the pin: `config_isolation` together with an explicit `CLAUDE_CONFIG_DIR`
or `CLAUDE_SECURESTORAGE_CONFIG_DIR` in `set` is a refusal, because a caller who set the root by
hand and forgot the pin would silently get a login screen instead of a session. An *ambient*
snapshot value is not a conflict — it is the input to the pin, and refusing it would lock out
exactly the operators who already run under a custom config root.

Step 6 of the environment order runs **after** the terminal-profile denylist. That list has acquired
a `CLAUDE*` name after each of four live failures; if step 6 ran earlier, the next `CLAUDE_` prefix
entry would silently strip the pin and turn every isolated session into a login screen. Running last
makes that class of regression unrepresentable.

### 5.2 The seed

An **untrusted** cwd makes Claude show the trust dialog, and turn 1 then hangs for its **full
deadline**. This is the single highest-cost misconfiguration in the whole pool. An incomplete
onboarding record does the same thing one screen earlier.

`<root>/.claude.json`, mode `0600`:

- `hasCompletedOnboarding: true` — REQUIRED. Gate:
  `let l=Rt(),c=!1;if(!l.hasCompletedOnboarding||…){c=!0;…Onboarding…}`.
- `projects[<canonical cwd>].hasTrustDialogAccepted: true` — REQUIRED, and exactly ONE key. `J3y()`
  walks up from `VMe(xt())`; `xt()` is the process cwd and `VMe` is `path.normalize`, and pmux
  launches the child with a `Path::canonicalize`d cwd, so the canonical cwd matches on the FIRST
  iteration of that walk. That proof is what makes a single key sufficient regardless of what
  `fbe()`'s own git-root indirection resolves to.
- `autoUpdates: false` — from Claude's own private-root recipe (`oOp`), and justified independently:
  an in-session updater that changes the binary mid-campaign invalidates the compatibility cell's
  version key.
- `bypassPermissionsModeAccepted: false` — always `false`. **This is a correction to the design
  spec**, which proposed mirroring `dangerous_permission_bypass` here. `true` does suppress the
  modal (`GyT` returns early on `PW()||Rt().bypassPermissionsModeAccepted`), but it is TRANSIENT:
  `cCm()` migrates it at startup by writing `skipDangerousModePermissionPrompt:true` into
  userSettings and then DELETING this key. A seed asserting `true` would be erased by the first
  launch, rewritten by every subsequent start, and would make a root whose live session had merely
  done what Claude always does fail the read-only check of §5.3.

`<root>/settings.json`, mode `0600`: `{}`, so "no ambient user settings, hooks, statusLine or output
styles are in play" is an asserted fact rather than an emergent one, and a later operator edit
inside pmux's root shows up as a diff against a known baseline. When the request carries
`--dangerously-skip-permissions` it instead carries
`{"skipDangerousModePermissionPrompt": true}` — the key `PW()` actually reads and the destination
Claude's own migration writes to. The alternative is a guaranteed turn-1 modal with no answering
surface. This is not pmux inventing consent: the caller declared the intent explicitly and every
turn of such a session already carries the `dangerous_permission_bypass` warning.

`<root>/projects/` is **not** created. `TranscriptLocator` guards its scan with
`projects_root.is_dir()` and its fast path yields non-existent candidates, so a missing directory is
an empty collision set — which is what a fresh root should mean. Claude creates it on first launch.

A root containing `.config.json` is REFUSED: `lE()` reads it in preference to `.claude.json`, so the
seed would be written, accepted, and then ignored, and the failure would present as a turn-1 hang
rather than as a bad seed.

### 5.3 The rule: pmux writes only when it is the sole writer

**The mandate survives verbatim, and its justification improves.** Under a private root a torn write
damages pmux's own file rather than unrelated state belonging to the operator — which is the
argument for landing the private root *before* the pool rather than after.

Claude writes this same file itself, under its own lock, with its own stale-write telemetry and an
auto-repair path. pmux does not implement that protocol, so it writes only when **no live session is
bound to that root**; when one is, it performs a read-only check and REFUSES the start when the
required state is absent, rather than racing. Writes are temp file + `create_new(0600)` +
`sync_all` + `rename` + directory fsync, and both files are opened `O_NOFOLLOW` — a symlink planted
in a root pmux claims to own is a refusal, not a follow. An unparseable existing file is a refusal,
never a silent replacement.

No per-root mutex is introduced. `NativeService::start_session_owned_with_retention` holds `start_guard` across
its whole body, so every seed in one daemon is already serialized against every other; a second lock
could only disagree with the first.

**The residual is CLOSED, and it is closed by the mechanism rather than by more sampling.** What
stood here was: 0/58 samples clean at 16 instances, taken at **2 s intervals**, so a sub-2 s torn
write would not have been observed. More samples could only ever have made that window smaller.
**Claude does not rewrite `.claude.json` in place — it writes a new file and renames it over the
old one**, and a `rename(2)` is atomic, so a reader gets the whole old file or the whole new one and
a torn read is structurally unreachable. This is the same class of evidence as `--bare` in §2.2:
read the behaviour, not the outcome.

MEASURED 2026-08-06 against a real 2.1.220 Path A session in a private root, sampling the file
**whole** — `stat` plus a full read plus a JSON parse — every ~2.9 ms for the life of the session
(6 turns, 25 s):

- **8,764 present samples. 0 unparseable. 0 short. 0 absent after the first write.**
- **25 distinct inodes across 25 distinct mtimes**: every single write landed as a NEW inode. Not
  one write was observed as a mutation of the file that was there.
- 9 `absent` samples, all in the first 20 ms, before pmux had seeded the root at all.

**With a positive control, because a negative result without one is the §0.3 rule 2 trap.** The same
sampler, against the same-sized file rewritten IN PLACE 25 times (`r+b`, `truncate(0)`, 4 KiB
writes), observed **36 unparseable reads out of 407 samples, across exactly one inode** — sizes 0,
4096, 8192, 16384, 20480. So the sampler can see a torn write; it saw none of Claude's, and it saw
the inode change every time instead. The one thing this does **not** cover is what happens if a
future Claude changes that write strategy, which is why the mechanism is named here: it is the thing
to re-check, not the sample count.

One capability is lost and is recorded here rather than discovered later: the operator can no longer
pre-trust a pmux cwd by hand in their own `~/.claude.json`. Under isolation, trust comes from the
seed or not at all — the correct direction, since it makes the seed a hard precondition rather than
something that silently works because of unrelated operator history.

### 5.4 What this does NOT isolate

**It isolates CONFIGURATION, not CREDENTIALS, and the mechanism that makes it work is the mechanism
that guarantees it does not.** The pin exists precisely so the isolated child reaches the **user's
own** keychain item. Consequently, and none of these are hypothetical: an OAuth refresh inside a
cell rewrites the operator's credential; a credential-clearing path inside a cell logs the operator
out machine-wide; usage, rate limits and subscription state are the operator's, so a pool of 16
spends the operator's quota; revoking a cell's access means revoking the operator's session; and the
blast radius of a compromised cell still includes the caller's Anthropic identity.

The only thing that would isolate credentials is a *separate login* into a separate securestorage
dir. pmux has no path to drive that flow — it would be a browser handoff inside a PTY the caller
cannot see — and it is an explicit **non-goal**. Anyone who later removes the pin to "improve
isolation" gets a login screen on every turn.

What the private root DOES buy beyond tidiness: a deny-by-default sandbox profile becomes writable.
Today the child needs read+write across `$HOME/.claude/**` and `$HOME/.claude.json`, and that hole
is not narrowable — granting it necessarily grants `history.jsonl` (every prompt the operator has
ever typed on that machine), `CLAUDE.md`/`agents/`/`commands/`/`plugins/`/`skills`,
`settings.json` including any `hooks` it defines, every other session's transcripts under
`projects/**`, and the machine-wide trust table. With a private root every one of those becomes
deniable. It does not make keychain access deniable, and it is a precondition for a narrow profile
rather than the profile.

### 5.5 No compatibility-cell dimension, deliberately

`TestedCompatibilityProfile`/`CompatibilityReport` key on version, os, arch, terminal profile and
input transport. The *content* of the config root has never been in that key: a promoted drain today
is calibrated against whatever the operator's `~/.claude` happened to contain, hooks and statusLine
included. Config content is already an uncontrolled variable of the current cell; a private root is
the first time it becomes controlled, and it removes variability rather than adding it.

A **path-keyed** dimension would be a defect, not a conservative choice. `resolve` requires an exact
match on every field and `RequireTested` refuses on no match, so a dimension carrying the root path
makes every per-instance — or, under a per-slot layout, every slot — its own cell. No promoted
profile can ever match, `require_tested` becomes structurally unpassable, and the only way to run
anything is `allow-untested` with its conservative 2000ms drain. That converts a safety mechanism
into a permanent bypass.

Not even a **mode**-keyed dimension yet. **CORRECTION (2026-08-06): this paragraph used to open
"No tested cell exists for 2.1.220 on macOS/aarch64 today". That is now FALSE — two ranges are
promoted** (macos / aarch64 / transparent / sdk, 2.1.220 through 2.1.238,
`transcript_drain_ms: 1000`, and linux / x86_64
2.1.227 through 2.1.236, drain 250), which is what makes Path B reachable with no
`--tested-claude-profile` on argv; MEASURED with the flag absent, a real turn
`served in 4540ms by claude 2.1.220`. **UPDATED 2026-08-09 / 2026-08-20 / 2026-08-21: each cell is a RANGE** —
see §12.4 for what was driven at each ceiling and what was not.
The argument the sentence was supporting is **unchanged and now stronger**: a
mode dimension doubles the cells that must be promoted before either mode can run under
`require_tested`, and a range is one cell per os/arch, not two versions.
`CompatibilityReport`'s field set is pinned by `tests/conformance/v1/cases.json`,
so adding a field is a protocol event that should be paid for by evidence rather than anticipation;
and nothing in the config root sits on the ~300ms of screen-stability the drain measures *unless*
the operator's root carried a statusLine or hook — which a private root removes, making the private
mode the *less* variable one. If measurement ever demands it, the shape is fixed here so nobody
reaches for the path: `ConfigIsolationMode { Inherit, Private }` on both types, compared inside
`matches`/`same_key`, with `#[serde(default)] = Inherit` so every already-written tested-profile
file keeps deserializing. Two values, forever.

**Unmeasured, and named so it is not mistaken for measured:** first-launch cost in a cold root. A
fresh root has no cached feature flags, statsig state or settings; whether that shifts readiness or
drain timing has never been measured and is not covered by any promoted profile. This is the one
place a mode dimension could become justified — by measurement, not by argument. `cache/changelog.md`
is 466 KB in the operator root; whether a cold root re-fetches it per instance is part of the same
unmeasured cost.

### 5.6 One root per cell, mandatory — and what a root carries that a transcript does not

**A private root is now a PRECONDITION of `cell: minified`, not an option for it.** The daemon
refuses a minified start with no `config_isolation`; refuses one whose root is already bound to a
live session; and refuses one whose root contains anything other than the two files pmux seeds. The
pool deletes the root at recycle/teardown, after §8's retention obligations have been discharged
against it.

**And the root rules are stated about the ROOT, not about the request.** Every earlier form of them
asked what the applicant looked like — does it say `cell: minified`, does it carry a
`config_isolation` block — and each was open to the next entry path that reached the same directory
in a different shape. MEASURED over the real socket: a start carrying
`environment.set["CLAUDE_CONFIG_DIR"] = <a live minified cell's root>` and no `config_isolation` at
all was ADMITTED, its child wrote into the cell's own `projects/`, and the cell's prompt was readable
from inside that root; so was an ordinary cell naming the live root explicitly. The rule now keys on
the root the request RESOLVES to — the same value that is delivered to the child and that the
transcript locator walks — and on the INCUMBENT: a root a live minified cell is bound to admits
nothing else, at it or anywhere under it, whatever cell or isolation shape the applicant carries.
Where several sessions reach one root, the strictest claim answers.

**And the SECOND DOOR is now shut rather than filtered.** Keying on the resolved root closed the
shapes that were known; it did not close the *spelling* surface, and the spelling surface produced
two more leaks. LEAK 5 reached a live cell's root through the APFS firmlink namespace, which
`Path::canonicalize` does not collapse. LEAK 5b reached it through `..` past a component that does
not exist: `std::fs::metadata("/X/NOPE/../rootA")` answers `NotFound` because the kernel resolves
left-to-right, while `mkdir -p "/X/NOPE/../rootA"` creates `NOPE` and *then* resolves `..`, landing
on the existing `/X/rootA` — which is exactly what Claude does to its own `CLAUDE_CONFIG_DIR`.
MEASURED over the real socket: `--env CLAUDE_CONFIG_DIR=/X/NOPE/../rootA` was ADMITTED against a
live minified cell holding `/X/rootA`, and the intruder's child wrote its own transcript physically
inside that cell's root.

Three rules, at three different altitudes, because the class has now outlived six rules written at
one:

1. **The spelling is refused, on the effective root.** A configuration root carrying a `..`
   component is refused outright, for every shape that produces one — an explicit `set`, an
   inherited snapshot value, a `HOME`-derived default. pmux does not collapse the `..` and trust the
   result: lexical collapsing is not the kernel's rule when a component is a symlink (`a/b/..` is
   `b`'s *target's* parent, not `a`), so a fix that collapsed and trusted would be wrong in the
   direction that leaks. A refusal costs the caller one spelling of a directory they can also spell
   without a `..`.
2. **The mechanism stops reading an absence as evidence.** The admission gate's whole claim is that
   "there is no such directory" proves the applicant is not a directory a live cell holds. For a
   `..` spelling that claim is false, so the gate refuses it — on BOTH bound resources, so the rule
   holds for any future entry path that computes a root some other way. The identity predicate
   itself keeps reporting the kernel's answer unchanged, because its other caller compares the
   securestorage PIN, which is a keychain-service input rather than a directory pmux binds.
3. **The door itself is closed for Path B.** A `cell: minified` start may not carry
   `CLAUDE_CONFIG_DIR` or `CLAUDE_SECURESTORAGE_CONFIG_DIR` in `environment.set` at all. A minified
   cell already has a first-class way to name its root — `config_isolation`, which is canonicalized,
   owner-checked, shadow-checked and pristine-checked — and the plain env value is the only spelling
   of that directory nothing canonicalizes. This deletes the surface instead of filtering it.
   **Path A keeps the door**: an ordinary cell owns its own isolation story, `pmux probe` predicts
   `--env CLAUDE_CONFIG_DIR=` without contacting a daemon, pmux seeds nothing there, and such a
   start is still admitted against every live session's resources on the inode.

The same defect class was also acknowledged and unclosed on the OTHER rule these two resources
share: `config_isolation` refuses a root and a cwd that contain one another, and it decided
containment by path prefix. Both sides are canonicalized, so it was never open to a name-prefix
collision — `Path::starts_with` compares whole components — but it was open to the firmlink alias,
under which `/System/Volumes/Data<W>` and `<W>/inner` are genuine containment sharing no component
prefix at all. Containment is now an ancestry question asked on `(st_dev, st_ino)`.

**LEAK 7 was not a seventh spelling. It was the wrong RELATION, and the correct predicate was
already in the tree.** The three rules above are all about naming ONE directory; the containment
predicate of the previous paragraph existed, was correct, and was wired to exactly one caller — the
intra-request root-versus-cwd rule. Live-cell admission never asked it. It asked
`must_treat_as_same_directory`, an IDENTITY test, so `R/sub` was not `R`, no incumbent was found,
and `admit_config_root` returned `SeedDisposition::Write` against a live minified cell's private
root. MEASURED over the real socket against a live cell, EIGHT shapes were ADMITTED, and nine
violations were swept: a configuration root nested in the cell's root (an absent subdirectory, and
the cell's own `projects/`); a configuration root that was an ANCESTOR of the cell's; `HOME`
redirected so the delivered root landed at `<cell root>/.claude`; a cwd that WAS the cell's
configuration root; a cwd inside it; a cwd inside the cell's workspace; and a minified applicant's
own canonicalized, owner-checked, pristine private root sitting INSIDE the victim's. The victim's
root ended up holding the intruder's transcript, `.claude.json` and `settings.json`.

The invariant that actually has to hold is therefore not "no other session may bind the same
directory" but **no directory a live minified cell binds may be reachable by any other session, in
any role, at any depth**. It is now one predicate, asked once, over the full cross-product: every
directory the applicant binds — the configuration root `effective_config_root` resolves for all four
shapes that produce one, and the canonical cwd — against every directory every live claim binds, in
both containment directions, with `one_directory_contains_the_other` generalized in place rather
than copied. Symmetric in the cell: a live minified claim answers on containment whatever the
applicant is, and a minified applicant gets containment against every live claim including ordinary
ones, because a private root nested inside a live ordinary session's workspace is the same leak one
second later. ORDINARY-versus-ORDINARY deliberately stays identity and role-matched — nesting is the
ordinary shape of a filesystem, and widening it would refuse a second ordinary session working in a
subdirectory and, through the seed disposition, stop pmux seeding a private root that merely sits
under a live session's cwd.

Two further doors were checked rather than assumed. `XDG_CONFIG_HOME` is NOT one: MEASURED against
the operator's own Claude Code 2.1.220 with `HOME` and `XDG_CONFIG_HOME` both redirected to fresh
directories, Claude created `$HOME/.claude`, `$HOME/.claude.json` and `$HOME/.claude/backups` and
wrote nothing at all under `$XDG_CONFIG_HOME` — the configuration root is `$CLAUDE_CONFIG_DIR` or
`$HOME/.claude` and nothing else, which is exactly what `effective_config_root` computes.
`CLAUDE_CODE_MANAGED_SETTINGS_PATH` is a READ door and binds no session to a directory: it is in no
allowlist entry and matches no inherited prefix, so only an explicit `environment.set` delivers it;
Claude treats it as one of three managed-settings SOURCES; pmux writes nothing through it; and
pointed at a live cell's root it selects the `settings.json` pmux itself seeded there, which carries
no cell state. It would become a directory binding the moment Claude wrote through it or managed
settings carried per-session state.

`HOME` itself is deliberately NOT added to the applicant's bound set. It is bound in exactly one way
that matters — as the source of the effective configuration root when `CLAUDE_CONFIG_DIR` is absent,
which `effective_config_root` already computes and admission already decides about. Treating `HOME`
as a bound directory in its own right would refuse every start whose `HOME` is an ancestor of any
live cell's private root, and the root is entirely operator-chosen with no default
(`bin/pmux/src/cli.rs`), so `~/.pmux/cells/N` is the ordinary place to put one: the rule would make
the product unstartable in its most likely deployment, in exchange for a reach that pmux cannot
remove anyway, because it does not sandbox the filesystem and any session's Bash tool can already
read any absolute path.

The reason is that `assert_empty_after_clear` reads exactly ONE file — the bound transcript — and
almost everything else a session leaves behind is scoped to the ROOT rather than to the session.
Verified against the 2.1.220 bundle and MEASURED against this host's own root:

| Channel | Scope | Written by a tool-less cell? | Evidence |
|---|---|---|---|
| `projects/` | per-project directory, per-session files | yes — every transcript, including the ones each `/clear` abandons | the known channel |
| `history.jsonl` | **per-ROOT, cross-project** | yes — every prompt, and a row for `/clear` itself | writer: `let r=xUs.join(fn(),"history.jsonl");await Gi().append(r,"",384),t=await fb(r,{stale:1e4,…});…await Gi().append(r,n.join(""),384)` — append-only under a lock, inside the root. Reader `BFd(e="project")` filters on `o.project!==t` for the default scope, i.e. on **cwd**, so recall spans every `/clear` by construction; `SHo=["session","project","everywhere"]` makes the wider scope one settings key away. MEASURED on this host: 1,556 rows, **49 distinct projects in one file**; a probe cwd contributed 146 rows across **77 distinct sessionIds**, of which **65 were `"display":"/clear"`** |
| `paste-cache/` | **per-ROOT, content-addressed, not project-scoped** | yes — for any prompt whose pasted content exceeds 1,024 characters, and pmux injects every prompt by bracketed paste | `Xcy="paste-cache"`, threshold `ruy=1024`, writer `PFd` at mode 0600; cleanup `MFd` is mtime-based only, so it **outlives transcript pruning**. MEASURED: one 49 KB blob in the operator root |
| `backups/` | per-ROOT, ≤5 rotating `.claude.json` snapshots | written by Claude | each snapshot carries the whole projects map: every cwd and `lastSessionId` |
| `.claude.json` | per-ROOT | yes — `projects.<cwd>` accrues `lastSessionId`, `lastCost`, token counts | MEASURED: no prompt text on 2.1.220 (0 of 55 project entries carry a `history` key; that is legacy) |
| `shell-snapshots/` | per-ROOT | no — 23 on this host, **zero** in the tool-less probe window | Claude's own cleanup says verbatim they "are not project-scoped and will not be touched" |
| `sessions/`, `tasks/`, `file-history/`, `session-env/` | per-session | no — 0 of 77 probe sessions produced any | |
| `debug/`, `cache/`, `stats-cache.json`, `teams/`, `ide/` | per-ROOT | no caller content observed | `debug/` could carry content under `--debug`, which the launch bundle does not pass |
| `todos/`, `statsig/` | **do not exist on 2.1.220** | — | the names survive only in the remove-old-directories loop `for n of ["todos","statsig","logs"]` |

`/clear` truncates none of it. The history writer is append-only and `/clear` is itself appended as a
row; 65 consecutive `/clear` rows and 77 rotated session ids coexist in one file.

**A per-cell root does not close the WITHIN-instance channel, and the design says so.** Over one
instance's life (up to 250 turns, i.e. up to 250 callers under fungibility) its own private
`history.jsonl` still accumulates every caller's prompt, and recall is cwd-scoped while the
instance's cwd never changes — so caller A's prompt is one up-arrow away from caller B *on the same
instance*. There is exactly one surface that reaches it, and that is why §3.5 refuses writable
attachment on this cell. The statelessness claim therefore rests on the CONJUNCTION of five things,
each closing a channel the others structurally cannot see:

1. assert-empty over the bound transcript (§3.2);
2. a per-cell config root, created empty and deleted at recycle (this section);
3. a per-instance cwd (§4);
4. no writable terminal attachment on a minified cell (§3.5);
5. quarantine on any doubt about a clear's outcome (§7).

**`CLAUDE_CODE_SKIP_PROMPT_HISTORY` is not an escape.** `cgr()` does skip history on it, but
`xfn()` returns `"skip_prompt_history"` from it, which makes `w1()` true and disables session
persistence entirely — the transcript authority pmux is built on. `claude_launch.rs` bails on the
variable for exactly that reason and must keep doing so.

**Cost of one root per cell, for a pool of 15.** The seed is ~300 bytes (two files). Measured
accumulation proxy: a 77-session, 65-clear probe cost 484 KB of transcripts plus ~29 KB of history
rows, so 5–20 MB per root steady state (transcripts under §8's 200-file cap dominate) and ≤300 MB for
15 roots — about 4% of the 7.6 GB RSS the same 16 instances MEASURED (§10.6). Seeding is two atomic
write+fsync+rename+dirfsync sequences against a 4.4 s relaunch: noise. pmux holds no file descriptor
per root after seeding. And `MAX_PROJECT_DIRECTORIES = 10_000` stops being a scaling concern at all,
because a per-cell root contains exactly ONE project directory — which retires §8's caveat about pool
size adding project directories to one shared root.

`SeedDisposition::VerifyOnly` and `SeedOutcome::AlreadySeeded` stay. They are the sole-writer race
protocol (§5.3) and shared or reused roots remain legitimate for **Path A** held sessions, where the
caller owns its own accumulation. For a minified start they are structurally inapplicable: sharing is
the thing being forbidden.

**The conjunction is verified as a conjunction, standing.** Enumerating five closed channels is a
reading, and every leak found so far was found by reproduction. `crates/e2e/tests/cross_cell_contamination.rs`
turns the claim into a sweep: N cells, N distinct unguessable secrets, one daemon, concurrent, then
every byte reachable from each cell searched for every other cell's secret, session id, transcript id
and cwd — across a channel TABLE rather than a scenario, with anything unnamed swept and reported by
name. It runs against the deterministic double at N=2/5/15 and against real Claude behind
`PMUX_CONTAMINATION_REAL_CLAUDE=1`, because the double reproduces neither the composer, nor
`history.jsonl` recall, nor the paste cache. Its ability to fail is itself a test (see `docs/testing.md`
S-37). Note what the real lane MEASURED and the double cannot: `history.jsonl` is written per root and
carries the typed prompt verbatim, so the per-cell root of item 2 is the only thing standing between
one caller's prompts and the next caller's up-arrow.

---

## 6. Recycle policy

**`/clear` resets context but does NOT reclaim memory.** That single sentence is the entire reason
recycle exists. A cleared instance is semantically fresh and metabolically old.

MEASURED: **375MB RSS at boot**, **+1.86MB per turn sustained**, **linear across instances** (16
processes = **7777MB**, i.e. 486MB each at turn 15). **No copy-on-write sharing** was observed, so
per-instance cost is genuinely per-instance and the pool budget is a plain multiplication.

**Predicate: recycle when RSS >= 1024MB (CHOSEN) OR turns served >= 250 (CHOSEN), whichever comes
first.**

**What SHIPPED, and how it differs (`crates/service/src/pool/config.rs`).** The design's two
predicates are both present, but the turn cap became an operator knob whose **default is 50, not
250**, with 250 as the `MAX_RECYCLE_TURNS` ceiling so the knob cannot be turned into "never
recycle". At the shipped default, DERIVED from the same measured `375 + 1.86n`:
`375 + 50 x 1.86 = 468 MB` expected per instance, against a 1024 MB per-instance ceiling that the
turn cap makes arithmetically unreachable. **So `RSS_CEILING_MB_PER_INSTANCE` gates nothing at
runtime today; it is a BOOT ASSERTION about how the host was sized** (`rss_budget_mb` defaults to
`pool_size x 1024`). Anyone reading §7 as a live admission gate should read this sentence first. The
other shipped constants, all CHOSEN: `MAX_POOL_SIZE = DEFAULT_POOL_SIZE = 15`,
`DEFAULT_INSTANCE_IDLE_TTL_MS = 300_000` (five minutes — long enough that a bursty caller keeps its
warm class, short enough that a cold class returns its slot within one coffee),
`MAX_SYSTEM_PROMPT_BYTES = 512` (§2.3).

The arithmetic below is retained at **250** because that is the ceiling an operator may legitimately
set, and it is the number the worst case must be sized against.

**DERIVED arithmetic.**

- Turn count reaching the RSS ceiling: `(1024 - 375) / 1.86 = 649 / 1.86 = 349 turns`.
- RSS at the 250-turn cap: `375 + (250 x 1.86) = 375 + 465 = 840MB`.

So under MEASURED growth the **turn cap binds first**, at 840MB — comfortably inside the ceiling.
The RSS ceiling is not the working limit; it is the **backstop for growth that is worse than
measured**. A workload that grows faster than 1.86MB/turn gets recycled early and automatically,
without anyone re-deriving a constant. Two predicates, one binding and one defensive, is the point.

**DERIVED amortisation of the ~4.4s relaunch.**

- At the 250-turn cap: `4400ms / 250 = 17.6ms per turn`.
- At the 349-turn RSS-limited maximum: `4400ms / 349 = 12.6ms per turn`.

The round "~10ms/turn" figure requires ~440 turns (`4400 / 440 = 10ms`), which would put RSS at
`375 + (440 x 1.86) = 1193MB` — **over the 1GB ceiling**. So the honest number is **13-18ms/turn,
order 10ms**, and the ~10ms figure is the optimistic end of that band, not a measured result. Even
at the pessimistic end, 17.6ms is **~3.2% of a MEASURED ~550ms Path B turn** (§10) — a real cost,
small enough that it is never worth raising the turn cap to shave it.

**Recycle mechanics.** Recycle is evaluated at **check-in**, after the result is delivered and after
rebind. An instance past a predicate is not returned to IDLE; it is torn down and a replacement is
launched. Recycle therefore never occurs mid-turn and never delays a result. During the ~4.4s
relaunch the pool is down one instance, which is a **capacity** cost, not a **correctness** cost —
admission (§7) must be sized so that losing one instance to recycle cannot make the pool refuse
work it would otherwise accept, i.e. keep at least one spare above the working set.

---

## 7. Admission control

**Path B admission is memory-budget based, and above budget it REFUSES rather than degrades.**
Degrading — admitting the turn and hoping, or running the pool into swap — converts a clean refusal
into a slow, possibly wrong turn. Under the governing asymmetry a refusal is merely bad; a turn that
returns from a thrashing instance is unacceptable.

The budget must be sized against the **worst RSS an instance can reach before recycle**, not against
the steady-state figure, because every instance is entitled to reach the cap.

MEASURED anchor: **486MB/instance at turn 15**; 16 instances = **7777MB** (measured directly, not
extrapolated).

| Instances | At turn 15 (486MB ea.) | At the 250-turn cap (840MB ea., DERIVED) | At the 1GB ceiling (1024MB ea., DERIVED) |
|---|---|---|---|
| 8 | 3888MB (3.8GB) | 6720MB (6.6GB) | 8192MB (8.0GB) |
| 10 | 4860MB (4.7GB) | 8400MB (8.2GB) | 10240MB (10.0GB) |
| 16 | **7777MB (7.6GB), MEASURED** | 13440MB (13.1GB) | 16384MB (16.0GB) |
| 20 | 9721MB (9.5GB), extrapolated | 16800MB (16.4GB) | 20480MB (20.0GB) |

Only the 16-instance turn-15 cell is measured. Every other cell is linear extrapolation from
`375 + 1.86n`, justified by the MEASURED linearity across 16 processes and by the absence of
copy-on-write sharing. **Extrapolation beyond 16 instances is not evidence**: 20 has never been run.

**The rule.** `pool_size x 1024MB` (the ceiling, not the steady state) must fit inside the operator's
configured Path B memory budget, with headroom for Path A cells (§8) and for the host. Sizing from
the turn-15 column is the mistake this table exists to prevent: an operator who budgets 7777MB for
16 instances is budgeting for turn 15 and will be over budget by turn 100.

Above budget, `start`/checkout returns a refusal naming the budget, the current pool RSS, and the
per-instance ceiling used. No queueing behind a memory wall, no silent shrink of the recycle
constants.

### 7.1 The slot budget: refuse, but not for housekeeping

**What actually shipped, stated so this section is not read as describing it.** The pool refuses at
the **slot cap** and names the budget **in the message, not only in the details blob**, and nothing
queues. That refusal is real, tested and reproduced over the socket. **Admission by a measured
memory budget is NOT shipped**: `rss_budget_mb` is validated at boot and never consulted per
checkout (§6). This section is therefore still DESIGN for its central claim, and the table above is
still the right sizing instrument for an operator choosing `--path-b-pool-size`.

There is **no queue** — no order, no fairness, no wait list, no per-class quota — and there is a
**bounded wait**, and those are different things. The pool answers a caller *before* it types
`/clear` (§3.4), so the ordinary state of a pool one instant after a burst is *every slot clearing*,
with nobody waiting on any of it. Refusing a caller there is a **false capacity signal**: the pool
says "no instance is available for this turn" about instances that are about to be.

MEASURED at 8 concurrent callers across 4 classes against 3 slots, over 3 rounds, before this:
**21 of 24 refused**, rounds 2 and 3 finishing in **782 µs and 539 µs**, and **3 launches for 3
served calls** — so no instance ever served a second caller and the wave's fungibility claims were
all vacuous. Each refusal read `3 clearing between turns, with no caller waiting, 0 idle`.

**The predicate.** `CensusBucket::comes_back_on_its_own` — `Clearing` and teardown come back with no
caller's help; `Serving` is holding a model and `Reserved` is a launch already spoken for. A caller
refused for the first two waits; a caller refused for the last two is refused on the first read, and
its refusal publishes `admission_wait_ms: 0`.

**The bounds, both hard.** `ADMISSION_WAIT_CEILING_MS` (2500 ms — a clear MEASURED at 703–756 ms end
to end at the double's 50 ms drain, ~1700 ms at the 1000 ms drain the promoted 2.1.220 profile
ships, and half again on top), and the caller's own `deadline_unix_ms`, resolved before admission and
re-read every pass. The smaller wins. Every outcome is still a refusal or a correct answer.

**The cold swap moved with it.** Rule 3 destroys an instance the pool has proven clean and pays a
full mint for the replacement; it is the alternative to *refusing*, not to *waiting*. It is deferred
while something is coming back **and** the caller still has budget, and it fires unconditionally on
the caller's last look — so "no caller is refused while another class sits idle" is unchanged, and a
pool holding nothing but idle instances of another class still swaps on the first read at no added
latency. Without the deferral, a waiter of a mismatched class always beat the matching caller to a
freshly idle slot: MEASURED **7 launches for 7 served calls** — every call served by a process just
built, having destroyed one just proven clean. With it, the same wave is **9 served from 4 launches**,
15 refused, 10 runs in 10.

**Two measured defects in the refusal itself, both in something that reported success**, because
they are the shape a future reader will re-create:

1. `in_flight` spanned `CheckedOut | Delivering | Clearing` and the census rendered it as "{n}
   serving a turn". MEASURED over the socket at 8 concurrent against 8 slots: *"8 of 8 usable
   instance(s) are live -- 7 serving a turn"* **at an instant when zero were**. `Clearing` is the
   DOMINANT refusal under load, precisely because `spawn_clear` exists so the caller is answered
   *before* `/clear` is typed — so a caller that retries immediately meets a pool whose slots are all
   clearing, work MEASURED at 703-756 ms end to end against the test double's 50 ms drain, and
   re-measured 2026-08-09 against **real Claude 2.1.226 at 682-860 ms, median 792** over seven
   clears (`docs/2.1.226-acceptance.md` §3.1) — no material change, and the 62 ms shift in the
   median is inside the sampler's own 60-90 ms granularity. The ~30 ms in §3.4 is the transcript
   rotation alone. Reported as however long a model takes. `live` is now the SUM of
   the five printed clauses rather than a separate count, and every bucket gets its phrase from a
   wildcard-free match.
2. `pool_exhausted` claimed *"Rule 7 fires iff every instance is mid-turn"*. Rule 4 also fires with
   instances in `Reserved`, `Warming`, `Quarantined` or `Destroying`, none of which `in_flight`
   counts, so a pool refusing with both instances in teardown rendered "serving 0 of its 2 configured
   instances". Both call sites passed `pool_size` where the **budget** belonged, so after a leak the
   message overstated the budget permanently.

Both are the governing rule applied to a *message*: a refusal that names a state it did not test is
the same defect as a guard whose message promises more than its predicate tests.

---

## 8. Retention and pruning

`/clear` leaves accumulated abandoned transcripts — one per clear, per instance cwd.

**Scope note added 2026-08-03:** everything below is about `projects/`. It is not the whole of what a
cell leaves on disk. `paste-cache/` in particular is content-addressed, mode 0600, NOT project-scoped
and cleaned on mtime alone, so a caller prompt over 1,024 characters persists there verbatim and
OUTLIVES the transcript that carried it (§5.6). Under the per-cell root mandate the operative
retention boundary for a Path B instance is the root, deleted whole at recycle after the obligations
below have been discharged against it; the byte thresholds at which a pmux bracketed paste is stored
out of line rather than inline are pinned only at the storage threshold (`ruy=1024`) and should be
probed before this policy is extended to cover the cache directly.

**Hard invariants. Neither may be relaxed by a tuning knob.**

1. **Never prune the currently bound file.** The bound file is the semantic authority for a live or
   just-finished turn.
2. **Never prune a transcript before its turn's result has been delivered to the caller.** A file
   that still backs an undelivered result is evidence, not garbage.

**Policy above those invariants (both CHOSEN):**

- **Time floor: retain every transcript for at least 60 minutes after its abandonment.** Long enough
  that an operator investigating a `ClearRebindTimeout` or an odd result still has the file.
- **Count cap: at most 200 abandoned transcripts per instance cwd**, oldest pruned first, and only
  those past the time floor. If the cap and the floor conflict, **the floor wins** and the directory
  is allowed to exceed the cap; losing evidence is worse than holding disk.

**Pruning is NOT a latency requirement.** MEASURED: `/clear` cost stayed **flat at ~30ms across 61
accumulated files**, so accumulation does not slow the mechanism it feeds. Anyone who later justifies
more aggressive pruning on performance grounds is arguing against a measurement. Pruning is **purely
an audit and disk policy**.

One structural note: the per-instance cwd (§4) means accumulated files pile up *within* one project
directory, and `TranscriptLocator`'s bounded fallback scan is bounded on the number of **project
directories** (`limits.project_directories`, `crates/claude/src/locator.rs:228`), not on files
within one. The primary lookup is a direct path construction. So file accumulation does not approach
that limit. **Pool size no longer adds project directories to one shared root either**, because
§5.6 makes each cell's root private and a per-cell root contains exactly ONE project directory. The
earlier caveat here — that an operator raising the pool size should confirm the directory scan limit
still clears it — applied to the shared-root layout and is retired.

---

## 9. Mixed operation: Path A and Path B in one daemon

| | **Path A — HELD** | **Path B — BORROWED** |
|---|---|---|
| Binding | **Sticky**: a session belongs to one caller for its lifetime | **Fungible**: any instance serves any turn |
| Tools | Tool-capable | **Tool-less** (`--disallowedTools "*"`) |
| State | **Stateful**: context accumulates across turns | **Stateless**: `/clear` between turns |
| Latency | **1,213 ms median** at the client clock against a *zero-latency* driver, n=60 — see §10.1 | **1,955 ms median**, same clock, same daemon, same run, n=60 — **NOT faster, and 741 ms slower**. For real model turns see §9.1 |
| Cwd | Caller's project | Private empty pre-trusted dir |
| Config root | Caller's own, or a shared private one | **Private, per cell, empty at launch, deleted at recycle (§5.6)** |
| Writable attach | Permitted | **Refused (§3.5)** |
| Failure of pool exhaustion | N/A | Refuse at the slot cap, naming the budget (§7) |

### 9.1 What a real Path B turn costs, MEASURED at concurrency

The figures in the §9 row above are pmux machinery, not turns. Real Claude, **sonnet/low, 61 turns**
at 2, 5 and 8 concurrent — a cold wave then a warm wave against one daemon, on 2.1.220:

| Concurrency | Cold median | Warm median |
|---:|---:|---:|
| 2 | 6,638 ms | 3,186 ms |
| 5 | 7,182 ms | 3,302 ms |
| 8 | 10,225 ms | 3,471 ms |

Every turn answered its own unguessable token, no answer carried another caller's, and the pool
parent was empty after every shutdown. **The 8-way run is the largest live concurrency this product
has been driven at.**

**Read the two rows together and do not average them.** The §9 row measures pmux's own machinery
against a driver with no model in it; this table measures whole turns including the model, and the
gap between them is model latency plus the warm/cold launch difference. Neither number is a target;
`docs/archive/current-state-2026-08.md` §6.4 is normative for why no latency target is gated.

#### 9.1.1 Where a warm turn's milliseconds actually go — MEASURED, both clocks, same turn

The table above and the §9 row were taken by different harnesses months apart, which is why the
relationship between them was §13's item 2 rather than a fact. It is now measured directly: **n=20
sequential warm Path A turns, real Claude 2.1.220, sonnet/low, one persistent session**, against the
promoted profile's own `transcript_drain_ms: 1000`, on `macOS 15.7.7 / Darwin 24.6.0 / arm64`, load
average 5.93. Receipt: `evidence/turn-latency-2.1.220-macos-aarch64.json`.

| Quantity | min | median | max |
|---|---:|---:|---:|
| `pmux turn` wall clock, client side | 2,966 ms | **3,583 ms** | 4,995 ms |
| `completed_at_ms - submitted_at_ms`, server side | 2,957 ms | **3,574 ms** | 4,983 ms |
| leg 1 — the input gate (`submitted` → `prompt_acknowledged`) | 646 ms | **675 ms** | 703 ms |
| leg 2 — **generation** (`prompt_acknowledged` → `terminal_candidate`) | 1,729 ms | **2,326 ms** | 3,729 ms |
| leg 3 — the commit gate (`terminal_candidate` → `completed`) | 526 ms | **552 ms** | 580 ms |

**Read leg 2 as the answer to the old question and legs 1+3 as the correction to it.** §13 asked
whether ~2.6 s of a warm sonnet/low turn is model latency. Measured: **2,326 ms median is
generation** — close, and the first time it has been observed rather than inferred. What was wrong
is the other side: pmux's own machinery on a warm turn is **legs 1+3 = ~1,227 ms**, not the
~550 ms the retired band implied, and it is split almost evenly between typing the prompt in and
proving the answer is finished. The client clock costs a further **9 ms median** over the server's
own view, which is `pmux`'s process spawn plus the socket round trip — small, and now stated rather
than assumed away.

**What leg 1 is NOT.** ~154 ms of that 675 ms is Claude echoing the typed prompt back into the JSONL,
not pmux waiting. The same code against `pmux-test-claude` spends ~91 ms of its 646 ms inside the
double's own `ensure_no_queued_input` guard, and removing that guard's 100 ms poll drops the double's
input gate to 556 ms median with both screen gates unmoved. pmux's own input-gate machinery is
~535 ms on both drivers; the rest of leg 1 belongs to whatever is on the other end of the pty.
`docs/archive/current-state-2026-08.md` §6.1.2 is the decomposition and the A/B.

**Path B, same run, same daemon, real Claude:** `pmux ask` wall clock 3,610 / **3,957** / 6,381 ms
over n=20. Consistent with the 3,186 ms warm median above at concurrency 2 to within the difference
between a held session and a pool checkout plus a `/clear`.

**What the deterministic double establishes and what it cannot.** Thirteen waves at 2/5/8/15
concurrent, four classes, against a real daemon, a real private rmux sidecar and one
`pmux-test-claude` per instance, **with zero wrong answers**, including kills mid-turn and
mid-`/clear`. Everything about admission, class routing, checkout, recycle, the cap and teardown is
established BY THE DOUBLE. The real lane establishes only that real turns complete concurrently and
what they cost, at 2/5/8. **Nothing in either lane says anything about Ink frame geometry, which the
double does not render** — that is the screen corpus's job (§10.7).

**Fungibility is proven from the CHILD side, and the first four waves proved nothing while
passing.** `StatelessResult::model` is the class key copied out of the request path, so asserting it
proves nothing: a pool that answered every `opus/max` call from a `haiku` process would still
publish `claude-opus-5`. The harness joins `prompts.jsonl` (which process received which prompt) to
`launches.jsonl` (which argv that process was launched with) on `cwd`, and reads the argv **whole**.
And 15 callers against 15 launches is a pool that mints one instance per caller, which cannot
mis-route one — so every fungibility check was true for a reason unrelated to routing until
`claim_reuse_was_exercised` began asserting that some instance served two different callers.

Both cell types run under the **same** `require_tested` compatibility gating (`spec.md` §1). Path B
is not a bypass: an instance whose normalized Claude version, OS/architecture, terminal profile, and
resolved input transport are not admitted together does not launch, exactly as for Path A. A pool is
a multiplier on cells; it is not an admission authority, and it must never become one.

Routing rule: a turn requiring any tool, or any continuity with a prior turn, is Path A. Path B is
for self-contained input -> output work. Path B must **refuse** a turn it cannot serve rather than
silently routing it to Path A with different semantics — the caller asked for a stateless cell and
is entitled to know it did not get one.

---

## 10. What is still UNVALIDATED

Stated plainly, because the rest of this document reads as more settled than the evidence is.

1. ~~**The ~371ms end-to-end projection is unmeasured.**~~ **MEASURED — and then MEASURED AGAIN,
   because the first measurement's own numbers could not be defended.**

   **What was here before, and why it is gone.** This item used to read *"Path B 540-575ms median,
   528-636ms band. Path A through the same pmux: 535.5ms"* from *"n=14 spot-check turns plus ~35
   more across the smoke matrix"*, and §1 carried **571 ms** for the same Path A quantity. No argv,
   no receipt, no commit and no harness for either figure survives anywhere in this repository — the
   two could not be told apart, and neither could be regenerated. **They are not reconciled. They
   are replaced, along with the band, by a measurement that ships its own method.**

   **THE MEASUREMENT OF RECORD.**

   **What is timed.** Two clocks, per turn, because they answer different questions and conflating
   them is how one quantity came to have two values:

   - `server_total_ms` = `TurnTimings.completed_at_ms - submitted_at_ms` — the daemon's own view of
     one turn, from accepting the prompt to committing the result.
   - `client_wall_ms` = `time.monotonic()` around one `pmux turn` (or `pmux ask`) **process** —
     spawn, connect, request, response. Always the larger. It is what a shell waits.

   **How.** `tools/promotion/measure_turn_latency.py`. One `pmuxd` in an owner-private `/tmp`
   sandbox; **one** persistent Path A session, so no sample pays a launch; then N sequential turns
   through the shipped `pmux` binary with `--output json`; then N sequential `pmux ask` calls
   against a pool of one instance with a warm floor of one, so no Path B sample pays a mint either.
   Three warm-up turns are discarded and recorded as discarded. Percentiles are nearest-rank, so
   every published number is a value that was actually observed.

   **The one configuration choice that matters, stated because it moves the answer by 858 ms.**
   `graduated_drain_ms` (`crates/service/src/v1/backend.rs`) lowers a turn's stability requirement
   to `TURN_DURATION_DRAIN_FLOOR_MS` = 250 ms once Claude's in-band `turn_duration` marker is seen.
   Real 2.1.220 writes that marker on **20 of 20** measured turns, so a real turn against the
   promoted 1000 ms profile owes **250 ms**, not 1000. `pmux-test-claude` never writes one. The
   machinery receipt therefore runs the double at `transcript_drain_ms: 250` — the requirement a
   real turn owes, expressed in the only way a driver with no marker can owe it.

   **MEASURED 2026-08-06**, `macOS 15.7.7 / Darwin 24.6.0 / arm64`, 10 CPUs, load average 6.51,
   `pmux-test-claude` 9.9.9, **n=60** measured turns per path. Receipt:
   `evidence/turn-latency-double-macos-aarch64.json`.

   | Quantity | min | p10 | **median** | p90 | max |
   |---|---:|---:|---:|---:|---:|
   | Path A `server_total_ms` | 1,150 | 1,169 | **1,204** | 1,242 | 1,257 |
   | Path A `client_wall_ms` | 1,160 | — | **1,213** | — | 1,318 |
   | — leg 1, the input gate | 620 | — | **646** | — | 675 |
   | — leg 2, generation (there is no model) | 0 | — | **0** | — | 23 |
   | — leg 3, the commit gate | 526 | — | **555** | — | 603 |
   | Path B `client_wall_ms` (`pmux ask`) | 1,885 | 1,913 | **1,955** | 2,005 | 2,050 |

   **~91 ms of that 646 ms input-gate leg is the driver, not pmux.**
   `crates/e2e/src/bin/pmux-test-claude.rs`'s `ensure_no_queued_input` polls stdin for 100 ms
   between reading Enter and writing the typed `user` row — its own proof that pmux sent exactly one
   byte — and pays it in full on every passing turn. Re-run with that timeout at 0, n=30: input gate
   **556.0 ms** median, commit gate unchanged at 550.5, both screen gates unmoved. Quote 646 as the
   leg and ~535 ms as pmux's share of it; `docs/archive/current-state-2026-08.md` §6.1.2 is the measurement and the
   decomposition of the rest.

   **Path B is not faster than Path A. It is 741 ms slower at the client clock**, on the same
   daemon, in the same run, against the same driver — the pool checkout and the `/clear` that make
   an instance fungible. The old claim that the two were within ~40 ms of each other described
   something this tree does not do.

   **The drain is still not the binding constraint, and here is the number.** Same tool, same
   n=60, `--drain-ms` the only variable:

   | `transcript_drain_ms` | Path A `server_total_ms` min / median / max | commit-gate leg median |
   |---:|---:|---:|
   | 50 | 1,160 / **1,219** / 1,315 | 564 ms |
   | 250 (what a real turn owes) | 1,150 / **1,204** / 1,257 | 555 ms |
   | 1000 (the profile's own value) | 2,006 / **2,062** / 2,137 | 1,418 ms |

   Between 50 ms and 250 ms the medians differ by 15 ms — less than the spread of either sample — so
   **at every value a real turn actually owes, the drain is dominated by the screen-stability wait
   and contributes nothing**. It only becomes binding at 1000 ms, which is a requirement a real turn
   never pays because the marker lowers it. The retired conclusion was right and is now quantified:
   there was never a ~200 ms drain saving available to take.

   **What would invalidate this**, in the receipt itself and repeated here: a different driver,
   version, OS or arch; a change to `quiet_for` (`driver_io.rs`), which is what leg 3 is currently
   made of; a change to `wait_for_stable_control_render` or the composer gate, which is what leg 1
   is made of; a change to `graduated_drain_ms`; a new `TurnTimings` boundary — the tool **FAILS**
   on an unclassified `*_at_ms` field rather than publishing a total that silently excludes it, and
   that guard fired on the first real-Claude run, which is how `turn_duration_observed_at_ms` came
   to be read at all (§13 item 6).

   **One observation offered as an observation and not as a reconciliation.** The single leg of
   today's turn that lands in the 535-571 ms neighbourhood is the **commit gate** — 555 ms median
   against the double, 552 ms against real Claude. Nothing in the repository says that is what the
   retired figures measured, and this document does not claim it. It is recorded so the next reader
   does not spend the same hour rediscovering the coincidence.

   Path B earns its place as a **stateless, fungible, horizontally-scalable cell**, and that was
   never a latency claim. Future overhead work belongs on the **screen-stability path** — legs 1 and
   3, which are 1,201 ms of the 1,204 ms median — and not on the drain.

   **NARROWED 2026-08-07: half of leg 3 is not available, and neither is ~91 ms of leg 1.**
   `docs/archive/current-state-2026-08.md` §6.1.2 decomposes both legs by measurement. Of leg 3's ~555 ms, ~275 ms is
   reaching the drain requirement and the other ~275 ms is the **post-marker catch window** — the
   truncation guarantee itself, now named `POST_MARKER_CATCH_WINDOW_FLOOR_MS`, measured to cost a
   real 352 ms row, and defended by a compile-time refusal. Of leg 1's 646 ms, ~91 ms is the
   driver's own guard and 521 ms is two 250 ms screen windows. "Belongs on the screen-stability
   path" is still right about where the time is: it is the input gate's two windows, and each needs
   a measurement of Claude's own render before it can be moved.
2. **The recycle path has never been exercised AGAINST REAL CLAUDE AT THE CAP.** ~~No
   teardown-and-relaunch inside a live pool has run.~~ **SUPERSEDED in part:** recycle,
   teardown-and-relaunch inside a live pool, and reuse of one instance by two different callers are
   all exercised — 13 deterministic waves at 2/5/8/15 concurrent against a real daemon and a real
   sidecar, with `expect_reuse` DERIVED from `rounds > 1 && recycle_turns > 1` rather than asserted,
   and `claim_reuse_was_exercised` failing the wave if no instance ever served two callers. **What
   is still unexercised is the two PREDICATES at their real values**: no instance has been driven to
   250 turns or to 1024 MB, and the RSS predicate is a boot assertion rather than a runtime gate
   (§6). The ~4.4 s relaunch cost is MEASURED for a TUI launch, and the pool's cost while down one
   instance is now visible in §9.1's cold-vs-warm gap rather than unknown.
3. **Shared-cwd ambiguity is argued, never demonstrated.** §4 justifies the per-instance cwd by
   reasoning about a ~39ms window and N concurrent clears. No shared-cwd collision has been
   observed. The per-instance cwd is cheap, so the design takes the structural guarantee rather than
   funding an experiment to prove a hazard it can simply avoid — but the hazard's magnitude is
   unknown, not zero.
4. ~~**The 2s-sampling blind spot on Claude's own `.claude.json` writes** (§5.3): 0/58 samples clean
   at 16 instances rules out persistent corruption, not a sub-2s torn write.~~ **CLOSED BY
   MECHANISM, 2026-08-06 — see §5.3. Claude replaces that file rather than rewriting it, so a torn
   read is not rare, it is impossible.**
5. **Pool scale beyond 16 instances is extrapolation.** MEASURED: 16 instances x 15 turns = 240/240
   turns OK, **zero cross-talk**. Rows in the §7 table for 20 instances, and every column past turn
   15, are linear projections from `375 + 1.86n`.
6. **`/clear` behavior is pinned to Claude Code 2.1.220** — ~~and has been measured at that version
   only~~ **UPDATED 2026-08-09: it now reproduces at 2.1.223 and 2.1.226 as well.** The rebind
   anchor (row 0 is a `mode` row), the 5-row write, the +39ms appearance, and the session_id
   rotation are all version-observed facts. `docs/2.1.226-compatibility.md` §3.2 re-measured the
   preamble on a raw 24x80 PTY at both later versions and every predicate held — the same five
   rows, in the same order, row 0 a `mode` row — and `docs/2.1.226-acceptance.md` §3 drove a real
   `/clear` at 2.1.226 and watched context die across it. **What is still 2.1.220-only is the
   +39 ms appearance latency**, which §3.2 observed as an appearance but never timed. The row-0
   check in §3.5 step 4 is what turns a change in any of them into a quarantine rather than a
   wrong answer, and that is the property this item exists to protect, not the version number.
7. **The `/clear` composer selection: the menu geometry HAS now been measured, and the residual risk
   is narrower than this item used to state.** It read *"the command menu's geometry has never been
   measured"*. **That is no longer true.** What was measured, and what each finding cost:

   - **The menu highlight is COLOUR-ONLY**: `fg=idx153` for the selected entry against `idx246` for
     the rest. No glyph, no reverse video. `terminal_snapshot` discarded the cell grid, so the
     highlight was **absent from pmux's data**, not merely hard to read — `/clear` was pressing
     Enter hopefully. The styled read is now widened; `TerminalSnapshot` itself is untouched because
     its equality is the input gate's fence.
   - **The selection is a fuzzy score over NAMES AND DESCRIPTIONS, not an alphabetical list.** At
     prefix `/c` the selected entry is **`/cd`** ("Move this session to a new working directory").
     `/doctor` is a candidate at prefix `/cl` because its *description* contains "Claude". Any
     reasoning of the form "`/clear` sorts first" is wrong.
   - **The composer gate was measuring from the wrong edge.** It measured from the physical bottom of
     the GRID; Ink does not always paint to the bottom, and after `/clear` the frame is four rows
     tall and top-anchored, so `24 - 5 - 1 = 18 > 4` and the editor was never found. Measured from
     the **last rendered row** instead: **2 rendered rows below the composer in 85/85 live 2.1.220
     screens and 5/5 recovered 2.1.70 fixtures**, enforced at four. **This was never `/clear`-specific
     — the same defect was failing plain second turns on PATH A**, intermittently, and it survived
     four review rounds because it is not findable by reading.

   **The selection is now proven before Enter.** `prove_control_command_selection` refuses to
   submit unless exactly one menu candidate has the typed token and a body colour equal to the
   composer's typed-command colour. 2.1.220/2.1.227 paint candidates below the composer at
   column 0; 2.1.238 paints them above the upper U+2500 rule with a two-space indent, and
   unselected rows are also uniform, so uniqueness-of-uniform-colour is not the discriminator.
   `wait_for_stable_control_render` remains weaker than the prompt gate. `wrong_local_command`
   remains the post-hoc detector if a rotating command still was not `/clear`. Residual: a
   pre-menu frame whose history already contains a unique composer-coloured `/clear` is the
   window the no-menu fixture refuses; a command that neither rotates nor writes a transcript
   still surfaces as `clear_rebind_not_observed` if the proof was skipped.

   **The standing machinery that replaced "measure it once".** The screen corpus records every
   `TerminalSnapshot` and `StyledScreen` pmux already reads, to versioned NDJSON stamped with Claude
   version, OS, arch and geometry, and replays it against the parsers with no Claude on the box
   (`crates/service/src/screen_corpus.rs`, off unless `PMUX_SCREEN_CORPUS_DIR` is set; a full queue
   DROPS rather than blocking the 25 ms poll it observes). `screen_properties.rs` asserts properties
   rather than outputs — the load-bearing one being that **appending blank rows below the frame must
   not change the verdict**, which fails on the FIRST generated case against the pre-fix expression.
   Note the anti-vacuity finding that came with it: replaying the corpus through the BROKEN composer
   gate **passed**, because every geometry invariant was conditional on the classifier's own verdict,
   so a classifier that stops saying `Ready` satisfies all of them by having no cases left.
   `CorpusFrame::expect_ready` is the unconditional half.

   The local-command geometry exercise remains scoped and NOT run, deliberately and for a stated
   reason: `ControlCommand` is a single-variant payload-free enum, so `/clear` is the only slash
   command pmux can ever type, and the only question worth wall-clock is the menu geometry AROUND
   `/clear` — not 85 commands (`tools/screen-corpus/local_command_geometry.md`).
8. **Assert-empty proves the transcript is clean, not that the model's context is.** §3.4 asserts
   "context is genuinely cleared" as a MEASURED fact about 2.1.220, not as something any check
   verifies. If a future release rotated the file but retained context server-side, every check in
   this design would report green. The transcript is a proxy, and the proxy is unfalsified rather
   than proven. Non-transcript state survives a clear by design: `turn_duration` (MEASURED 60/60),
   the `--system-prompt` replacement, and RSS — which is the entire reason §6 exists.

### Found by driving pmux end to end (no ordinal spent) — all three now CLOSED

These three were numbered 7-9, colliding with items 7-8 above. They are renumbered E1-E3 and their
current standing is stated first, because each one was quoted downstream while it was open.

**E1 — ~~The launch bundle is INCOMPLETE and does not work as written.~~ CLOSED.** Every Path B turn
used to die at launch with `UnsupportedClaudeVersion — Claude Code 2.1.220 has no tested pmux
compatibility profile for macos/aarch64, Transparent, Sdk` unless `--compatibility allow-untested`
was passed. The diagnosis was right and the cause was correctly identified as the empty promoted
registry rather than the three inexpressible flags. **`compatibility::PROMOTED_PROFILES` now ships
that cell** (§5.5), so a supported host needs no flag; MEASURED with `--tested-claude-profile`
ABSENT, `served in 4540ms by claude 2.1.220`. `allow-untested` remains a development crutch that
swaps in a conservative 2000 ms drain ceiling and remains the wrong way to run anything formal.

**E2 — ~~Concurrent `pmux run` can permanently poison the daemon, and `pmux doctor` reported
`healthy: true` throughout.~~ CLOSED, and the health surface was rebuilt because of it.** The
boolean `healthy` lied through four real failures. `DaemonDiagnosis` now carries `layers` — one
entry per `HealthLayerName`, each `exercised` / `faulted` / `not_established`, folded so that **a
layer nobody reported is `unproven`**, built from an exhaustive match rather than a hand-written
array. Two layers were split apart because they fail differently: the **control plane** is "was a
connection made", the **private runtime** is "did the sidecar COMPLETE a dispatch-path exchange" —
and all four false-healthy reproductions failed at the second, because a sidecar that has been
stopped, killed or wedged still owns a socket that accepts. Concurrency is no longer speculative
either: §9.1's 13 waves through 15 concurrent, including kills mid-turn and mid-`/clear`, with zero
wrong answers.

Two corrections the rebuild itself needed, recorded because both are the governing rule applied to a
**detail string**:

- An empty set is vacuous **only when nothing declared it should be occupied**. A pool holding no
  instances passes; a pool told to hold two by `--path-b-warm` and holding none is `faulted`. The
  first version of the pool layer printed *"and the next call of any class mints one"* — a claim its
  predicate never tested, and false in the state that produced it.
- A daemon that DECLINES Path B is healthy. `sessions_layer` mapped "the registry holds no sessions"
  to `unproven`, and pool instances are deliberately absent from `DaemonDiagnosis::sessions` (their
  session id is the one name no client may learn), so a daemon serving only `pmux ask` reported
  `sessions: []` on every probe it would ever answer and `pmux doctor` exited 1 forever.

**E3 — ~~"every Path B session currently spawns MCP servers silently".~~ RETRACTED. MEASURED FALSE,
and it was never an observation.** It was an inference from *"pmux cannot pass
`--strict-mcp-config`"*. **A complete descendant inventory of the live `claude` PID, sampled every 50
ms across four cells, is exactly `security find-generic-password` and `caffeinate -i -t 300`. No
node, no python, no npx — in any configuration.** The private config root is what killed it. A
poisoned `.mcp.json` planted in the cwd does not produce a covert spawn either: it **blocks on an
approval modal**, i.e. a hang, which the cell's own `dontAsk` posture and liveness gate turn into
unavailability rather than a wrong answer.

The half of E3 that was true and stays true: the three flags **do not affect completion**, measured
across a bare expressible bundle, an empty `--mcp-config`, and a planted adversarial config.

**The half that was wrong, corrected 2026-08-09.** This paragraph went on to conclude that work on
`--strict-mcp-config` was *"deferrable for isolation claims too, not merely for completion-focused
ones, because there is no spawn for those flags to prevent"*. That is the §0.2 error in its final
form: **no spawn is not no reach.** A remote connector is an HTTP endpoint, the process inventory
could not see one, and a 2.1.226 cell was measured fetching the caller's account connector list at
startup. `--strict-mcp-config` is emitted now (§2.1); it was never a protocol-surface question,
because it is driver-owned argv. `--safe-mode` and `--setting-sources` remain out, with reasons in
§2.2 that do not rest on the process inventory. The isolation claim also rests on the standing
cross-cell contamination sweep
(`crates/e2e/tests/cross_cell_contamination.rs`, `docs/testing.md` S-37), which is a reproduction
rather than a reading.

---

## 11. Changes required outside this document

Reported, not made — file ownership is exclusive.

- ~~**`crates/service/src/driver_io.rs`** — `FileTranscriptSource` must honour the per-call
  `session_id`~~ **DONE.** The tail is armed and polled under the id it is handed, and re-arming is
  what follows a rotation.
- ~~**An internal-only `/clear` injection path**~~ **DONE** (§3.6). `ControlCommand` is private,
  payload-free and single-variant; `validate_prompt` is unchanged and the caller-facing `/` refusal
  stays exactly as it is.
- ~~**`--no-session-persistence` should be added to `FORBIDDEN_DRIVER_FLAGS`**~~ **DONE.** It is
  entry `--no-session-persistence` at `claude_launch.rs:49` in that list. It is inert in the TUI today, but if it were ever
  honoured it would delete the transcript — the one artifact pmux treats as authoritative.
- ~~**A distinct `ClearRebindTimeout` error code**~~ **DECLINED, for now, with a stated trigger.**
  The requirement §3.5 actually states — "it must not be reported as `TurnTimeout`" — is met by the
  existing `TranscriptUnavailable` + `clear_rebind_not_observed` diagnostic, which is strictly
  richer than a code. Adding an `ErrorCode` variant is *not* the same kind of change as adding a
  `Request` variant: old clients never *send* an unknown method, but they cannot refuse to *receive*
  an unknown error code, and both shipped clients hard-reject one — TypeScript via
  `requireEnumField(data, "code", …, PMUX_ERROR_CODES)`, Python via its `KNOWN_ERROR_CODES` check.
  A daemon that emitted it to an older client would make that client reject the whole response
  frame. **Trigger to add it:** when a Path B pool must branch programmatically on *which* rebind
  failure occurred and can therefore no longer key on `details.violation`, which is opaque JSON and
  not part of the pinned surface. **Required migration order:** widen `manifest.json` `error_codes`
  and ship the TS and Python accept-lists; let those releases reach every deployment; only then may
  the daemon emit it. Bundle it with any other closed-enum additions into one loud protocol event.
- **A capability signal on `Pong`.** A new client cannot today distinguish "old daemon, no
  `clear_session`" from "I sent a malformed `clear_session`": both are `InvalidConfig` from the
  handler's typed-parse recovery path. v1 has no capability negotiation, so a Path B caller must
  gate on `Pong.server_version`. This is a pre-existing gap that the new method surfaces and
  deliberately does not fix; fixing it is its own protocol event. **Still open**, and it now applies
  to `run_stateless` as well as to `clear_session`.
- **`PoolExhausted` as an `ErrorCode`** — **DECLINED**, on the identical closed-enum argument, with
  the same four-step migration order written down. The refusal names the budget in its message.

---

## 12. The product as it now is

Written last, in the present tense, because §§1-11 read as a design and a reader who stops before
here will not know what shipped.

### 12.1 The caller surface: `(model, effort, prompt) -> tokens`

`pmux run --model sonnet --effort low 'What is 2 plus 2?'` answers `4` with
`input_tokens=174 output_tokens=3`. **Nothing else is named on the way in.** The response object
carries exactly `model reported_model effort text stop_reason usage claude_version` — **no session
id, no cwd, no configuration root**. `RunStatelessRequest` denies unknown fields, so sixteen resource
names a caller might reach for are refused **by name**; `StatelessResult` publishes seven keys and no
id, which makes `attach_session`, `inspect_session`, `subscribe_events`, `cancel_turn` and
`close_session` **unconstructible** against a pool instance rather than merely refused. Same surface
through MCP `run_stateless`, driven against a live daemon with the answer joined to the child side so
a fabricated one fails.

`pmux mints every resource and the caller names none`: `launch_request_for` is a free function of
`MintSpec` and nothing else, which is the form in which that sentence is **checkable** — there is no
caller string in scope to leak.

### 12.2 The pool

`BTreeMap<InstanceClass, IdleSet>` plus a global counter. `--model` and `--effort` are launch-time
argv and `/clear` does not re-exec, so **"any instance serves any turn" is false once model and
effort are caller inputs** — the class key is what restores fungibility *within* a class, and it is
produced by the same `resolve_model_effort` call that renders argv, so the pool's model of an
instance cannot drift from the process. `AdmittedEffort` pairs each tier with its argv token on one
table, so no expression anywhere produces an `--effort` value from an `EffortLevel` alone.

Warming is an operator-declared warm set (`--path-b-warm`), high-water-mark re-warm when a checkout
empties a class, and an idle TTL that drains a cold class to its declared floor and no further. Cold
swap may still take a floor instance, because refusing a live caller to hold a speculative one is
starvation.

A Messages conversation may pin an instance in `Leased` between turns: the instance is not idle,
`/clear` has not run, and a stateless `ask` cannot steal it. `--path-b-messages-bind` is the
opt-in loopback facade in front of that pin. The default daemon still binds only its owner-only
UDS. `CensusBucket::Leased` is one of the six live buckets; `comes_back_on_its_own` is false for it.

**Teardown order is the guarantee**: close and require a *positive* reaping, then discharge
retention, then erase the tree, and only then release the slot. A close that cannot confirm reaping
**leaks the slot permanently and keeps the tree**, because a root a live process may still be writing
to is evidence. A quarantine keeps its evidence under `--path-b-retain-dir`; a clean recycle gets no
floor. `machine::shutdown_action` is total over `InstanceState` with no wildcard — which it was not,
and the gap was exactly the ordinary state: since the pool answers *before* it clears, the ordinary
state at the end of any burst is "every instance is `Clearing`", so a daemon stopped after serving
traffic skipped every instance it had just used and **left the whole config root of each on disk**,
carrying that caller's prompt, with `leaked` still 0 and nothing logged.

### 12.3 The health tree

`pmux doctor` is a VIEW: four local checks only a client can make, plus the daemon's own per-layer
findings, folded on `ProbeOutcome`'s severity order. Eight layers — configuration, control plane,
private runtime, launch broker, compatibility profile, pool, sessions, performance — each stating
**what it exercised, for every finding including `exercised`**, because a pass with an empty detail
is the boolean this replaced one level down. `LayerFinding` has four values: `NothingToExercise` is
**pass** and means the layer was reached, evaluated, and found to have no subject;
`NotEstablished` is **unproven** and means the subject exists and could not be reached. A layer that
is ABSENT is `unproven`. The performance envelope is READ from `PrivateRuntime::operation_timeout`
rather than restated beside it, because a constant here is a second copy of an enforced bound, free
to drift.

### 12.4 The promoted profile

**Two cells, and each is a RANGE, not a version.** `PROMOTED_PROFILES`
(`crates/service/src/compatibility.rs:484`) ships macos / aarch64 floor **2.1.220** through
tested-ceiling **2.1.238**, `transcript_drain_ms: 1000`; and linux / x86_64
`claude_version_floor` **2.1.227** (`crates/service/src/compatibility.rs:513`) through
tested-ceiling **2.1.236**, `transcript_drain_ms: 250`
(`crates/service/src/compatibility.rs:519`). `resolve` searches the
OPERATOR's cells first, so an operator profile for the same identity **overrides** it rather than
colliding.

**The drain is MEASURED, not CHOSEN, and it is no longer a per-version fit.** It was one: 438 ms max
over 456 turns in 189 real 2.1.220 transcripts, x2.28. `docs/version-drift.md` §P1 replaced that with
a **pooled** bound, because a per-version fit on a thin corpus produces a number that TRUNCATES
answers — 2.1.223's own free corpus fits **250 ms**, which is below the 438 ms arrival already
observed one version earlier. What ships is the same 438 ms maximum taken over **226 arrivals in 425
macos/aarch64 transcripts spanning 2.1.207 / 2.1.215 / 2.1.220 / 2.1.223**, doubled and rounded up to
a 250 ms step. The bound is also **priced**: the full drain binds only on the 166 of 385 `cli` turns
that carry no `turn_duration` marker.

Reachable arrivals are **structural end-of-turn rows** — `turn_duration` and `stop_hook_summary` —
and **no semantic row has ever followed an answer**. Every excluded arrival is excluded *with a
reason on the record*: `queue-operation` (the task queue is a harness feature),
`system/away_summary` (an interactive-session feature), a post-answer `user` row (a harness
injection), and `system/api_error`, which is stamped at the moment of the failure inside the turn and
so is retrospective rather than a post-answer arrival at all.

**2.1.226, 2.1.227, and 2.1.238 were each driven; 2.1.238 is the ceiling.**
`tools/promotion/promote_claude_version.py` ran nine ordered checks and five real minified-cell turns
at `claude-sonnet-5` low/high per version, and **generated** the `range_provenance` sentence the
profile ships; `every_promoted_range_is_the_sentence_its_promotion_receipt_generated` requires the
shipped copy to equal the one in the receipt for the CEILING and that receipt to read
`verdict: promotable`.

| ceiling driven | reachable arrivals | max | per-version fit, published and NOT shipped |
| --- | --- | --- | --- |
| 2.1.226 | 5 | 223 ms | 500 ms |
| 2.1.227 | 5 | 52 ms | **250 ms** |
| 2.1.238 | 5 | 54 ms | **250 ms** |

The fit is the row to read. 250 ms is below `POST_MARKER_CATCH_WINDOW_FLOOR_MS`, so a promotion that
fitted its own version rather than reading the pooled bound would have shipped a drain that
truncates answers — the §P1 hazard, reproduced on later ceilings (2.1.227, then 2.1.238) rather than argued.

The receipts are `evidence/pooled-transcript-drain-macos-aarch64.json` (the macos bound),
`evidence/promoted-profile-2.1.220-macos-aarch64.json` (the macos floor's original per-version receipt) and
`evidence/promotion-2.1.238-macos-aarch64.json` (the macos ceiling; previous ceilings
`evidence/promotion-2.1.227-macos-aarch64.json` and
`evidence/promotion-2.1.226-macos-aarch64.json` are retained and are what a range that stopped there
rested on). Linux is `evidence/pooled-transcript-drain-linux-x86_64.json` (the bound),
`evidence/promoted-profile-2.1.227-linux-x86_64.json` (the floor) and
`evidence/promotion-2.1.236-linux-x86_64.json` (the ceiling).
`tools/promotion/measure_transcript_drain.py` regenerates the pooled receipts and **fails on a row kind nobody
classified rather than defaulting**; a unit test binds each shipped drain to that OS's receipt so the two
cannot drift. Each receipt names what would invalidate it, which is the
part to read before reusing the number.

### 12.5 The screen corpus

The standing answer to "Path B drives a TUI, so every gate is a claim about geometry Claude can
change without notice". Two hook lines at the existing `gated_snapshot` / `gated_styled_screen` choke
points, so the screens pmux **already discards** become evidence; the frame is borrowed and cloned
only once a recorder exists, so the disabled path allocates nothing. It replays offline, in bulk,
with no Claude on the box. Two geometry claims had already been wrong and **neither was findable by
reading** — the composer gate's edge and the menu highlight's colour — and both died to a live
capture. See §10 item 7.

### 12.6 Geometry actually delivered

**MEASURED: panes rendered 24x80, not the 24x120 `bin/pmux/src/cli.rs` requested.** `create` had been
fixed for this and the *resize* path was left behind: `TerminalSession::resize` called
`pane.resize`, which for a single-pane window becomes `resize-pane -x/-y` — and a lone pane cannot
exceed the window it sits in, so 120 collapsed to 80 **and `resize-pane` returned success**. Every
resize after creation was accepted and silently clamped. The resize now takes a window handle, and
`private_runtime.rs::a_terminal_is_created_at_the_requested_geometry_and_not_the_rmux_default`
asserts the delivered snapshot against the request, starting from rmux's 24x80 default so the clamp
is an upper bound the test must grow out of.

This matters here and not only in `cli.rs`: **every minified-cell screen predicate in §10 item 7 is
calibrated against a real pane**, so a requested geometry that was fiction would have made the 85/85
composer corpus a measurement of the wrong screen.

### 12.7 The turn-latency receipt

The second thing in this repository that measures rather than asserts, built in the same shape as
§12.4's drain receipt and for the same reason: **this document carried two different numbers for one
quantity for months, and neither had an artifact behind it.** A number without a regenerable method
is a number the next reader has to take on trust, and trust is what produced 571 ms and 535.5 ms.

`tools/promotion/measure_turn_latency.py` stands up a daemon in an owner-private sandbox, runs N
sequential turns on one held Path A session and N `pmux ask` calls against a warm pool of one, and
emits one JSON receipt: host and load average, the exact `pmuxd` argv, the compatibility profile,
every per-turn sample, the per-leg distribution, and what would invalidate it. Two receipts ship:
`evidence/turn-latency-double-macos-aarch64.json` (pmux's machinery, against the zero-latency
driver) and `evidence/turn-latency-2.1.220-macos-aarch64.json` (real turns, both clocks). §10.1 and
§9.1.1 are their prose.

**The property worth copying is the refusal.** The leg table is checked against the shape the daemon
returned: a `*_at_ms` field the tool has never classified **fails the run** rather than being
dropped from a total that still calls itself a total — the same rule `measure_transcript_drain.py`
applies to a row kind nobody classified. That guard fired on its first real-Claude run, on
`turn_duration_observed_at_ms`, which is how §13 item 6 came to be answered from an instrument pmux
already shipped instead of from a new one.

---

## 13. RECONCILED — the seven, each with the probe that settled it

This section used to be **UNRECONCILED**, seven claims this document could neither confirm nor
refute. All seven were taken up on **2026-08-06** on `macOS 15.7.7 / Darwin 24.6.0 / arm64` against
Claude Code 2.1.220, and **all seven are now closed**. Three of them did not close the way they were
written, and those are the three worth reading:

- **Item 1 was not a choice between two numbers.** Neither had an artifact, so neither could be
  defended even if it had been right; both are replaced by a receipt.
- **Item 4 asked for a comparison pmux structurally forbids.** There is no such thing as a warm
  private root for a minified cell — the daemon refuses one — so "time a mint into a warm root
  against a cold one" is not an experiment this product permits, and saying that is the answer.
- **Item 7's own premise was half wrong.** It recorded that `ultracode` "warns on stderr and falls
  back to the default". Reading that stderr shows it does neither.

Two rules from §0.3 did visible work here and are worth naming before the list. **One variable per
probe**: every real-Claude arm below runs through the *same* shim that execs the operator's own
Claude, so the shim is in both arms and is never the variable. **A negative result needs a positive
control**: the `.claude.json` sampler that saw no torn write was first shown to *see* one.

**What this section is NOT.** Closing seven items is not a claim that nothing is unknown. §10 still
holds nine open items of its own, and three residuals are named inside the entries below — the
`ultracode` tier is undocumented and one release from becoming a silent default (item 7), 20 turns
is evidence and not proof that `turn_duration` is always last (item 6), and the `.claude.json`
mechanism is Claude's and can change under us (item 5). A future pass that finds one of those false
should expect to find it the way these were found: by running the thing, not by re-reading it.

1. ~~**Path A's latency anchor: 571 ms or 535.5 ms?**~~ **NEITHER, AND NEITHER IS QUOTED ANY MORE.**
   No probe, receipt, harness or commit behind either figure survives in this repository, so there
   was nothing to reconcile — one of them being right would still have left a number nobody could
   regenerate. **Replaced, not chosen between**: `tools/promotion/measure_turn_latency.py` and
   `evidence/turn-latency-double-macos-aarch64.json`. Path A through pmux against the zero-latency
   driver is **1,204 ms median server-side over n=60** (1,150 / 1,169 / **1,204** / 1,242 / 1,257
   for min / p10 / median / p90 / max), split 646 ms input gate + 0 ms generation + 555 ms commit
   gate — of which **~91 ms of the input gate is the double's own `ensure_no_queued_input` guard
   rather than pmux** (`docs/archive/current-state-2026-08.md` §6.1.2). Path B on the same clock in the same run is
   **1,955 ms**, i.e. 741 ms *slower*. §10.1 is the full method and the invalidation list; §9 and §1
   now quote it.

2. ~~**How the 540-575 ms band relates to §9.1's 3,186 ms warm median.**~~ **MEASURED, both clocks,
   the same turns.** n=20 sequential warm real turns, sonnet/low, one held Path A session, promoted
   drain: **3,574 ms median server-side, 3,583 ms at the client clock**, of which **2,326 ms median
   is generation** and **1,227 ms is the two pmux gates** (675 ms input gate + 552 ms commit gate).
   The old hypothesis — "consistent only if ~2.6 s is model latency" — is close on the model side
   and **wrong on pmux's side by more than a factor of two**: the machinery was never ~550 ms. See
   §9.1.1 and `evidence/turn-latency-2.1.220-macos-aarch64.json`. The client clock adds a further
   9 ms median for process spawn and the socket round trip. **AMENDED 2026-08-07: not all 1,227 ms
   is pmux's to spend.** ~154 ms of leg 1 is the driver echoing the typed prompt back into the
   JSONL, so pmux's own machinery is ~1,073 ms. Measured on the double, where the same slot is that
   driver's own `ensure_no_queued_input` guard: removing its 100 ms poll drops the input gate 645 →
   556 ms median with both screen gates unmoved. `docs/archive/current-state-2026-08.md` §6.1.2.

3. ~~**Whether the `--safe-mode` FLAG breaks a cell the way `CLAUDE_CODE_SAFE_MODE` does.**~~
   **MEASURED: IT DOES NOT.** Probe: a `/bin/sh` shim that `exec`s the operator's Claude, handed to
   `pmux start --claude <shim> --cell minified` with a fresh private config root per cell — a real
   minified cell over the real socket, not a bench. **Control arm** — shim without the flag —
   5 cells: 5/5 reached `ready` and 5/5
   answered their own unguessable token. **Probe arm** — the same shim with `--safe-mode` prepended
   and nothing else changed — 5 cells: **5/5 reached `ready`** (start 1,108 / 1,125 / 1,186 / 1,243 /
   1,190 ms, against the control's 1,059-1,473 ms) and **5/5 answered their own token**. So the flag
   and the environment variable are **not** interchangeable: `CLAUDE_CODE_SAFE_MODE` breaks the cell
   5/5 (§2.3) and `--safe-mode` breaks nothing. §2.2's row stands as written — the flag was removed
   for a product reason, not a measured harm — and the "was never probed" caveat is retired.
   **What this does not license**: the flag remains inexpressible through the protocol
   (`SAFE_EXTRA_FLAGS` is `--debug` and `--verbose`, `claude_launch.rs:52`), so nothing in the
   bundle changes; what changes is that a future proposal to add it can no longer be refused with
   "the env var broke the cell".

4. ~~**First-launch cost in a COLD private root.**~~ **MEASURED — and the comparison the item asked
   for turns out to be one pmux REFUSES to let a minified cell make.** Three findings, in the order
   they matter:

   - **A cold root does re-fetch, and here is the file.** A fresh private root ends a single cell
     holding 8 files and **~524-537 KB**, of which **`cache/changelog.md` is 477 KB**. Present in
     **10 of 10 cold minified roots** (5 control cells + the 5 `--safe-mode` cells of item 3) and
     **10 of 10 cold `full` roots**. The operator's own root holds the same file at 474 KB. So the
     answer to "whether a cold root re-fetches it per instance" is **yes**, once per root, every
     time. What it is NOT is the 6.4 MB official marketplace: §2.3's
     `CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL=1` holds, and no `plugins/marketplaces`
     directory appeared in any minified root — it did appear, at 6.4 MB, in a `full` Path A root
     with that variable absent, which is the control that makes the absence mean something.
   - **It does not sit in the readiness window.** `pmux start` → `state: ready` over n=10 cold roots:
     1,048 / **1,122** / 1,377 ms (min / median / max). Over n=10 warm reuses of one root: 782 /
     **1,249** / 1,303 ms. The warm arm's median is *higher*, the distributions overlap almost
     entirely, and the difference is smaller than either sample's spread. **There is no measurable
     cold-root penalty in the window a caller waits.**
   - **The warm arm had to be run as a `full` cell, and that is the finding, not a caveat.** pmux
     refuses a minified cell a root anything has used before: `a minified cell requires a config
     isolation root it alone has ever used, and this one contains cache` (§5.6). **So for Path B
     there is no such thing as a warm root** — every mint is a cold mint, by construction, and
     "timing a mint into a warm one" is not an experiment this product permits. The n=10/n=10
     comparison above therefore holds the cell type at `full` and moves only the root, with
     `CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL=1` set on **both** arms so the 6.4 MB
     marketplace download §2.3 suppresses for minified cells is not a second variable.

5. ~~**The 2 s sampling blind spot on Claude's own `.claude.json` writes.**~~ **CLOSED BY MECHANISM.**
   Claude **replaces** that file rather than rewriting it: 25 writes during one real session
   produced **25 distinct inodes**, and a whole-file read plus JSON parse every ~2.9 ms returned
   **8,764 parseable samples and 0 torn ones**, with no absent sample after the first write. A
   `rename(2)` is atomic, so a torn read is not rare — it is unreachable. **Positive control**: the
   same sampler against an in-place rewrite of the same file caught **36 torn reads in 407 samples
   across one inode**. Full detail and what to re-check in §5.3. This is the §0.3 rule-3 shape —
   prefer the mechanism to the outcome — and it is why no amount of extra sampling was the answer.

6. ~~**Sub-turn arrival ORDER of `turn_duration`.**~~ **MEASURED IN ARRIVAL ORDER, with pmux's own
   instrument.** The item said the scan behind "no semantic row follows `turn_duration`" read
   finished files. It does — but `TurnTimings` already publishes
   `turn_duration_observed_at_ms` and `post_turn_duration_row_observed_at_ms`, which are stamped
   against reads pmux was going to perform anyway and are exactly *arrival* instants
   (`crates/protocol/src/v1.rs`). Nothing needed building; they needed reading, and the latency
   tool's refusal to publish a total containing an unclassified `*_at_ms` field is what forced them
   to be read. **n=20 real warm turns: 20/20 carried a marker, and 0/20 had any analysis-changing
   row arrive after the batch that carried it.** An independent instrument agrees: tailing the
   live transcript from a byte offset every ~2.9 ms across 6 turns recorded `assistant` then
   `system/turn_duration` and then **nothing at all** until the next prompt's rows 1.28-3.5 s later.
   **What it would buy, since it is now a number**: the commit gate spends **552 ms median
   (526-580) after the marker has already arrived**, ~15% of a warm turn. **What is still a
   decision and not a consequence**: 20 turns of one trivial-prompt shape on one host is evidence
   that the marker is last, not a proof that it always is, and the fast path stays unbuilt until
   somebody decides that evidence is enough. The promoted drain of §12.4 does not depend on it.

7. ~~**Whether `--effort` values beyond the shipped enum change model behaviour.**~~ **SETTLED, and
   the item's own premise was half wrong.** Both routes it named were taken. Reading the child's
   stderr turned out to need **no product change at all** — a shim that `exec`s Claude with its own
   stderr redirected reads it from outside pmux, which is the same trick §13's other real-Claude
   arms use for their one variable.

   **What the stderr says, verbatim from 2.1.220.** A genuinely unknown spelling warns and defaults,
   exactly as recorded: `--effort nonsense-value` prints
   `Warning: Unknown --effort value 'nonsense-value' — ignoring it and using the default effort.
   Valid values: low, medium, high, xhigh, max.` and exits 0. **`--effort ultracode` prints
   nothing.** Across 30 real cell launches through pmux there were **zero** warnings on any child's
   stderr. So `ultracode` is a **recognised** value the warning's own list omits, not an unknown one
   — and §2.2's row, which attached "warns and falls back to the default" to `ultracode`
   specifically, is corrected there. Six spellings are accepted silently — `low`, `medium`, `high`,
   `xhigh`, `max`, `ultracode` — and **the match is case-insensitive**: `Low` and `ULTRACODE` are
   accepted too, which costs pmux nothing today because its enum renders lowercase, but is the kind
   of thing a case-sensitive guard gets wrong.

   **What it changes, MEASURED through pmux.** A shim rewrote only the *value* of pmux's own
   `--effort` — the same shim in every arm, so the shim is not the variable — driving
   `--cell minified` Path A cells with a fresh private root each and one reasoning prompt. Effort is
   the only thing that moved:

   | `--effort` | n | output tokens min-max | median | generation leg median |
   |---|---:|---:|---:|---:|
   | `low` | 5 | 176-367 | 236 | 4,019 ms |
   | `medium` | 5 | 261-361 | 285 | 4,351 ms |
   | `high` | 5 | 349-747 | 405 | 5,717 ms |
   | *(flag absent — the CLI's own default)* | 14 | 427-700 | 516 | 6,137 ms |
   | **`ultracode`** | 15 | 502-1,818 | **788** | 9,204 ms |
   | `max` | 5 | 1,193-1,870 | 1,217 | 12,150 ms |

   **Yes, it changes behaviour, and it is its own tier.** `ultracode` is separated from `low` and
   from `max` with no overlap at all, and it is separated from **the default** — the arm that would
   have to be indistinguishable if "silently uses the default" were true of it — by
   **195 of 210 pairwise comparisons on output tokens (93%, Mann-Whitney U=195, z=3.93, p≈8.6e-5)**
   and 193/210 on generation latency. Two-sided, so this is not a one-tailed courtesy.

   **What this does NOT license, stated because the shape invites it.** pmux still ships five effort
   variants and this is not an argument to ship a sixth: an undocumented value that the CLI's own
   warning text does not list is one Claude release away from becoming an unknown spelling, at which
   point it degrades **silently to the default** on a channel pmux does not read. That is the
   failure mode §2.2's row exists to name. If it is ever admitted, it needs the stderr channel
   first. Recorded here because "we measured that it works" is exactly how a value nobody can see
   fail gets adopted.

   **One incidental observation, kept because it is a refusal and not a wrong answer.** 1 of 30 real
   cell launches failed at startup with `NeedsInput — Claude startup did not reach a ready or
   recognized interactive screen`. Unrelated to effort (it was a default-arm cell), it cost a sample
   rather than an answer, and it is the asymmetry of §1 working as designed.
