# The defect log

This is the commit log of pmux's development, preserved before that log was
squashed into a single commit for publication. It is here because of what these
messages are: each one names **the defect found**, not the change made. Read as
a set they are a catalogue of what actually goes wrong in a system that drives
real interactive Claude Code instances inside terminal panes and sells the
result as a stateless `(model, effort, prompt) -> tokens` engine.

A newcomer should care because the failure modes are not the ones a code review
predicts. They are: a prompt beginning `!` that switched the composer into bash
mode and ran the rest as a shell command on the host; a modal classifier whose
ten spare phrases no screen in the suite could reach; an inherited environment
variable that made every turn hang forever with no transcript ever written; a
green report over forty-nine tests that never ran. The diffs that fixed these
are recoverable from the tree. The reasoning that found them is only here.

The messages are quoted verbatim. Nothing has been summarised, softened or
re-worded.

## What was changed, and how

Three substitutions were applied mechanically to every message and to nothing
else. All three are declared here so the transformation is checkable, all three
are idempotent, and none was applied by hand.

**1. Machine-specific identifiers were replaced with structure-preserving
placeholders.** The map:

| from | to |
| --- | --- |
| the checkout's own absolute path | `<REPO>` |
| the distance from the home directory to that checkout | `<WORKSPACES>` |
| the home directory | `<HOME>` |
| the worktree's own directory name | `<REPO>` |
| the login name standing alone | `<USER>` |
| the temporary directory, where this host's differs from the platform's | `<TMPDIR>` |

Not one of those is written down anywhere. `tools/defect-log/machine.py` asks
the running machine for all six and returns them longest first, so a shorter
needle cannot half-substitute a longer one, and both the generator that applies
the map and `tools/gate-a/tests/test_redaction.py`, which fails if any needle
survives in this file, read that one derivation. A scrubber whose
set-of-things-to-scrub is a literal is the class this log's section A is about:
the list gets written on the host that has nothing left to find, so it passes,
and it keeps passing on the next host for the same reason.

The left column describes the identifiers rather than spelling them for the
same reason -- this file is scanned for them, and a map table that spelled its
own inputs would be the one live instance of the shape the checker refuses,
sitting inside the paragraph that declares it.

The last row found nothing here, and that is a fact about the messages rather
than a gap: the two that mention a temporary directory already write it elided,
with no hashed component to remove. `/private/tmp` is deliberately not a needle
-- it is the same string on every host of this platform, so it names the
platform and not the machine, and one of the log's own findings is about `/tmp`
being a symlink to it.

`macos`, `aarch64` and `macOS-15.7.7` are **not** machine-specific and are
untouched: the compatibility profile is keyed on them, and the whole Linux
handoff is about that boundary. `smithers` is a shipped product module and is
untouched.

**2. Commit hashes were replaced with this document's own ordinals.** The
squash destroys every hash these messages cite. A token was rewritten only if
git resolves it *and* the commit it resolves to is one of the 166 catalogued
here; `sha256` digests, upstream rmux hashes and references that were already
dead resolve to nothing and were left exactly as written. So `<c152>` in a
message body means entry 152 of this log, and no replacement hash has been
invented.

**3. Line numbers were dropped from citations of a linted Path B document.** 6
sites, all in messages that are themselves about such a citation having
rotted. `crates/service/tests/path_b_doc_citations.rs` fails the build if
anything in this repository cites one of those documents by line, for the
reason it gives: *a section survives insertion above it; a line number does
not*, and this repository has already had a stale line citation become a live
isolation leak. An archive that reproduced those citations would arm that
guard against a file nobody can edit. The document set and the
suffix-resolution rule are read out of `docs/path-b.md` §0.0, the same table
the guard reads, so a document promoted or demoted there moves this too. The
path is kept and only the `:NNN` is dropped -- the same rule as for hashes,
drop the reference rather than invent a replacement. The other 150 distinct
`path:line` citations in these messages, all of them into source files, are
untouched.

## How the grouping was derived

The messages classify themselves twice over, and both readings were used.

**First, the repository names one class explicitly and machine-checks the
count.** 20 of the 166 messages use the phrase "bug class", over 24 lines, and
5 of them number the instance in words under a heading reading `THE BUG CLASS,
instance ...`. The counter is not prose: the test
`test_every_statement_of_the_bug_class_counter_spells_the_same_ordinal` exists
in the tree and holds four Rust sites and the last such heading to one number.
That class is section A below, quoted from the tree:

> A guard, comment, document, test name or receipt whose message promises more
> than its predicate tests; or a check whose set-of-things-to-check is
> hand-written where it could be derived.

**Second, the remaining subjects partition on their own recurring
vocabulary.** Which terms to read is editorial; the counts are not. Over the
166 messages quoted below, each term counted case-insensitively wherever a
word starts, so `gate` counts `gates` and `Gate A`: `gate` 427, `pool` 238,
`receipt` 183, `prompt` 159, `evidence` 151, `instance` 131, `daemon` 127,
`refusal` 125, `drain` 115, `agent` 114, `transcript` 92, `citation` 87,
`mutation` 87, `composer` 83, `ordinal` 75, `screen` 64, `ledger` 58, `MCP`
49, `/clear` 44, `isolation` 20. Those terms cluster into six subjects that do
not overlap in what a reader would go looking for -- what a caller types, when
a turn is over, what the pool holds, what a cell can reach, what was written
down, and what does the measuring. They are sections B through G.

Every entry appears exactly once, filed under the class of the defect its
**subject** names first. Most subjects here name two defects, joined by "and",
and they are frequently in two different classes; only the first one decides
where the entry is filed, and no cross-listing is offered, because assigning a
second class to 166 subjects would be a hand-written set of exactly the kind
section A is about. Use the index and search the text.

Within each section, entries are in commit order, oldest first. The index
below carries all 166 in commit order regardless of class, so the log can
still be read as a chronology.


## Index, in commit order

| # | date | class | subject |
| --- | --- | --- | --- |
| 1 | 2026-07-27 | [E](#e-completion-authority) | [pmux v1: Claude-aware protocol-v1 control plane](#c1) |
| 2 | 2026-07-27 | [G](#g-tooling) | [Gate A passes 75/75; characterize pmux overhead; fix 4 defects the capture found](#c2) |
| 3 | 2026-07-27 | [F](#f-receipts-evidence) | [Gate B attempt 1: ordinal 30 consumed, turn stuck in awaiting_prompt_ack](#c3) |
| 4 | 2026-07-27 | [D](#d-isolation) | [Agent profiles, --dangerously-skip-permissions, and a value-enum drift fence](#c4) |
| 5 | 2026-07-27 | [D](#d-isolation) | [Strip CLAUDE_CODE_CHILD_SESSION: nested-marker inheritance broke every turn](#c5) |
| 6 | 2026-07-27 | [D](#d-isolation) | [Allowlist the launch environment; one authoritative policy in protocol](#c6) |
| 7 | 2026-07-28 | [E](#e-completion-authority) | [Instrument late-row arrival; the drain is almost entirely margin on 24/24 turns](#c7) |
| 8 | 2026-07-28 | [F](#f-receipts-evidence) | [Untrack Gate A receipts; they live in .context/gate-a/](#c8) |
| 9 | 2026-07-28 | [B](#b-composer-screen) | [Drop the POSIX terminator from --prompt-file; every file prompt failed](#c9) |
| 10 | 2026-07-28 | [G](#g-tooling) | [Gate B calibration suite: nine graded prompts and an offline verifier](#c10) |
| 11 | 2026-07-28 | [F](#f-receipts-evidence) | [Ledger: record ordinals 31-32, reconcile four detached reservations](#c11) |
| 12 | 2026-07-28 | [G](#g-tooling) | [Grade attempts by prompt content hash, not argv position](#c12) |
| 13 | 2026-07-28 | [D](#d-isolation) | [Reject agent-team markers that reach the child, not ones merely present](#c13) |
| 14 | 2026-07-28 | [G](#g-tooling) | [Gate C Linux handoff, and correct the ledger's two ordinal spellings](#c14) |
| 15 | 2026-07-28 | [G](#g-tooling) | [Instrumentation fix plan: 8 defects in the tools that judge pmux](#c15) |
| 16 | 2026-07-28 | [G](#g-tooling) | [Verifier: noise band, partial-hash state, and the right expects_hash entry](#c16) |
| 17 | 2026-07-28 | [E](#e-completion-authority) | [Publish when the Stop hook arrived, so the drain question becomes answerable](#c17) |
| 18 | 2026-07-28 | [G](#g-tooling) | [Gate the claim, not the environment: five instrument fixes and two dispositions](#c18) |
| 19 | 2026-07-28 | [F](#f-receipts-evidence) | [Publish the Gate B receipt; the campaign was reproducible only from an untracked dir](#c19) |
| 20 | 2026-07-28 | [G](#g-tooling) | [C9: a pre-connect regression hung the gate command instead of failing it](#c20) |
| 21 | 2026-07-28 | [G](#g-tooling) | [Gate A 75/75 with a valid source identity, and the setup that took four runs](#c21) |
| 22 | 2026-07-28 | [G](#g-tooling) | [Handoff: say which files moved after its citations were verified](#c22) |
| 23 | 2026-07-28 | [G](#g-tooling) | [Say that the fix plan was applied, and record the three deferrals in the repo](#c23) |
| 24 | 2026-07-29 | [G](#g-tooling) | [Apply the validated pre-push review: 4 blocking, 5 high, 7 medium, 5 low](#c24) |
| 25 | 2026-07-29 | [G](#g-tooling) | [Gate A 75/75 on the tree being pushed, and the fourth typescript-dist trap](#c25) |
| 26 | 2026-07-29 | [G](#g-tooling) | [Phase 0: make the drain question free to answer, and harden the live runners](#c26) |
| 27 | 2026-07-29 | [E](#e-completion-authority) | [Admit stop_hook_summary only when its payload proves the turn is over](#c27) |
| 28 | 2026-07-29 | [F](#f-receipts-evidence) | [Phase 1/2 live: every envelope-reachable scenario now has coverage](#c28) |
| 29 | 2026-07-30 | [G](#g-tooling) | [Gate A 75/75 on the SchemaDrift gate and the Phase 1/2 tree](#c29) |
| 30 | 2026-07-30 | [E](#e-completion-authority) | [Never complete a turn mid-retry, and measure the in-band end-of-turn marker](#c30) |
| 31 | 2026-07-30 | [G](#g-tooling) | [Gate A 75/75 on the api_error gate and arrival instrumentation](#c31) |
| 32 | 2026-07-30 | [G](#g-tooling) | [Correct two coverage rows that understated what ordinals 44-55 bought](#c32) |
| 33 | 2026-07-30 | [E](#e-completion-authority) | [Graduated drain: a proven end-of-turn marker buys a 250ms floor, not 2000ms](#c33) |
| 34 | 2026-07-30 | [E](#e-completion-authority) | [Guard the byte that re-arms the drain, and stop the verifier flattering itself](#c34) |
| 35 | 2026-07-31 | [B](#b-composer-screen) | [Path B: follow the session across /clear, and close the guard that let a caller ride along](#c35) |
| 36 | 2026-07-31 | [F](#f-receipts-evidence) | [Measure Path B through pmux, and retract the latency claim it was sold on](#c36) |
| 37 | 2026-08-02 | [C](#c-pool-lifecycle) | [A cancelled turn no longer bricks the daemon, and the census was twice as long](#c37) |
| 38 | 2026-08-02 | [C](#c-pool-lifecycle) | [A poisoned connection now costs one session, and two comments were wrong](#c38) |
| 39 | 2026-08-04 | [B](#b-composer-screen) | [The composer was judged by where it sat, and eight guards read the spelling](#c39) |
| 40 | 2026-08-04 | [C](#c-pool-lifecycle) | [The pool keys on the argv a process was launched with, and the idle set is the proof](#c40) |
| 41 | 2026-08-04 | [C](#c-pool-lifecycle) | [An expiring instance left its name in the idle set, and a refused transition was already applied](#c41) |
| 42 | 2026-08-04 | [G](#g-tooling) | [Keep the screens pmux discards, and four checks that could not have failed](#c42) |
| 43 | 2026-08-04 | [C](#c-pool-lifecycle) | [A later resize now reaches the window, and four claims were measured](#c43) |
| 44 | 2026-08-05 | [C](#c-pool-lifecycle) | [Merge the pool core: the stateless engine, refusing until it is wired](#c44) |
| 45 | 2026-08-05 | [G](#g-tooling) | [Merge the screen corpus: every discarded screen is now evidence](#c45) |
| 46 | 2026-08-05 | [D](#d-isolation) | [Path B is reachable: pmux mints every resource and the caller names none](#c46) |
| 47 | 2026-08-05 | [C](#c-pool-lifecycle) | [Health is a proof tree, and a layer nobody reported is not a healthy layer](#c47) |
| 48 | 2026-08-05 | [C](#c-pool-lifecycle) | [Live: a caller gets tokens for (model, effort, prompt), and two defects the socket found](#c48) |
| 49 | 2026-08-05 | [G](#g-tooling) | [The per-binary harness covers thirteen packages, and doctor names the layers nobody reported](#c49) |
| 50 | 2026-08-05 | [C](#c-pool-lifecycle) | [A daemon holding nothing is healthy, and pmux sealed only the last directory it made](#c50) |
| 51 | 2026-08-05 | [C](#c-pool-lifecycle) | [A declared warm floor is a promise, and a daemon that declined Path B could still prove itself](#c51) |
| 52 | 2026-08-05 | [C](#c-pool-lifecycle) | [Nobody waits on a clearing instance, and shutdown left the roots it had just used](#c52) |
| 53 | 2026-08-05 | [F](#f-receipts-evidence) | [Path B works for someone who never read our argv, and a wave could not tell which daemon it drove](#c53) |
| 54 | 2026-08-06 | [F](#f-receipts-evidence) | [A retracted claim outlives the wrong belief it caused, and path-b.md now describes a shipped thing](#c54) |
| 55 | 2026-08-06 | [G](#g-tooling) | [The gate linted the directories somebody listed, and the residue audit never looked inside /tmp](#c55) |
| 56 | 2026-08-06 | [C](#c-pool-lifecycle) | [Nobody waited for a clearing slot after all, and a cold swap raced the caller it was meant to save](#c56) |
| 57 | 2026-08-06 | [G](#g-tooling) | [A phase the driver could not start, and a green report over forty-nine tests that never ran](#c57) |
| 58 | 2026-08-06 | [G](#g-tooling) | [Five waves demanded a variable no gate can pass, and a mode drift the umask had already applied](#c58) |
| 59 | 2026-08-06 | [F](#f-receipts-evidence) | [Merge the docs reconciliation: three measured claims were false, and one fixture called itself a measurement](#c59) |
| 60 | 2026-08-06 | [F](#f-receipts-evidence) | [Two numbers for one quantity and neither had a receipt, and a directory mtime that was never identity](#c60) |
| 61 | 2026-08-06 | [C](#c-pool-lifecycle) | [A terminal that could still read but never write again, and a deadline that answered to whichever call it expired in](#c61) |
| 62 | 2026-08-06 | [A](#a-house-bug-class) | [Seven values the help offered and the daemon refused, and a refusal that knew the answer it never printed](#c62) |
| 63 | 2026-08-06 | [D](#d-isolation) | [An agent the caller pins by version, and a corpus that covered eleven of twelve methods](#c63) |
| 64 | 2026-08-06 | [A](#a-house-bug-class) | [Eleven of twelve behind three copies of a number, and a stop reason two clients read and neither checked](#c64) |
| 65 | 2026-08-06 | [D](#d-isolation) | [An agent the request pins by version, a cwd it may bound but never name, and a digest that lied about what it covered](#c65) |
| 66 | 2026-08-06 | [A](#a-house-bug-class) | [A refusal that named the colliding field and a transport that said "does not match protocol v1"](#c66) |
| 67 | 2026-08-06 | [G](#g-tooling) | [Three instances recorded, twelve matrix rows, and a design document that stopped saying nothing here is built](#c67) |
| 68 | 2026-08-06 | [A](#a-house-bug-class) | [A comment that named a syscall the code never made, and a listing that lost every record to one](#c68) |
| 69 | 2026-08-07 | [C](#c-pool-lifecycle) | [A safe direction that was a locked door, and a pointer that was only ever a lower bound](#c69) |
| 70 | 2026-08-07 | [A](#a-house-bug-class) | [A test that chmod'd away the wrong bit, and a scope named for the one file it leaves out](#c70) |
| 71 | 2026-08-07 | [G](#g-tooling) | [A re-warm counted with a lower bound, and eighteen survivors left open under a reason nobody checked](#c71) |
| 72 | 2026-08-07 | [E](#e-completion-authority) | [A window named for the guarantee it was spending, and a revision that counted captures, not changes](#c72) |
| 73 | 2026-08-08 | [G](#g-tooling) | [A hundred and two unviable mutants that were a compile log, and six dependencies a `#[path]` made load-bearing](#c73) |
| 74 | 2026-08-08 | [D](#d-isolation) | [A guest that was the wrong OS to reach the keychain, and an `os` that would have vouched for Linux](#c74) |
| 75 | 2026-08-08 | [G](#g-tooling) | [A brief that promised three drifted cells and thirteen were measured, and a debt row citing a line that moved nineteen](#c75) |
| 76 | 2026-08-08 | [F](#f-receipts-evidence) | [A tree that did have the newer receipt, behind the one ignore rule the search could not see](#c76) |
| 77 | 2026-08-08 | [F](#f-receipts-evidence) | [A ledger whose own recount command falsified the budget it published, and a pool that erased a root under a child it never counted](#c77) |
| 78 | 2026-08-08 | [F](#f-receipts-evidence) | [A budget the ledger's own recount command contradicted by 38 ordinals, deleted rather than corrected](#c78) |
| 79 | 2026-08-08 | [G](#g-tooling) | [Fifty-seven line citations into the two phase0 files the budget fix grew, re-anchored to what each one resolved to before it](#c79) |
| 80 | 2026-08-08 | [A](#a-house-bug-class) | [A mutation gate whose refusal named debug-assertions and overflow-checks and whose predicate read neither](#c80) |
| 81 | 2026-08-08 | [A](#a-house-bug-class) | [A README that documented ten of thirteen subcommands and none of the priority product, and a quickstart whose daemon refused every ask](#c81) |
| 82 | 2026-08-08 | [G](#g-tooling) | [A review that refuted nothing over a hundred and eight findings, replaced by one that killed nine of twenty-eight and reproduced the two it could not](#c82) |
| 83 | 2026-08-08 | [G](#g-tooling) | [A review ranked on thirty-one claims it re-measured by hand because the other seventy-seven never arrived, replaced by one merged with all hundred and eight and their adjudications](#c83) |
| 84 | 2026-08-08 | [B](#b-composer-screen) | [A facade whose piped prompt kept the terminator no composer can hold, and the one rule two binaries each owned a copy of](#c84) |
| 85 | 2026-08-08 | [A](#a-house-bug-class) | [The last manifest section pinned by hand, whose exhaustive name table forced one arm and left the array free to stay short](#c85) |
| 86 | 2026-08-08 | [A](#a-house-bug-class) | [A citation to the exactly-one-CR rule that had drifted eight lines off the line it names](#c86) |
| 87 | 2026-08-08 | [C](#c-pool-lifecycle) | [A teardown arm that spelled "no handle yet" the same as "no process ever", and the launch whose late handle it left nobody accountable for](#c87) |
| 88 | 2026-08-08 | [C](#c-pool-lifecycle) | [The one read of the instance map that indexed where its neighbours ask, on the resume path of a clear nobody waits on](#c88) |
| 89 | 2026-08-08 | [C](#c-pool-lifecycle) | [A doctor that exited 0 healthy while holding both operands of the refusal the next ask returned](#c89) |
| 90 | 2026-08-08 | [E](#e-completion-authority) | [An api_error stamped before the answer it was counted as arriving after, and the retrospective column that now has to prove it still is](#c90) |
| 91 | 2026-08-08 | [F](#f-receipts-evidence) | [A promotion that would fit 2.1.223 a 250 ms drain from one arrival, and a 2,344-turn free corpus that is 219 rows](#c91) |
| 92 | 2026-08-08 | [A](#a-house-bug-class) | [Six internally-tagged v1 unions that each took an appended variant with every suite in all three languages green, and the manifest section that now asks serde what every variant of them spells](#c92) |
| 93 | 2026-08-08 | [A](#a-house-bug-class) | [Fourteen regression names in six hand-written copies, every lane running them with an --exact filter that skips in silence, replaced by one module the patch defines and a scan that refuses any file but two the right to name one](#c93) |
| 94 | 2026-08-08 | [G](#g-tooling) | [A launcher refusal bound whose stopwatch spent 348 of its 350 milliseconds sha256ing the harness's own candidate, and three documents that recorded the remaining 4 ms as 600x headroom](#c94) |
| 95 | 2026-08-09 | [D](#d-isolation) | [A --strict-mcp-config retracted as "no longer load-bearing" on a descendant-process inventory that cannot see an HTTP endpoint, and a 2.1.226 cell that loads the caller's account MCP connector until the flag it was retracted from is passed](#c95) |
| 96 | 2026-08-09 | [F](#f-receipts-evidence) | [A drain whose only receipt was one version's own fit, promoted as a bound over four, and a tool whose exit 0 meant "nothing to check" at exactly the version nobody has measured](#c96) |
| 97 | 2026-08-09 | [F](#f-receipts-evidence) | [An exact-version key that spent 13 ledger ordinals per patch release to pin the one quantity that does not move, and a pool that halted on one of the seven refusals its own comment described](#c97) |
| 98 | 2026-08-09 | [F](#f-receipts-evidence) | [A drain corpus that existed only because a Gate B campaign had just been paid for, and the `cli` transcripts every ordinary Path B turn writes and the pool erased four lines later](#c98) |
| 99 | 2026-08-09 | [F](#f-receipts-evidence) | [A refusal that named the global real-Claude ceiling while testing this file's own numbering, and the four detached reservations it therefore would have spent twice](#c99) |
| 100 | 2026-08-09 | [F](#f-receipts-evidence) | [A range promoted through 2.1.226 on a drain its own provenance said was never measured there, now measured at 70 ms over four real turns, and the warm-pool mint that runs before pmuxd installs its SIGTERM handler](#c100) |
| 101 | 2026-08-09 | [D](#d-isolation) | [A minified cell whose isolation rested on a process inventory that cannot see an HTTP endpoint, and the launch bundle three source files described and one of them was right about](#c101) |
| 102 | 2026-08-09 | [C](#c-pool-lifecycle) | [A SIGTERM window whose whole warm mint ran at the kernel's disposition, and the recovery chain that grew by one restart per tree it erased and two per tree it abandoned](#c102) |
| 103 | 2026-08-09 | [B](#b-composer-screen) | [A prompt beginning `!` that switched the composer into bash mode and ran the rest as a shell command on the host, and the two other prompt shapes pmux admitted and could not deliver](#c103) |
| 104 | 2026-08-09 | [G](#g-tooling) | [A promotion no one could repeat without improvising, whose only campaign envelope has never once launched the minified cell its gate exists for and still spelled a flag the product renamed](#c104) |
| 105 | 2026-08-09 | [F](#f-receipts-evidence) | [The 2.1.226 half of the promoted range, restated as the sentence a run of the promotion path generated rather than one written beside it](#c105) |
| 106 | 2026-08-09 | [A](#a-house-bug-class) | [A retraction three source files cited by a line number that stopped being the row the same commit moved, and the five ASCII punctuation characters a sweep claiming every one of them never sent](#c106) |
| 107 | 2026-08-09 | [B](#b-composer-screen) | [A render proof named for the prompt whose predicate was five clauses of cursor geometry, and the one row of a 24-row pane a 1 MiB prompt can still be checked against](#c107) |
| 108 | 2026-08-09 | [B](#b-composer-screen) | [A composer that trims the end of every buffer it submits, a backslash that makes Enter insert a newline instead, and the four caller inputs each of those cost a pooled instance](#c108) |
| 109 | 2026-08-10 | [A](#a-house-bug-class) | [A citation ban that knew one of the four spellings a reader resolves identically, and the one live instance of the shape it refuses sitting inside the paragraph that forbids it](#c109) |
| 110 | 2026-08-10 | [F](#f-receipts-evidence) | [The Path B verdict, with two of the five criteria NOT MET and a clippy error five parallel reviewers read past because none of them ran the gate](#c110) |
| 111 | 2026-08-10 | [A](#a-house-bug-class) | [Five of the seven stateless refusals named what went wrong and no action, and the one advice string that did ship described its own detector instead of a next step](#c111) |
| 112 | 2026-08-10 | [A](#a-house-bug-class) | [The MCP surface answered every daemon refusal there is with one constant sentence, so a `/`-prefixed prompt and a daemon with no pool arrived as the same payload](#c112) |
| 113 | 2026-08-10 | [B](#b-composer-screen) | [Three clauses of the render gate could be disabled without reddening one test, and the one that could never have refused anything was the one hiding the other two](#c113) |
| 114 | 2026-08-10 | [B](#b-composer-screen) | [The render gate accepted one delivered character of a seventeen-character prompt, and the two tables that recorded the same measured wrap disagreed by four characters because nothing could tell](#c114) |
| 115 | 2026-08-10 | [B](#b-composer-screen) | [The trade that justified trimming a character JS keeps was refuted by the guard three lines below the one that would have paid for it](#c115) |
| 116 | 2026-08-10 | [A](#a-house-bug-class) | [A citation grader that skipped 70 of the 132 claims its heading said "every", and the 37 line citations of the document it protects that sat in the half of docs/ it never opened](#c116) |
| 117 | 2026-08-10 | [A](#a-house-bug-class) | [The five register rows nobody had worked, and a source digest that omitted every committed byte of evidence while hashing ten files Finder rewrites](#c117) |
| 118 | 2026-08-10 | [F](#f-receipts-evidence) | [The register's own citation for a line `<c108>` deleted, and criterion 5 rewritten around what a total grader measured rather than what a partial one could reach](#c118) |
| 119 | 2026-08-10 | [B](#b-composer-screen) | [The modal classifier whose ten spare phrases no screen in the suite could reach, and the four guard clauses that could not have been the ones that refused](#c119) |
| 120 | 2026-08-10 | [E](#e-completion-authority) | [The three lifecycle fields a modal completion return could drop into their own defaults, and the scan bound whose only witness was 20,001 directory entries](#c120) |
| 121 | 2026-08-10 | [A](#a-house-bug-class) | [The version query nothing ever called, the field count no serializer in this tree reads, and the two Drop bodies the compiler already writes](#c121) |
| 122 | 2026-08-10 | [F](#f-receipts-evidence) | [The 94 floor enforced for the first time against the scope that fails it, and a disposition for all 136 survivors keyed on something other than the line number that moved for 100 of them](#c122) |
| 123 | 2026-08-10 | [F](#f-receipts-evidence) | [A gate that owned the whole tree for three hours given a checkout of its own, and a receipt that names the commit it graded rather than leaving a reader to assume HEAD](#c123) |
| 124 | 2026-08-11 | [G](#g-tooling) | [Five criteria a person checked by reading, made a script that reads the set out of the document stating them and refuses a sixth it cannot measure](#c124) |
| 125 | 2026-08-11 | [G](#g-tooling) | [The seven rustfmt hunks three commits of survivor-killing left in the one file the mutation gate mutates most, red in a cell whose reports only ever named clippy](#c125) |
| 126 | 2026-08-11 | [A](#a-house-bug-class) | [The two live rows the adversarial derivation dropped without a word, under a criterion titled for the suite they are the live half of](#c126) |
| 127 | 2026-08-11 | [F](#f-receipts-evidence) | [Nine mutants that flipped between two runs of the same 1653, every one of them decided by a test that needs a real rmux rather than by the code it mutates](#c127) |
| 128 | 2026-08-11 | [F](#f-receipts-evidence) | [The eight cells no receipt had ever covered, run at a named commit, and the gate mutation number that was guessed stale in the safe direction turning out to be 97](#c128) |
| 129 | 2026-08-11 | [A](#a-house-bug-class) | [A remedy sentence written into the verdict without being run, refuted by running it: `--commit` exits 2 rather than re-reading the verdict it promised](#c129) |
| 130 | 2026-08-11 | [B](#b-composer-screen) | [The trailing U+0085 a composer was measured KEEPING, and the three other characters pmux refused inside a prompt and deleted from the end of one](#c130) |
| 131 | 2026-08-11 | [F](#f-receipts-evidence) | [The register row the fix could not close until there was a commit to name, and the count in its title going from one character to four](#c131) |
| 132 | 2026-08-11 | [F](#f-receipts-evidence) | [The nine ordered checks run against 2.1.227, and the per-version drain fit landing at 250 ms again — 188 ms below the floor the catch window would need](#c132) |
| 133 | 2026-08-11 | [F](#f-receipts-evidence) | [The promoted range widened to a version whose every calibrated property was measured first, and a citation four lines from where the same edit moved it](#c133) |
| 134 | 2026-08-11 | [A](#a-house-bug-class) | [The site count a receipt would have published from arithmetic, and the 95 measured claims the scan that produced it cannot see](#c134) |
| 135 | 2026-08-11 | [A](#a-house-bug-class) | [A probe count that added the two flags it excluded, a frame that stopped one row short of its own footer, and a byte difference explained by a cause nothing varied](#c135) |
| 136 | 2026-08-11 | [A](#a-house-bug-class) | [A pane size named twice with no source, and a startup disclosure inherited from a receipt for a different version instead of read from this one](#c136) |
| 137 | 2026-08-11 | [A](#a-house-bug-class) | [Every instrument this repository owns, in a verdict that ran sixteen of forty-four, and the difference between an A/B and one measurement counted rather than blurred](#c137) |
| 138 | 2026-08-11 | [A](#a-house-bug-class) | [A `ready` that was said to prove the credential pin, when a logged-out cell renders the same composer](#c138) |
| 139 | 2026-08-11 | [C](#c-pool-lifecycle) | [A service no unit test could build because its runtime needed a real sidecar, the twenty-one survivor rows that cost, and an entry-path scan that read the new test file as production](#c139) |
| 140 | 2026-08-11 | [B](#b-composer-screen) | [An `Unknown` that meant proceed on every screen the classifier was never taught, the recovery loop whose frames were classified and then dropped, and twenty-four turns that priced refusing them](#c140) |
| 141 | 2026-08-11 | [A](#a-house-bug-class) | [The shape the veto carried for a whole run and never reported, beside the comment that said it was the one a refusal would name](#c141) |
| 142 | 2026-08-11 | [F](#f-receipts-evidence) | [A hand-written receipt that named whatever HEAD happened to be when it was saved, for numbers produced by a working tree that was not that commit](#c142) |
| 143 | 2026-08-11 | [G](#g-tooling) | [Three intra-doc links from public documentation to items rustdoc cannot reach, and a receipt for twenty-four real turns that the turn budget refused to classify](#c143) |
| 144 | 2026-08-12 | [A](#a-house-bug-class) | [One prompt limit stated six times in three binaries and tied nowhere, a release daemon the first live pass measured and this tree never built, and the thirty-sixth survivor of a run that scored ninety-six](#c144) |
| 145 | 2026-08-12 | [F](#f-receipts-evidence) | [A survivor register that wrote KILLED for twelve mutants one campaign happened to catch, and the gate-scope campaign four hours later that missed four of them](#c145) |
| 146 | 2026-08-12 | [F](#f-receipts-evidence) | [A currency check that compares whole files and cannot see a test at all, and the one KILLED row a deleted test made false while criterion 1 stayed green](#c146) |
| 147 | 2026-08-12 | [F](#f-receipts-evidence) | [A currency check that called all 144 register rows stale for one moved comment, and the eighty-three KILLED rows that now name the test whose deletion would make them false](#c147) |
| 148 | 2026-08-12 | [F](#f-receipts-evidence) | [A filtered run that handed cargo-mutants one shared target directory and graded 101 of 291 mutants against the previous mutant's binary, and the thirty-five KILLED rows that named the wrong test because of it](#c148) |
| 149 | 2026-08-12 | [F](#f-receipts-evidence) | [A census receipt one commit added to `evidence/` and no turn budget could classify, and a README sentence whose two ratios read as the ledger figure that document refuses to print](#c149) |
| 150 | 2026-08-12 | [G](#g-tooling) | [Three drafts whose defects a maintainer can still reproduce at 0.10.0, and the one draft whose code quote upstream demoted to a test](#c150) |
| 151 | 2026-08-12 | [G](#g-tooling) | [A draft that predicted `unknown attach-stream message tag 13` from the one byte value the decoder accepts as a valid tag, and the two upstream reports whose repros now run against the published 0.10.0 crates](#c151) |
| 152 | 2026-08-12 | [G](#g-tooling) | [A revision documented as advancing on every mutation, whose registry holds one fingerprint per pane and learns nothing from an interval no capture observed](#c152) |
| 153 | 2026-08-12 | [F](#f-receipts-evidence) | [A receipt for 70 graded cells written where the run that produced it is reaped, and the criterion that answered cells_executed=0 without naming the file it wanted](#c153) |
| 154 | 2026-08-12 | [A](#a-house-bug-class) | [A scan that gave two files the right to name a patch regression, and the two upstream documents that had been failing it since the day each landed](#c154) |
| 155 | 2026-08-12 | [F](#f-receipts-evidence) | [A verdict document whose newest section called itself final, and the four commits at which its own workspace suite was red](#c155) |
| 156 | 2026-08-12 | [F](#f-receipts-evidence) | [A certification section written in the past tense about a pinned run no receipt on this host records, and the nine durability self-tests that fail in the one cell that runs them](#c156) |
| 157 | 2026-08-13 | [G](#g-tooling) | [A census whose seventeen categories close exactly to the 585,839 tracked lines, and the two cfg(test) scanners whose braces desynced on a char literal](#c157) |
| 158 | 2026-08-13 | [G](#g-tooling) | [A handoff rewritten from the tree rather than from its predecessor, and the C6 decomposition that summed seven names out of six and seven](#c158) |
| 159 | 2026-08-13 | [G](#g-tooling) | [A handoff whose own six numbers were each wider or narrower than the command that produced them, and the conservative Linux birth token that already exists in a test](#c159) |
| 160 | 2026-08-13 | [F](#f-receipts-evidence) | [A pre-push review that refutes its own round's only blocker, and the pinned receipt that refuses on a commit rather than on a digest](#c160) |
| 161 | 2026-08-13 | [F](#f-receipts-evidence) | [A commit log that names one defect per message and dies at the squash, and a redaction map that would have been a literal list written on the host with nothing left to find](#c161) |
| 162 | 2026-08-13 | [G](#g-tooling) | [A generator whose own commit put the range one ahead of its class table, so the archive that exists because history is about to be rewritten could not be re-run against the history that carries it](#c162) |
| 163 | 2026-08-13 | [G](#g-tooling) | [A substitution map whose needles were resolved paths against emitters that record unresolved ones, and the sealed ledger a scrub would have forged rather than redacted](#c163) |
| 164 | 2026-08-13 | [A](#a-house-bug-class) | [A redaction map whose scope was two locations somebody remembered, and a rewriter that would have forged the one file its own test exempts](#c164) |
| 165 | 2026-08-13 | [A](#a-house-bug-class) | [A worked refusal example naming the version the product supports, a keychain digest whose only evidence was the literal it was taken over, and a review that republished the display name it recommended removing](#c165) |
| 166 | 2026-08-13 | [F](#f-receipts-evidence) | [A finding that published the address it existed to keep unpublished, an archive whose tail boundary moved with the catalogue it bounded, and a preamble whose counts of itself no rule in the tree reproduced](#c166) |

---

## A. The house bug class

**A guard, comment, document, test name or receipt whose message promises more than its predicate tests; or a check whose set-of-things-to-check is hand-written where it could be derived.**

This is the class the repository names and counts itself. It is the largest single group and it recurs in the instruments built to find it: a citation grader that skipped 70 of the 132 claims its heading called "every", a mutation gate whose refusal named two compiler settings its predicate never read, a survivor register keyed on a line number that had moved for 100 of its rows. The fix is almost always the same edit: replace the list with the derivation, and assert the derivation is not empty.

30 entries.

<a id="c62"></a>

### 62. Seven values the help offered and the daemon refused, and a refusal that knew the answer it never printed

*2026-08-06*

`````text
THE BUG CLASS, instance twenty, in the place with the most traffic and no tests: `--help`.

Read `bin/pmux/src/cli.rs`, `bin/pmux-mcp/src/tools.rs` and `bin/pmuxd/src/main.rs` end to end as a
user meets them, then drove all three live against Claude 2.1.223 over a real socket. Seven values
this CLI advertises are refused or ignored by the daemon, and the help said none of it, so
`[possible values: ...]` was the only thing a caller had and it was an advertisement:
`--terminal-profile rmux-standard` and `--input-transport attached-stream` are reserved
(`compatibility.rs:375/:381`); `--retention one-shot` is refused on every CLI path and OVERWRITTEN by
`run_once` (`native.rs:3049/:1475`); `--on-disconnect cancel-turn|close-session` and
`--heartbeat-timeout-ms` want a leased connection API that does not exist (`native.rs:2371`);
`attach --read-only` is refused on every session, which with the minified cell's writable refusal
means a minified cell cannot be attached AT ALL (`native.rs:1725`); and `close --policy` is accepted
by both values and changes nothing, because every `TerminalControl::close` in the tree takes
`_policy` and discards it (`driver_io.rs:1690`). Nothing is withdrawn from the wire -- the daemon
owns the verdict and `pmux probe` must keep building the exact DTO -- but every one now says so.

Nine subcommands and twenty-three arguments rendered with NO description: `ping`, `inspect`,
`cancel`, `close` and `attach` were bare names. The one `global = true` `--output` string told all
twelve subcommands that "NDJSON includes turn events" when only `run` and `turn` publish any. Every
subcommand now says which product it is -- Path A, Path B, or neither -- because two products share
this binary and nothing said which was which.

The MCP schema had it in its purest form: `permission_mode` listed SIX of `PermissionMode`'s SEVEN,
so every agent caller was told `dangerously_skip_permissions` did not exist while the CLI offered it
and the daemon ran it. `additionalProperties: false` does not police an enum's contents and nothing
else did. `run_stateless`'s description also carried fourteen spaces of Rust indentation mid-sentence.

THREE REFUSALS KNEW THE ANSWER AND NEVER PRINTED IT. `ClientError::Server`'s `Display` renders
code/message/retryable and drops `details` -- where every refusal in this tree puts the actionable
half. The first repair printed `details` verbatim and `cli_contract_matrix.rs` caught it inside one
sweep: `details` also carries attach capability tokens. The shipped repair is a contract rather than
a key allowlist, which would have been this same defect one level up: `recommendation` is the advice
channel and `pmux` renders that key and no other. A modal-blocked turn answered `{:?}` of the state
while holding `kind: trust` and Claude's own words for it. `path_b_not_enabled` never named
`--path-b-parent`, though the health tree's answer for the same condition already did.

Prompts are now takeable from a file EVERYWHERE one is taken: `--system-prompt` and
`--append-system-prompt` were argv-only, visible to `ps`, while `pmuxd` had already grown
`--path-b-system-prompt-file` for exactly that reason. The composer's slash guard is the one rule a
system prompt does not get -- it never reaches a composer -- and it stays exactly where it was
relative to the control-character scan, because U+0085 is both whitespace and a control character and
only one of the two refusals tells the caller what they did.

Eighteen checks, each mutation-proved and restored byte-exact, and the derived ones are derived on
both sides: clap's own command tree for help coverage and the path labels, `get_possible_values()`
for the refused-value census, argument names for the prompt-file rule, a walk of every tool schema
against variants parsed out of `crates/protocol/src/v1.rs`, and clap's `serve` arguments for the
absent-parent guard -- whose hand-written list of ten names beside eleven declarations was the same
shape, proven blind by putting the list back one name short.

61/61 targets, 1054 cases, 0 ignored, with the TypeScript dist staged so the sweep can claim what it
says. `cargo fmt` clean, clippy clean first-party (the 4 pre-existing rmux-server vendor warnings).
Zero daemons, pool parents or temp roots left; residue audit passed at candidate_executables=8 after
the E2E runs.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c64"></a>

### 64. Eleven of twelve behind three copies of a number, and a stop reason two clients read and neither checked

*2026-08-06*

`````text
`tests/conformance/v1/README.md` promised "one complete request/result pair for
every method". MEASURED against the manifest it is pinned to, `golden.json`
carried eleven of twelve: `run_stateless` -- the whole of Path B, the method
`pmux ask` reaches, and the only producer of `stateless_result` -- had no pair
in any of the three languages, while both shipped clients implement it and both
validate its result against no shared vector.

The guard could not see it because it compared the corpus to a NUMBER, in three
hand-written copies of `11`, none derived from `manifest.methods`. A literal
freezes the corpus at the size it had the day it was typed: deleting an entry
reddens it, failing to ADD one does not, which is exactly how an appended
method slips through. This is the same defect `shared_manifest_matches_the_
closed_v1_surface` already fixed for `manifest.json` with an exhaustive `match`
(`v1_conformance_vectors.rs:126-135` records that history); the fix was applied
to one checker in this directory and not to the other.

The count is now derived in all three languages, and compared by NAME rather
than by cardinality so the failure says which method is uncovered. The
per-corpus inventories (`client_required_field_deletions.results`,
`strict_request_object_pointers`) are derived from the corpus too, so a method
appended with no inventory of its own reddens rather than passing by having no
cases.

AND THE PAIR FOUND A DEFECT ON ARRIVAL. `stateless_result` carries an optional
`stop_reason`, and the required-field inventory deletes `stop_reason/kind` from
every result that carries one. The TypeScript and Python `run_stateless`
validators were the only two of the three that read such a field and never
checked it -- they accepted the mutilated frame. Both now route it through the
same `stop_reason` validator every other result already used. That gap survived
precisely because this was the one method with no golden pair.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c66"></a>

### 66. A refusal that named the colliding field and a transport that said "does not match protocol v1"

*2026-08-06*

`````text
MEASURED against a live daemon, with the agent surface driven end to end. The
decoder composes an exact sentence for a start that names both an agent and a
launch field -- and the wire answered:

    code   : invalid_config
    message: request does not match protocol v1

for that, for a start that named neither, and for `version: 0`. Every one of
them arrived indistinguishable from a typo. `bin/pmux-mcp`'s `start_session`
description promises "refused with invalid_config naming the colliding field",
`docs/spec.md` 4.8.1 says "refused by name", and the whole point of writing the
both-modes rule over PRESENCE rather than over equality-to-default was that the
caller would be told which field collided. Three claims, one predicate, and the
predicate stopped at the decoder.

The flattening was not a bug, though. Forwarding a decoder's rendered text
wholesale returns the caller's own values: MEASURED against this crate's own
decoder, `{"environment":{"set":{"SECRET":42}}}` renders as ``invalid type:
integer `42`, expected a string``, and a start frame carries environment values,
inline settings and MCP documents, and system prompts.

So the transport now forwards exactly one span and nothing else.
`DECODE_REFUSAL_MARKER` prefixes every `de::Error::custom` this protocol crate
writes, and `caller_actionable_decode_refusal` returns only the text between it
and serde's own ` at line N column M` suffix -- text composed out of field paths
and this crate's own argument, never a value a caller sent. A refusal that wants
to be actionable adds the marker; one that does not, is not forwarded.

Both directions are pinned, and the second is the one that would catch a future
"just forward the decoder's message":
`a_decode_refusal_pmux_wrote_reaches_the_caller_and_one_serde_wrote_does_not`
asserts the three pmux-authored refusals arrive by name AND that a caller's own
environment value never comes back. Deleting the forwarding reddens the first
half; forwarding wholesale reddens the second.

Re-measured live after the change:

    a start naming `agent` may not also carry `terminal`: an agent supplies the
    whole launch policy, and merging is refused rather than resolved, ...
    Drop `terminal`, or drop `agent` and send the inline launch fields

    a start must carry either `claude` (the inline launch configuration) or
    `agent` (an id and an exact stored version)

    agent version starts at 1; there is no version 0

    request does not match protocol v1        <- the frame carrying a value

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c68"></a>

### 68. A comment that named a syscall the code never made, and a listing that lost every record to one

*2026-08-06*

`````text
`agent.rs:880` said, in bold, that an existing `<version>.json` is never opened for write because
`create_new(true)` fails rather than truncating. `grep -n create_new crates/service/src/agent.rs`
returned one hit: that comment. The real guard was `if path.exists()` -- check-then-act with the
whole write in the window -- in front of a `rename` that silently overwrites, and the temporary
file's name was a pure function of the destination, so two writers of one `(agent_id, version)`
shared one file and `truncate(true)` let the second cut the first's bytes off mid-write. `pmuxd`
serves 64 connections at once and `update` takes no lock between reading `head` and writing, so two
ordinary callers reach it. MEASURED, 25 rounds of two concurrent updates on one fence: 7 bricked
(`head` naming a file that no longer parsed -- `trailing characters at line 1 column 1497`, which
also took `list_agents` down for the whole store), 13 answered the winner a `config_digest` the store
does not hold, 5 clean. After: 25 clean, 0 bricked, 0 divergent.

Publication is now whole AND exclusive, with two mechanisms because they are two properties: the
bytes are written and `fsync`ed under a name no other writer shares, and `link(2)` gives the finished
inode its real name and fails with `EEXIST` rather than replacing. Naming the file and refusing to
overwrite one are the same syscall, which is what a fence has to be. `head` is a pointer that moves
only after the version it names is durable. The comment and the code now say the same thing.

`list_agents` propagated `?` per entry, so one unreadable record answered the whole listing with that
record's refusal -- and `no agent <id>` recommends "list the stored agents with `pmux agent list`",
unreachable in precisely the state it is offered. Reproduced three ways, including a UUID directory
with no `head`, which `create` itself could leave. `AgentList` now carries `unreadable`: each such
record by id with the sentence `get_agent` gives, omitted from the wire when empty so an ordinary
listing's bytes are unchanged, validated by both shipped clients and printed by `pmux agent list`.
Dropping the record instead would be a stored agent silently ceasing to exist. `create` is assembled
under a staging name that is not a UUID and published in one `rename(2)`; a reader walking the store
through 200 creates never sees a half-made one.

`StartSessionRequest`'s serializer hand-wrote five `emit_policy` calls where
`agent_supplied_start_paths()` supplies nine, 130 lines above in the same file, and the comment
excused the four it dropped by saying they were "refused by name" -- in `Deserialize`, which no
in-process caller runs, since `validate_v1_serializable` only serializes. MEASURED: an embedder
sending `cell: "minified"` beside a `full` agent was accepted and launched `full`. Both presence
tables now return `[(&str, bool); AGENT_SUPPLIED_START_PATHS.len()]`, so a path added to the list
stops both compiling until each classifies it.

`admit_agent_containment`'s doc named `containment_can_only_refuse_more_never_admit_more` as the test
that proves "THE WHOLE RULE". That identifier's only occurrence was that comment; the test now
exists, in `native.rs`, because `admit_bound_resources` is private there. The golden corpus's EVENT
coverage was still a hand-written `14` -- in the same file and the same commit that derived the
METHOD count -- and neither client asserted it at all; appending `"future_event"` to
`manifest.events` was green in all three languages and is now red in all three. `pmux start --agent`
refused by naming `--model` when the value came from `PMUX_MODEL` in a shell rc, a flag the caller
never typed, which locked that shell out of `--agent`; clap's value sources now reach `LaunchArgs`,
a typed flag is still refused by its spelling, and an exported one is overridden and reported by the
variable's own name. `run_once` set `retention = OneShot` and agent resolution replaced it with the
stored `Persistent`; the decision now travels beside the request and is applied after resolution, in
the one function that owns both steps. The MCP surface promised "refused with invalid_config naming
the colliding field" and answered `invalid_arguments` with no field name for all four cases; it now
forwards the same bounded span `pmuxd` forwards, and the input schema states the rule as
`dependentSchemas` derived from the same list. The per-read owner-only guard `stat`ed the symlink and
read through it -- under `umask 077` a `1.json` linking to a `0666` file outside the store was
accepted and launched -- and now opens with `O_NOFOLLOW`, `fstat`s the handle, and reads that handle.

Per-binary isolated: 68 targets, 1048 passed, 0 failed, 50 ignored (1033 passed at <c67>; +15, no
new `#[ignore]`). TypeScript 52/52, Python 37/37. 22 delete-the-check proofs, every one reddened and
every file restored byte-exact; `manifest.json` sha256 `93c14f75...` before and after. One did not
redden and is recorded as such in the code: with publication exclusive, re-reading the published
version and redacting what was sent are equal by construction, so that re-read is defence in depth
rather than the fence.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c70"></a>

### 70. A test that chmod'd away the wrong bit, and a scope named for the one file it leaves out

*2026-08-07*

`````text
INSTANCE TWENTY-NINE, and the tool found it in its own installation. `cargo-mutants 27.1.0` is
pinned under the workspace beside `cargo-fuzz`, wired as
`gate_b/mutation_score_agent_launch_pool_protocol`, and the first thing it did was refuse four
claims the pass that installed it had written.

FOUR OF TWENTY-TWO "SURVIVING MUTANT CLOSED" COMMENTS NAMED MUTANTS THEY DID NOT CLOSE. A claim
of that shape is checkable and nobody had checked one. Re-running checks all twenty-two at once.

  * `agent.rs:969`, the `NotFound` match guard, was claimed by a VACUOUS TEST.
    `an_agent_directory_that_cannot_be_inspected_is_not_reported_as_a_missing_agent` chmod'd the
    store's parent to `0o300` and expected `stat` to fail. `stat(2)` needs SEARCH permission on
    each parent -- the EXECUTE bit -- and `0o300` is `-wx`, which grants exactly that. The stat
    succeeded, `get` returned `Ok`, and the test took an unguarded `Ok(_) => return` and asserted
    nothing at all. It passed identically with the guard deleted. MEASURED here: a parent at
    `0o300` or `0o100` lets `lstat` through; `0o600` and `0o400` give `EACCES`. The mode is
    `0o600` now, the escape hatch is gone, and the fixture's premise -- that `stat` really did
    fail with `PermissionDenied` -- is asserted before the assertions that depend on it.
  * `v1.rs:151 - -> +` in `NativeFrameAccumulator::push` needs `filled > 0` AND an input longer
    than the payload has left. The "two frames in one push" case starts its payload at
    `filled == 0`; the split-payload case ends with exactly the bytes it needs. `+` and `-` agree
    in both. A third case -- a part-filled payload finished by a push that overshoots the frame
    boundary -- closes it.
  * `v1.rs:287 > -> >=` is UNKILLABLE. Line 287 is in the `as_u64` arm, reached only when
    `as_i64()` returned `None`, i.e. above `i64::MAX` = 9_223_372_036_854_775_807.
    `MAX_SAFE_JSON_INTEGER` is 9_007_199_254_740_991, which is smaller, so everything reaching
    that line is already strictly greater than the bound. Triaged as equivalent.
  * `claude_launch is_executable -> false` had NEVER SURVIVED. All five mutants of the
    `#[cfg(unix)]` twin were already caught. The survivor is the `#[cfg(not(unix))]` twin, which
    no test on this host can kill. That test closes nothing and now says so.

THE CELL WAS NAMED FOR A CONCEPT IT EXCLUDES. `mutation_score_service_admission_and_protocol`, at
`PMUX_MUTANTS_SCOPE=admission`, mutates no file containing an admission guard:
`admit_bound_resources`, `admit_config_root`, `admit_cwd`, `claim_reaches` and
`effective_config_root` are all in `native.rs`, which the scope leaves out for wall time. It is
`mutation_score_agent_launch_pool_protocol` at `PMUX_MUTANTS_SCOPE=gate` now. Both scopes are
built from one `FULL_GLOBS` list, the printed exclusions are the set difference -- so a `full` run
prints none -- and the script prints its globs beside the score on every run.

`Cargo.toml` cited `gate_a/mutation_profile_is_dev_without_debuginfo` as asserting the mutation
profile's shape. No such cell exists in any phase. The guard is real; the citation was invented.

THE DIFFERENTIAL ENTRY-PATH TEST, AND PROOF IT CAN FAIL. Leaks 1, 2 and 3 were each "this path
lacks the guard". `every_entry_path_that_reaches_admission_answers_the_alias_family_identically`
drives one operation through every DERIVED entry path -- three scans over this crate's sources,
with a table checked in both directions -- and asserts the four answers are identical for every
alias the leaks taught: firmlink, `..` through a missing component, a terminal symlink, and a path
inside a live cell's subtree. Removing a guard from one path at a time reddens it, naming it:

  claim_reaches, containment arm dropped    -> Refused for ["pool_start"] alone
  resolve_agent_start, stored `set` dropped -> Refused for ["agent_start"] alone, caught only by
                                               the unheld-pair control row

The second experiment found a defect in the harness: `disagreement` reported dissent against the
alphabetically FIRST route, so a lone deviant sorting first was reported as three deviants that
were right. It partitions now.

SURVIVOR TRIAGE. Every survivor is named as unclosable-with-its-reason or as a real gap. Twenty
real gaps closed, eighteen of them in the pool by four DERIVED tests: eight `Display` impls that
rendered the empty string with the suite green (the set scanned from `pool/*.rs`), six
`BucketCounts` accessor constants, two on `admitted_model_list` (the `supplied_start_paths` defect
again), and two `InstanceState` predicates with `is_terminal` derived from the edge table. The
remaining eighteen pool survivors are REAL GAPS, left open and NAMED rather than filed as
equivalents: they need a live pool actor under `tokio`, which this pass did not build.

THREE MORE FALSE CITATIONS, ALL IN THE NEW TOOLING'S OWN PROSE. The script header said only the
test targets of `PMUX_MUTANTS_TEST_PACKAGES` run -- an environment variable nothing in this tree
reads or sets; the real thing is a fixed `TEST_PACKAGES` array, and the tell is mechanical, since
every real `PMUX_MUTANTS_*` name occurs at least twice in the file and that one occurred once. The
same header called `vendor/` "75% of the Rust in the tree", which is neither of its two true
values: **84.4% by file (643 of 762), 70.7% by line (311,685 of 440,778)**, both from
`git ls-files '*.rs'`. And `tools/gate-a/README.md` said `gate_b` "needs five" placeholders and is
"budgeted at four hours" -- it uses seven, and 14400 s is applied PER CELL, not per phase.

THE MEASUREMENT, AND THE HALF OF IT THAT IS NOT DONE. At the cell's own settings the four
non-`pool` files were measured to completion in one run: **428 enumerated, 61 unviable, 367 decided,
344 caught, 23 missed -- 93.73%** (`v1.rs` 90%, from 82% before this branch; `agent.rs` 96%, from
80%; `claude_launch.rs` 97%; `launch_environment.rs` 100%). **`pool/**` was NOT completed on this
tree.** Three attempts: two stopped at exactly 3599 s by this host's one-hour cap on background
jobs, and a third launched detached to outlive it and stopped by hand when the budget ran out. Its
last COMPLETE measurement is 197 of 233 decided, 84.5%, and that predates the four pool tests here.
**So no single number for the whole 702 has been measured and the docs print none.** The floor is
85% because every figure in hand clears it: 93.73% measured, and 541/600 = 90.2% even holding
`pool/**` at its stale pre-fix value. 702 mutants measured 469 in 60 minutes, extrapolating to ~2.3
hours -- inside `phase_timeouts_seconds.gate_b` = 14400 s per cell, with ~1.7x headroom. **The
cell's own first green pass is what produces the whole-scope number, which is where it belongs.**

WHAT THIS DOES NOT COVER. `native.rs` and `driver_io.rs` -- 886 of the 1,588 mutants the full
first-party scope enumerates -- are out of the cell. Only `pseudomux-protocol`, `pseudomux-client`
and `pseudomux-service` test targets decide a mutant, so the score is a LOWER bound. `timeout`
counts as caught, the one property that drifts the UNSAFE way: a loaded host scores higher.

TWO GATE CELLS ARE RED AT `<c69>` THAT THE RECEIPT OF RECORD DOES NOT NAME, measured in a
pristine worktree at that commit and byte-identical to what this tree produces:
`gate_f/phase0_self_tests` (3 of 243 -- line-range citations into `driver_io.rs` and `v1.rs` that
have rotted, the bug class inside the tool that checks for the bug class) and
`gate_f/package_smoke_self_tests` (1 of 35 -- environmental: Python 3.13 ships no `setuptools`).
Neither is caused or fixed here. The honest verdict on this host is 78/81, not 80/81.

Eight guards deleted one at a time, each target run red, each file restored byte-exact:
  claim_reaches containment arm     -> the differential reddens naming pool_start
  resolve_agent_start stored `set`  -> the differential reddens naming agent_start
  agent.rs:969 guard made `true`    -> "an unreadable store must not be reported as an
                                       absent agent: no agent 90dc69c3-..."
  v1.rs:151 `-` made `+`            -> "range end index 18 out of range for slice of length 10"
  InstanceState::fmt made Ok(())    -> "InstanceState rendered the empty string"
  BucketCounts::idle made `1`       -> "must report the 2 instance(s) recorded in it": left 1, right 2
  admitted_model_list made "xyzzy"  -> "the refusal offers \"xyzzy\", which does not name
                                       \"claude-opus-5\""
  owns_a_root made `true`           -> "reserved owns_a_root must be false": left true, right false

No new test targets: 62 before, 62 after. Per-binary isolated: 1080 passed, 0 failed, 50 ignored.
`gate_a/rust_tests` already runs every test added here.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c80"></a>

### 80. A mutation gate whose refusal named debug-assertions and overflow-checks and whose predicate read neither

*2026-08-08*

`````text
`assert_profile_is_dev_without_debuginfo` validated that `[profile.mutants]` is
exactly `{inherits = "dev", debug = false}` and refused with: "Any other key
changes what a mutation score measures: with debug-assertions or overflow-checks
off, every assertion in the tree stops being a test." The predicate never
checked either setting, anywhere. `mutants` inherits `dev`, `Cargo.toml`
declares no `[profile.dev]`, and both came from cargo defaults that nothing
here pins -- so `[profile.dev] debug-assertions = false` left every key of
`[profile.mutants]` exactly as demanded, kept the guard green, and compiled
every `debug_assert!` in the tree out of the run whose score was published.
MEASURED, both directions, by putting that table in and taking it out again.

The claim is now measured instead of read. `crates/protocol/tests/
mutation_profile.rs` is compiled under `--profile mutants` and has a
`debug_assert!` and an integer overflow fired at it; it reports the properties
it found live to `PMUX_PROFILE_PROBE_REPORT`, and
`assert_profile_properties_are_live` compares that report to
`PROFILE_PROPERTIES` -- so the guard fails whether the property is off or the
probe stopped covering it. The refusal text is interpolated from that array
rather than written out, which is the part that cannot rot: a message built from
the predicate cannot name something the predicate does not test.

Text-parsing is what created the gap, so the manifest check keeps only the claim
it can support and says so in its own refusal: it establishes NOTHING about
either property. `Cargo.toml` and `docs/testing.md` carried the same overclaim
and now separate the two guards.

`test_run_gate.py::test_the_mutation_gate_probes_every_profile_property_it_names`
is the static half: the shell array, the probe's declared constants and the
constants the probe actually asserts on must be one set, and the guard's own
body must spell none of them. Both failure modes were introduced by hand and
watched to fail -- a third property named but unprobed, and one property written
into the message by hand.

The probe costs the score nothing: it is a `tests/` target, outside every
mutation glob, and `cargo-mutants --list` enumerates the same 702 mutants with
it as without. The gate script was run for real to the end of enumeration -- all
four preflight premises green, candidates built, `enumerated_mutants=702` -- and
then stopped. The mutation run itself takes hours and was NOT run to completion,
so no score is claimed here.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c81"></a>

### 81. A README that documented ten of thirteen subcommands and none of the priority product, and a quickstart whose daemon refused every ask

*2026-08-08*

`````text
MEASURED at <c77> over README.md's 613 lines: `pmux ask` -- the entire Path B
surface -- appeared zero times, "Path B"/"stateless" appeared once between them,
and six of thirteen subcommands were named in a `pmux <name>` form. `pmux --help`
was already better than the document: it labels every subcommand `Path A:`,
`Path B:` or `Neither path:` and says which one is the product.

The quickstart was worse than incomplete. It started

    pmuxd serve --socket "$SOCKET" --runtime-parent "$RUNTIME_DIR"

which is a daemon with no pool, so a reader who followed it end to end reached

    code=UnsupportedFeature message="the stateless token engine is not enabled
    on this daemon: it is off unless pmuxd was started with --path-b-parent"

REPRODUCED, both halves: that daemon booted, `ping` and `doctor` passed, and
`pmux ask --model sonnet` was refused. The same daemon with `--path-b-parent`
and `--path-b-claude` reports `path_b_enabled: true` and a 15-slot pool, and
`ask` then reaches the compatibility gate -- refused here with
`UnsupportedClaudeVersion` because this host runs Claude Code 2.1.223 against a
promoted 2.1.220, before any child is spawned. Every quickstart block in the new
document was run in that order; the model call itself was NOT run, and no
answer, token count or latency is claimed from one.

The README also contradicted itself and the code: it said this distribution
ships "no built-in supported Claude cell" in its status block and described the
promoted set eighty lines later. `compatibility.rs`'s `PROMOTED_PROFILES` has
one entry. The MCP section named eight tools where `tools/list` answers with
thirteen, omitting `run_stateless` -- Path B again.

What the document now carries: a command table with every subcommand and the
label the binary prints for it; a Path B section with the output shape, the
model table, every `--path-b-*` flag with its default, the boot refusals
verbatim, and the five ways `ask` says no; and worked examples for `turn`,
`inspect`, `cancel`, `close`, `probe`, `attach` and `clear`.

THE DELIVERABLE IS THE TEST, because prose rots.
`tools/gate-a/tests/test_documented_surface.py` runs in Gate A cell
`gate_f/gate_driver_self_tests` and restates no list. The subcommand set and
each one's label come from `pmux --help`, cross-checked against the `Command`
variants in `cli.rs` so a stale build cannot agree with a stale README; the
Path B flags from `pmuxd serve --help`; the cap from `MAX_POOL_SIZE`; the model
table from `MODEL_TABLE`; the MCP tools from a real `tools/list` exchange on
`pmux-mcp`'s stdin; and the flags the quickstart's daemon must carry from
`pool::refusal::path_b_not_enabled` -- the message the reader who followed the
old quickstart actually hit. It runs `--help` and one stdin exchange: no daemon,
no socket, no Claude, no tokens.

Fourteen mutations were introduced and each watched to fail, then restored:
dropping the `ask` row, relabelling `clear`, inventing a subcommand, deleting
and misspelling a `--path-b-*` flag, stating a cap of 32, giving haiku an effort
tier, dropping `run_stateless`, restoring the old quickstart, adding a variant
to the clap tree, raising `MAX_POOL_SIZE`, adding a model, widening haiku's
ladder in the table itself, renaming a variant with `#[command(name = ...)]`,
and changing the cap in pmuxd's own help. One of them found a real hole on the
first run: `--path-b-retain-dir` is a prefix of `--path-b-retain-directory`, so
the substring test passed a README that named the flag wrongly; it is bounded
now.

Two counts of this suite were stale and are re-measured rather than deleted,
because `tools/gate-a/README.md` already labels the pair a description and not a
pin: it said "43 tests, ~42 s" over 45, having said "35 tests, ~8 s" over 38.
It is 51 tests, ~33 s here. `test_run_gate.py`'s own docstring said no test
touches the tracked workspace, which the budget derivation and five others have
not been true of for some time.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c85"></a>

### 85. The last manifest section pinned by hand, whose exhaustive name table forced one arm and left the array free to stay short

*2026-08-08*

`````text
`crates/protocol/tests/v1_conformance_vectors.rs` compile-enforced methods,
results and events through `wire_tags!` and left error codes as a 34-element
literal array beside a hand-written `error_code_name` table 140 lines away. Both
halves were measured to fail before anything was changed.

Appending a 35th `ErrorCode` did stop the build -- `error_code_name` is
exhaustive -- but the one arm the compiler demanded was the whole price:
`cargo test -p pseudomux-protocol` then ran 0/1/3/8/3/66 green with
`manifest.error_codes` still at 34. Renaming `ErrorCode::Cancelled` to
`Aborted`, which changes its wire spelling under `rename_all = "snake_case"`,
forced that same one arm while `"cancelled"` sat untouched beside it, and all
three conformance tests passed with the manifest naming a code the daemon no
longer emits. `clients/python/pmux_client/client.py:1146` and
`clients/typescript/src/client.ts:366` both throw on a code they do not
recognize and both pin only against this manifest, so either mistake reaches
them as an unparseable frame masking the real error.

The array and the table are one list now, under `wire_values!` rather than
`wire_tags!`. `wire_values!` was already in this file, proven on twenty-odd
plain-string enums, and it names only the variant: the string it compares is
whatever `serde_json` actually emits. `wire_tags!` would have closed the append
hole and left the rename hole, because its tag is a literal a renamer is free to
leave behind -- measured, not assumed, by running the rename against both forms.
`ErrorCode` is a unit enum, so it satisfies `wire_values!`'s constructibility
requirement; the macro definition moved up beside `wire_tags!` because
`macro_rules!` is textually scoped and the error-code assertion precedes it.

Re-verified against the shipped shape: a throwaway 35th variant fails to compile
until it is listed, and once listed fails the manifest assertion. Removed.
`cargo test -p pseudomux-protocol` is 81 passed, 0 failed across seven binaries.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c86"></a>

### 86. A citation to the exactly-one-CR rule that had drifted eight lines off the line it names

*2026-08-08*

`````text
`bin/pmux/src/cli.rs`'s prompt-terminator comment cited `cursor.rs:188` for the
"exactly one" precedent it borrows. Line 188 is `let mut consumed = 0;`. The rule
it names -- `if line_end > consumed && self.pending[line_end - 1] == b'\r' {
line_end -= 1; }` -- is at `crates/claude/src/cursor.rs:196`. Repaired while
rewriting the comment around it, since republishing a citation known to be wrong
is the class of defect this repo counts.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c92"></a>

### 92. Six internally-tagged v1 unions that each took an appended variant with every suite in all three languages green, and the manifest section that now asks serde what every variant of them spells

*2026-08-08*

`````text
`wire_values!` cannot pin them: it reads the whole serialized value with
`as_str`, which is `None` the moment a variant carries a payload, and
`wire_tags!` is not a substitute because its tag beside each pattern is a
literal that no assertion compares against the wire. So the problem was
constructibility -- you cannot ask serde what a payload-bearing variant
serializes to without building one.

`wire_tagged!` builds one. Each variant is named twice: as a wildcard-free
`match` pattern, so appending a variant to the Rust enum stops the file
compiling, and as a constructed sample handed to `serde_json`, with a
`matches!` between them so the two namings cannot mean different variants.
MEASURED at HEAD, appending one payload-bearing variant to `ConfigSource`,
`LifecycleMode`, `MessageBlock`, `RetentionPolicy`, `SessionIdentity` and
`SystemPromptPolicy` left `cargo test -p pseudomux-protocol` green six times
out of six; with the macro in place all six stop at
`error[E0004]: non-exhaustive patterns`.

The discriminant key is derived too, in Rust and Python, as the only key every
variant carries with a string value no other variant repeats -- a property
serde's internal tag always has, so the derivation either finds it alone or
panics rather than choosing. Each sample is decoded back through its own
`Deserialize`: `SystemPromptPolicy`, `LifecycleMode` and `RetentionPolicy`
hand-write that impl over a private mirror enum, and a variant added to the
public enum and not the mirror serializes perfectly and never decodes.

The six pinned by name are themselves a hand-written list, so
`internally_tagged_pub_enums` scans this crate's sources for every `pub enum`
serde tags internally -- `content` excludes the three adjacently-tagged
envelope enums, `pub` excludes the three private mirrors -- and a seventh union
that never reaches the manifest reddens rather than passing.

Both shipped clients pin against the new `tagged_unions` section, in the shape
that already worked for `value_enums`: TypeScript ties its arrays to the union
types with `satisfies`, Python reads the `Literal` discriminant off each
`TypedDict` member. `SameStrings` returns `false` and not `never`, because
MEASURED with the `never` spelling and a variant deleted, `tsc` stayed silent:
`never extends true` is true. `validateMessageBlock`'s four hand-written kinds
are now that union's own variant list with a `never` default, so a variant
pinned and not validated is a TypeScript compile error instead of a block
admitted with none of its fields checked.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c93"></a>

### 93. Fourteen regression names in six hand-written copies, every lane running them with an --exact filter that skips in silence, replaced by one module the patch defines and a scan that refuses any file but two the right to name one

*2026-08-08*

`````text
Reproduced first at HEAD: appending a fifteenth `#[tokio::test]` to the pmux
block of `vendor/rmux-server/src/pane_io/tests.rs` and bumping
`patched_pane_io_tests_sha256` left `cargo test -p pseudomux-rmux --test
vendor_server_patch` green, all fourteen macOS `--exact` cells green at
`1 passed` each, and `tools/linux-docker/tests` at its one pre-existing
failure. The crate's test count went 2751 to 2752 -- it compiled everywhere
and ran nowhere, exactly as the review said.

The six copies were `crates/rmux/tests/vendor_server_patch.rs:576`,
`docs/testing.md`, `tools/gate-a-candidate/phase-manifest.json` (14 cells),
`tools/linux-docker/suite.sh` (14 gates),
`tools/linux-docker/tests/test_runner.py` and
`vendor/rmux-server/PMUX-PATCH.md`, with
`tools/linux-docker/gate-a-manifest.json` carrying a seventh copy of the same
cardinality as one gate id per name.

`patch_regression_names` reads the `#[tokio::test]` identifiers out of the two
spans `reconstruct_upstream_pane_io_tests` already removes -- a test the patch
adds is a test the reconstruction deletes -- over bytes the sha256 fixture
already freezes. `patch_regression_module_filter` derives `pane_io::tests::`
from the source path, and libtest matches a filter as a substring unless
`--exact` is given, so the 28 per-name cells across the two manifests are now
one cell each that runs whatever the module holds. Gate A is 70 cells.

The names now occur in exactly two files, and the new test walks the workspace
and refuses every other one; nothing enumerates lanes for that half. The
patch document's list is compared element for element against the derivation,
and the four prose claims about the set's size are asserted with the count
supplied by the derivation rather than typed.

Proved by the only measurement that counts: with a fifteenth regression added
and published, the macOS cell, the `suite.sh` command body and the documented
command each ran `pane_io::tests::probe_...` at 81 passed with no lane edited,
`vendor_server_patch` had already refused the unpublished one by name, and the
Linux runner self-test's subTest set grew to include it. Six more deliberate
breaks -- a deleted published name, a stale count word, a restated name in
`suite.sh`, `--exact` back in the cell, a lane without the filter, and the gate
missing from the Linux projection -- each went red with its own message and
each was restored.

Not run here: the Linux docker lane itself (debt row C6). Its only pre-existing
red, `test_linux_manifest_is_the_exact_ordered_candidate_projection`, reports
the same six and seven gate names before and after.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c106"></a>

### 106. A retraction three source files cited by a line number that stopped being the row the same commit moved, and the five ASCII punctuation characters a sweep claiming every one of them never sent

*2026-08-09*

`````text
`docs/path-b.md` line 187 was §2.2's MCP-isolation row. `<c101>` -- the commit that
fixed the leak that retraction caused -- inserted rows above it, and line 187 became a
paragraph about the replace-mode system prompt. Four files in the code tree and six
cross-references in two dated receipts were still pointing at it, and one of the four was
written by that same commit.

Measured over the six documents `docs/path-b.md` §0.0 now marks as Path B documents,
counting only citations naming an identifier the cited file holds: 22 of 37 pointed at a
line that did not hold the thing the sentence named -- 83% in `path-b.md`, 71% in
`version-drift.md`, 54% and 50% in the two 2.1.226 receipts, 40% in an adversarial
document four days old. Zero paths rotted and zero pointed past a file's end, so nothing
announced any of it.

`crates/service/tests/path_b_doc_citations.rs` refuses a Path B document cited by line
from a code tree or from another Path B document, requires every `§N.M` a comment names
to be a heading that document has, and requires a `path:line` citation inside one to land
on the line holding the identifier its sentence names. Its document set is read out of
§0.0's table, not listed: a row naming a missing file, a status outside the published
three, or a deleted table all refuse. 0 of 40 today, and it caught four citations broken
by this change's own eight-line insertion into `claude_launch.rs`.

Re-asking §2.2's eleven standings against their own predicates: four rested on an
instrument that could not observe the case, two of those had wrong conclusions.
`--max-turns` is parsed at 2.1.223 and 2.1.226 -- three near-miss spellings name
themselves, and the host running this session passes it -- against a row reading STILL
TRUE on "does not exist in 2.1.220". `--setting-sources user` was excused by a private
root that pins the `user` source and cannot see `project` or `local`, which the empty cwd
of §4 is what actually closes. `--safe-mode` was excused by a distinction from
`CLAUDE_CODE_SAFE_MODE` that 2.1.226's help denies in as many words.

`crates/claude/src/composer.rs` said its sets were complete over ASCII punctuation and
swept 27 of 32. `PUNCTUATION_THE_SWEEP_DID_NOT_SEND` declares the five with a reason each
-- `@`, the one with positive evidence of mode-like behaviour, was argued about in prose
instead of being sent -- and the completeness check derives the alphabet from
`char::is_ascii_punctuation` rather than restating it.

Gate A's `rustdoc` cell had been failing since `<c101>`, which made `MINIFIED_CELL_FLAGS`
public with two intra-doc links to private items. Neither `cargo test --all-targets` nor
`cargo clippy` runs that lint, so three sessions reported a green workspace over a red
gate cell while each disclosed it had not run Gate A.

Reconciled to what the tree now holds: the manifest is 70 cells and Gate A as run is 62,
not 83; the drain is a pooled bound over four versions, not a per-version fit; the
promoted cell is the range 2.1.220 through 2.1.226; the ledger's 85 sits beside 49 real
turns outside it; `docs/spec.md`'s normative argv list gains the cell-owned appendix it
never mentioned.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c109"></a>

### 109. A citation ban that knew one of the four spellings a reader resolves identically, and the one live instance of the shape it refuses sitting inside the paragraph that forbids it

*2026-08-10*

`````text
`nothing_cites_a_path_b_document_by_line_number` searched for each linted
document's fully-qualified path followed by a colon, as a literal. `path-b.md`
written bare walked through, and so did `./docs/path-b.md`, whose leading `/`
the left-boundary guard read as evidence of a different file. The one live
instance in the tree was `docs/path-b.md` §0.4 itself, narrating the defect the
rule exists for -- the exemption the test's own module doc says it does not
need. The ban now resolves a cited path the way a reader does, by
path-component suffix, so `evidence/README.md` stays out (it is LONGER than the
reading order's `README.md`, therefore a different file) and every shorter
spelling of a linted document comes in. Proven able to fail on all three
spellings; before the change only the qualified one was caught. §0.4's sentence
now describes the citation instead of spelling it, in the same seven lines, so
the thirty line citations into this file from the unscanned half of `docs/` did
not move.

The grader joined only the line ABOVE a citation. Markdown hard-wraps in both
directions: `docs/version-drift.md` said *"flipping the predicate ... from
`since_candidate > 0`"* with the identifier on the FOLLOWING line, so the
citation named no gradable anchor, was skipped, and pointed at a `continue`
inside a JSON-decode loop 163 lines from the predicate it quoted. Two more in
the same paragraph named a counter and an exit code and pointed at the same
loop. The one-sidedness was never a rule about markdown; `structural` and the
own-citation guard, which were always symmetric, do the work it was standing in
for. Coverage 56 -> 62 of 133, and the module doc no longer opens with "every"
over a rule that grades what it can and skips what it cannot -- the vacuity
assertion prints the pair it saw, so the figure comes from the run.

`a_head_that_is_not_this_prompts_head_proves_nothing` named the `contains`
weakening in a comment and did not test it: `"hello".contains("hello world")`
and `"hello".contains("hellp")` are both false, so neither assertion could tell
the two predicates apart. `prompt.contains(head)` survived
`pseudomux-claude` (31) and `pseudomux-service` (415). It now fails, on this
module's own reproduction with the shell command moved to the end.

`is_trimmed_from_the_end` was documented as "JS `String.prototype.trimEnd`'s set
-- White_Space plus U+FEFF" as though those named one set. `trimEnd` strips 25
code points, White_Space contains 25, and they are not the same 25: the shipped
`White_Space u {U+FEFF}` is 26, a strict superset by U+0085 (NEL) -- the one
member of the shipped set no row of `MEASURED_LAST_CHARACTER_SWEEP` has sent.
The set is kept, because which way it errs is the point: trimming a character
Claude keeps costs the caller a trailing NEL, while keeping one Claude trims
costs the instance. The identity claim is retracted, the trade is stated, and
the difference is now derived from both sets rather than asserted -- three
mutants caught, including the narrowing that would "fix" it.

`gate_a/rust_clippy` was red at `<c108>`: `rest.starts_with(|c: char| c == '-')`
is `manual_pattern_char_comparison`, which `-D warnings` makes an error. Five
parallel reviewers ran no Gate A cell and did not see it.

README's `invalid_config` row still named only the tab. `bin/pmux/src/cli.rs`
still said "exactly one text-file terminator dropped" over a call that drops the
whole trailing run, and drew the equivalence with the cursor's CR rule that
would invite the `strip_suffix` back.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c111"></a>

### 111. Five of the seven stateless refusals named what went wrong and no action, and the one advice string that did ship described its own detector instead of a next step

*2026-08-10*

`````text
`pool/refusal.rs` has seven `pub fn` returning an `ErrorBody`. Two carried a
`details.recommendation`, which is the one key `bin/pmux` renders and the only
channel a refused caller can read an instruction out of; five carried a census,
a violation and nothing to do. `retryable` is not that sentence — it says a
retry CAN succeed, not what else has to change first, and `daemon_shutting_down`
is `retryable: true` against a daemon that will refuse every remaining turn.

All five now advise, and the advice is checked from the module's own census
rather than from a list beside it: a new `pub fn` reddens
`the_refusal_census_names_every_constructor_this_module_has`, which forces a
census entry, which forces a body in the shared `every_refusal_body`, which
lands in `every_pool_refusal_says_what_to_do_next` with no action. Proven by
building an eighth refusal three ways — as a bare `pub fn` (census red), and
censused and bodied with no advice (`pool_wedged refuses a caller without
naming an action`) — and by deleting the advice from two of the seven.

`ErrorBody::advising` and `RECOMMENDATION_KEY` move the key into the protocol
crate so the daemon that writes it and the two surfaces that read it share one
spelling. It MERGES into `details`: every advised refusal also carries a
`violation`, and a builder that replaced would have dropped whichever half was
written second — mutated to replace, nine tests redden.

The bar found a second defect while being written. `pool_halted`'s
recommendation is `RepromotionTrigger::ClearScreenOrPreambleMismatch`'s `how`,
a field documented as "what the operator does about it", and that one value of
five was three clauses of description with no next step — while its four
siblings all end in an imperative. It now says to re-promote and restart, and
the assertion in `compatibility.rs` that claimed to check exactly this now says
what it actually tests, which is that the field is not blank.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c112"></a>

### 112. The MCP surface answered every daemon refusal there is with one constant sentence, so a `/`-prefixed prompt and a daemon with no pool arrived as the same payload

*2026-08-10*

`````text
`redact_client_error` kept `code` and `retryable` and threw away `message` and
the whole of `details`; `ToolCallError::result` then rendered the constant
`"pmuxd rejected the native request"`. On the one Path B surface whose reader
cannot ask a human, "the stateless token engine is not enabled on this daemon"
and "your prompt starts with `/`" were byte-identical: same code, same
retryability, same sentence.

`details.recommendation` now crosses, in both channels — the `content` text a
model reads and the structured error — and nothing else in `details` does. The
key is read through `RECOMMENDATION_KEY`, the same constant the daemon writes
it with, so it cannot go quiet on one side of a rename.

**`message` deliberately does NOT cross, and that is the half this does not
fix.** A daemon message is not always pmux's own composition: MEASURED,
`{"environment":{"set":{"SECRET":42}}}` comes back as ``invalid type: integer
`42`, expected a string``, so forwarding messages would forward caller values
out of every start frame's environment, inline settings and system prompts.
`recommendation` is written by `ErrorBody::advising` and by nothing else,
always out of pmux's own vocabulary. A test pins that asymmetry: a body whose
message holds `42` and whose advice says "send a string" renders the advice and
not the 42.

That left the second half of the example. `ComposerRefusal` published its
remedy only INSIDE the message, so on a redacting surface a mode-prefix refusal
still said nothing. `explain()` and `remedy()` are now separate and `describe()`
is the two joined, byte for byte as before; the daemon puts the remedy in the
advice channel and the CLI prints message then advice, which reads as the one
sentence it always printed. Splitting them found a variant with no remedy at
all — the general `RewrittenCharacter` arm, which no constant can reach today
because `COMPOSER_REWRITTEN_CHARACTERS` holds one character and the specific
arm covers it, so the test that requires a remedy had never rendered it. Both
general arms are now asserted directly.

Six mutants, each applied and restored: MCP drops the recommendation again; MCP
keeps it structured but out of `content`; the composer refusal stops advising;
the message goes back to `describe` so the halves double up; the general arm
loses its remedy; `describe` stops joining. All six redden, five of them on the
test written for them.

The 121 `path:line` citations this session's edits moved were repaired by
mapping each one's ANCHOR TEXT from `<c110>` to where that line is now, rather
than by shifting numbers by a delta — including the 59 in `docs/` that no
grader scans and the ones in `bin/pmux/src/cli.rs` and
`tools/phase0/tests/test_verify_calibration.py` that live in sources.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c116"></a>

### 116. A citation grader that skipped 70 of the 132 claims its heading said "every", and the 37 line citations of the document it protects that sat in the half of docs/ it never opened

*2026-08-10*

`````text
`path_b_doc_citations` graded a citation only when the sentence named an
identifier the cited file holds, and passed over the rest in silence. That was
70 of 132. A table row giving a path, a number and an English paraphrase has
nothing a predicate can hold to, so the grader said nothing and the module said
"every". Rule 2 is now total: a citation it cannot grade is REFUSED with a
message naming what to add, which is the answer rule 3 already gave for the
abbreviated form -- a citation that escapes the checker is worth less than none.

The set of things graded is derived rather than listed, in four places that were
each a literal:

* The scanned set is the workspace minus build output and `vendor`, not six
  named trees. `docs/` outside the linted set had never been opened, and it held
  47 line citations of a linted document -- 37 into `docs/path-b.md` from
  `docs/sandbox-spike.md` and `docs/linux-handoff.md`, most already pointing at
  unrelated paragraphs. All 47 are now section citations or fully-qualified
  paths, including six that meant `tools/linux-docker/README.md` and one that
  meant microsandbox's.
* A quotation is four marks, not one: backticks, straight quotes, typographic
  quotes and markdown emphasis. `crates/service/src/pool/config.rs` records a
  wave as a bolded list of milliseconds and the document that cites it repeats
  them in bold; that is a quotation of the cited line, and the grader could not
  see it.
* An anchor is an identifier the file holds OR a phrase that occurs in it
  verbatim, compared after both sides are read past comment markers, line
  wrapping and emphasis. Half of these documents cite a MEASURED *comment*, which
  the identifier rule could never grade.
* The citation scanner and the path-shaped-span filter kept two copies of the
  extension list and they disagreed, so a `.tsx` citation was invisible to one
  and visible to the other.

Four grader defects fell out of running it: a span containing a slash was read
as a path and discarded, which threw away every quoted sentence mentioning
`/clear`; a `path:line::test_name` span was discarded whole, losing the test
name inside it; a bullet or table row could not reach its own wrapped
continuation; and `>` was treated as structural, so two lines of one blockquote
were two claims.

Nine of the 60 offences were rot rather than absence, three of them invisible
for the same reason the 70 were skipped -- the composer's two anchoring
measurements are 126 lines below the rows that cited them, and a docstring 168
lines below the sentence quoting it. `§10.7` of `docs/path-b.md` has never
existed; the two files naming it and the two places inside the document itself
now say §10 item 7.

Rule 2's predicate over source is measured and NOT shipped, and the module says
so with the number: 38 of 55 gradable citations in `.rs`, `.py` and `.sh` do not
land on what they name. The 23 whose anchor sat in exactly one place are
repaired here -- `spec.md:546-547` for R1's normal turn path was 650 lines out,
`engine.rs:126` for `UnexpectedTypedPrompt` one line out. Turning the rest on is
a defect list of 38, mostly Path A.

Proved red both ways, six mutants, each restored: a line citation of a linted
document, a `§N.M` that is not a heading, an anchor removed from a sentence, a
citation moved one line, a path abbreviated to a basename two files share, and
an abbreviated citation. `cargo test --workspace`: 1169 passed, 0 failed.
`cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`
and `cargo fmt --all -- --check` clean; `ruff check --no-cache .` clean.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c117"></a>

### 117. The five register rows nobody had worked, and a source digest that omitted every committed byte of evidence while hashing ten files Finder rewrites

*2026-08-10*

`````text
`docs/repo-review.md` and `docs/path-b-verdict.md` had these adjudicated and
open. Working the register rather than re-deriving it:

**The Gate A source digest was two hand-written lists.** `SOURCE_ROOT_FILES` and
`SOURCE_ROOT_DIRS` hashed 951 files: 10 gitignored `.DS_Store`, which Finder
rewrites when anybody browses a directory, and none of the 12 tracked files
outside the nine named directories -- `evidence/` entire (8 files, one of them
the model-attempt ledger `gate_f/gate_driver_self_tests` reads), `LICENSE-*`,
`.gitignore`, `.dockerignore`. A digest that omits committed evidence and
includes untracked noise is not the integrity claim `source_unchanged` makes.

The set is now derived from `.gitignore` -- the repository's own committed
declaration of what is not source -- and NOT from `git ls-files`, because
`docs/gate-c-linux-handoff.md` documents running this driver against a
`git archive` export with no repository at all. The parser refuses any pattern
form it does not implement, so an unsupported one is fatal rather than silently
unmatched. `test_the_source_digest_is_exactly_what_the_repository_calls_source`
asserts the derived set equals `git ls-files --cached --others
--exclude-standard` in both directions: 953 files, identical. Two independent
derivations of one set is what keeps a hand-written matcher honest.

`.git` is skipped by NAME, not only as a directory: in a worktree it is a file
naming somebody's checkout, and the walk had been hashing it into a number whose
whole purpose is to be equal on two hosts.

**`docs/testing.md` §F listed seven shell scripts; the manifest lints eight** --
it was missing `scripts/gate-a-mutants.sh`. Fixed, and now checked against the
two cells' argv, so the block cannot drift from the gate again.

**`tools/gate-a/README.md` published "gate_b 6/6 in 138 s"** two lines under its
own statement that `gate_b` has eight cells: a receipt for a phase that no
longer exists. Removed, with the real number named -- the last real `gate_b`
receipt spent 5,285 s on the mutation cell alone -- and the census line above it
is now derived from the manifest.

**`scripts/gate-a-fuzz.sh` hand-listed its three targets in five places.** The
set comes from `fuzz/Cargo.toml`'s `[[bin]]` entries now; the seeds and length
bounds stay written down, because they are the evidence, and a target the
declaration does not know ABORTS the gate. Proved: a fourth `[[bin]]` exits 1
naming it.

**`tools/screen-corpus/seed_corpus.py` ignored argv and resolved two relative
paths against the working directory.** Run from one directory up it matched no
fixtures, created a `crates/…/corpus` wherever you stood, and wrote a corpus
holding the 2.1.220 frame and none of the five 2.1.70 captures -- exit 0, with a
count. Paths now come from `__file__`, argv is parsed, and a fixture set that is
not exactly five is fatal. Verified from `/tmp`: writes the real corpus
byte-identically, creates nothing local, refuses `--nope`.

**`ask` and `agent` were absent from `bin/pmux/tests/cli_contract_matrix.rs`**,
whose every test is named "for every command", in a file covering 11 of
`Command`'s 13 variants -- omitting the one `pmux --help` calls the entire
surface of Path B. Both added; all five boundaries pass for both in all three
output modes. `the_matrix_covers_every_subcommand_pmux_publishes` derives the
set from `pmux --help` and reddens when a surface is dropped, proved by dropping
`ask`.

Every citation into `run_gate.py` and `test_run_gate.py` that this moved is
repaired -- 20 of them across four documents, a test and an e2e comment -- and
each was re-resolved against the line it now names.

`cargo test --workspace`: 1170 passed, 0 failed. `tools/gate-a/tests`: 56
passed. clippy `-D warnings`, `cargo fmt --check`, `ruff check --no-cache`,
`ruff format --check`, `bash -n`, `shellcheck` all clean.
`PMUX_E2E_BIN_DIR="$PWD/target/release" bash scripts/gate-a-residue.sh`: passed.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c121"></a>

### 121. The version query nothing ever called, the field count no serializer in this tree reads, and the two Drop bodies the compiler already writes

*2026-08-10*

`````text
Thirty-five of the 136 full-scope survivors, closed. Every one of the 24
mutations below was applied by hand, watched red against the test written for
it, and restored.

**The field count is now checked against the emissions it counts.**
`StartSessionRequest::serialize` computes a field count for
`serialize_struct`, which `serde_json` discards -- so a full-scope run made
every term of that arithmetic wrong in turn (`+=` as `-=` and as `*=`, `4 *` as
`4 +` and as `4 /`, the `!` deleted) and thirteen mutants survived, because the
only serializer this workspace runs it through is the one that ignores the
number. A non-self-describing format writes it as the frame's own element
count, where a wrong number is a corrupt frame rather than a wrong one. The
count is a second statement of the emission rules below it, so it is now
compared against them: every field leaves through one `emit!` that counts
itself, and a disagreement is a serialization error naming both numbers. The
`4 / usize::from(emit_policy)` mutant needed one more thing -- it divides by
zero exactly when an agent is named, and no test in the workspace had ever
serialized an agent-naming start that this serializer ACCEPTS, only ones it
refuses. `an_agent_start_emits_the_agent_and_omits_every_path_the_agent_supplies`
is that test, and its expected key set is the inline start's own keys minus
`agent_supplied_start_paths` rather than a second list.

**`claude_version_of` had no caller in any test.** It is the input to
`RequireTested`: the whole compatibility decision rests on the string it
returns. Replacing its body with `Ok("xyzzy")` or `Ok(String::new())` left the
suite green, and so did deleting the `!` from `if !output.status.success()`,
which admits exactly the runs that FAILED and refuses the ones that succeeded.
`the_version_query_reads_the_child_it_actually_ran` runs three real probe
executables -- one that prints a version, one that exits non-zero, one that
prints no version -- and then goes through `detect_claude_version`, the caller a
start actually uses.

**`performance_layer` was never asked about the envelope itself**, only one
millisecond over it and far under; read as `>=` it reports a healthy daemon as
faulted at the one duration the envelope names.
**`startup_screen_diagnostics`'s `!line.trim().is_empty()`**, with the `!`
deleted, computes every count and offset in an operator's refusal over the BLANK
lines. **`wait_until_ready_with_timings`'s stability equality INVERTS under
`!=`**: a screen repainting on every poll settles, a screen holding still never
does, and both directions are now asserted because the mutant passes any test
shown only one of them. The two `diagnostic_*` encoders are pure and were
untested; `Default::default()` publishes `null` for every count in that same
refusal.

**Two survivors are EQUIVALENT and the argument is worth recording.** Replacing
`Drop for SessionLifecycle::drop` or `Drop for IdleReaper::drop` with `()` does
not remove the field drops. Both bodies are `request_shutdown()` -- which drops
the `oneshot::Sender` -- followed by `drop(self.task.take())`, which detaches;
field drop runs the same two in the same declaration order, and the task's
`tokio::select!` arm is `_ = &mut shutdown_requested`, which fires on the
`Err(RecvError)` a dropped sender delivers exactly as it fires on `Ok(())`.
Neither is a hole; both are drop glue restated.

`docs/agent-resource.md` and `docs/current-state.md` cited eleven v1.rs lines
the serializer change moved; each relocation was verified as identical text at
the old line in HEAD and the new line here, not assumed from the diff.
`docs/repo-review.md`'s four native.rs citations are left alone: that document
is pinned at HEAD `<c82>`.

cargo test --workspace: 70 binaries, 1202 passed, 0 failed, 51 ignored.
cargo clippy --workspace --all-targets -- -D warnings: clean.
Gate A residue audit: passed.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c126"></a>

### 126. The two live rows the adversarial derivation dropped without a word, under a criterion titled for the suite they are the live half of

*2026-08-11*

`````text
`adversarial_commands` reads `docs/path-b-adversarial.md`'s "Verification at
this commit" tables and keeps every row whose first cell is one backticked
`cargo test`. Its docstring accounted for what it discarded as "`cargo fmt`,
`cargo clippy`, `ruff`, the residue audit -- Gate A cells, and criterion 4 reads
a Gate A receipt for them rather than running them twice."

That accounting is short by the rows that matter. Printed rather than argued,
the discards are six, and two of them are

    live re-verification, rebuilt release binaries        (section 10)
    live verification, rebuilt release binaries           (section 11.6)

-- the only rows in either table that record a real model turn. No Gate A cell
covers them and nothing else in this file looks at them, so criterion 2 reported
"The adversarial suite passes" having measured seven offline `cargo test`
commands and nothing live. A criterion may be offline; it may not be offline
under the name of a suite whose live half its own derivation deleted on the way
past.

The third kind of row is now derived alongside the first two and NOTED, one
line per label, so the criterion prints what it did not measure. Not refused:
which rows are live turns is a reading of the owner's criterion and a script
that promoted a dropped row to a failure would be legislating it.

Proved responsive to the document rather than to a constant: relabelling section
10's live row as `ruff check --no-cache .` drops exactly that entry and leaves
section 11.6's, and the document was restored byte-exact.

Also corrected, from the same habit of reading a set back instead of asserting
it: `deliberate_red_cells`'s comment credited row **C10** with naming
`release_full_stack_e2e`. Printing the per-row name sets says it is **C6**, in
the sentence about the ordering `test_runner.py:821` forbids. The derivation was
right and its stated reason named the wrong row.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c129"></a>

### 129. A remedy sentence written into the verdict without being run, refuted by running it: `--commit` exits 2 rather than re-reading the verdict it promised

*2026-08-11*

`````text
`<c128>` added, to section 8.4, the sentence "`scripts/path-b-done.sh --commit
<c127>` re-reads the verdict for the commit the gates actually graded." It was
reasoned from the flag's existence and never executed.

Executed, it exits **2** and refuses:

    this working tree is at <c128> and the commit being judged is <c127>.
    Criteria 2 and 5 run tests against the tree in front of them, so a verdict
    taken here would be a verdict about neither.

The refusal is correct -- `--commit` binds the receipts and the registers, and
cannot bind `cargo test` -- and it is the same defect this repository keeps
finding, committed into the document that exists to name it, by the commit that
named it.

Section 8.4 now records the refusal verbatim and states the only two honest ways
to re-read criterion 4: put the commit in front of the gate, or re-run both
pinned gates at the new head for 2 h 20 m. Section 8.6 carries the correction as
its own bullet rather than quietly absorbing it, and names the verdict of record
for this work: the one taken at `<c127>` with both receipts, NOT DONE 3/5.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c134"></a>

### 134. The site count a receipt would have published from arithmetic, and the 95 measured claims the scan that produced it cannot see

*2026-08-11*

`````text
The paragraph this replaces said nineteen new version-keyed sites had all come
from the composer work. That was 44 minus 25 and a plausible story, not a
reading: the 25-site list is not in this tree, and `native.rs`'s MEASURED note
is in the earlier count and NOT in this one. Subtracting two totals and naming
the difference is the same defect as a comment that promises more than its
predicate tests, committed inside the document that spends two sections on it.

What replaces it is what the scan actually returns: 44 version-keyed production
sites, broken down by which versions each window names (14 at 2.1.220, 20 at
2.1.226, 6 mentioning 2.1.227), and a group table that sums to 44 rather than
to whatever the rows happened to add up to.

The number worth having is the one the scan was never asked for. Run for
`MEASURED` with NO version literal in the window, the same boundary finds **95
more** production sites -- so 44 is how many claims a version gate could be
reasoned about and 139 is how many measured claims exist. One of the 95 is a
version claim in every sense but the spelling: `native.rs` says "MEASURED, on
this host, at two versions and byte-identically" and names neither, which is
exactly the shape a scan keyed to `2.1.x` cannot see.

The opening caveat is also narrowed. It named "the four 2.1.220-keyed constants
that need a minor step", a count from `version-drift.md` sec.3.6's hand-written
list rather than from this document's own scan; the scan says 17 sites still
name 2.1.220.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c135"></a>

### 135. A probe count that added the two flags it excluded, a frame that stopped one row short of its own footer, and a byte difference explained by a cause nothing varied

*2026-08-11*

`````text
Three numbers in the new receipt were arithmetic rather than readings, and the
document spends two sections saying not to do that.

* "24 of 24 derived spellings accepted" counted a set that does not exist: 25
  spellings less the two prompt-consuming ones less `--effort` and
  `--permission-mode`, whose vocabularies are probed value by value instead, is
  **21**, and 21 is what the probe returned at both versions. 33 probes per
  version still adds up; the row describing them did not.
* The post-`/clear` frame was given as rows 20-22. It is rule, composer, rule,
  footer -- four rows, 20-23 -- which is the shape every constant derived from
  it is about, so quoting three of the four undercut the sentence's own point.
* The 1935-byte preamble was explained by "this harness's working-directory
  path length". Nothing varied the cwd. What the A/B establishes is the useful
  half and the only measured one -- it is NOT a version difference, because both
  versions agree under one harness -- and the cause is now written down as the
  inference it is.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c136"></a>

### 136. A pane size named twice with no source, and a startup disclosure inherited from a receipt for a different version instead of read from this one

*2026-08-11*

`````text
Two corrections to the same document, both the shape it is about.

The "not established" list said "one 24x80 pane, one 24x120 pane" without
saying where either came from. 24x80 is every PTY replay's; 24x120 is
`stateless.rs::POOL_TERMINAL`, which is what every turn the promotion run drove
actually ran at, and naming the constant is the difference between a caveat and
a number someone can check.

The startup quota check was copied from `docs/2.1.226-compatibility.md` sec.7.2
and asserted "present at both versions". It was not read at either version by
this session. It is now read at 2.1.227 from the child's own `--debug-file`:
one line, `[API REQUEST] /v1/messages ... source=quota_check`. A disclosure
inherited without measurement is exactly what the section above it spends a
paragraph refusing.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c137"></a>

### 137. Every instrument this repository owns, in a verdict that ran sixteen of forty-four, and the difference between an A/B and one measurement counted rather than blurred

*2026-08-11*

`````text
The document's own verdict line said "every version-keyed instrument was run
against 2.1.226 and 2.1.227". Its section 9 then listed four sites nothing
re-measured, and its section 2 marked twenty-four more as measured at 2.1.227 by
the criterion-1 work rather than by this session's A/B. The summary was true of
no set: not of 44, and not of the 16 the A/B actually covers.

It now counts three groups instead of collapsing them, because the distinction
is the evidence and not bookkeeping. A property measured at BOTH versions can
answer "did the step move it". A property measured at 2.1.227 alone cannot --
it is known to hold there and there is nothing to compare it against. Sixteen
answer the question this document exists to ask; twenty-four are known at the
new version; four are unmeasured and named.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c138"></a>

### 138. A `ready` that was said to prove the credential pin, when a logged-out cell renders the same composer

*2026-08-11*

`````text
`pmux start --cell minified` returning `state: ready` proves argv acceptance,
the private root, the pane geometry and the composer gate. It does not prove
the cell authenticated: `classify_terminal_snapshot` is looking for a ready
composer, and a cell that resolved no credential still draws one.

The reading that does prove it was already taken and was not being used. The
PTY replay's own welcome box, in a private root pmux seeded, names the plan and
the organization -- which a cell that failed to resolve the operator's keychain
item through `sha256(config_dir)[0..8]` cannot draw, because it is showing a
login prompt instead. Same session, same measurement, a predicate that can
actually fail.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c141"></a>

### 141. The shape the veto carried for a whole run and never reported, beside the comment that said it was the one a refusal would name

*2026-08-11*

`````text
`unrecognised_screen` held `(ScreenShape, u64)` -- the first frame of the
current unbroken run, and the instant it began -- and its declaration said so.
Every read of it was `Some((_, since))`: the shape went in and nothing ever took
it out, because the refusal names `shape`, the frame observed on the iteration
that fires. That is the right frame to name, since it is what a person opening
`pmux attach` a moment later is looking at, so the stored one was not a
disagreement about behaviour. It was a comment promising a field nothing read.

Now `unrecognised_screen_since: Option<u64>`, holding the one thing the veto
uses, and the declaration says which frame the refusal reports and why. Both
veto tests still fail under both mutations -- the window never elapsing, and a
transcript row no longer restarting the clock.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c144"></a>

### 144. One prompt limit stated six times in three binaries and tied nowhere, a release daemon the first live pass measured and this tree never built, and the thirty-sixth survivor of a run that scored ninety-six

*2026-08-12*

`````text
The guard list the live adversarial suite fires is read out of `composer.rs`
and `validate_prompt` rather than written down, and the harness refuses to send
anything until its probes cover both sets in both directions: 47 probes, 47
refused by the daemon with the refusal a transcription of the two shipped
predicates predicted, over `pmux ask` AND a hand-framed request on the daemon's
own socket. One diverged. `bin/pmux` carries its own `MAX_PROMPT_BYTES` and
refuses an oversized prompt saying "CLI limit" before the daemon's "service
limit" can fire; that constant is declared six times across `bin/pmux`,
`bin/claude-p`, `crates/service` and three test files, all `1024 * 1024`, with
nothing holding them equal. `bin/pmux/tests/prompt_limit.rs` scans for every
declaration instead of restating the number.

The first pass of the whole suite ran against a `target/release/pmuxd` that
`cargo build --locked --release --workspace` then replaced, leaving `pmux`
byte-identical because it does not link the service crate. Those 23 turns are
counted in the receipt and their results are discarded.

Full scope at `<c143>`, 3 h 16 m: 1,661 enumerated, 1,120 caught, 36 missed,
**96** against a floor re-derived from this campaign's own per-mutant logs at
**94**. Thirty-five survivors were held; the thirty-sixth is the CR strip in
`read_rotation_anchor`, EQUIVALENT because a probe test showed `from_slice`
returns an equal value with the CR left on. Twelve rows moved to KILLED and
nothing regressed.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c154"></a>

### 154. A scan that gave two files the right to name a patch regression, and the two upstream documents that had been failing it since the day each landed

*2026-08-12*

`````text
`every_gate_lane_runs_the_derived_regression_module_and_no_file_restates_a_name`
walks the workspace and refuses any file but `vendor/rmux-server/src/pane_io/tests.rs`
and `vendor/rmux-server/PMUX-PATCH.md` the right to spell one of the fourteen
patch-owned regression names. `<c150>` added `docs/rmux-upstream-state.md`, which
records which vendored file the upstream repro was copied from and quotes the
libtest line naming it; `<c151>` added
`docs/upstream-issues/02-rmux-server-attach-eof-drops-buffered-frames.md`, whose
repro IS that regression -- its source, the `cargo test` line a maintainer runs,
and the measured failure output all spell the name. Neither commit ran the gate.
`cargo test --workspace` has therefore been red at `<c150>`, `<c151>`, `<c152>`
and `<c153>`, and Gate A's `gate_a/rust_tests` and `gate_a/rmux_server_vendor_patch`
cells would have been red at all four.

The scan's set-of-files-to-check was hand-written, which is the half of the house
bug class that says prefer deriving. It is now two BOUNDARIES compared by prefix:
`vendor_root()` -- the crate the patch patches, which owns the names outright,
definition and publication both, and which no longer has to name the two files
inside it -- and `UPSTREAM_REPORT_HOMES`, the reports that quote them. A third
file inside either needs no edit here. The refusal message is built from the same
derivation it enforces, so it names the boundaries rather than restating them.

The upstream half is not derived and cannot be, for the reason `REGRESSION_LANES`
already gives about its own membership: only the address distinguishes a document
that quotes a name from a lane that restates one, and that is said in the comment
rather than left to look derived. MEASURED both ways: the four
`vendor_server_patch` tests pass, and a one-line file planted under `docs/` is
still refused, by a message that now reads *"the names belong under
docs/rmux-upstream-state.md, docs/upstream-issues, vendor/rmux-server and nowhere
else"*.

`cargo test --workspace --no-fail-fast`: 1226 passed, 0 failed, across 73 binaries.

`docs/rmux-upstream-state.md:284` cited this test's `let required = [` block by
line; rustfmt collapsed the replaced constant onto one line and moved the block up
three rows, so the citation follows it.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c164"></a>

### 164. A redaction map whose scope was two locations somebody remembered, and a rewriter that would have forged the one file its own test exempts

*2026-08-13*

`````text
The map was already derived; its SCOPE was not. `docs/defect-log.md` and
`git ls-files evidence` were where the scrub had reached, named in a docstring
that said so honestly -- "the rest of the tree is still not scrubbed" -- and the
seven files outside that pair were outside it because nobody had widened the
sentence, not because anything decided they were. A set-of-things-to-check
written on the host that has already been searched is the same defect as a
set-of-things-to-search-for written there: complete where it was composed and
nowhere else. `tracked_files` is now `git ls-files`, `tree_offences` is the one
scan `--check` runs and the gate asserts on, and a file added tomorrow is in
scope tomorrow.

THE DEFECT THE TITLE NAMES. `keeps_its_paths` decides which files may keep an
absolute path, and until now that predicate lived only in the test that read
`evidence/`. The writer had never heard of it. `portable_paths.py --rewrite
evidence/model-attempt-ledger.ndjson` was one command away at every commit since
the exemption was written, and it would have substituted 2,365 placeholders into
77 sealed records -- not redacting the ledger but forging it, and refusing the
next live campaign, because `phase0_lib` re-verifies every `reservation_sha256`
and every `previous_ledger_sha256` before it appends. The seal predicate has
moved into the map, `--rewrite` refuses a sealed file even when the file is named
on the command line, and the exemption is one derivation that the rewriter and
the checker share rather than two that can come apart.

A file that cannot be decoded is now scanned rather than skipped. Nothing tracked
here fails to decode today, which is exactly the reason not to assume it: reading
through `surrogateescape` means the first binary fixture somebody commits is
searched, and rewriting one cannot corrupt it.

The canonical encoding the seal is taken over is imported from
`tools/phase0/phase0_lib.py`, not restated -- a second spelling of a canonical
encoding is a second encoding, and the copy is the one that drifts. It is
imported inside the function so that an emitter rendering a receipt in flight
does not pull in the campaign library to do it.

Nine tests, each proved able to fail by mutating the map, running the test that
should catch it, restoring, and verifying the restore by sha256: the file set
taken from the disk instead of from git; undecodable bytes dropped instead of
carried; the scan ignoring the seal; the exemption bought by writing the field
name; the seal not required to reach the last record; the map made a no-op;
the map ordered shortest-needle-first; a substitution that keeps what it
substituted, so a second pass is not the first; `--tracked` accepting a
hand-given file set as well.

The tenth mutation is worth recording because it did NOT fire where it was
aimed. Ordering the map shortest-first was expected to break the idempotence
test and did not: `<USER>` -> `<USER>` first leaves `/Users/<USER>/x`, which is
mangled but stable, so re-running it is still a no-op. Idempotence and ordering
are two properties, the ordering one was already held by
`test_the_map_substitutes_longer_identifiers_before_shorter_ones`, and the
mutation was re-aimed at the claim each test actually makes rather than the test
being widened to cover a mutation it was never about.

MEASURED, this commit: `python3 tools/evidence_common/portable_paths.py --check
--tracked` reports 26 unsealed occurrences across 7 files and leaves the ledger
alone, naming it. `ruff check --no-cache`, `ruff format --check`, `cargo fmt
--all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and
`scripts/gate-a-residue.sh` (candidate_executables=8) all exit 0;
`tools/evidence_common/tests` is 74 tests, up from 65. No Rust source changed.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c165"></a>

### 165. A worked refusal example naming the version the product supports, a keychain digest whose only evidence was the literal it was taken over, and a review that republished the display name it recommended removing

*2026-08-13*

`````text
The map from the previous commit, applied. `python3 tools/evidence_common/
portable_paths.py --rewrite --tracked` substituted 26 occurrences across six
files and refused the seventh by name; the tree-wide check that was red before it
is green after it and is now part of `tools/gate-a/tests/test_redaction.py`,
whose scope was `docs/defect-log.md` plus `git ls-files evidence` and is now
`git ls-files`. Running the rewrite a second time changes nothing: the sha256 of
the sha256s of every tracked file is identical either side of the second pass.

THREE THINGS THE MAP MUST NOT DO, and each is the reason a hand-fix precedes it
rather than a hand-exemption sitting inside it.

`README.md`'s worked `UnsupportedClaudeVersion` refusal named 2.1.227 -- the
version the same file says ships supported two paragraphs earlier, the version
the promoted range ends at, and the version installed on this host. The first
thing a newcomer read was the product refusing what it supports. It names 2.1.228
now, which is genuinely past the tested ceiling, and the message is still the one
`crates/service/src/compatibility.rs:747` formats.

`docs/2.1.226-compatibility.md` §4.1 carried `sha256("<a literal home path>")
[0:8] = <DIGEST>`, where the input IS the evidence: rendered to a placeholder the
arithmetic stops closing, and deleted it becomes an assertion. The claim is the
MECHANISM -- the keychain service name is namespaced by `sha256(config_dir)[0:8]`
-- so the input is now stated as `$HOME/.claude-1` with the command that
reproduces it, `printf %s "$HOME/.claude-1" | shasum -a 256 | cut -c1-8`. That
prints `<DIGEST>` here, and it prints something else for the reader, whose own
keychain entry and daemon socket then carry that instead. The claim got stronger:
it is now checkable by somebody who is not the author.

`docs/pre-push-review.md` recommended removing a display name from two fixtures
and quoted the name six times while doing it, which is how a review outlives the
fix it asks for. The fixture pair reads `pmuxdev` -- same seven columns, so the
box drawing still aligns and the corpus `visible_text` still equals the fixture
byte for byte -- and the review states the finding with the name elided and says
that it is elided. MEASURED, as the brief asked me to check rather than believe:
`screen_corpus_replay` 11 passed, `actor_model` 9 passed.

TWO COMMENTS PROMISED MORE THAN THE BYTES BELOW THEM once the map had run. Both
said the captured `stop_hook_summary` and `turn_duration` rows were "copied byte
for byte" out of the session that failed live ordinal 49. After a substitution
that stopped being true, and a comment that survives the change it describes is
this repository's own bug class aimed at itself. Both now say exactly which two
tokens differ and why the rest is carried. Claude's project directory spells a
path with `-` for `/`, which is why the login name appears there without the home
directory around it, and `crates/claude/tests/transcript_engine.rs` says so.

THE CITATION GUARD WAS RED AT HEAD AND IS GREEN. `nothing_cites_a_path_b_document
_by_line_number` reported eight offences, every one in `docs/pre-push-review.md`,
in the commit whose own message read "MEASURED at HEAD: cargo test --workspace
exit 0". Five of the eight cited a line I was about to move -- the README refusal
example and §4.1's digest are two of the three fixes above -- so leaving them
would have rotted them further rather than left them alone. Each is now a section
citation, which survives insertion; none of the underlying findings was decided
in the process, including the `smithers` phrasing, which is a shipped product
module and stays. `docs/path-b.md` §2.2 is where line 327 was.

THE SEALED LEDGER IS STILL UNTOUCHED, and the rewriter now refuses it rather than
merely not being pointed at it. `evidence/model-attempt-ledger.ndjson` holds 113
of the reported occurrence lines and every one is inside a digest. Publishing that
file publishes the operator's home directory and campaign-tree names; that is a
decision the owner has to make, and it cannot be made mechanically, because the
mechanical answer forges the record.

Every check the brief named, before and after. `cargo test --workspace
--no-fail-fast`: 73 targets, **1,226 passed, 0 failed**, 51 ignored -- against a
baseline of one failed target at HEAD. `phase0.py budget`: `consumed 85 / ceiling
100 / remaining 15 / records 77 / first_ordinal 5 / last_ordinal 81 / detached 4
/ predating_the_file 4`, and `real_turns_outside_the_ledger` 45/5/5/24/44/0 for a
total of 123 -- every field identical to before. Python: phase0 261, gate-a 79
(up from 76), evidence_common 74, gate-a-candidate 20, scripts 48, package-smoke
36, clients 40, all green; `tools/linux-docker/tests` carries its one documented
deliberate red, unchanged. `cargo fmt --all --check`, `cargo clippy --workspace
--all-targets -- -D warnings`, `ruff check --no-cache`, `ruff format --check`,
`shellcheck`, `bash -n` and `scripts/gate-a-residue.sh` (candidate_executables=8)
all exit 0.

Five mutations against the tree-wide gate, each proved to make it red and each
restore verified byte-exact by sha256: a home path planted back into a document,
into a Rust source file and into a JSON fixture; a placeholder substituted into
a placeholder; and the derivation narrowed to nothing, which is what would make
the whole check pass over an unscrubbed tree.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

---

## B. Composer, screen and prompt delivery

**Everything that follows from typing into a real Claude Code TUI: mode prefixes, normalisation, geometry gates, render proofs, modal screens.**

pmux drives a terminal it does not own, so every input and completion gate is a claim about geometry and text handling that Claude can change without notice. This group holds the only defects in the log that let a caller's bytes do something other than become a prompt -- a leading `!` that ran a shell command on the host, a BOM-prefixed `/clear` -- and the long tail of ordinary caller inputs that each destroyed a pooled instance because the composer rewrote them.

13 entries.

<a id="c9"></a>

### 9. Drop the POSIX terminator from --prompt-file; every file prompt failed

*2026-07-28*

`````text
read_prompt normalized CRLF but kept the trailing newline every conventional
tool writes, so the turn was armed with "...ok\n" while Claude's composer --
which cannot hold a trailing newline -- recorded "...ok". engine.rs:126
compares the two for equality, so --prompt-file could not complete for any
caller who did nothing wrong. Observed live against 2.1.220 as
UnexpectedTypedPrompt; the next run with this fix returned outcome=completed.

Fixed at the file->prompt boundary rather than in normalize_prompt, whose
docstring promises whitespace is otherwise never trimmed -- weakening that
would make a documented sentence false. Exactly one newline is dropped, the
same rule cursor.rs:188 applies to a trailing CR, so a deliberate trailing
blank line still survives.
`````

<a id="c35"></a>

### 35. Path B: follow the session across /clear, and close the guard that let a caller ride along

*2026-07-31*

`````text
pmux can now drive a stateless cell: the Claude Code TUI launched with no tool
surface, cleared between turns instead of relaunched. /clear costs ~30ms and a
relaunch costs ~4.4s, which is the whole reason this exists. Measured on 2.1.220:
/clear rotates the session id and opens a NEW transcript, turn_duration and the
--system-prompt replacement both survive it, and context is genuinely gone.

THE SESSION ID NOW TRAVELS PER CALL. `TranscriptSource::arm_at_eof`/`poll` always
took a session id; `FileTranscriptSource` discarded both (`_session_id`) and read
through a locator bound at construction. The seam existed and was never wired.
Now the armed identity lives INSIDE TailState beside the cursor, under the same
lock, so the id, the path and the offset cannot disagree. `TranscriptLocator` is
bound to a cwd rather than a session id -- the cwd selects the project directory
and /clear opens the new file in that same directory -- so `locate_for(id)` is
per call.

A poll under an id the tail is not armed on REFUSES rather than following the
rotation. Both silent continuations are unsound: reading the old file at the old
offset tails a transcript that will never grow again, and re-locating to the new
file at offset 0 hands the turn a history that could acknowledge and finish it
before the work is done. Only `arm_at_eof` establishes an authority boundary, so
a rotated id has to go back through one.

/clear ABANDONS the old transcript rather than truncating it -- inode unchanged,
length unchanged -- so every existing fence stays green while pmux tails a dead
file. That failed CLOSED already (Terminal is unreachable without a prompt
acknowledgement) but surfaced as a bare TurnTimeout with no thread to pull.
Rotation now names itself, and the rebind refuses on zero or >=2 candidates
rather than guessing: no mtime ordering, no newest-file heuristic.

A CALLER COULD STILL HAVE TYPED /clear. `validate_prompt` refused a leading '/'
after `trim_start()`, but Rust does not treat U+FEFF as whitespace and it is a
format char, not a control char, so it cleared every check -- while JavaScript's
trim() DOES strip it, and the composer is a Node TUI. "\u{feff}/clear" would have
executed. Both guards now strip whitespace plus every Cf format char. The test
corpus listed that exact string as safe, so it was a test defending the bug; it
moved to the refused set rather than being loosened. Verified with node that the
same is NOT true of U+200B -- one measured bypass, not a family.

The control channel is a TYPE, not a laxer filter: `ControlCommand::Clear` never
carries caller bytes, so `validate_prompt` is byte-for-byte unchanged and there
is no shared parsing surface to get wrong.

The ten per-turn checks land as a pure predicate over already-published data,
plus an eleventh: `final.stop_reason` is reached through TurnStatus::Terminal, so
a non-terminal status has no defined answer and TurnNotTerminal fails it closed.
The verdict collects every refusal in check order rather than short-circuiting,
so a leaked tool surface cannot hide behind an earlier failure. Not yet reachable
from the shipping binary -- wiring it needs the cell in the compatibility
surface, which is gated on calibration anyway.

`--no-session-persistence` is now forbidden. It is inert in the TUI today, which
is exactly why: no caller depends on it, and a release that honoured it would
stop writing the transcript pmux completes turns from.

The phase0 banner cited driver_io.rs line numbers that this diff moved by ~2000
lines, and the citation-freshness test caught it. The numbers lived in the banner
AND restated in the test, so code motion had to be chased through both -- and a
test that gets chased is a test that gets loosened. The test now reads the
citations out of the banner. Re-pointing the re-arm citation at the other,
byte-identical `last_change = Instant::now()` still fails it.

cargo test --workspace 662 passed / 0 failed; fmt clean; clippy at baseline
(4 pre-existing, vendored); phase0 235 OK. No ordinal spent: every measurement
behind this came from driving `claude` directly, at a cost of cents.
`````

<a id="c39"></a>

### 39. The composer was judged by where it sat, and eight guards read the spelling

*2026-08-04*

`````text
Path B's core loop now closes against real Claude: start -> turn -> /clear ->
turn, with input_tokens identical either side of the clear.

The composer gate refused a provably empty composer because it measured from
the physical bottom of the grid. Claude's Ink frame does not always paint to
the bottom -- after /clear it is four rows tall and top-anchored -- so
24 - 5 - 1 = 18 > 4 and the editor was never found. Measured from the last
rendered row instead: 2 rendered rows below the composer in 85/85 live 2.1.220
screens and 5/5 recovered 2.1.70 fixtures. The one test that changed padded
with blank rows, which is the post-clear shape, so it had become a control
against the fix. This was never /clear-specific: a short first turn produces
the same geometry, and the same defect was failing plain second turns on
Path A. Gate 2 learned the dual growth law for a top-anchored box.

Eight leaks in one family, each reproduced over the socket before being fixed:
a retry window nothing invalidated; the same window re-keyed on a counter that
writable-attach paths mutate without touching; admission gated on the
applicant's request shape rather than the resolved resource; macOS firmlink
aliases, which canonicalize() does not collapse; ".." through a missing
component, which stat reports absent and mkdir -p then creates; identity asked
where the error message promised containment; and a containment walk that
walked the spelling, so a terminal symlink was invisible to it.

Resource identity is now (device, inode); admission asks one containment
predicate over every directory a start binds against every directory every
live minified cell binds, in both directions, walking the path the child will
actually reach. Unresolvable identity is refused rather than guessed.

/clear no longer presses Enter hopefully. The menu's selection is rendered in
colour alone, and terminal_snapshot discarded the cell grid -- the highlight
was absent from pmux's data, not merely hard to read. The read is widened;
TerminalSnapshot is untouched because its equality is the input gate's fence.
At prefix /c the selected entry is /cd, "Move this session to a new working
directory".

Measured, not inferred: history.jsonl never reaches model context (40k tokens
seeded, input_tokens unchanged at 186); --disallowedTools '*' removes tools,
subagents and bundled skills, ~29,000 tokens; no MCP server process is spawned
in any configuration, so docs/path-b.md's claim to the contrary is retracted.

Known open, reproduced, not fixed here: HOME is examined only when it is the
source of the config root, so setting both CLAUDE_CONFIG_DIR and HOME hides it
from admission. The fix is a policy decision with Path A blast radius, not a
mechanism swap.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c84"></a>

### 84. A facade whose piped prompt kept the terminator no composer can hold, and the one rule two binaries each owned a copy of

*2026-08-08*

`````text
`echo q | claude-p` is the invocation the compatibility facade exists for, and
every producer of piped text ends it with the POSIX terminator. The facade's
whole normalization was `prompt.replace("\r\n", "\n").replace('\r', "\n")`, so
that terminator reached the daemon and armed the turn with it. Claude records a
typed prompt without a trailing newline -- a composer cannot hold one -- so
`engine.rs:126` compared expected "ok\n" against actual "ok" and every such turn
died in `UnexpectedTypedPrompt`.

Reproduced in both halves before changing anything. A scratch probe against the
real `TranscriptEngine` armed "Reply with exactly: ok\n" against a recorded
"Reply with exactly: ok" and got
`UnexpectedTypedPrompt { expected: "Reply with exactly: ok\n", actual: "Reply
with exactly: ok" }`, while the same pair without the terminator acknowledged.
The new blackbox test showed the facade transmitting "Reply with exactly: ok\n"
verbatim to a canned daemon. NOT confirmed against a live turn: this host runs
Claude Code 2.1.223 against a sole promoted 2.1.220, which is refused before any
model call.

`bin/pmux/src/cli.rs` had already measured this death and dropped exactly one
terminator. That the two binaries each owned a copy of the rule is why only one
of them carried the fix, so the copy is gone: `normalize_cli_prompt` in
`crates/client` -- the crate both binaries already link -- folds line endings by
calling `pseudomux_claude::normalize_prompt`, the same function the daemon
applies to the recorded prompt it compares against, then drops exactly one
trailing newline. One rule, one authority, two callers.

`facade_blackbox.rs` pinned the failing shape against a canned server that
cannot observe the death it enshrined; that assertion is inverted, and a new
case table asserts the rule rather than the mere absence of a newline: a
deliberate trailing blank line survives, and a CRLF file folds before its
terminator is found. Mutating the fix to `trim_end_matches` turns both that test
and `pmux`'s `prompt_drops_exactly_one_trailing_newline` red on the "poem\n\n"
case, so both callers hold the rule.

claude-p 6 + 21 pass, pmux 34/6/5/8/3/25 pass, pseudomux-client 12/20/20/6 pass.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c103"></a>

### 103. A prompt beginning `!` that switched the composer into bash mode and ran the rest as a shell command on the host, and the two other prompt shapes pmux admitted and could not deliver

*2026-08-09*

`````text
Path B's contract is that a caller can do exactly one thing: think, then get
text. `pmux ask '!echo … > /tmp/file'` created the file. MEASURED at Claude
Code 2.1.226 through the shipped release binaries, six times out of six on a
warm pooled instance -- three of them concurrently at the 15-instance cap --
with the child's own transcript row reading

    user  <bash-input>echo PMUX_BASH_MODE_ESCAPE > /tmp/pmux-bash-escape.txt</bash-input>
    user  <bash-stdout>(Bash completed with no output)</bash-stdout>

It is not a tool call, so `--disallowedTools "*"` cannot see it; not a
permission decision, so `--permission-mode dontAsk` cannot; no sidechain, so
`pool/refusal.rs:436` cannot; and it leaves a clean `turn_duration`, so the
fast-path checks cannot. The turn then ran to the caller's deadline -- 600 000
ms under daemon policy -- because the acknowledgement compares the recorded
prompt to the typed one and the recorded prompt was a `<bash-input>` row.

Two causes, both this house's bug class.

The guard named one member of a set of two: `validate_prompt` refused a leading
`/` and `bin/pmux/src/cli.rs`, `bin/pmux-mcp/src/tools.rs` and a 22-entry test
list each said the same thing about the same one character. And
`rendered_prompt_is_proven` (`driver_io.rs:835`) promises the prompt rendered
and tests that SOMETHING rendered into the same composer -- no comparison of
the composer's text to the prompt exists, which is why Enter was pressed on a
buffer pmux had not read. The second is reported, not fixed: a 1 MiB prompt
does not fit on a 24-row pane, which is why that predicate is geometric.

The set is now measured, not guessed. Every ASCII punctuation character was
sent as a prompt's first character on a warm instance and the recorded user row
compared to the bytes sent, 31 turns: `/` and `!` are modes, and
`# $ % > ? \ | ~ ^ & * - + . , : ; = ` " ' ( [ { <` are text. `#`, `@` and `$`
were the plausible extra candidates and all three are ordinary. That table is
the one literal in `crates/claude/src/composer.rs`, as
`MEASURED_FIRST_CHARACTER_SWEEP`, and `COMPOSER_MODE_PREFIXES` is checked
against it rather than iterated over -- the difference between a test that
catches a character being dropped from the guard and one that shrinks with it.
Both refusal lists and the MCP tool description are derived from the constant;
the two hand-typed 24-range copies of the invisible-prefix table are gone.

The same invariant was violated twice more, and both cost a pooled instance
each. `normalize_prompt` is applied to what pmux types AND to what Claude
records, so a prompt the composer rewrites arms a turn that can never be
acknowledged. A tab: MEASURED recorded as four spaces, and `validate_prompt`
explicitly exempted `\t` from the one scan that would have caught it -- 15 tab
prompts fired at a full pool left zero survivors from the pre-wave pid set in
3.2 s. Non-NFC text: MEASURED recorded composed, so `e` + U+0301 came back
U+00E9 and any accented character copied off macOS emptied a slot. The tab is
refused, because four spaces is not canonically equivalent to a tab and pmux
must not invent three characters the caller did not write; the NFC case is
normalized, because NFC(x) and x are the same string by Unicode's own
definition, which is the rule `TranscriptLocator` already applies to the cwd
for the same measured reason about the same program.

Verified live on the rebuilt binaries: the escape refuses in 19 ms at both the
CLI and the raw socket and writes no file; the tab refuses in 20 ms; and the
three unicode prompts that each used to destroy an instance now answer on one
pid that never changed. 69 test binaries, 0 failed. Every new test proved red
first by emptying the constant it rests on.

`docs/path-b-adversarial.md` carries all of it, including what held: zero
`tool_use` blocks, zero `tool_result` blocks and zero sidechain rows over 100
transcripts, statelessness across `/clear` for facts, instructions and
personas alike on one reused process, and 15 concurrent adversarial asks at the
cap with a census that never lied. It also names two defects left open -- a
model refusal reported to the operator as `SchemaDrift` and costing an
instance, and the render proof above.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c107"></a>

### 107. A render proof named for the prompt whose predicate was five clauses of cursor geometry, and the one row of a 24-row pane a 1 MiB prompt can still be checked against

*2026-08-09*

`````text
`rendered_prompt_is_proven` compared `cursor_row_from_anchor`,
`cursor_col_from_prompt`, a revision and two row invariants, and never once
compared the composer's text to the prompt. A composer holding
`! echo PWNED > /tmp/…` satisfies every one of those clauses under the prompt
`What is 2 plus 2?`; the reproduction asserted `(1, 1)` and passed, which is
Enter pressed on a buffer pmux had not read. `docs/path-b-adversarial.md`
sec. 4.3(b) filed it as reported and not fixed, on the argument that a 1 MiB
prompt does not fit on a 24-row pane. The premise is true and the conclusion
does not follow: a prompt that does not fit still has a head that does.

`rendered_prompt_head_is_proven` keeps the geometry as necessary and adds the
head as the clause it is named for. `composer_head` takes the composer's first
rendered row by `prompt_glyph_col`'s own rule -- indent, glyph, at most one
whitespace cell -- and `pseudomux_claude::composer_head_proof` requires it to be
this prompt's opening characters byte for byte, or the placeholder the composer
substitutes for a collapsed paste, carrying this prompt's own line-break count.
The name, the doc comment and `input_render_failure`'s message all say head and
nothing wider: at most `cols - 2` characters, nothing below the first row, and
the full equality left where it can be complete, post-Enter in
`UnexpectedTypedPrompt`.

The placeholder is measured, not guessed. Eleven real turns at 2.1.226 read out
of the input gate's own corpus recorder say `+n` is the line-break count exactly
(41 lines showed `+40`), that the ` +n lines` clause is ABSENT when a paste has
no line breaks, and that `#k` is a per-process counter over collapsed pastes
that survives `/clear` -- so `n` is derived from the prompt and `k` is admitted
at any value. `[Pasted text #1 +12000 lines]`, the render a test invented for a
megabyte of `x` with no newline in it, is wrong twice over and is now the
negative case beside `[Pasted text #1]`.

Five fixtures were free to render text their prompts do not contain, because
nothing compared them: a wrapped composer reading `first wrapped row` under
`long prompt segment …`, a `❯ typed` served to nine prompts none of which
contain the word, and a `safe` / `safe!` pair whose instability test would have
gone red for a reason its name does not mention. `ready_control_showing` derives
its row from the prompt instead.

Six mutants, each restored: dropping the head clause, `composer_head` skipping
the separator step, the collapsed arm ignoring its line count, a blank row
proving anything, the two placeholder forms merged into one, and the prefix
relation allowed to run either way. Every one is caught.

Verified live at 2.1.226 after the change: short, wrapping, three-line,
41-line collapsed, 3021-character collapsed and CJK-plus-emoji prompts all
submit and answer, with the corpus showing the gate accepting
`[Pasted text #1 +40 lines]` and `[Pasted text #2]` through the collapsed arm.
Ledger untouched at consumed 85 / remaining 15, digest 439e4853 before and
after.

Not fixed, and now written down in sec. 8: `active_editor` can be anchored on
the CALLER'S text. A two-line prompt whose second line begins with `❯` renders
as `  ❯ …`, `prompt_glyph_col` accepts the two-space indent as leading
whitespace, and the gate correlates to the caller's own row -- measured live as
a `PromptNotAcknowledged` at 17.5 s that cost the pooled instance.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c108"></a>

### 108. A composer that trims the end of every buffer it submits, a backslash that makes Enter insert a newline instead, and the four caller inputs each of those cost a pooled instance

*2026-08-09*

`````text
`docs/path-b-adversarial.md` had only ever been run while it was finding bugs. Re-run clean at
`<c107>` with every guard required to fire: the `!` bash escape refused in 28 ms at the CLI and 0 ms
at the raw socket with no file written, 26 of 26 mode-prefix shapes refused across 13 invisible
prefixes, five tab positions refused, three non-NFC prompts answered on one pid, statelessness proven
from nine transcripts with each needle in exactly one, 0 tool_use / 0 tool_result / 0 sidechain rows
over 48 transcripts, and 15 concurrent adversarial asks returning with 15 of 15 survivors.

Then past it, to the end of the buffer, which nothing had asked about. pmux writes one bracketed
paste and one Enter, and every guard in this tree was about the paste. Enter was MEASURED doing three
things that are not "submit this buffer": it submits the buffer trailing-trimmed, it does nothing at
all when the trim leaves nothing, and it deletes a trailing `\` and inserts a newline. Four ordinary
caller inputs -- a Windows path at the end of a sentence, a prompt of spaces, `printf` with no
newline, a text file -- each destroyed a pooled instance, two of them after holding it for the full
600 000 ms turn timeout.

The trim is the composer's own rule and belongs where the other two live: `normalize_prompt` already
says it is "the exact form Claude records one in" and was incomplete against that sentence. Its set
is White_Space plus U+FEFF -- JS `trimEnd`'s, measured in both directions, since U+FEFF is removed
and U+200B is kept -- so it cannot be `is_ignorable_prompt_prefix`, whose superset would eat a
character Claude keeps. An all-whitespace prompt then needs no rule of its own: it reaches the
empty-prompt refusal as the empty string. The trailing `\` is refused instead, because no rewrite
delivers it, and doubling it does not escape it.

`crates/client/src/prompt.rs` had half the rule and promised the other half was unnecessary --
"a caller who deliberately ends a prompt with a blank line still gets one". They did not: it was
armed as one newline, recorded as none, and cost the instance.

Also measured, and it corrects §4.4's reasoning rather than its conclusion: a mode prefix fires
through a paste and the `@` file picker does not. The input gate's own recorded frame shows a pasted
`!` replacing the `❯` glyph under `! for shell mode`, collapsed placeholder and all, while `@Nonce`
against a live cell whose cwd held a matching file was recorded verbatim and answered. What protects
Path B is not the empty cwd; it is that only a sticky interpretation survives the rest of a paste.
The five ASCII punctuation characters the sweep had never sent were sent -- `@ ) ] } _`, all recorded
verbatim -- so `MEASURED_FIRST_CHARACTER_SWEEP` is 32 of 32 and the unswept table is empty.

`path_b_doc_citations` was grading 39 of its citations and had never seen the other 14, because its
grader anchors on a file extension and an abbreviated citation carries none. Three of §4.5's own
citations had rotted inside that blind spot. The shape is now refused outright; all 30 abbreviations
across five documents are written out in full, which put them under the existing grader, which then
found six more rotted. Graded citations: 39 -> 53.

Twelve mutants applied, run and restored, each caught by a test rather than a type error, including
the one that showed the restated normalization property was a one-directional bound a
do-nothing normalization satisfies. 1156 passed, 0 failed; fmt, clippy, ruff and gate-a-residue
clean. Ledger `consumed 85, remaining 15`, digest `439e4853...f167153`, byte-identical.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c113"></a>

### 113. Three clauses of the render gate could be disabled without reddening one test, and the one that could never have refused anything was the one hiding the other two

*2026-08-10*

`````text
At `<c110>`, disabling `!empty_cursor_position`, `cursor_moved ||
rendered_rows_changed` or `same_editor_geometry` in turn each left
`pseudomux-service --lib` at **415 passed, 0 failed**, while the control —
disabling `head_is_this_prompt` — reddened 2. Re-measured here before touching
anything; all three reproduce. They do not have one answer.

**`!empty_cursor_position` is load-bearing, and the case is measured.** Claude
renders placeholder text into an EMPTY composer: `❯` U+00A0 `Try "refactor
<filepath>"`, cursor at (18, 2), recorded on this host at 2.1.226 at
`input_gate.post_paste` before the paste landed. The row rotates between runs,
so a caller sending one of them as a prompt is doing nothing exotic. Every
other clause holds for it — revision moved, prompt column identical, anchor and
cursor where the fence left them, head equal to the prompt byte for byte — and
without this clause pmux presses Enter on a composer holding nothing, which
submits nothing and then spends the caller's whole deadline proving it.

**`same_editor_geometry` is load-bearing**, and it is the only guard on
`docs/path-b-adversarial.md` sec. 8: a prompt whose second line begins with `❯`
renders as `  ❯ …` and `active_editor` correlates to the CALLER'S row. The head
clause cannot see it, because that row really does hold this prompt's text.
Both halves are now separately reached — the second-editor case is refused by
the row relation as well, so the prompt-column half needed a case of its own
where the caller's indented `❯` lands on the fence's own row.

**`cursor_moved || rendered_rows_changed` is redundant and is deleted.** It is
`editor.signature != baseline.signature` written out over the signature's three
fields, and `empty_cursor_position` is computed from two of those three, so an
editor that is not at its empty position cannot carry the fence's signature —
and the baseline IS the fence, filtered on exactly that by
`prove_stable_empty_editor`. It could never have been the clause that refused.
Both steps are asserted from `active_editor`'s own output rather than argued:
equal signatures give equal `empty_cursor_position` across different absolute
rows, and no populated editor wears an empty one's signature. A fourth
signature field reddens that.

`baseline.empty_cursor_position` replaces it, and is not the same statement: it
CHECKS the invariant the deletion rests on instead of assuming it, so the
predicate is right for any baseline rather than for the two call sites that
happen to pass a fenced one.

Six mutants, each applied and restored, each now caught by the test written for
it: the three above, the two halves of `same_editor_geometry` separately, and
the new baseline clause. `pseudomux-service --lib` 422 passed.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c114"></a>

### 114. The render gate accepted one delivered character of a seventeen-character prompt, and the two tables that recorded the same measured wrap disagreed by four characters because nothing could tell

*2026-08-10*

`````text
`composer_head_proof` asked `prompt.starts_with(head)` over the composer's FIRST
ROW ONLY, and a `starts_with` with no lower bound is not a bound. PROBED at
`<c110>` through the gate itself: a composer showing `W` proved the prompt
`What is 2 plus 2?`, every geometric clause held, Enter went in, and the
post-Enter equality then refused the turn and destroyed the pooled instance.

**The tree recorded that rule's own measurement two ways and they disagreed.**
`composer.rs` had the 274-character prompt's first row as `long_wrapping[..118]`
and `driver_io.rs` had it cut at 114. Both tests passed, because a rule that
accepts any prefix accepts both. RE-MEASURED on 2026-08-10 by driving the
shipped `pmuxd` with `PMUX_SCREEN_CORPUS_DIR` set and reading the frame: it is
**114**, broken at the word boundary before `that`.

Eighteen more real renders were taken to settle the wrap, and they refute both
obvious repairs. The content region is **116 columns on a 120-column pane**, not
the `cols - 2` this tree claimed — established three ways, by a 200-`x` word
breaking at exactly 116, by a CJK line breaking at 58 double-width characters,
and by a wrapping prompt's second row filled to exactly 116. And a 600-character
prompt renders six rows whose THIRD ends 8 columns short of that width with a
7-character word next, so the composer is not a greedy word-wrapper at a
constant width and every "the row must be full" rule refuses a render Claude
actually produced.

So the rule asks a different question, and needs no width model at all:
`composer_render_proof` takes EVERY rendered row and requires them, in order, to
spell the prompt from its first character to its last. The rows are the whole
buffer — `active_editor` takes them from the `❯` anchor through the cursor, and
the cursor sits at the buffer's last character on all twelve renders — so this
is answerable rather than approximable. A row may be missing only what a
terminal cannot draw: whitespace the right-trim took, the character a break ate,
and the invisible ones, which is why a trailing U+200B still passes and one
missing full stop does not.

That closes two more things on the way. A blank first row used to prove any
prompt whose first LINE was blank, however many lines followed it. And
`composer_head_proof`'s stated reason for choosing a prefix — "`\"   \"` is a
prompt `validate_prompt` accepts" — was falsified by `<c108>` and is gone with
the prefix.

Six mutants, applied and restored: the old any-prefix rule; a break that may eat
anything; a tail not required to be omittable; a placeholder with rows under it;
a gutter trimmed greedily instead of exactly; and only the first row reaching the
proof. All six redden, each on the test written for it — the gutter one needed a
case of its own, because a row indented MORE than the gutter is the only place
greedy trimming and exact removal differ.

The synthetic screens that stood in for composers were rebuilt from the renders:
a multi-line composer carries its measured two-cell gutter, a 21-row wrapped
composer is built from the prompt it is supposed to be holding rather than
filled with plausible text, and `ready_control_showing` renders every line of a
prompt instead of its first.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c115"></a>

### 115. The trade that justified trimming a character JS keeps was refuted by the guard three lines below the one that would have paid for it

*2026-08-10*

`````text
`is_trimmed_from_the_end` strips U+0085, which `String.prototype.trimEnd` does
not, and the comment defending that superset read: *"Keeping a character Claude
trims costs the POOL: pmux would paste `"ok\u{85}"`, Claude would record `"ok"`,
and `UnexpectedTypedPrompt` destroys the instance."* It also said the narrowing
"needs a real turn behind it".

The turn was spent. U+0085 was removed from the set, the release binaries were
rebuilt, and a prompt ending in one was sent: **pmux never pastes it.** U+0085
is a C1 control character and `validate_prompt`'s next guard refuses every
control character but `\n`, so the prompt came back `invalid_config` — *"prompt
contains an unsafe control character"* — in 0 ms with the instance untouched.
The availability cost the superset was chosen to avoid is unreachable.

So the real trade is *silently alter the caller's prompt* against *refuse it
with a message*, and it makes U+0085 the one character whose treatment depends
on where it stands: interior it is refused, trailing it is deleted without a
word. The predicate is STILL unchanged, now for a stated reason rather than a
wrong one — whether Claude keeps a trailing NEL remains unmeasured, because
reaching the composer with one needs the control-character guard relaxed as
well, and which behaviour should ship is a design call. Both halves of the
asymmetry are pinned by a test that derives the overlap between the trim set and
`char::is_control` over the whole of Unicode rather than spot-checking it.

The verdict document records the four closures, the 30 live turns behind them,
and a §6 of what they do not close — starting with the two that matter: **Gate A
has not been re-run at these commits**, and no mutation run was made, which is
now worth more than it was because `driver_io.rs` and `composer.rs` both changed
substantially and the gate's mutation cell is configured not to mutate the
first.

And the live verification of the stricter render proof, because a gate that
compares every row can refuse what it should admit: twelve prompts, one per
composer behaviour — short, wrapping, six-row, a 200-character unbroken word,
CJK wrapping, two lines, three lines with an indented middle line, a four-line
collapse, a 3000-character collapse, trailing spaces, a trailing U+200B, and a
line sized to the 116-column boundary. **12 answered, 0 refused.**

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c119"></a>

### 119. The modal classifier whose ten spare phrases no screen in the suite could reach, and the four guard clauses that could not have been the ones that refused

*2026-08-10*

`````text
A full-scope mutation run at <c118> left 136 survivors, 105 of them in the two
files the `gate` scope omits. This kills 35 of them and deletes the code behind
4 more, all in the guards that hold Path B's contract rather than in the
arithmetic that happens to sit beside them.

THE HEADLINE. `blocking_screen` recognizes 24 independent screen shapes and the
table testing it held SIX -- one per kind, the first phrase of each arm. So ten
mutants could each turn an `||` into an `&&` (or an `&&` into an `||`) and the
whole workspace stayed green: under any one of them pmux answers `unknown` to a
real "trust this directory", "not logged in", "please update claude code",
"quota exceeded" or "esc to cancel / press enter" screen, and an `unknown`
screen is not a refusal -- the caller's turn runs on into its deadline with the
instance sitting on a modal. The replacement is a table of every ALTERNATIVE,
positive and negative (each phrase of a conjunction dropped in turn), and
because a hand-written table of a hand-written predicate is the same defect one
level up, a second test reads the classifier's own source and fails if it
matches on a phrase the table does not list.

FOUR CLAUSES DELETED RATHER THAN TESTED, each because no input can reach it:

  * `active_editor`'s `rows == 0 || cols == 0`. A cursor position is unsigned,
    so the two bound comparisons behind it already refuse every cursor in a
    zero-sized grid.
  * `validate_prompt`'s `'\0'` and `'\u{1b}'` comparisons. Both are Cc; the
    clause behind them refused both already -- while the 62 control characters
    only it names had no test at all. They do now, over the whole C0/DEL/C1
    domain rather than over two of its members.
  * `composer_head`'s whitespace test. `prompt_glyph_col` admits a row only
    when the cell after the glyph is whitespace or absent, so the removal was
    restating the admission rather than following it. Both are now one function
    and the head is what that rule's own iterator stepped past.
  * `prove_stable_empty_editor`'s re-derivation of the editor behind
    `fence == baseline`. `active_editor` is a function of the snapshot alone.

The rest are the guards themselves: the control-channel render gate (a frame
the fence already saw, a frame reusing the fence's revision, a screen that
keeps repainting), the Gate-2 fence that Enter is sent on, the composer
geometry that admits exactly two measured shapes, the assert-empty row/byte/
user-row budgets at their boundaries rather than one past them, the `jsonl`
extension AND regular-file pair, the resize guard on both axes, the lifecycle
stop instant at the epoch and at the safe-integer edge, `strictest_cell` read
from either side, and the version token under its decoration.

Every one was proved: mutation applied, the new test watched go red, mutation
restored, green. Two targeted `cargo-mutants` runs over the touched
functions then reported 163 caught / 16 missed / 48 unviable, and the 11 that
remain are named in the report rather than smoothed over -- the completion
evidence fields, `list_transcripts`'s 20,000-entry scan bound, and two
subtractions serde's whitespace tolerance masks.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c130"></a>

### 130. The trailing U+0085 a composer was measured KEEPING, and the three other characters pmux refused inside a prompt and deleted from the end of one

*2026-08-11*

`````text
`verdict-1b-trailing-nel-is-deleted` was the defect register's one OPEN row and
the only reason criterion 1 was NOT MET. It waited on a design call nobody could
make, because the fact under it was unmeasured: `is_trimmed_from_the_end`
deleted a trailing U+0085 while `validate_prompt` refused an interior one, and
whether the composer keeps or removes a trailing NEL had never been sent. The
stated reason it could not be sent was that reaching a composer with one needs
two guards relaxed at once.

It needed neither. The question is about the composer, and a composer can be
driven without pmux -- nine turns against an isolated Claude Code **2.1.227** in
a 120x24 pane, with the paste framing `bracketed_paste_payload` builds and an
Enter after it, reading the child's own recorded `user` rows:

    …and nothing else.\u{85}    RECORDED WITH IT -- 65 6c 73 65 2e c2 85
    …and nothing else.\u{b}     recorded as ^K
    …and nothing else.\u{c}     recorded as ^L
    …and nothing else.\u{200b}  kept, as at 2.1.226
    " " / "\n" / U+FEFF / U+3000  removed, as at 2.1.226

**The composer keeps a trailing U+0085.** Trimming it was never matching the
composer; it was pmux answering a prompt the caller did not send. An interior
one is recorded verbatim too, so the byte reaching the composer is not in
question -- the first turn of the session sent one on purpose, for exactly that
reason.

**And it was never one character.** U+0009, U+000B and U+000C were refused
inside a prompt and deleted from the end of one in the same way. The count in
the register row's own title was inferred from the character somebody was
looking at, and the reproduction printed all four the first time it ran, against
the tree as it stood.

The rule is one statement now where there were two sets:

    pub fn is_refused_wherever_it_stands(character: char) -> bool {
        character.is_control() && character != '\n'
    }

    pub fn is_trimmed_from_the_end(character: char) -> bool {
        (character.is_whitespace() || character == '\u{feff}')
            && !is_refused_wherever_it_stands(character)
    }

pmux removes what the composer removes, less anything pmux refuses to paste, so
the trimmed set and the refused set cannot disagree again. **No daemon file
changed**: the refusal a caller now meets for a trailing U+0085 is the
control-character refusal that always applied to an interior one, reached
because nothing deletes the character in front of it any more. U+000B and
U+000C join `COMPOSER_REWRITTEN_CHARACTERS`, so what a caller is told names what
the composer does with them.

U+0085 is refused rather than delivered, and that is a decision. The composer
would record it; pmux will not paste a control character into a pseudoterminal
on the strength of one measurement, and the guard that refuses it is unchanged.
What is no longer available is the third option pmux was taking.

Seven mutants, applied and restored, each caught by a test rather than a type
error -- the first is the defect restored, and it reddens 5. One of the seven
was rejected in its first form: deleting the U+0085 row from the sweep changes
the table's length, and a mutant caught by the type checker proves nothing about
a test.

`cargo test --workspace --no-fail-fast`: **1204 passed, 0 failed**, 71 binaries.
`cargo fmt --all --check` and `cargo clippy --workspace --all-targets --
-D warnings` clean; the residue audit passes. The citation grader caught four
`composer.rs:NNN` citations this work moved, twice, which is what it is for.

`docs/path-b-adversarial.md` §12 carries the run, the decision and what it did
not establish; §11.1's "one set, two spellings" sentence carries the retraction
it earned. The register row is closed by the commit after this one, which is the
only way to name a commit in the file that has to name it.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c140"></a>

### 140. An `Unknown` that meant proceed on every screen the classifier was never taught, the recovery loop whose frames were classified and then dropped, and twenty-four turns that priced refusing them

*2026-08-11*

`````text
`blocking_screen` recognises 24 screen shapes and answers `Option<NeedsInput>`.
`None` -- no rule matched -- reached every caller as the same value as "an
ordinary non-modal screen", because both became `TerminalScreenState::Unknown`,
and `Unknown` meant PROCEED. A real "trust this directory", "not logged in",
"please update claude code" or "quota exceeded" screen outside those 24 ran the
turn to its 600,000 ms deadline sitting on the modal, and named nothing.

`Unknown` is gone. `Recognised(RecognisedScreen)` is a screen pmux positively
knows; `Unrecognised(ScreenShape)` is one no rule matched, and because the enum
is matched exhaustively everywhere a classifier added tomorrow cannot answer it
by accident. The split crosses the actor boundary too: an arm the actor cannot
see is an arm the actor cannot act on. `ScreenShape` is eight structural facts
and never the text, and `to_json` destructures the whole struct so a field added
later and not published is a compile error rather than a narrower refusal.

`UNRECOGNISED_SCREEN_VETO` refuses a turn held on such a screen for 30,000 ms
with no transcript row arriving. Any row restarts the clock, which is what makes
it a liveness veto and not a second opinion about completion; the screen remains
a veto over the transcript and never the reverse. No new `ErrorCode`, per
`pool/refusal.rs`: both clients hard-reject an unknown code. Not `TurnTimeout`
either -- that is the code the silent hang already reported.

Two tests read this crate's own source rather than a list. The rendering
register derives 22 sites from the three names a rendering can enter by and
makes each say what its unmatched arm does. The read sweep found a real gap:
`interrupt`'s recovery loop classified every frame it took and recorded none, so
the recording a failed recovery most needs was the one nothing kept.

MEASURED, 24 real Sonnet 5 turns at 2.1.227, 4,415 frames replayed through the
production classifier: zero unrecognised frames in 2,629 turn-path observations,
844 ms the longest legitimate run anywhere, and a false-refusal rate of 0/24.
The window is a bound ~35x above that, not a fit, and the veto never fired --
so this prices the refusal and says nothing about the firing path in production.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

---

## E. Completion authority and the transcript

**When a turn is over: the drain, the end-of-turn marker, schema drift, session rotation, and the rows that mean a turn is still running.**

The founding decision of the product is that Claude's own JSONL transcript is the sole authority for whether a turn finished. Almost every defect here is the same shape from a different side: something that looks terminal is not, and committing on it returns a truncated answer -- the one failure mode the architecture exists to make unrepresentable. The recurring discipline is that a mistake must cost unavailability, never wrongness.

10 entries.

<a id="c1"></a>

### 1. pmux v1: Claude-aware protocol-v1 control plane

*2026-07-27*

`````text
Replaces the experimental VTE/screen-scraper design with a native, owner-only
local API that drives the real interactive Claude Code TUI inside a private
rmux 0.9.0 PTY sidecar, using Claude's own project JSONL transcript as the
semantic authority.

The decision that defines the product: the transcript is the sole authority for
assistant content, tools, stop reason, usage, and completion — CompletionAuthority
is a single-variant enum, so "the screen became the semantic authority" is
unrepresentable. The terminal is an independently required liveness gate, one of
nine completion factors. The old design put its heuristics in the answer
position, where a mistake produces a silently wrong result; this one puts them in
the admission position, where a mistake produces a loudly refused prompt.
Measured effect: answer extraction went from 1,140 lines of chrome-stripping
heuristics to ~10 lines of text-block concatenation, and the screen model from
2,043 lines to 382.

Layout:
  bin/       seven binary crates: pmuxd, pmux, pmux-mcp, claude-p, pmux-rmuxd,
             pmux-launcher, pmux-hook
  crates/    protocol, claude, rmux, service, client, e2e (owns the eighth
             release binary, the deterministic pmux-test-claude double)
  clients/   dependency-free TypeScript and Python clients
  docs/      spec.md (normative behavior), testing.md (test ownership, coverage
             matrix, gate commands), current-state.md (state + design debt)
  evidence/  the immutable global real-Claude attempt ledger
  vendor/    rmux-client and rmux-server, patched and hash-frozen

Verified at this tree: 519 tests pass / 0 fail / 17 ignored; the full-stack suite
passes 8/8 against the real daemon, private sidecar, real PTYs and the
deterministic fake Claude; fmt, clippy -D warnings, and rustdoc -D warnings are
clean across the workspace and all four vendored lanes; TypeScript 48/48, Python
32/32; phase0 87/87, package-smoke 35, gate-a-candidate 20/20; ruff clean. No
owned process or temporary residue survives a run.

Not yet claimed: no Gate A receipt exists, fuzz has never run at its mandated
50,000 iterations per target, and no real-Claude or Docker Linux promotion has
been performed. 71 of 100 authorized live attempts remain; the 24 recorded ones
are budget accounting only, bound to source digests that are no longer
reproducible. docs/current-state.md records this and every deferred design item
under rule D9.

.context/ is workspace coordination and is deliberately untracked.
`````

<a id="c7"></a>

### 7. Instrument late-row arrival; the drain is almost entirely margin on 24/24 turns

*2026-07-28*

`````text
Makes the transcript drain calibratable, and finds the answer in evidence that
already existed rather than by spending live attempts.

The drain requires the transcript to sit unchanged for transcript_drain_ms before
a result commits (backend.rs:195). It exists because Claude writes JSONL
incrementally with no end-of-stream marker: a terminal stop_reason might be the
end, or another block, tool call or usage row might still be coming. Commit early
and you return a truncated answer -- a silent wrong result. It is also ~2,354 ms
of a ~3,111 ms turn, so it is the dominant cost in the product.

The corpus could not calibrate it, and the reason is subtle: drain_ms was ALREADY
the raw stable_for_ms, published without an anchor under a name that reads as
"how long pmux waited". So the number was uninterpretable -- an analyst could not
tell whether it showed a wait or a measured need. Adds
TurnTimings.last_transcript_activity_at_ms, an absolute anchor in the same
wall-clock domain as terminal_candidate_at_ms, so the late-arrival gap is one
subtraction between two timestamps the actor stamped itself. A timestamp rather
than a derived duration because the gap can be legitimately negative and a u64
would clamp at zero, destroying the exact distinction between "the candidate row
was the last row" and "one more row landed a millisecond later" -- the only thing
the field exists to measure. The drain gate is untouched; backend.rs has an empty
diff and satisfies() is byte-for-byte identical.

The finding, from the existing 2026-07-19 corpus and independently re-derived:
across 24 of 24 real Claude turns, (completed_at_ms - terminal_candidate_at_ms)
minus drain_ms is 0..1 ms, while the drain waited 2,320..2,479 ms. Nothing was
ever appended after the terminal candidate. On this evidence the 2,000 ms drain
is very nearly pure margin.

The caveat is load-bearing and must not be dropped: every one of those 24 turns
was effort=low, fresh or warm, no tools, trivial prompts -- the shape most likely
to flush in a single write and least likely to expose a late row. This is a
strong result on the easiest case and still absence of evidence on the cases that
matter. It is not yet grounds to cut the drain. tools/phase0/README.md now
records that a calibration campaign must use structured prompts (a tool call
after text, multiple blocks, a long chunked answer) or its number is not
trustworthy, and the campaign summary reports the count of zero-gap attempts
separately so nobody reads absence of evidence as permission.

phase0 can now reach the launch surface pmux gained: --permission-mode with all
seven values including dangerously-skip-permissions, --env, --env-passthrough,
and --agent/--agent-file, all bound into the campaign contract with environment
values recorded by name only.

Second flaky test found and recorded as C9:
pmux-hook's stalled_relay_is_bounded_and_does_not_echo_private_input failed 1 of
3 runs under load. Its assertion is an UPPER bound on kill latency, so it trips
when the host is busy rather than when the product is wrong. Same class as C8,
and the pattern is now the finding: both are process-boundary tests asserting
wall-clock bounds, both fail under load, and two of them means a Gate A run has
two independent chances to go red for reasons unrelated to correctness.
docs/testing.md:110-112 is explicit that a flaky gate command fails the gate.

Workspace 571 -> 573 passed, 0 failed, 17 ignored (--no-fail-fast). fmt clean,
clippy -D warnings zero, ruff clean.

Operational note: /private/tmp is not durable. macOS wiped every validation root
including the frozen release candidates. Only what had been copied into
evidence/gate-a/ survived, which is why receipts are published into the repo
rather than referenced in place. Gate A needs a re-run against a rebuilt
candidate; the previous receipt no longer matches this tree.
`````

<a id="c17"></a>

### 17. Publish when the Stop hook arrived, so the drain question becomes answerable

*2026-07-28*

`````text
The drain waits ~2,000 ms per turn proving the transcript stopped growing --
roughly 50x all other pmux per-turn overhead combined (41 ms p50). The proposed
fast path is (stop_hook_observed || stable_for_ms >= drain), where the hook can
only make completion faster and the drain remains the fallback. It is sound only
if Claude flushes the transcript BEFORE firing Stop; if Stop can precede the
final write, completing on it would TRUNCATE a turn, which is the one failure
mode this architecture exists to make unrepresentable.

The daemon knew only a boolean -- 'a Stop arrived since this turn was armed' --
so the ordering could not be asked, and the drain would have stayed at 2,000 ms
forever: not because it was measured as necessary, but because the question was
unanswerable. TurnTimings::stop_hook_at_ms now records the instant, and the
signed difference against last_transcript_activity_at_ms settles it. Consistently
positive means Claude flushed first and the fast path is sound; a single negative
observation means it would truncate and must never be built. Either way the
question closes.

A timestamp, not a duration, for the same reason as its counterpart: the sign IS
the answer, and an unsigned subtraction would clamp a negative to zero and erase
exactly the observation that would forbid the optimization.

No completion logic is touched. This is measurement only; the fast path stays in
DESIGN-DEBT until the number justifies it.

My implementation brief was wrong on one point and the fix matters. I specified
None when 'the stored value is 0'. But the instant is SESSION-scoped while
lifecycle_hook_observed is TURN-scoped (sequence > baseline, re-armed each
submit_prompt), so on a turn where no Stop has yet arrived that reading would
publish the PREVIOUS turn's stamp against this turn's transcript activity -- a
large spurious NEGATIVE difference that would have falsely condemned the fast
path on the very measurement built to evaluate it. The instant is now reported
only when the same sequence read says a hook arrived for this turn, asserted at
driver_io.rs:2444. The writer stamps before bumping the sequence and the reader
Acquire-loads the sequence before the stamp, so a fresh sequence is never paired
with a stale instant.

Boxing ResponseResult::TurnResult was forced, not chosen: Option<u64> grew the
variant past clippy::large_enum_variant's 200-byte threshold under -D warnings.
Boxed rather than allowed, matching EventPayload::TurnCompleted in the same enum
family. Box is serde-transparent so the wire shape is unchanged; only the Rust
API moved, across 6 call sites. Verified: no 'Box' appears in the golden fixture.

580 passed, 0 failed, 17 ignored (576 baseline + 4 new). fmt and clippy clean.
The full-stack assertions are #[ignore]d behind PMUX_E2E_BIN_DIR and are
unverified locally; that hybrid test is where the measurement gets collected once
Gate A stages the release binaries.
`````

<a id="c27"></a>

### 27. Admit stop_hook_summary only when its payload proves the turn is over

*2026-07-29*

`````text
Ordinal 49 died on SchemaDrift: installing Claude's Stop hook (--lifecycle
hybrid) makes Claude write a main-chain system row with subtype
stop_hook_summary, and validate_strict_active_path rejected any active-chain
system row that was not exactly turn_duration. So hybrid -- a documented public
lifecycle mode that spec.md:878-887 designates as the planned replacement for
liveness factor 6 -- failed EVERY turn. That is a defect reachable by a
non-adversarial caller. D9(a).

pmux refused rather than guessed, which is the architecture working: unavailability,
never a wrong answer.

THE OBVIOUS PATCH WOULD HAVE BEEN UNSOUND, and this is the whole point. The row
is not unconditionally inert -- it carries preventedContinuation, and a blocking
Stop hook makes Claude CONTINUE the turn. Since turn_status treats any system
leaf as terminal-compatible, allowlisting the subtype alone opens a real
truncation race: assistant writes end_turn, a blocking hook fires, Claude writes
stop_hook_summary(preventedContinuation: true), Claude continues -- and in the
window before the continuation's first row lands, plausibly longer than the
~2350ms drain because it includes a fresh model call's first-token latency, the
leaf is a system row, the latest message says end_turn, and the screen shows a
ready prompt. compose_caller_settings merges caller hooks additively, so a
caller's own blocking hook coexisting with pmux's is ordinary, not adversarial.

A reviewer proved that race is real rather than argued: in a scratch copy with
the preventedContinuation check deleted, driving that exact window through the
engine yields Terminal(FinalTurn { outcome: Completed, final_text: "partial
answer" }) -- pmux commits the truncated turn. With the check it is SchemaDrift
at $.preventedContinuation. Two tests fail without it, so the check is
load-bearing.

Acceptance therefore requires payload-PROVEN inertness, mirroring the tier
turn_duration already occupies (parser.rs rejects a semantic payload there "so a
future semantic payload cannot hide behind the allowlisted subtype"):
preventedContinuation present and exactly false -- absence never defaults to
false, because absence means the guarantee cannot be proven; hookErrors and
hookAdditionalContext absent or empty; no message/content/attachment. The
accepted row joins turn_duration's trailing zone, so a semantic row after it is
drift. Reject-by-default stands for every other subtype, ParseMode is still
single-variant, CompletionAuthority is untouched.

Why an allowlist rather than a structural bookkeeping classifier: the property
you can check (payload shape) is not the property you need (behavioural
inertness). stop_hook_summary is its own counterexample -- byte-for-byte it looks
like bookkeeping, and one boolean flips it into "the turn continues".

RECORDED, NOT FIXED: api_error is the next ordinal-killer -- 114 wild instances,
main-chain, mid-turn, so an ordinary rate-limit or 5xx retry fails a pmux turn
today with the same $.subtype drift. It must not inherit the system-leaf terminal
generosity without its own leaf-semantics analysis. compact_boundary (deliberate
chain break via logicalParentUuid), model_refusal_fallback (mid-turn model
substitution), away_summary and local_command are classified in the same table so
the next drift failure is a lookup rather than a research project.

S2 CORRECTED, and it was my error: the drain measurement as specified was
self-contaminating and would have returned a false permanent "no".
last_transcript_activity_at_ms is the last FILE write, but installing the hook
CAUSES writes after it -- the summary row carries the hooks' own durationMs so it
is written after them, then turn_duration follows (.364 assistant, 14ms hook,
.414 summary, .415 turn_duration). stop_hook_at_ms therefore sits before the last
write on essentially every turn, the signed difference reads negative for
bookkeeping reasons, and S2's own "one negative is decisive" rule would have
closed the question on an artifact of the instrument.

Re-anchored to turn_duration instead, on evidence that cost zero ordinals: across
82 turns and four Claude versions, NO model-generated semantic row ever arrives
after turn_duration in-turn; only benign system rows follow; it is present on 96%
of turns. So "Claude appends with no end-of-stream marker" is false for the CLI.
The candidate fast path (turn_duration_seen && at_eof && !has_partial_line) ||
drain.satisfies(...) has strictly better provenance: in-band in the authority
channel, no settings mutation, no relay, no contamination. Limits recorded
honestly: this is file-write order, not observed arrival order; the 4% absence is
safe because the disjunction falls back to the drain, costing latency and never
correctness; turn_duration is CLI-only. The fast path stays S2/NOT-DONE.

Also fixed six citations that landed on blank lines or table separators --
spec.md:1104 was a table rule in three places -- plus C5's spec.md:753-755, and
tracked the test fixture, which was untracked and would have been absent from a
fresh clone.

Thinnest plank, stated plainly: n = 1. Exactly one stop_hook_summary row exists
on this machine and preventedContinuation: true was never observed; the blocking
semantics come from Claude's documented hook contract, not a captured transcript.
The gate REFUSES that case rather than handling it, so being wrong about the field
costs unavailability, not a wrong answer.

Verified: cargo 597/0/17 (592 at HEAD~ plus exactly five new tests, both test
diffs purely additive), fmt clean, phase0 187, and the fixture is byte-identical
to the real transcript rows by cmp.
`````

<a id="c30"></a>

### 30. Never complete a turn mid-retry, and measure the in-band end-of-turn marker

*2026-07-30*

`````text
TWO CHANGES. One closes a live defect a normal caller hits on a dropped wifi
connection; the other adds the measurement that would justify recovering ~2350ms
per turn, without building the thing it would justify.

api_error: A THIRD TIER, THE OPPOSITE OF THE OTHER TWO.
Measured across every transcript on this machine: 115 api_error rows, ALL
main-chain, ALL with children -- mid-chain, never trailing. One incident emits a
LADDER (retryAttempt 1..8 of maxRetries 10; exhaustion NEVER observed), and the
errors are ordinary transport flakiness: 48 ECONNRESET, 15 timeouts, 12
ConnectionRefused, 3 auth, 2 certificate, and 2 literally "Connection interrupted
by system sleep". Zero 529s, zero 400/404s. Claude retries and usually succeeds --
and pmux failed the whole turn with SchemaDrift anyway. Closing a laptop lid
could do it.

turn_duration and stop_hook_summary are gated as proven INERT: the turn is over,
so they open a trailing zone. api_error means the reverse -- a retry is in flight,
the turn is NOT over. So it is admitted onto the active chain (no drift) but is
explicitly NON-TERMINAL:

  is_admitted_on_active_chain = is_proven_inert_marker() || is_retry_in_flight_marker()
  leaf_allows_terminal        = System(s) => !s.is_retry_in_flight_marker()

That second line is the whole fix. turn_status historically treated ANY System
leaf as terminal-compatible, so without it pmux completes a turn MID-RETRY and
returns a truncated answer during a network blip. A reviewer proved it: making
api_error terminal-compatible in a scratch tree makes
an_api_error_leaf_is_never_a_terminal_leaf fail with
Terminal(FinalTurn { outcome: Completed, final_text: "partial answer" }) -- the
exact truncation class. Adding api_error to is_proven_inert_marker instead breaks
three tests with "api_error must be trailing", proving the trailing machinery
must NOT be reused: a semantic row after api_error is the retry SUCCEEDING, and
treating it as drift would turn every recovered blip into a failure while
appearing to fix something.

Retry exhaustion is non-terminal too, unconditionally. It was never observed in
115 rows, so the guess goes on the unavailability side of the asymmetry rather
than the wrongness side -- and the reviewer confirmed that coverage is
independently load-bearing, not shadowed by the general case. The parser refuses
what it cannot prove: retryAttempt and maxRetries required as non-negative
integers, absence is drift, never a permissive default.

ARRIVAL INSTRUMENTATION: measurement only, and it cannot contaminate.
turn_duration_observed_at_ms and post_turn_duration_row_observed_at_ms record
when pmux OBSERVED the in-band end-of-turn marker, and whether anything
analysis-changing was observed after it. This is the last thing standing between
the project and the fast path (turn_duration_seen && at_eof &&
!has_partial_line) || drain.satisfies(...), which would recover ~2350ms of the
~2391ms per-turn overhead.

stop_hook_at_ms was added for exactly this purpose and turned out
SELF-CONTAMINATING -- installing the Stop hook CAUSES post-hook transcript writes,
so its signed difference read negative on 3 of 3 live samples for bookkeeping
reasons, and the decision rule would have permanently killed the optimization on
an artifact of the instrument. This one installs nothing, writes nothing Claude
reads, mutates no settings, changes no poll cadence, and touches neither the
completion gate nor the drain predicate (backend.rs has zero diff lines). Its
only side effect is reading pmux's own clock against reads the worker already
performs, and its residual error direction is exclusively "the drain looks MORE
necessary" -- which can only under-justify the fast path, never justify an unsound
one. THE FAST PATH IS NOT BUILT: no code reads either field.

Also declared both names in KNOWN_TURN_TIMING_FIELDS. This is not bookkeeping:
phase0 DISCOVERS the late-arrival field as "the one name TurnTimings carries
beyond this set", so two undeclared additions would have made that discovery
ambiguous and every published gap could have been computed from the wrong field.
Same fence that caught stop_hook_at_ms, working a second time. Two stale counts
in its own prose ("four", "six" -- now eight) corrected with the reason recorded,
since the count is load-bearing rather than decorative.

Verified: cargo 616/0/17 (was 597; the new-test census sums to exactly +19), fmt
clean, phase0 187 OK, ruff clean, and every guard above proven load-bearing by a
failing test under deliberate mutation in a scratch tree -- including that
dropping sidechain from is_analysis_changing makes the wire publish None where it
must publish Some, catching the exact lie the instrument must never tell.
`````

<a id="c33"></a>

### 33. Graduated drain: a proven end-of-turn marker buys a 250ms floor, not 2000ms

*2026-07-30*

`````text
Per-turn pmux overhead is ~2391ms mean / 2348ms median across 15 retained turns,
against 41ms p50 for everything that is not the drain. So the drain is ~98% of
pmux's own cost, and it exists because Claude appends the transcript with no
end-of-stream marker -- a premise that is FALSE for the CLI. `turn_duration` is
one, and the engine already parsed it, already enforced it is trailing, and
already exposed `turn_duration_seen`.

    required_stable_ms = if turn_duration_seen { 250 } else { transcript_drain_ms }

Across 87 main-chain markers and Claude 2.1.177/207/215/220, the only semantic
rows ever observed after the marker are harness-injected task-notification rows
at gaps of 25ms, 25ms, 284s, 3014s and 18079s. The band (25ms, 284s) is
empirically EMPTY, so nothing observed distinguishes a 2000ms drain from a 250ms
one. Turns without the marker -- about 4% -- still owe the full configured drain,
so absence costs latency and never correctness.

A MORE AGGRESSIVE DESIGN WAS INVESTIGATED AND REJECTED, which is why this one is
conservative. Skipping the stability wait and classifying a single frame was
falsified twice from evidence on disk: those +25ms notifications are followed by
autonomous generation including a real Bash tool_use at +6.2s and, in one case, a
DIFFERENT final answer 12.4s later, so committing at ~20ms converts a
deterministic refusal into a race; and `!frame_is_needs_input` fails OPEN,
because classify_terminal_snapshot returns Unknown for revision-0, cursorless and
populated-editor frames while rmux applies PTY bytes with no ?2026 buffering, so
torn mid-repaint frames are real inputs. That design would have inverted the
project's invariant that a wrong screen constant causes unavailability, never
wrongness.

TWO AMENDMENTS THE ADJUDICATION REQUIRED, both load-bearing:

The 250ms floor is stated IN THE DRAIN as TURN_DURATION_DRAIN_FLOOR_MS rather
than inherited from the screen's `quiet_for`. Today's protection against a late
row is an ACCIDENT: wait_for_snapshot_stability sets stable_since at entry, so
quiet_for is a hard per-call floor that happens to run inside the 2000ms drain
window. That coincidence stops protecting anything the moment the drain no longer
dominates, so the floor must survive any future quiet_for tuning.

prove_turn_duration_inert now rejects `pendingWorkflowCount` present-and-nonzero.
It is a continuation signal -- present on 2.1.177, absent on 2.1.207+ -- and a
marker announcing pending work has not proven the turn is over. The repo's own
test explicitly ADMITTED `pendingWorkflowCount: 1`, so that test encoded the
permissive behaviour; it was changed deliberately with the reason stated and now
asserts both directions rather than being deleted.

Both drain evaluations share ONE binding of the graduated value: the gate and the
confirming re-poll read the same `required_drain_ms`, so the deciding read and
the confirming read cannot disagree. Changing only one would have saved nothing
or, worse, made them inconsistent.

VERIFIED BY MUTATION, not by inspection. A reviewer ran five targeted mutants in
an isolated scratch tree and the suite killed all five:
  - neutralise the floor -> the fail-closed test panics with
    `outcome: Completed, text: "premature answer"`, i.e. the mutant commits
    precisely the truncated answer the floor exists to prevent;
  - gate-only or re-poll-only graduation -> caught in both directions (41-42
    polls observed vs 7 expected);
  - drop the pendingWorkflowCount check -> parser rejection table fails;
  - let the floor override a SHORTER configured drain -> killed by a pre-existing
    fixture whose configured drain is 10ms. The reviewer predicted that mutant
    would survive and was wrong.
That last case is now stated rather than inherited: a unit test pins that the
marker only ever LOWERS the required drain, because an operator who configures a
drain below the floor asked for something faster and observing the marker must
not make them wait longer.

`ready_prompt && quiet` stay unconditional in the conjunction -- the marker is
transcript evidence and the screen is the liveness gate, and substituting one for
the other would also lose the NeedsInput short-circuit. CompletionAuthority stays
single-variant. The trailing zone is untouched: a semantic row after the marker
is still SchemaDrift, which is what makes the shortened window fail closed rather
than truncate.

S2 under D9 and owner-approved as a latency change. Verified: cargo 621/0/17
(616 baseline plus four new tests plus this unit test), fmt clean, phase0 187 OK.
`````

<a id="c34"></a>

### 34. Guard the byte that re-arms the drain, and stop the verifier flattering itself

*2026-07-30*

`````text
The graduated drain lets a proven `turn_duration` marker buy a 250ms floor
instead of 2000ms. Nothing tested the line that makes that safe.

`stable_for_ms` measures quiet since the last transcript BYTE, not since the
marker. Two lines of driver_io.rs make that true: `read_available` returns early
on a zero-length read WITHOUT touching `last_change`, and `read_observed_range`
assigns `last_change` on every read that produced bytes. That asymmetry is why
ordinal 70's row, landing 352ms after the marker and 102ms ABOVE the floor, was
not lost: the window restarts from the arriving byte instead of counting down
from the marker.

Both halves were unguarded. Deleting the re-arm passed 231/231. Adding a bogus
bump to the zero-read path -- which would reset the quiet counter on every empty
poll and stop the drain ever being satisfied -- passed 7/7. A subagent made
exactly that second mistake while trying to fix the first, which is the sharpest
evidence the hole was real.

`a_read_that_produced_bytes_rearms_stability_and_an_empty_read_does_not` pins
both directions against the real FileTranscriptSource over the TranscriptSource
boundary. A fake can only assert its own arithmetic, which is why the earlier
attempt -- whose fake hard-coded the re-arm semantics -- proved nothing.

The band tests now reach genuine post-marker quiet gaps by varying the poll step
rather than dripping keepalive rows inside the window (a drip every 200ms under
a 250ms floor makes the gate unsatisfiable by construction, so those arms tested
nothing). `graduated_polls`/`catchable_window_ms` derive their arithmetic from
TURN_DURATION_DRAIN_FLOOR_MS, so raising the floor to 500 keeps every test green
and lowering it to 1 turns one red. A test must never fail because the system
got safer.

verify_calibration.py stops reporting margin it never had. `headroom_ms` was
computed against the CONFIGURED drain, so a graduated run whose gate asked for
250ms reported 1648ms of headroom against 2000 -- and a regression that silently
disabled graduation would have left every field identical. It now reports the
EFFECTIVE required drain, and names the truncation-oracle blind spot out loud:
"9 answers, no mismatches" reads as "7 answers checked", because two graded
prompts ask for no hash and would grade an EMPTY reply exactly like a complete
one. Loud, not fatal -- those prompts are un-oracled by design, and a gate must
gate exactly the claim it protects.

Live coverage this round: ordinals 56-81 (26 attempts), including the nine-grade
hash suite at 73-81 -- 7 matched, 0 mismatches, on a build whose required drain
was 250ms. That run is honest about its own weakness: every gap sat at 0-1ms, so
it never exercised the shortened window and would have passed at drain=0. The
band evidence is the offline tests, not that campaign.

cargo test --workspace 626 passed / 0 failed; fmt clean; phase0 235 OK.
`````

<a id="c72"></a>

### 72. A window named for the guarantee it was spending, and a revision that counted captures, not changes

*2026-08-07*

`````text
The commit gate's second sampling period had no name. It is how late a
transcript row may arrive and still be read before the turn commits, and it
existed only as the product of a screen constant and a poll interval, neither of
which knows it decides truncation risk. `TURN_DURATION_DRAIN_FLOOR_MS` warns in
its own doc that tuning `quiet_for` could silently delete the 250ms floor; the
same tuning silently narrows this, one level up, and nobody had written it down.

`POST_MARKER_CATCH_WINDOW_FLOOR_MS` is 438ms, the campaign's largest post-answer
arrival over 456 turns, with ordinal 70's 352ms recorded beside it as the only
one ever seen live. `post_marker_catch_window_ms` derives the window and bounds
the measurement from below: nominal period 275ms derives 550ms, and all nine
observed `drain_ms` medians at the shipped constants -- 550.0 to 573.5 -- are at
or above it, as is 468.0 against the 450 derived for `quiet_for` 125. So it can
only ever refuse a configuration that would in fact have been safe.

`SCREEN_QUIET_FOR` discharges two assertions in its own initialiser, so a screen
configuration that narrows the window does not compile. The MINIFIED cell binds
first: at `SCREEN_QUIET_FOR_MS` 194 its window is exactly 438 and it builds, at
193 it is 436 and it does not, and at 125 -- a value measured here for latency,
saving 245ms, that no test in the crate notices even at 1ms -- it is 300ms,
below the row that really arrived. Asserting only the graduated floor admits
that. Latency-neutral by construction and measured to be: pooled n=60 before and
after, server total median 1,208 -> 1,201ms against a p10-p90 spread of ~76ms.

Gate 1's 250ms window is NOT taken, and the reason is the second finding. The
proposal to spend it rests on `TerminalSnapshot.revision` being a mutation
counter. The daemon pmux ships assigns it per capture, comparing against the
previous capture's fingerprint; over 30 turns it advanced 8 -> 67, about two
increments per turn, while the pane changed many times per turn. The interval
property is true today only through `output_sequence`, a `pub(crate)` field that
is not on the wire, that pmux cannot read or assert, and whose removal pmux
could not detect. The published contract compares two captures and says nothing
about the interval between them. Declined, with the upstream ask drafted.

Instance thirty-three: R2 called that reordering "verified gate-equivalent"
against a run that decides a different property -- it holds `drain_ms` min at
250, and it cuts the catchable window to 276ms. Six tests say so. §6.2's table
cited six `driver_io.rs` line numbers, every one correct at `<c1>` and every
one wrong now, and `performance_diagnostics.rs` published the same six into the
Gate A receipt; both now cite names. And a draft of §6.1.2 quoted a `drain_ms`
range of "550.0-558.5 over seven runs" that excluded two of this pass's own runs
including its largest -- recorded in place rather than quietly fixed, because a
range chosen to fit is the same defect, committed by the pass writing it up.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c90"></a>

### 90. An api_error stamped before the answer it was counted as arriving after, and the retrospective column that now has to prove it still is

*2026-08-08*

`````text
`measure_transcript_drain.py` refused every 2.1.220 and 2.1.223 measurement on
`~/.claude-1/projects` with `{"system/api_error": 9}`. Failing closed on an
unknown kind is right and stays; what was missing was the classification.

Read whole, those rows are HTTP-client retry records -- 67 `Connection error.`,
9 timeouts, 9 `529 overloaded_error`, 2 `401`, `retryAttempt` 1..10 of 10, all
on `entrypoint: sdk-ts`. Their `timestamp` is the moment the call failed, not
the moment the row was appended: all 87 post-answer ones are stamped BEFORE the
turn's final assistant row, the nearest by 1288 ms and the median by 185.8 s.
That row is what the successful retry produced. They sit after it in FILE order
only where a queue reorders the append stream -- 0 of 143,609 consecutive pairs
in queue-free files invert on an api_error, against 43 in queue-bearing ones,
and all 98 api_error rows in this corpus are in files that hold a
queue-operation. A minified cell has no queue.

So it is not a post-answer arrival, and the retry that DOES produce a further
answer already has its entry: `("assistant", None)`, classified reachable,
which retracts any measured value taken without it.

An argument in a comment is the bug class this repo counts. `retrospective` is
therefore a column the tool tests rather than asserts: `post_answer_arrivals`
now also returns each row's offset from the terminal candidate, and `main`
fails with a new exit 3 -- distinct from the unclassified-kind exit 2 -- if any
row of such a kind is ever stamped after the candidate. Flipping that
predicate's direction turns 2.1.223 red on the same 9 rows.

No other unknown kind was classified: `pr-link/None`, `system/compact_boundary`
and `system/model_refusal_fallback` still refuse, and 2.1.201 and below with
them. The 2.1.220 receipt reproduces in every measured field.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c120"></a>

### 120. The three lifecycle fields a modal completion return could drop into their own defaults, and the scan bound whose only witness was 20,001 directory entries

*2026-08-10*

`````text
`completion_evidence` builds a `TerminalEvidence` at three sites. The ordinary
one names every field, so deleting one does not compile; the two modal returns
carry `..TerminalEvidence::default()`, and every default is the value that means
"this turn was never armed and no Stop hook arrived". Five of the six field
deletions across those two sites survived the full-scope run, and the sixth was
a timeout counted as caught. A modal screen is negative readiness evidence the
actor takes and polls again on, which is exactly why the loss is silent.

`completion_evidence_carries_the_lifecycle_observation_on_the_modal_returns`
drives both modal paths with the observation armed and with the hook fired, and
`every_completion_evidence_return_names_the_whole_lifecycle_observation` reads
the method's own source so a return site added tomorrow is covered by something:
the behavioural test can only ever name the sites that exist when it is written.
The `TerminalEvidence` destructuring in it is what keeps "the lifecycle
observation" tied to the struct rather than to three strings.

`list_transcripts` and the rotation ledger were both bounds nothing observed.
The scan bound is 20,000 directory entries, so the fixture that proves `scanned
> limit` is not `scanned >= limit` is 20,001 files, built once per mutant by a
gate that runs the suite about 1,650 times; the bound is now
`list_transcripts_within`'s parameter and the constant is still the only one the
driver enforces. `MAX_REMEMBERED_ROTATIONS` needed no seam at all -- it is 8 --
and its two comparison mutants both settle the ledger one record short, losing
the named rotation diagnostic that exists to replace a bare `TurnTimeout`.

`transcripts_under` is the same `extension && is_file` shape already killed in
`list_transcripts`, on the path that copies what it finds into the evidence
tree. Read as `||`, every ordinary file beside a transcript is opened, redacted
and republished under `<evidence>/`.

Twelve mutations applied by hand, each watched red against the test written for
it and restored: six field deletions, three scan-bound, two ledger-bound, one
`&&`. `docs/2.1.226-compatibility.md` cited two driver_io lines the
`list_transcripts_within` seam moved. `docs/repo-review.md`'s four are left
alone: that document is pinned at HEAD `<c82>` and its citations are already
as-of that head, so relocating them to today's lines would be false precision.

cargo test --workspace: 70 binaries, 1197 passed, 0 failed, 51 ignored.
cargo clippy --workspace --all-targets -- -D warnings: clean.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

---

## C. Pool, daemon and store lifecycle

**Instance states, teardown order, transports, signals, concurrency, health, and durable stores.**

Path B pools fifteen stateless engines and recycles them with `/clear`, so almost every defect in this group is about what a second task sees while the first is between locks, or about a teardown arm that could not tell two situations apart. The other half is the daemon around it: a poisoned transport that was daemon-wide, a SIGTERM window whose whole warm mint ran at the kernel's disposition, a health surface that reported `healthy` through four real failures.

19 entries.

<a id="c37"></a>

### 37. A cancelled turn no longer bricks the daemon, and the census was twice as long

*2026-08-02*

`````text
rmux-sdk treats a DROPPED in-flight request as a permanent, never-cleared
transport failure: `OrderedResponseGuard::drop` calls `abort_with`, which sets
`TransportState::terminal_failure` (written only `if is_none()`, cleared by
nothing) and aborts the actor task. pmux held ONE such transport for the whole
daemon lifetime and dropped in-flight requests all over it. One
`timeout(Duration::ZERO, snapshot())` bricked every session in 7.75s -- proven
against a real PrivateRuntime with no Claude and no concurrency -- while `ping`
kept `pmux doctor` reporting healthy:true and `native.rs` discarded the typed
cause, so the daemon's stderr stayed completely empty through the whole event.

The census was six sites. It is thirteen. The three that mattered most were the
frequent ones: `await_turn_step` drops `submit_prompt`, `observe_screen` and
`completion_evidence` on turn deadline OR cancel signal, and `observe_screen`
runs every poll -- so CANCELLING A TURN was an ordinary daemon-wide kill. Site
12 was in no one's plan: inside `observe_cleanup_request`, `boundary.observe()?`
early-returned and dropped a PINNED in-flight `owned.cleanup()`, propagating out
of `close()` without reaching the force_reap fallback.

Fixed beneath the `TerminalSession` trait, which is why sites 7-11 needed no
edit at all. Writes detach onto a task behind a per-terminal FIFO gate; reads
and the SDK waits mint a throwaway handle from a lazy facade, which also
dissolves a drop site inside rmux-sdk itself (`wait.rs` times out over
`pane.snapshot()`) that pmux cannot reach by detaching. Detach-everywhere was
rejected: it guarantees abandoned writes always land, and the 25ms poll is a
read.

The ordering gate is load-bearing, not belt-and-braces. Removing only the
`write_order` acquisition makes the new R5 fail 10/10, with the pane rendering
`^C` BEFORE the bracketed paste -- the exact inversion the design predicted. The
witness is the line discipline: both are echoes of one input stream, so screen
order is receive order.

Two premises turned out false and are corrected in the design of record. A
poisoned transport does NOT reap PTYs -- `KillOnOwnerExit` is a lease heartbeat
on its own dedicated connection, and vendor/rmux-server has no CleanupPolicy at
all. And `timeout(Duration::ZERO, f)` is not one poll: its delay is a real timer
entry, so a `send-keys` round trip completed instead of being abandoned in 1 run
in 8. The regressions abandon via `poll_fn` instead, and R4 stalls the exact
sidecar with SIGSTOP because no `operation_timeout` value both admits a terminal
and then fails one operation on it.

`observe_cleanup_request` can no longer fail: it returns the drained result, the
latched escape evidence and an observation-failed bit, and `close()` folds all
three in with `cleanup_requested = true` hoisted unconditionally -- which is
what the old `?` left false. `force_reap_terminal` is detached and now takes a
shutdown permit; the fence that was supposed to cover it ran BEFORE the close
loop started, so a second one was added after. A retry-close previously skipped
the write gate entirely.

Five regressions, each proven red on reverted implementation and green on
restored, twice, by two independent agents. Two pre-existing flakes root-caused
in the SDK and fixed: 22/25 -> 25/25 and 12/16 -> 20/20. No manifest cell added,
so C6's stale projection is untouched.

Not done, deliberately: per-session transports, per-session rebind, a doctor
probe that reaches the control plane, and drop site 13
(`PendingStartupCleanup::close_terminal`). Two unmeasured latency figures were
deleted rather than inherited -- none was measured for this change.

cargo test --workspace 672 passed / 0 failed; private_runtime 8/8;
concurrency_backpressure 7/7; fmt clean; first-party clippy clean.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c38"></a>

### 38. A poisoned connection now costs one session, and two comments were wrong

*2026-08-02*

`````text
pmux held ONE rmux-sdk transport for the whole daemon and every session cloned
it, so the write-once poison latch was a daemon-wide latch. The facade is now
inert -- `Rmux::builder()...build()`, never `.connect()` -- and each session
mints its own: an owned-session control connection, the lease heartbeat, and a
retained write pane, plus the transient one each read already minted. Measured
during review: poisoning one session leaves siblings working, `create_terminal`
working, and the stalled terminal still reading; the negative control (shared
facade restored) fails R4 with "a sibling session must still write after another
session's write transport latched".

The pane comes from `Rmux::pane`, not `owned.pane(0,0)`, and that is not style.
`Session::pane` clones the connection `owned.cleanup()` runs on, so a latched
write would have disarmed teardown. It uses the SLOT form and not `pane_by_id`,
because a stable-id handle whose pane has died fans `list-sessions` plus one
`list-panes` per session out twice per call -- O(N) RPCs on a 20ms poll, which
can push a poll past the SDK deadline and poison the very transport it is
polling.

The staged `retryable: true -> false` flip is REJECTED, and the reasoning is
recorded rather than the flip. Narrowing the blast radius makes a retry MORE
likely to succeed; moving to non-retryable on strictly better news is not
defensible. `full_stack.rs:394-395` also pins `daemon_lost` AND `retryable` for a
real sidecar SIGKILL mid-turn, so flipping meant rewriting a passing wire
assertion. `lease_lost()` reports the scope in the message but does NOT key the
flag: the heartbeat retries until `last_success + ttl`, so after a real daemon
death it still reads false for a whole TTL while snapshots already fail --
keying on it would report genuine daemon death as non-retryable.

Removing `.connect()` removed an implicit startup reachability check, so the
probe replaces it and is strictly stronger: a socket that accepts but does not
speak rmux passed the old check and fails this one. It is also documented for
what it CANNOT prove -- `Request::Handshake` is answered before the sidecar's
global `HandlerState` lock, so the probe passes against a daemon whose dispatch
is wedged. It proves protocol, not dispatch health.

Drop site 13 is closed. `PendingStartupCleanup::close_terminal` dropped a pinned
`owned.cleanup()` and was unreachable from `force_reap_terminal`. The close now
spawns and joins, with the terminal moved into the task and its handle parked in
`self`, so a cancelled caller requeues an owner that still knows where its
terminal went and whose retry ADOPTS the close instead of issuing a second kill.
That adoption claim is scoped in the doc to the arm that actually adopts. A
permanently-failed `Lost` entry is now parked rather than requeued every idle
tick for the daemon's life.

Two comments were retracted because they were false. A lost lease is NOT
something "no single poisoned request can cause": one renew timing out under CPU
starvation latches the lease transport write-once, `lost` flips at TTL, and the
sidecar then reaps a healthy session. And the non-retryable residue is not
"writes" -- only `interrupt` and `resize` reach `map_terminal_error`; `paste` and
`enter` end in ambiguity failures that are already non-retryable. That smaller,
checkable claim is the one layer (b) has to close.

The fd cost is recorded and deliberately not fixed here: ~3.5/session in pmuxd
and 4/session in the sidecar, against a soft RLIMIT_NOFILE of 256 that both
processes inherit -- roughly an 80-session ceiling that nothing defends, detects,
or names when it is hit. A limit change is a deployment change, not a transport
change.

cargo test --workspace 680 passed / 0 failed / 22 ignored; private_runtime
--ignored 8/8 including the SIGSTOP blast-radius assertions; fmt clean;
first-party clippy clean.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c40"></a>

### 40. The pool keys on the argv a process was launched with, and the idle set is the proof

*2026-08-04*

`````text
Path B's engine now exists as `crates/service/src/pool`, with no live Claude
dependency: everything that touches a child, a TUI, a transcript or the session
registry is behind `InstanceHost`, so every edge of the machine is driven by a
deterministic double.

`--model` and `--effort` are launch-time argv and `/clear` does not re-exec, so
"any instance serves any turn" is false once model and effort are caller inputs.
The pool is `BTreeMap<InstanceClass, IdleSet>` plus a global counter, and the
class key is produced by `resolve_model_effort` -- the same call that renders
argv -- so the pool's model of an instance cannot drift from the process.
`AdmittedEffort` pairs each tier with its argv token on the table, which means
no expression anywhere produces an `--effort` value from an `EffortLevel` alone.

Membership in the idle set IS the emptiness proof. `Idle` has exactly two
inbound edges, `WarmProven` and `ClearProven`, both proof-carrying; every other
outcome quarantines. There is no cached proof re-checked at checkout. The
implication is asserted over the edge set rather than over the two the author
had in mind, and `Instance::check_invariants` refuses an idle instance whose
last transition carried no proof, whose turns reached the cap, or whose system
prompt fingerprint no longer matches configuration.

Teardown order is the guarantee: close and require a positive reaping, then
discharge retention, then erase the tree, and only then release the slot. A
close that cannot confirm reaping leaks the slot permanently and keeps the tree,
because a root a live process may still be writing to is evidence. A quarantine
keeps its evidence under `--path-b-retain-dir`; a clean recycle gets no floor.

Recycle is capacity hygiene, not a privacy bound: with 40k tokens seeded into
`history.jsonl` the next turn's `input_tokens` was unchanged at 186, so the file
never reaches model context. The cap bounds process growth and filesystem
residue and the code says so where somebody would otherwise assume otherwise.

Warming is an operator-declared warm set, high-water-mark re-warm when a
checkout empties a class, and an idle TTL that drains a cold class to its
declared floor and no further. Cold swap may still take a floor instance,
because refusing a live caller to hold a speculative one is starvation.

At the cap the pool refuses and names the budget in the message, not only in
the details blob, and nothing queues. No new `ErrorCode`: `PoolExhausted` is
declined, with its trigger and its four-step migration order written down.

`RunStatelessRequest` and `StatelessResult` are appended last to `Request` and
`ResponseResult`. The request denies unknown fields so sixteen resource names a
caller might reach for are refused by name; the result publishes seven keys and
no id, so `attach_session`, `inspect_session`, `subscribe_events`, `cancel_turn`
and `close_session` are unconstructible against a pool instance rather than
merely refused.

`native.rs` gains one arm, refusing `run_stateless` with `path_b_not_enabled`
until the pool is wired. That refusal is true today.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c41"></a>

### 41. An expiring instance left its name in the idle set, and a refused transition was already applied

*2026-08-04*

`````text
Three defects in the pool, each found by asking what a second task sees while
the first is between locks.

The idle set was tidied by whoever caused the departure, and two of the four
callers did not: the TTL sweep and the shutdown drain both marked an instance
`Destroying` and released the lock with that instance still named in its
class's idle set. A caller arriving in that window would find the slot, try to
check it out, and get `Internal` instead of an answer. Unpublishing now happens
inside `transition_locked`, the one place a state can change, so a call site
cannot forget. The test drives a sweep while a second task polls
`check_invariants`, because a window is only closed if something can look
through it.

`transition_locked` mutated the instance and validated afterwards, so a
transition its own invariant rejected left the instance half-applied -- neither
serviceable nor destroyable, holding a slot with no path out. It now builds a
candidate, validates that, and commits only on success, so a refused transition
leaves the instance exactly as it was.

A refused publish into the idle set was ignored, which stranded the slot. Every
publish site now tears the instance down, along the path that matches where the
refusal happened: a launch proof that did not stick is a mint failure, a clear
proof that did not stick follows a `/clear` that may already have been typed and
is therefore a quarantine. A refused CHECKOUT is deliberately not in that set --
the launch proof stands, so the instance is serviceable and this caller simply
cannot have it.

The publish-refusal arm is unreachable through the public API today, because
`PoolConfig` is immutable for the pool's life and the prompt fingerprint is the
only invariant a legal transition can break. It is defence against a config
reload the pool does not have yet, and no test covers it; saying so beats
building a back door to pretend otherwise.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c43"></a>

### 43. A later resize now reaches the window, and four claims were measured

*2026-08-04*

`````text
`TerminalSession::resize` called `pane.resize`. For a single-pane window that
becomes `resize-pane -x/-y`, which records a requested main width and then
rebuilds the layout tree against the WINDOW's size, so a lone pane cannot
exceed the window it sits in -- and the call returns success. `create` was
fixed for this and the resize path was left behind, so every resize after
creation was accepted and silently clamped. It now takes a window handle,
still inside `detached_write`'s FIFO gate. The new live check starts a session
at rmux's 24x80 default, so the clamp is an upper bound the test must grow out
of, and asserts on the snapshot: with the old call restored the `.expect` still
passes and the geometry reads 24x80.

The `--effort` guard was an array of five spellings written by hand, so a sixth
variant was a value the guard could not see. Both the service guard and the
facade blackbox now derive their arrays from the enum through an `every_variant!`
macro whose inner wildcard-free `match` will not compile while a variant is
missing, the same shape `wire_values!` already uses in the protocol conformance
vectors. The facade's flag word and wire word are now the same derived string.
Control: with a sixth variant live and emitting `--effort ultracode`, the old
hand-written array still passed.

`is_accepting`'s false case had no test -- with its body replaced by `true` the
whole suite passed -- because every deterministic check ran against a broker
that was accepting. It has one now, ending the same task by the same means
`Drop` does and waiting until the loop has actually stopped. That test measured
the accompanying doc false: the listener moves into the accept task, so a later
launcher does not hang in the handshake, it gets `ConnectionRefused` (os error
61) while the socket file survives and still passes `pmux-launcher`'s
`is_socket()` pre-check. Three copies of the false claim were corrected,
including the operator string.

`ControlPlaneFault::Unreachable` said "Nothing was dispatched." A peer that
accepts and then dies mid-exchange lands there too, with the SDK's own
operation name reporting the exchange as in flight. The doc is narrowed rather
than a fourth variant added -- there is no third answer to give a mid-exchange
death, and a new variant would have to reach `RuntimeFinding`,
`runtime_finding_text` and every client to tell an operator to do the identical
thing -- and the narrowed wording is now measured by a test rather than being
one more untested promise.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c44"></a>

### 44. Merge the pool core: the stateless engine, refusing until it is wired

*2026-08-05*

`````text
Both request enums were appended on separate branches. `Diagnose` keeps its
index because the shared conformance corpus already carries it; `RunStateless`
follows. The client tag table and the client golden driver take both arms, and
the two blocks of wire tests concatenate rather than replace.

`native.rs` answers `Request::RunStateless` with `pool::path_b_not_enabled()`
-- an honest refusal, not a stub that pretends -- until the integration lands.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c47"></a>

### 47. Health is a proof tree, and a layer nobody reported is not a healthy layer

*2026-08-05*

`````text
`DaemonDiagnosis` gains `layers`: one entry per `HealthLayerName`, each either
`exercised`, `faulted`, or `not_established`, with `outcome` derived from the
finding the way `RuntimeProbe`'s already is. The layers are configuration,
control plane, private runtime, launch broker, compatibility profile, pool,
sessions and performance.

The rule that makes it a tree rather than a longer boolean is in `outcome()`:
it folds every layer AND every layer that is absent, as `unproven`.
`ProbeOutcome::fold` over an empty set is `pass` -- right for sessions, where
holding none is a capacity fact -- so a fold over `self.layers` alone would
report a daemon that established nothing as healthy, which is the exact
sentence this surface exists to make unsayable. `HealthLayerName::ALL` is built
from an exhaustive `match` rather than written out, because `missing_layers`
reads it: a layer absent from a hand-written array is a layer nothing ever
notices is missing.

Two layers were split apart because they fail differently and an operator does
different things about them. The control plane is "was a connection made"; the
private runtime is "did the sidecar COMPLETE a dispatch-path exchange". A
sidecar that has been stopped, killed or wedged still owns a socket that
accepts, which is why all four false-healthy reproductions failed at the
second. `Unreachable` is a fault of the first and `not_established` for the
second -- nothing was asked, so nothing is claimed. It reuses the foundation's
`probe_request_path` rather than inventing a second probe: two probes of one
subject is two answers that can disagree.

The performance envelope is READ from `PrivateRuntime::operation_timeout`, not
restated beside it. A constant here would be a second copy of the bound the
runtime enforces, free to drift from the one that is enforced.

Each layer states what it exercised, for every finding including `exercised`.
A pass with an empty detail is the boolean this replaced, one level down, and
both shipped clients refuse an empty one.

`pmux doctor` renders the layers verbatim through their own `detail` strings
and names every layer the daemon did not report -- an older daemon that knows
`diagnose` but not the tree reaches that path, and silence there is what this
command was repaired for. It stays a VIEW: the four local checks only a client
can make, plus the daemon's own findings, folded on `ProbeOutcome`'s severity
order under different names.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c48"></a>

### 48. Live: a caller gets tokens for (model, effort, prompt), and two defects the socket found

*2026-08-05*

`````text
`pmux ask --model sonnet --effort low 'What is 2 plus 2?'` answers `4` with
`input_tokens=174 output_tokens=3`. Nothing else is named on the way in, and
the response object carries exactly `model reported_model effort text
stop_reason usage claude_version` -- no session id, no cwd, no configuration
root.

Getting there took three fixes the default suite could not have found.

The pool launched its children with an EMPTY environment, on the argument that
an inherited `HOME` is how a config root escapes. It is also how a child
authenticates: with no `HOME` and no `PATH` the first turn returned
`needs_login`. The host now carries the DAEMON's own environment, captured once
at pool construction -- daemon configuration in the same sense
`--path-b-claude` is, and nothing on the wire can put a byte in it. It still
goes through the allowlist, the subscription-auth removals and the transparent
profile's denylist, and step 6 of `build_environment` still overwrites
`CLAUDE_CONFIG_DIR` with the slot's root after every removal, so the inherited
`HOME` cannot move the root the child lands on.

`wait_for_turn` took `(session_id, generation_id)` and re-resolved them through
`SessionRegistry::stored_turn`, which goes through the caller-only resolver.
Every pool turn therefore resolved its actor correctly, submitted correctly,
typed the prompt into a real Claude, and then asked a resolver that refuses
pool sessions for the answer: `code=SessionNotFound message="session
662eb2d7-... is not registered"` while the pool census reported that instance
live and idle. It now takes the ACTOR. A handle cannot be obtained without
having already decided the owner, so a second owner decision is not expressible
there.

The first health tree this daemon produced listed a POOL INSTANCE's session id
and generation id in `DaemonDiagnosis::sessions`, and reported it as "left the
registry while the probe was running" -- because the caller-only resolver had
refused it. Two defects in one entry: the report was wrong, and it published
the one name `SessionOwner` exists to hide, in a report any client may ask for.
`diagnose` now enumerates caller sessions only; the pool's instances are still
probed, and the pool layer says how many of them the sidecar reports and never
which.

A fourth was found by the wire itself: the configuration layer put a `u64` FNV
fingerprint in its evidence, and protocol v1 refuses an opaque integer outside
the signed safe range -- so one layer's evidence cost the ENTIRE diagnosis and
`doctor` reported `unproven` with no report at all. The fingerprint is hex now,
every pure layer is checked for representability in the default suite, and
`diagnose` replaces an unrepresentable layer with one that says so rather than
dropping it, because a dropped layer is silently not-established with no reason
attached.

Two guards gained the test they were missing. `SessionOwner`'s refusal is now
pinned against the refusal a session that never existed gets -- same code, same
retryable, same details, and the same answer for a wrong generation, because a
`stale_session_generation` body names the session and so confirms it exists.
And the sidechain guard's USAGE half, which is the only half a live daemon has
now that the host reports no row count, had no test at all: deleting it left the
whole suite green.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c50"></a>

### 50. A daemon holding nothing is healthy, and pmux sealed only the last directory it made

*2026-08-05*

`````text
`pmux doctor` exited 1 on every correct Path B daemon, forever. `sessions_layer`
mapped "the registry holds no sessions" to `NotEstablished` -> `unproven`, and
`<c48>` had deliberately removed pool instances from `DaemonDiagnosis::sessions`
-- a pool instance's session id is the one name no client may learn -- so a daemon
serving only `pmux ask` reports `sessions: []` on every probe it will ever answer.
MEASURED against a warm pool of two idle instances with every other layer `pass`:

    status: unproven
    unproven: ['sessions: the registry holds no sessions, so no session was exercised']
    pmux: doctor could not prove every check it ran   (exit 1)

`pool_layer` had the same defect one branch along, and confessed it: its detail
read "holding none is a capacity fact rather than a fault" while its finding
encoded strictly worse than pass. The surface built because a boolean `healthy`
lied through four real failures was crying wolf on every healthy daemon, and a
genuine `unproven` was indistinguishable from the permanent one.

`LayerFinding` gains a fourth value. `NothingToExercise` is `pass` and means the
layer was reached, evaluated, and found to have no subject; `NotEstablished` is
still `unproven` and still means the subject exists and could not be reached. A
layer that is ABSENT is still `unproven`, and a session whose actor never
answered still is. Same daemon now: `status: healthy`, exit 0.

The test that let it ship built the tree by hand: `every_layer_exercised()` beside
`sessions: []`, a pair no producer can emit, asserted `healthy`. The sessions
layer now has ONE producer, `HealthLayer::for_sessions`, in the protocol crate,
and every fixture calls it -- an unreachable combination is no longer sayable.

`create_owner_only` called `create_dir_all` and chmod'd only the leaf, so
`<parent>` and `<parent>/<slot>` were minted 0755 and survived shutdown, leaking
pool size, epoch counters and turn timing to any local user. The test that proved
otherwise walked a hand-written `[epoch_dir, root, cwd]` -- the one unsealed level
was the one absent from the list. The walk is now `SlotPaths::minted_dirs`, an
ancestor chain, which cannot omit an intermediate level. `pmuxd`'s own
`ensure_private_directory` had the identical bug and is the fourteenth instance of
this class: `--socket /tmp/x/deep/run/pmux.sock` left `drwxr-xr-x /tmp/x` and
`drwxr-xr-x /tmp/x/deep` behind a guard whose message says "must be owner-only".
One `create_private_dir_all` now serves the socket dir, the log dir, the pool
parent and the quarantine dir, and the pool parent is refused at boot on the same
mode-and-ownership bar as the socket directory -- refused, never silently chmod'd.

`ModelEffortRefusal` said "does not admit --effort XHigh; it admits [\"low\", ...]":
one sentence, two spellings, and the one after the literal `--effort` is rejected
by clap and by `EffortLevel`'s own `Deserialize`. No test read any of these
strings. `EffortLevel::as_str` is now the single spelling, pinned against
`Serialize`, and a test parses the token each message prints after `--effort` back
through the same parser the CLI uses.

`pool_exhausted` claimed "Rule 7 fires iff every instance is mid-turn". Rule 4 also
fires with instances in `Reserved`, `Warming`, `Quarantined` or `Destroying`, none
of which `in_flight` counts, so a pool refusing with both instances in teardown
rendered "serving 0 of its 2 configured instances". Both call sites passed
`pool_size` where the budget belonged, so after a leak the message overstated the
budget permanently. The counts are derived once by `PoolState::pressure`, the
budget is `capacity`, and the leaked-slot reclaim -- reachable with nothing in
flight and another class idle -- has a refusal of its own.

25 checks added or changed; each was deleted and its target rerun, each failed,
each file restored byte-exact. All 60 test targets pass in isolation.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c51"></a>

### 51. A declared warm floor is a promise, and a daemon that declined Path B could still prove itself

*2026-08-05*

`````text
`<c50>` moved the pool layer's empty-set arm from `NotEstablished` -> `unproven`
to `NothingToExercise` -> `pass`. Both encodings are the same error: the layer
asked "is the pool empty?" when the question is "is the pool empty when something
declared it should not be?". Its inputs were `(pool_size, census)`, the declared
warm floor was not among them, and without it the layer cannot tell a cold pool
nobody asked to hold anything from a pool told to hold two and holding none.

MEASURED through the product against `pmux-test-claude`, on a daemon booted
`--path-b-pool-size 2 --path-b-warm claude-sonnet-5/low=2` that was healthy and
serving, then had its Claude executable replaced and its two instances killed:

    $ pmux ask --model sonnet --effort low 'Say OK.'     (six consecutive calls)
    pmux: pmuxd error code=DaemonLost message="private rmux lease was lost ..."
    $ pmux doctor --output json; echo $?
    status healthy   errors []   unproven []
    pool  pass  nothing_to_exercise
      "the stateless pool is configured for 2 instances and holds none, so there
       was nothing to exercise; ... and the next call of any class mints one"
      {"live": 0, "idle": 0, "pool_size": 2, "capacity": 2}
    0

The detail's closing clause is a claim the predicate never tested and that was
false in the state that produced it. Nothing else in the daemon records the
condition: `spawn_rewarm` discards a failed mint with no log and no counter
(`pool/mod.rs:746`), while the TTL sweep already treats the floor as a live
invariant and `Pool::start` calls the identical condition fatal at boot -- "operator
errors worth failing startup over, not degraded modes". Thirty seconds later it was
`pass`.

`pool_layer` now takes `PoolSubject { pool_size, declared_warm, census }`, and
`declared_warm` is `PoolConfig::declared_warm_total`, folded from the same
`warm_set` `Pool::start` refuses to boot without. A floor of zero with an empty
pool is vacuous and passes; a declared floor with an empty pool is `faulted`, and
the detail names the floor and says why. Only the whole floor being absent is
claimed: a cold swap may take a declared-but-idle instance for a live caller and a
recycle holds a slot between destroy and mint, so `live < declared_warm` alone is a
state a correct pool passes through under load. Neither arm promises a mint any
more, because neither tests one. Same drained daemon now: `unhealthy`, exit 1, one
error naming the floor. Same census booted without `--path-b-warm`: `pass`, exit 0.

The sentence `<c50>` set out to delete was still true one layer over for every
PATH A daemon. `compatibility_layer`'s `(admitted: 0, path_b: false)` arm was
`NotEstablished`, and a daemon booted without `--tested-claude-profile` and without
`--path-b-parent` is correct, supported and serving -- `full_stack.rs:4907`
exercises exactly that shape. MEASURED: that daemon served a real turn over the
same socket, and `pmux doctor` exited 1 on it, permanently, for declining a
feature. No pool means nothing the daemon runs requires a promoted cell, so the
layer has no subject rather than an unreachable one; a caller who explicitly
demands a tested cell is still refused at that request, which the same test
measures one assertion earlier. It was a method for no reason but two reads off
`self`, and being one kept every arm of it out of the pure-builder test; it takes
the two numbers as arguments now and all three arms are covered.

`LayerFinding` gains no value, so the wire and both shipped clients' validators are
untouched -- but the generalisation the fourth value was documented under was wrong
in `docs/spec.md:1396`, `LayerFinding`'s own docs, `ProbeOutcome::fold`'s and both
clients', each of which named "a pool holding no warm instances" as an empty-set
subject. All five now carry the qualifier and the rule: an empty set is vacuous
only when nothing declared it should be occupied, and a `detail` may state only
what its own predicate tested.

Every other layer's empty/zero arm was audited against the same question and found
correct. `configuration`, `control_plane`, `launch_broker` and `runtime_finding`
have no set to be empty. `private_runtime` publishes a zero terminal count as
evidence and branches on none of it, which is right: whether zero terminals is a
fault belongs to the layers that know what was declared. `sessions` is vacuous on
an empty registry because nothing in a daemon's configuration declares a session
must exist. `performance`'s subject is the exchange it timed; its "every session
actor answered" clause is a vacuous universal over an empty set and decides no arm.
`pool_layer`'s terminal-count arm compares the registry's own instance list against
the sidecar's, so an empty list is "nothing was declared" and not "something is
missing". `DoctorReport::fold` treats empty `errors` and `unproven` as healthy
safely, and only because `missing_layers` has already pushed every unreported layer
into `unproven` from `HealthLayerName::ALL` -- the same declaration-first shape the
pool layer now has.

Fifteen checks added or changed, in `native.rs`, `pool/config.rs` and the
full-stack lane. Each was verified by mutating the production code it guards,
rerunning its target, and confirming that check reported the failure; every file
was restored byte-exact by sha256, and no mutant survived. The full-stack
assertion was run for real against candidate binaries -- `--ignored`, one test,
19.35s -- and fails on `LayerFinding::NotEstablished` for the profile-less daemon.

All 60 test targets pass in isolation, one `cargo test` invocation each; the
aggregate is deliberately not quoted, because it is not a stable number on this
host. `pseudomux-service` unit 353, `path_b_pool` 27, `v1_actor` 56,
`pmux/process_boundary` 25, `protocol/v1_wire` 44, zero failures anywhere.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c52"></a>

### 52. Nobody waits on a clearing instance, and shutdown left the roots it had just used

*2026-08-05*

`````text
`crates/e2e/tests/pool_concurrency.rs` is the wave harness the pool never had. Every
concurrency claim in the product was a single-threaded assertion against an in-process
fake host, and four concurrent callers was the most that had ever run live. Thirteen
deterministic waves now run 2, 5, 8 and 15 concurrent callers across four classes
against a real daemon, a real private rmux sidecar and one `pmux-test-claude` per
instance, plus three real-Claude waves behind `PMUX_POOL_REAL_CLAUDE`.

**Fungibility is proven from the CHILD side.** `StatelessResult::model` is the class key
copied out of the request path, so asserting it proves nothing: a pool that answered
every `opus/max` call from a `haiku` process would still publish `claude-opus-5`. The
harness joins `prompts.jsonl` (which process received which prompt -- new, one row per
accepted submission) to `launches.jsonl` (which argv that process was launched with) on
`cwd`, which is `<parent>/<slot>/<epoch>/cwd` and belongs to one process for its whole
life. The argv is read whole; no summary of the class key is restated in the evidence
path.

**The first four waves proved nothing and passed.** 15 callers, 15 launches: a pool that
mints one instance per caller cannot mis-route one, so every fungibility check was true
for a reason unrelated to routing. `claim_reuse_was_exercised` now asserts the exercise --
some instance served two different callers -- and `expect_reuse` is DERIVED from
`rounds > 1 && recycle_turns > 1`, because at a cap of one no instance can ever serve a
second caller. It fired three times while this was being written.

**Four defects, all in something that reported success.**

1. `in_flight` spanned `CheckedOut | Delivering | Clearing` and the census rendered it as
   "{n} serving a turn". `Clearing`'s own doc says "the caller's response has already been
   handed back. Nobody waits here." MEASURED over the socket at 8 concurrent against 8
   slots: "8 of 8 usable instance(s) are live -- 7 serving a turn" at an instant when zero
   were. It is the DOMINANT refusal under load, because `spawn_clear` exists so the caller
   is answered before `/clear` is typed, so a caller that retries immediately meets a pool
   whose slots are all clearing -- ~30ms of work, reported as however long a model takes.
   `InstanceState::census_bucket` is now the one wildcard-free grouping, `PoolPressure`
   holds `BucketCounts` filled by one pass over the instances, `live` is the SUM of the
   five printed clauses rather than a separate count, and every bucket gets its phrase from
   a wildcard-free match. `an_exhausted_pool_refuses_immediately_and_names_its_budget`
   asserted `in_flight == 2` under a comment reading "both instances are mid-turn from the
   pool's point of view". Both were false and together they pinned the defect; the
   assertion is now `in_flight == 0`, `clearing == 2`, and the sentence.

2. `Pool::shutdown` decided per state with `_ => continue` under the comment "a turn in
   flight keeps its instance: the caller is owed either an answer or a refusal". True of
   `CheckedOut` and `Delivering`, false of the other four states it covered. Since the pool
   answers before it clears, the ordinary state at the end of any burst of work is "every
   instance is `Clearing`" -- so a daemon stopped after serving traffic skipped every
   instance it had just used and left the whole config root of each on disk. MEASURED: one
   `pmux ask`, then SIGTERM, left `<parent>/0/0/root/projects/pmux-e2e/<id>.jsonl` carrying
   that caller's prompt, beside `.claude.json` and `settings.json`, with `leaked` still 0,
   nothing logged, and the daemon exiting 1 on `SessionNotFound`. `machine::shutdown_action`
   is now total over `InstanceState` with no wildcard; `Clearing` drains, `Quarantined`
   begins its destroy, `Destroying` is completed rather than raced, and only the two states
   that owe a caller an answer are kept. The daemon now exits 0 and the parent is empty.

3. `pool_layer` reached its `exercised` arm through `terminals_present.is_some_and(...)`,
   which reads false both when every terminal is present and when the control-plane probe
   never answered. After the sidecar was SIGKILLed under fifteen concurrent callers it
   reported `exercised` -- pass -- over `instance_terminals_present: null` with an instance
   registered, above a detail string that said "no instance terminal was looked for".
   `private_runtime` and `performance` are built from the same probe and both already said
   `not_established`; this layer was the outlier. It now says `not_established` too, and no
   existing test changed colour, which is how it was invisible.

4. `append_json` in `pmux-test-claude` streamed each row token-by-token to an unbuffered
   `File`, so `serde_json` issued a write per token. Five concurrent instances appending to
   one ledger produced `Error("key must be a string", line: 1, column: 25)` -- one process's
   tokens spliced into another's row. The row is serialized into memory with its newline and
   written once; a short write is REFUSED rather than looped, because looping resumes from
   the middle of a row, which is the interleaving being prevented.

**Twelve checks verified by mutation.** Each was verified by breaking the production code it
guards, rerunning its target, and confirming that check named the failure; every file was
restored and re-hashed byte-exact, and no mutant survived. Six unit-level (shutdown drains
`Clearing`; shutdown keeps a caller's instance; `Clearing` is its own bucket; every counted
state gets a clause; an unanswered probe is unproven; the refusal separates serving from
clearing) and six against a live daemon (fungibility, the layer's census sum, no wrong
answer, `/clear` really rotates, message-vs-details agreement, and the ledger under
concurrent writers).

**Real Claude, sonnet/low, 61 turns.** 2, 5 and 8 concurrent, cold wave then warm wave
against one daemon, on 2.1.220. Cold median 6638 / 7182 / 10225 ms; warm median 3186 /
3302 / 3471 ms. Every turn answered its own unguessable token, no answer carried another
caller's, and the pool parent was empty after every shutdown. The 8-way run is the largest
live concurrency this product has been driven at.

Everything about admission, class routing, checkout, recycle, the cap and teardown is
established here BY THE DOUBLE at 2/5/8/15; the real lane establishes only that real turns
complete concurrently and what they cost, at 2/5/8. Nothing here says anything about Ink
frame geometry, which the double does not render.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c56"></a>

### 56. Nobody waited for a clearing slot after all, and a cold swap raced the caller it was meant to save

*2026-08-06*

`````text
`<c52>` is titled "Nobody waits on a clearing instance" and it never touched `Pool::admit`.
What it fixed was the CENSUS -- `Clearing` had been counted by `in_flight` and printed as
"serving a turn" -- and `shutdown`, which skipped the instances it had just used. Admission
went on refusing a caller whose whole pool was clearing, and the refusal it printed after
that commit said so in as many words.

**MEASURED at HEAD, release daemon, ten isolated runs of
`eight_concurrent_callers_against_three_slots_cold_swap_rather_than_starve`: 10 of 10 FAILED.**
served 3, refused 21 (21 at the cap), rounds `[2.3s, 337-810us, 337-810us]`, 3.25-7.76 s wall.
Rounds 2 and 3 refused all sixteen callers in MICROSECONDS over

    3 of 3 usable instance(s) are live -- 0 serving a turn,
    3 clearing between turns, with no caller waiting, 0 idle

3 launches for 3 served calls, so no instance ever served a second caller and every
fungibility check in the run passed vacuously -- which is what `claim_reuse_was_exercised`
refused to report as a pass. It failed FASTER than it passed, which is the signature of a
capacity signal that is false rather than a pool that is busy.

**A caller now waits, and the predicate is a claim about WHO the pool is waiting for.**
`CensusBucket::comes_back_on_its_own`, wildcard-free over the grouping that already exists:
`Clearing` and teardown come back with no caller's help; `Serving` is holding a model and
waiting there is a queue; `Reserved` is a launch already spoken for -- a background re-warm is
the one reservation that ends up idle for somebody else and this bucket cannot tell the two
apart, so it answers for the case it can prove. Genuine exhaustion still refuses on the first
read, and publishes `admission_wait_ms: 0` to say it looked. `PoolPressure::coming_back` is
DERIVED by filtering the same clause table the census prints, so the number a caller waits on
and the numbers the refusal names cannot disagree; both come off ONE `state.pressure(..)`.

**THE CEILING WAS FIRST SET WRONG, from a number that is about something else.** It was 500 ms,
on the strength of the "~30 ms" `docs/path-b.md` sec.3.4 measures. That number is the transcript
ROTATION -- Enter to the new file existing. What `finish_turn` awaits is `InstanceHost::clear`
end to end: `/clear` into the composer, the local-command menu resolved, then the rebound
transcript PROVEN inert, carrying the profile's `transcript_drain_ms`. MEASURED over the socket,
all seven clears of one wave at the double's 50 ms drain: **703, 723, 727, 730, 748, 749, 756 ms**,
median 730. At 500 ms that wave served 7 of 24 and printed refusals reading "1 clearing between
turns" ~230 ms before that clear finished. `ADMISSION_WAIT_CEILING_MS` is 2500: above the
~1700 ms the same clear costs at the promoted 2.1.220 profile's 1000 ms drain, with half again
on top. The caller's own `deadline_unix_ms` is the other bound, resolved BEFORE admission and
re-read every pass; the smaller wins. The wait polls at 5 ms rather than waiting on a
notification, because a notification is only as live as the set of sites that remember to signal
it and a re-read of the pool cannot be wrong about what the pool holds.

**A COLD SWAP IS THE ALTERNATIVE TO REFUSING, NOT TO WAITING** -- and with the wait in and this
half out, the wave still failed, for a new reason. Rule 3 destroys an instance the pool has
proven clean and pays a full mint for the replacement; fired the instant a slot appears, it also
takes that slot out from under a caller of the instance's OWN class waiting beside it. Once
callers wait at all that is not an edge case, it is every admission. MEASURED: **7 launches for
7 served calls** -- every call served by a process the pool had just built, having destroyed one
it had just proven clean. Rule 3 is now deferred while something is coming back AND the caller
has budget, and `may_wait_longer` is false on the last look, so it can never become a refusal:
"no caller is refused while another class sits idle" is unchanged, and a pool holding only idle
instances of another class has `coming_back == 0` and swaps on the first read at no added latency.

**MEASURED after, ten isolated runs: 10 of 10 PASSED.** served **9** -- the most three slots can
serve in three rounds -- refused 15, **3 to 5 launches for 9 served calls**, 2 to 3 instances
serving more than one caller, rounds `[2.3s, 2.35s, 2.1s]`, 7.64-8.50 s wall. Round 1 still
refuses five immediately over `3 reserved or warming`, with `no slot was on its way back, so none
was waited for`; rounds 2 and 3 refuse over `3 serving a turn` with `no slot came back in the
754 ms this turn waited for one`. Both sentences are new and both are true.

**One test predicate was wider than its own message, and this change walked into the gap.**
`claim_slot_accounting` asserts that after a sidecar kill the pool layer must not roll up green,
"while holding an idle instance whose process is gone" -- but it fired on `present != registered`,
and `None != Some(0)` is a pool holding NOTHING beside a probe that did not answer. That state is
new because it is BETTER: once refused callers wait, the disrupted round uses every slot, so every
instance meets the dead sidecar and every one is destroyed, where before 5 to 15 callers were
refused at the cap and the pool ended holding idle instances with dead panes. The pool layer
answers `nothing_to_exercise` there and is right to -- its one non-self-referential question is
whether the sidecar holds a terminal for the instances the pool believes in, and it believes in
none. The claim MOVES rather than being dropped: the daemon as a whole must not roll up green, and
`DaemonDiagnosis::outcome` folds `control_plane` and `private_runtime`, which are the layers that
ask about the sidecar. MEASURED across six runs, the original arm is still taken in four
(`Some(1)` registered against `None`, asserted `NotEstablished/Unproven`) and the new one in two
(daemon rolled up `Fail`).

**Twelve mutants, none survived.** Each was verified by deleting the production rule the check
guards, rerunning its target, and confirming that check named the failure; every file was restored
and re-hashed byte-exact, and `sha256` before and after match for all of them.

1. `Clearing|TearingDown => true` deleted -> `path_b_pool` five checks (`a_caller_waits_for_a_clearing_slot_instead_of_being_refused`, `a_cold_swap_waits_for_a_clearing_slot_before_destroying_a_warm_one`, `a_deferred_cold_swap_fires_when_the_wait_runs_out_rather_than_refusing`, `an_exhausted_pool_refuses_after_a_bounded_wait_and_names_its_budget`, `the_wait_ends_at_the_callers_deadline_when_that_comes_first`); and the unit pair
   `a_bucket_comes_back_on_its_own_exactly_when_nobody_has_to_finish_a_turn` +
   `the_slots_a_caller_waits_for_are_the_ones_the_census_says_come_back`.
2. `Serving => false` widened to `true` -> `a_pool_whose_slot_is_serving_a_turn_refuses_without_waiting`, plus the same unit pair.
3. `coming_back`'s bucket filter deleted -> `the_slots_a_caller_waits_for_are_the_ones_the_census_says_come_back`.
4. the waited/not-waited clause collapsed to one sentence -> `an_exhausted_pool_refuses_after_a_bounded_wait_and_names_its_budget` and `a_capacity_refusal_says_whether_it_waited_and_for_how_long`.
5. the wait itself deleted -> the same five `path_b_pool` checks as (1).
6. the CEILING bound deleted -> the target HANGS, killed at a 240 s timeout. That is the mutant's confirmation: without it a pool under sustained load always has something clearing, and the predicate alone waits forever.
7. the DEADLINE bound deleted -> `the_wait_ends_at_the_callers_deadline_when_that_comes_first`.
8. the cold-swap deferral forced off -> `a_cold_swap_waits_for_a_clearing_slot_before_destroying_a_warm_one` and `a_deferred_cold_swap_fires_when_the_wait_runs_out_rather_than_refusing`.
9. `may_wait_longer` dropped from the deferral -> `a_deferred_cold_swap_fires_when_the_wait_runs_out_rather_than_refusing`.

The narrowed E2E arm was verified the same way, live: `ProbeOutcome::fold` forced to `Pass`, then
eight runs of the sidecar wave -- caught in all four that reached the empty-pool branch, over the
folded layer list including `(ControlPlane, Faulted, Fail)`, so it is reading the roll-up and not
one layer.

`an_exhausted_pool_refuses_immediately_and_names_its_budget` is renamed
`..._refuses_after_a_bounded_wait_...`: its every assertion still holds and none was weakened, but
its NAME asserted the defect. It gains the two halves that have to be asserted together -- the wait
is REAL (`admission_wait_ms >= ADMISSION_WAIT_CEILING_MS`, read from the pool's own constant) and
BOUNDED (wall clock in `[ceiling, 4x ceiling)`), because a refusal that never waited is the defect
and a wait with no ceiling is the hang it must not be traded for.

Verified: `path_b_pool` 35 tests; `pseudomux-service --lib pool::` 71; `cargo test --workspace` 67
binaries, zero failures; `pool_concurrency --include-ignored` 14 of 14 double-lane waves (the 5
real-Claude lanes fail on the absent `PMUX_POOL_REAL_CLAUDE` at HEAD identically, confirmed in a
clean worktree); `cargo fmt`/`clippy` clean first-party (4 pre-existing `vendor/rmux-server`
warnings); `cargo doc --locked --workspace --all-features --no-deps` under `RUSTDOCFLAGS=-D warnings`
clean; `ruff`, `shellcheck` and `bash -n` clean; `scripts/gate-a-residue.sh` passes with no leaked
daemon, temp root or pool parent.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c61"></a>

### 61. A terminal that could still read but never write again, and a deadline that answered to whichever call it expired in

*2026-08-06*

`````text
Path A's two open items, closed. Both were pre-existing, both tracked, both
deliberately deferred while Path B was the priority.

TRANSPORT LAYER (b) — implemented as ONE change, and the rest of it declined.

After layer (c) every read minted its own throwaway connection, but writes rode
one `Pane` captured at `create` for the terminal's whole life. rmux-sdk binds a
handle to its `TransportClient` at construction and
`TransportState::set_terminal_failure` is write-once and never cleared
(transport/state.rs:39-44), so one aborted write left `paste`, `enter` and
`interrupt` failing on that terminal FOREVER while `map_terminal_error` went on
answering `DaemonLost retryable: true` — a retry the caller could not win.
Nothing had to be abandoned by pmux to reach it: the SDK's own
`operation_timeout` against a stalled sidecar is enough.

REPRODUCED at <c60>, then fixed:
`private_runtime.rs::private_terminal_write_recovers_after_the_sdk_aborts_its_write_transport`
SIGSTOPs the exact private sidecar, awaits one `paste` to completion, SIGCONTs,
and writes again. Before: `the same terminal must write again once the daemon
answers: "ControlPlaneLost"`. The regression proves the pane, not the return
code — the recovered paste is found by `wait_visible_text` and the recovered
interrupt by the fixture's own SIGINT trap.

`write_pane`/`write_window` mint per write from the same lazy facade reads use,
INSIDE the spawned task and under the FIFO permit. Outside it, a write abandoned
on its first poll would take the connect with it and never be issued at all —
`private_abandoned_paste_reaches_the_pane_strictly_before_a_following_interrupt`
is what pins that, and it is why this is not a two-line change.

The rest of (b) was decided against the tree (a)/(c)/(d) actually produced, not
implemented because it was written down. The `ControlPlane` with epochs, watch
channels and rebuild budgets is obsolete — (c) deleted the thing it rebuilt. The
`pane_by_id` identity validation is rejected as the O(N)-RPC hazard the file's
own doc forbids. `matches_pid` and the `process_reaped` rule were already
satisfied in the tree, verified by reading. The sidecar death latch is
SUPERSEDED by (d): `try_wait()` cannot see a SIGSTOPped sidecar and
`probe_request_path` can, and a killed sidecar already fails every request
immediately with nothing spinning behind it. The fd budget is the one item still
genuinely open. Full disposition table: docs/current-state.md §9.11.

MEASURED, four sessions, sampled at rest, before and after: per-session fds went
from 3.00 owner / 4.00 sidecar to 2.00 owner / 3.00 sidecar. The `~80 concurrent
sessions` ceiling is NOT restated — it does not follow from 4/session and a 256
soft limit, and it was not re-derived.

ONE PHYSICAL DEADLINE, ONE ANSWER.

`InputGateBudget::cap` is `min(gate maximum, remaining turn)`, so a fired
`tokio::time::timeout` means either "the turn is over" or "this operation could
not be proven inside the gate's own bound". `gated_snapshot` and
`gated_styled_screen` asked which; `paste_once` and `enter_once` did not. The
same physical event therefore reached callers under two different codes
depending on nothing any caller could observe.

On the `/clear` path it was not even a race. `DEFAULT_CLEAR_TIMEOUT_MS` and
`INPUT_GATE_MAX_DURATION` are both 15,000 ms and the deadline is computed first,
so the remaining turn binds on EVERY clear — every write expiry there was a
deadline wearing another code, and `clear_and_rebind` is not wrapped in
`await_turn_step`, so it reached `pmux clear` and the pool unaltered.

The question now lives once, on the budget (`InputGateBudget::expiry`), read by
all four sites. `enter_once`'s deadline answer keeps `mark_enter_attempted`:
`clear_and_rebind` reads that one key to decide whether the bound transcript is
suspect, and a bare `TurnTimeout` out of `enter_once` published
`clear_not_submitted: true` for a `/clear` whose Enter had already gone in.

THE BUG CLASS, instance nineteen — a census that said EVERY over six of seven.

`pool/refusal.rs` exposes seven refusal constructors.
`every_pool_refusal_uses_a_code_both_shipped_clients_already_know` hand-listed
six. PROVEN blind: with `sidechain_rows_not_counted` answering
`ErrorCode::Internal`, a code the module says it never adds, the whole suite
reported `test result: ok. 12 passed; 0 failed`. The set is now derived from the
module's own source and checked from two directions. Two prose defects went with
it: `pool_concurrency.rs` said "exactly these four" above a ten-element list, and
`current-state.md` §8 said the matrix was "95 rows, re-parsed today" when parsing
it returns 116.

MEASURED AGAINST C10, AND IT DID NOT COME OUT CLEAN.

Eight whole-target `pool_concurrency` sequences with layer (b): 2 red. Eight
with `backend.rs` restored byte-exact from <c60>: 0 red. Both reds are debt
row C10's own test with C10's own census. At n=8 per arm that is not a
significant difference, and the pre-fix arm's 0/8 does not reproduce C10's
recorded 2-in-12 at the same commit either — a null result over an unstable
baseline, not a clearance. The one mechanism this change plausibly opens (a
detached write now holds the FIFO permit across a `connect` as well as a
request) and the experiment that would settle it — ~30 sequences per arm, not
eight — are written into the C10 row. Shipped with that stated rather than
hidden.

No wire code, no `retryable` value and no protocol variant changes.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c69"></a>

### 69. A safe direction that was a locked door, and a pointer that was only ever a lower bound

*2026-08-07*

`````text
DEFECT TEN. `AgentStore::update` published a version file and then moved `head`, and the
comment between the two lines claimed a crash there "reads as 'the update did not land' --
the safe direction". `docs/spec.md` Sec. 4.8.2 repeated it. Nothing tested it, and it was the
same author's second false measured claim in consecutive commits.

MEASURED FALSE with a SIGKILL harness -- a child updating one agent in a loop, killed at an
offset jittered uniformly across one *measured* update cycle -- 19 of 45 trials:

    head=4  published_max=5
      retry@4 -> IdConflict: agent 8f830f27-... is at version 5, not the expected version 4
      retry@5 -> IdConflict: agent 8f830f27-... is at version 4, not the expected version 5
      get(None) -> 4   list -> 4   unreadable=0

It did not read as "the update did not land". It read as "this agent can never be updated
again": `update` always recomputed `head.next()`, so it always targeted the number already on
disk, and `link(2)` always refused it. Consecutive attempts on one fence were told it was
stale in OPPOSITE directions, and `list` reported the record healthy at the older version with
nothing unreadable.

The harness's own first version found ZERO wedges in 40 trials, because it took the offset
from `subsec_nanos() % 4000` and sampled one phase of a 20ms cycle. A crash-safety property
confirmed by a harness that samples one phase is the same defect one level up, so the harness
now calibrates the cycle and reports how many trials landed in the window.

THE FIX IS IN THE READER, BECAUSE IT CANNOT BE IN THE WINDOW. Two files cannot be one syscall.
`head` is now documented and used as a durable LOWER BOUND, and `published_head` walks forward
from it over every version NAME that exists. A loop and not a one-step lookahead: `advance_head`
writes an absolute value with no lock, so a descheduled writer can make the pointer regress past
two later ones. The step predicate is `link(2)`'s exactly -- any name, not "a readable version"
-- so `update` mints a number no name is taken for and the wedge is unconstructible rather than
recovered from. A taken name that is not a readable version is REPORTED, in `unreadable` and by
`get_agent`, instead of being stepped around.

ADOPTED, NOT DISCARDED, and the caller-visible consequence is written down in all three places:
an update interrupted by a crash MAY have landed. No ordering avoids that -- the same crash one
line later would have moved the pointer -- and it is exactly the case the fence documentation
already prescribed `get_agent` + `config_digest` for. Adoption makes that recovery truthful.
Discarding would have required unlinking a published file, which makes `missing_version`'s own
"a version is never removed" false for a version a session may have pinned, and no reader can
tell a crashed writer's orphan from a live writer's version published microseconds ago.

AFTER: 45 trials, 0 broken, 17 landing in that window; and 40 crash-and-restart cycles on one
store, 179 versions published, 0 whose bytes changed, 0 that stopped reading.

THE REST OF THE MODULE'S CRASH AND CONCURRENCY CLAIMS, AUDITED BY MEASUREMENT. `create`
publishes whole or not at all: 40 SIGKILL trials, 0 half-made records. Publication is atomic
and exclusive: re-measured at eight writers on one fence, 30 rounds, exactly one new version
file. `(agent_id, version)` denotes one byte-string for all time: 179 versions across 40 crash
cycles, 0 changed. And "durable" is now scoped rather than assumed -- `sync_all` really is
`fcntl(F_FULLFSYNC)` on this target (rustc 1.88.0, `library/std/src/sys/fs/unix.rs:1212`), while
`sync_parent_directory` discards its result on purpose, so the barrier covers the version bytes
and not the directory entry naming them.

Five checks, each deleted, its target run red, restored byte-exact:
  get/summarize/update reading the raw pointer   -> 3, 3 and 3 of the new tests redden
  the forward walk made a one-step `if`          -> the regressed-pointer test reddens
  the step predicate narrowed to a valid version -> the taken-name test reddens, listing the
                                                    record healthy at version 1 with unreadable []
`link(2)` swapped for an overwriting `rename` reddens the eight-writer test. The reader/writer
race test is NOT deletion-observable on this change and says so in its own doc comment.

The harness lives in `tools/crash-harness`, detached from the workspace with its own
`[workspace]` table: `per_binary_tests.sh` enumerates workspace targets and prints "every one
of the N test targets passed", and five binaries that assert nothing would have widened that N
while adding zero cases.

Instance TWENTY-SEVEN of the bug class; counters in `crates/protocol/src/v1.rs` and
`crates/service/tests/agent_resource.rs` moved, `docs/current-state.md` Sec. 9.21 records it.
`docs/agent-resource.md` Sec. 2.3 still said step 3 was a `rename` two commits after it became
a `link`; fixed, with rows 27 and 28 added to the design-claims table.

62 targets in isolation, 1103 cases, 0 ignored; the one red is `pseudomux-e2e/full_stack`,
which needs `PMUX_E2E_TYPESCRIPT_DIST_DIR` and which `per_binary_tests.sh` warns about by name
up front. Residue audit passes.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c87"></a>

### 87. A teardown arm that spelled "no handle yet" the same as "no process ever", and the launch whose late handle it left nobody accountable for

*2026-08-08*

`````text
`Pool::mint` releases the lock across `InstanceHost::mint`, so for the whole
width of a launch the instance is `Warming` with `handle: None`. `shutdown`
drains `Warming`, and `destroy`'s only handle-less arm read that absence as
"no process was ever launched, so the boundary is empty by construction" --
a comment defending a predicate that cannot tell the two cases apart. The
child was launched into a root the pool then erased under it, the slot was
released, and `census` reported `leaked: 0`. Reproduced at
`mints=1 clears=0 destroys=0 leaked=0 trees=[]`.

`Instance::mint_in_flight` is the bit that separates them, taken immediately
before the launch and cleared by whichever outcome arrives -- a handle
supersedes it, and a failure carries `HostFailure::process_may_survive`,
which the host measured and this only ever meant "nobody has measured yet".
Teardown is its only reader: a destroy that finds it set cannot prove the
boundary empty, so it leaks the slot and keeps the tree rather than erasing
a root a child may be writing into.

The handle that arrives after its slot is gone is no longer dropped on the
floor either: it is spent closing the child it names. Not to un-leak the
slot -- the root is retained because a live process may have been writing
into it, and reaping the process now does not make that root pmux's to
delete.

The double's turn gate became a `Gate` type rather than growing a second
copy of its three fields for mints.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c88"></a>

### 88. The one read of the instance map that indexed where its neighbours ask, on the resume path of a clear nobody waits on

*2026-08-08*

`````text
`finish_turn` re-locks after `InstanceHost::clear` and indexed
`state.instances[&slot]`. `spawn_clear` runs it on a task nobody joins and
`shutdown` drains `Clearing`, so the slot can be reaped, its tree erased and
its entry removed while the clear is still in the host. Reproduced:

    panicked at crates/service/src/pool/mod.rs:1122:52: no entry found for key

`destroy` had already been asking the fallible question three lines of the
same file away. The absent key is now given a meaning rather than a
`get(..).ok()`: an entry leaves that map only through `destroy` proving the
process reaped or `leak` subtracting the slot, and both have already
reported themselves -- so there is nothing to return to service and nothing
to tear down, and the clear says so and stops.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c89"></a>

### 89. A doctor that exited 0 healthy while holding both operands of the refusal the next ask returned

*2026-08-08*

`````text
On this host: installed Claude Code 2.1.223, sole promoted profile 2.1.220,
`RequireTested` -> `unsupported_claude_version`. `pmux doctor` validated the
Claude executable by exec-bit only and reported `healthy`; every `pmux ask`
was then refused. The worst pairing a first-time user can get, and both
numbers were already inside the daemon.

The compatibility layer's own doc defended not comparing them -- "nothing
here knows which Claude the pool will launch" -- which is false of a daemon
that holds `pool.config().claude_executable`. `NativeService::admit_pool_claude`
now runs that executable and asks the registry the question a mint asks, so
the layer reports `faulted` naming the installed version, the cells that did
not admit it, the refusal a mint would get, and `--tested-claude-profile`.
`pmux doctor` inherits the verdict through the fold it already runs over the
layers rather than growing a second copy of the admission rule.

Derived, not restated: the policy, terminal identity and cell come from the
constants `stateless::launch_request_for` writes into a mint request, and
the refusal comes from the same `resolve` plus
`require_tested_for_minified_cell` pair `start_session` runs.
`claude_version_of` is `detect_claude_version`'s own body, split so the probe
spawns the version query exactly the way a launch does.

Three states and not a bool: an executable that could not be asked is
`not_established` -- unproven, exit 1, never healthy -- because "refused" and
"unknown" are different operator problems.

Nothing is promoted and `RequireTested` is untouched. VERIFIED LIVE, three
daemons on this host: Path B on 2.1.223 exits 1 unhealthy naming the escape
hatch; the same daemon with `--tested-claude-profile` for 2.1.223 exits 0
exercised; a Path A daemon with no pool exits 0 nothing_to_exercise.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c102"></a>

### 102. A SIGTERM window whose whole warm mint ran at the kernel's disposition, and the recovery chain that grew by one restart per tree it erased and two per tree it abandoned

*2026-08-09*

`````text
`shutdown_signal()` was an `async fn` handed to `serve_until` in argument
position. An `async fn` runs none of its body until the future is first polled,
and `signal(SignalKind::terminate())` is the call that installs the disposition,
so it ran after `NativeService::start` had minted the entire declared warm set.

Reproduced at HEAD on the shipped release binaries against real Claude 2.1.226,
`--path-b-warm claude-sonnet-5/low=3`, SIGTERM 2.6 s in:

  exit 143 | trees 0/0 1/0 2/0 | socket PRESENT | daemon log 1 line

The one line is the raw startup `writeln!`. Every `tracing` record, including
`pmuxd protocol v1 listening`, died in the `tracing_appender::non_blocking`
buffer whose `WorkerGuard` never dropped.

The half `docs/2.1.226-acceptance.md` §6 explicitly did not characterise -- "I
did not characterise that distribution" -- is the one that changes the severity.
The chain is EXPONENTIAL. A failed start erases the ONE tree it collides with
(`mint_roots` refuses, `abandon_mint` destroys) and abandons every tree it had
already minted, because `bin/pmuxd` drops the `NativeService` a failed
`NativeService::start` never handed it. So

  L  ->  (L \ {min L})  union  {0 .. min L - 1}

which is `2^w - 1` restarts for a warm set of `w` -- 32,767 at the owner's cap
of 15. Every transition of that recurrence was observed, not assumed: the three
trees above took restarts 1..7 refusing, in the order
{0,1,2}->{1,2}->{0,2}->{2}->{0,1}->{1}->{0}->{}, with restart 8 serving.

Three repairs, one per consequence.

The handlers are installed before anything is minted, as a value rather than a
call in argument position, and both SIGTERM and SIGINT are registered eagerly.
Tokio buffers a delivery after the handle exists, so a signal arriving mid-mint
is not lost -- `serve_until` sees it on its first poll. What this deliberately
does NOT do is cancel the mint: main holds no `NativeService` until start
returns, so racing that future orphans exactly the trees and children
`start_pool`'s own comment says must stay accountable. Disclosed and measured:
signal-to-exit went 18 ms (a kernel kill) to 4,560 ms (two more mints plus a
full teardown).

`NativeService::start` drains on a `start_pool` failure. `start_pool` already
published the pool before minting, with a comment naming this hazard; nothing
used the handle. Draining turns the recurrence into `L -> L \ {min L}`:
three planted trees, three refusing restarts, measured.

The refusal names the situation and not only the rule. It said "the pool never
adopts a tree it did not create" and now also says a previous daemon did not
shut down cleanly and that this mint erases that tree as it fails -- a promise
about what pmux DID, so it gets a test that plants a tree with a file in it and
asserts the disk. `run_server` also logs the startup failure, which reaches the
file now that the guard drops.

At the fix, same harness, same three-instance warm set: exit 0, no tree, socket
removed, log carrying both `pmuxd protocol v1 listening` and `pmuxd stopped`,
and the next start served with no refusal at all.

Each new test proved red before it was green. Restoring the pre-fix ordering
turned the e2e red naming `signal: 15 (SIGTERM)` against `Some(0)`; removing
`abandon_mint` from the refusal path turned both pool tests red, one on the
disk and one on the mint count; flattening the refusal message back to the rule
alone turned the message test red. Each was restored.

The e2e test aims its signal at the epoch-tree count rather than at a clock,
and `ping` is documented as the wrong probe: `bind_socket` binds before
`NativeService::start`, so a client's connect lands in the backlog and the call
BLOCKS THROUGH the window and returns Ok on the far side -- measured, three
iterations, the third answering Ok with the tree count still 0.

`cargo test --workspace`: 69 test binaries, 0 failed. `cargo test -p
pseudomux-service pool::` 90 passed, `--test path_b_pool` 46 passed (44 before),
`--test minified_cell` 22 passed. The new e2e cell runs under the existing
`--include-ignored` lane. fmt/clippy clean outside vendor; `gate-a-residue.sh`
passed and every scratch harness was removed.

Not established: the behaviour under SIGKILL, which leaves the same residue by
definition and which the drain cannot help; and the `w = 15` end of the chain,
which is derived from a recurrence whose every step was observed at `w = 3`
rather than run.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c139"></a>

### 139. A service no unit test could build because its runtime needed a real sidecar, the twenty-one survivor rows that cost, and an entry-path scan that read the new test file as production

*2026-08-11*

`````text
`NativeService` held an `Arc<PrivateRuntime>`, and a `PrivateRuntime` cannot
exist without a real `pmux-rmuxd` sidecar, a real launcher socket and a
completed rmux handshake. So no fast test could construct one, and the last
full-scope mutation run recorded exactly what that costs: 22 survivor rows whose
`closeable` was `seam`, holding the completion proof, three generation fences,
all three clauses of the idle reaper, the pool-disclosure filter in `diagnose`,
the minified cell's `RequireTested` admission, `shutdown`'s first-error rule and
the clear deadline's domain. `wait_for_turn`'s safety guard read as `<` answers
`daemon_lost` on the FIRST poll of every turn -- the whole of Path B failing on
its happy path -- and the suite stayed green.

`SessionRuntime` is the eight methods `native.rs` actually calls on its runtime,
taken from the call sites rather than from the type; `PrivateRuntime`'s inherent
copies were deleted rather than kept beside it, so each is stated once, and
`runtime.rs` is outside the mutation gate's FULL_GLOBS so the seam adds no
mutants to the measured set. `ScriptedRuntime` is the double, and every one of
its methods refuses what no test scripted, because a double that answers
everything plausibly makes every guard above it pass whatever the guard says.
One test scripts a terminal instead of letting the refusal stand, so the
refusals are falsifiable rather than assumed.

Twenty-one rows move ACCEPTED -> KILLED. Each was proven by applying its own
mutation, watching the named test go red, and restoring; a filtered
`cargo mutants` run over the same functions then re-tested them as 37 mutants,
25 caught, 1 missed, 11 unviable, receipt in
`evidence/mutation-filtered-run-native-seam.json` with the digests of the three
files it graded. Three of the tests needed an interleaving rather than an
assertion: two fences read the session map only on the far side of an `await`,
so a gate holds a terminal's close open while a successor generation is
published, and `wait_for_turn` is held open across the waiter's first poll
because a turn that has already published is answered above the guard.

The one survivor is `<impl Drop for SessionLifecycle>::drop -> ()`, now
EQUIVALENT with the argument written out: the empty body still drops both fields
in the order the body itself uses, and dropping the sender wakes the same
`select!` arm the send wakes. One row stays OPEN and is not this seam's --
`RmuxTerminalControl::interrupt`'s `<` -> `<=` differs only when the clock reads
exactly the deadline instant, so it needs an injectable clock in `driver_io.rs`.

Two instruments were wrong and are fixed. The two `MatchArmGuard` rows on
`shutdown` are keyed at the arm that reports a private-runtime failure while
their reason described the close loop above it; both guards now have a test. And
the differential entry-path test cut each source file at its inline `mod tests
{` -- a rule with no file-tree form -- so it read `native/tests/seam.rs` as two
new routes into admission; it now excludes the same module in its directory form
and checks the exclusion instead of trusting it.

`recorded_at` is stale for `native.rs` until a full-scope run is re-recorded, so
the done-gate reports criterion 1 NOT MET on drift. That is the price section
8.1 said closing these rows would cost.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

---

## D. Isolation and containment

**What a launched cell may reach: environment inheritance, config roots, MCP, containment predicates, credentials, the agent resource.**

A stateless cell is only stateless if nothing crosses into it and nothing escapes. The most expensive defects here were inherited environment variables, and the most instructive is a retraction: a claim that no MCP server is spawned was measured correctly by a descendant-process inventory, and the sentence built on it -- that a flag was no longer load-bearing -- was about an HTTP endpoint that inventory structurally cannot see.

10 entries.

<a id="c4"></a>

### 4. Agent profiles, --dangerously-skip-permissions, and a value-enum drift fence

*2026-07-27*

`````text
Answers a real product question: spawning the agent you want should not mean
retyping seventeen flags. Two gaps blocked it.

--dangerously-skip-permissions was unreachable. Not forbidden -- absent from a
closed two-entry allowlist (SAFE_EXTRA_FLAGS = ["--debug","--verbose"]), so it
failed as "not in the v1 allowlist". Note PermissionMode::BypassPermissions is a
permission-mode *value*, a different thing. Now a typed variant,
PermissionMode::DangerouslySkipPermissions, wire value
"dangerously_skip_permissions".

The argv mapping is a deliberate special case. Every other variant emits the pair
["--permission-mode", "<value>"]; this one emits the single flag
--dangerously-skip-permissions, because no --permission-mode value exists for it.
Implemented as a wildcard-free PermissionModeArgv::{Pair,Single} match so a
future variant is a compile error rather than a silent omission, and the
byte-for-byte argv test now pins both shapes.

Because the flag disables Claude's own safety prompts, every turn of such a
session carries a dangerous_permission_bypass warning in TurnResult.warnings --
on the cancelled path as well as the normal one. Not a protocol change:
ProtocolWarning.code is an open string domain. It surfaces through
turn_completed and --output ndjson, and deliberately not as EventPayload::Warning
(that stream carries only engine warnings; untested_compatibility_profile behaves
identically).

Zero configuration persistence existed -- no config loading anywhere in crates/
or bin/. Adds client-side agent profiles: crates/client/src/agent_profile.rs plus
--agent/--agent-file with PMUX_AGENT/PMUX_AGENT_FILE fallbacks. Profiles are
CLIENT-SIDE and never a server registry: daemon and clients run as the same uid,
so a registry adds zero enforcement while making the daemon stateful and breaking
the property that child argv is a pure function of the request. The contrast with
the tested-profile registry is the argument -- that one is correctly server-side
because it carries an operator's assertion about reviewed evidence, and a caller
must never be able to claim a cell is tested. Evidence admission belongs to the
operator; preferences belong to the caller.

cwd is never expressible in a profile. It is the most consequential parameter,
and a config file that silently redirects where an agent operates is exactly the
ambient resolution this codebase refuses everywhere else. No discovery either --
explicit path plus one env fallback, no XDG search, no upward walk.

Composition is fail-closed: scalars replace and absent inherits, lists append
parent-first, extends is one chain bounded at depth 4 with cycle detection, JSON
null is a parse error, unknown and per-invocation keys are rejected by name, and
reserved-but-unimplemented values are rejected at expansion rather than as a
later daemon error. The loader adds two checks the service does not perform: an
inline document forces the profile file itself to be owner-only, and referenced
config files must satisfy mode & 0o077 == 0. require_env asserts presence without
ever reading a value and warns when a name would be stripped by the subscription
auth policy or the transparent terminal profile.

Also closes the last silent-drift channel in the public surface. The seventeen
nested value enums were pinned by nothing; the manifest covered only methods,
results, events and error codes. All seventeen are now in
tests/conformance/v1/manifest.json under value_enums with both-direction
exhaustiveness assertions in Rust, TypeScript and Python. This also re-sourced
duplicated inline validator literals in both clients -- each carried a local
SESSION_STATES plus nine inline arrays -- so the runtime validators that would
have hard-rejected a conformant server are now transitively manifest-pinned.

Gate A re-run on the shipped tree: 75/75, 0 failed, driver exit 0. Receipt
evidence/gate-a/receipt-20260727-agent-profiles.json, sha256 aeb39a9e..., source
digest 32519f39... over 861 files, unchanged across the run. The earlier receipt
is superseded and marked invalid for this tree. Workspace tests 519 -> 544;
TypeScript 48 -> 49; Python 32 -> 34; fmt clean; clippy -D warnings zero.

Three of the eight release binaries (pmux-launcher, pmux-rmuxd, pmux-test-claude)
are bit-identical to the previous candidate from an independent build in a
different validation root -- evidence the build is deterministic for unchanged
crates.

The re-run took two attempts, 73/75 then 75/75, on two setup failures and no
product failure: the TypeScript dist was pre-staged so the manifest's own prepare
cell found a non-empty root, and nine __pycache__/.ruff_cache findings were left
by the implementation work. The residue gate catching real generated-output
pollution is the gate working.

C8 remains open and is not softened by this receipt. The flaky /bin/ps ECHILD
cell passed again here, which is three consecutive green runs since its single
failure with nothing diagnosed or changed -- accumulating green runs is the
failure mode that entry exists to name.
`````

<a id="c5"></a>

### 5. Strip CLAUDE_CODE_CHILD_SESSION: nested-marker inheritance broke every turn

*2026-07-27*

`````text
Root cause of the Gate B failure, and it was neither Claude nor the terminal
geometry. A parent Claude Code session exports CLAUDE_CODE_CHILD_SESSION=1 to
mark a nested invocation. pmux inherited it, so the child behaved as somebody
else's subordinate session: it still rendered a composer, pmux still located the
cursor-correlated editor and reached `ready`, the single bracketed paste and the
one Enter still landed, and run_turn still returned acceptance -- but the child
never wrote a transcript of its own, so the exact post-arm typed-user row could
never appear and every turn died at `awaiting_prompt_ack` with TurnTimeout.

That is why no transcript file existed for any pmux session UUID while unrelated
sessions written in the same window were present. The absence was the symptom,
not a locator or parser defect.

spec.md:378 already promised that launch removes "nested/remote Claude markers".
SUBSCRIPTION_AUTH_KEYS carries only auth/provider keys and TRANSPARENT_PREFIXES
covers CLAUDE_AGENT_SDK_ and CLAUDE_CODE_SDK_ but not this one, so the spec
promised a guarantee the code did not deliver. Adding it to
TRANSPARENT_EXACT_KEYS delivers it, and extends the existing parent-behavior
strip test rather than adding a parallel one.

How it was isolated, at zero live-attempt cost:
- The earlier hypothesis was that Claude 2.1.220 changed its TUI or needed
  --dangerously-skip-permissions. Both were refuted by direct runs, which spend
  Claude usage but never touch the immutable ledger.
- Claude 2.1.215 -- the exact version behind the 24 successful turns of
  2026-07-19 -- failed identically, which ruled out a Claude-side regression and
  pointed at pmux or the environment.
- A minimal environment completed the full lifecycle on both 2.1.215 and
  2.1.220: submitting -> awaiting_prompt_ack -> prompt_acknowledged -> running ->
  logical_message -> terminal_candidate -> draining -> ready -> completed.
- Bisecting the inherited variables cleared CLAUDE_CODE_SESSION_ID, then
  reproduced the hang with CLAUDE_CODE_CHILD_SESSION alone, whose removal alone
  fixes it.
- Verified in situ: from the contaminated shell with the variable still set, the
  turn now completes.

No test could have caught this. The deterministic suite drives pmux-test-claude,
which does not read the variable, so all 544 tests pass either way. It is
reachable only through a real Claude child, which is what Gate B exists to
exercise -- and the one consumed live attempt is what surfaced it.

Ordinal 30 remains consumed and correctly recorded; 70 attempts remain.
Workspace 544 passed / 0 failed, clippy -D warnings zero, fmt clean.
`````

<a id="c6"></a>

### 6. Allowlist the launch environment; one authoritative policy in protocol

*2026-07-27*

`````text
Structural answer to the CLAUDE_CODE_CHILD_SESSION bug. That variable was the
FOURTH nested-Claude marker added to a denylist (after CLAUDECODE,
CLAUDE_CODE_ENTRYPOINT, CLAUDE_CODE_REMOTE), and the one that cost a live
attempt to find: inheriting it made the child render a composer, accept the
paste and the Enter, and never write a transcript of its own, so every turn died
at awaiting_prompt_ack. A denylist cannot be completed. Unknown-means-denied can.

The inherited snapshot is now filtered by an auth-policy-aware allowlist. Order
is allowlist(snapshot) - unset + set - policy_removals + profile_changes. The
denylist is KEPT and still runs afterwards, so a name that is both allowed and
explicitly forbidden is still removed. Provider routing survives under
AuthPolicy::Inherit and is denied under Subscription; the existing test asserting
ANTHROPIC_API_KEY survives under Inherit passes unmodified.

The load-bearing test proves the mechanism rather than the data:
CLAUDE_CODE_CHILD_SESSION is denied by the allowlist with it REMOVED from the
denylist, alongside a deliberately invented CLAUDE_CODE_NOT_INVENTED_YET.

An adversarial review measured the real blast radius on this machine -- 78
variables in, 10 kept, 68 dropped, of which only 5 were previously removed -- and
found the allowlist was enforced in one place but believed in three others. Four
blocking gaps, all closed:

- The escape hatch did not exist. claude_launch.rs justified a tight list by
  calling `set` "the caller's explicit channel", but nothing could populate it:
  the CLI built requests from exact_environment_snapshot() alone. Added --env,
  --env-passthrough and --unset to both pmux and claude-p. --env-passthrough
  forwards by NAME so a secret never reaches ps output.
- pmux probe could not show drops, though the code claimed completeness was
  "what probe needs to stay an honest audit surface". It now reports dropped
  names only, on both the dry-run and --launch paths, with values still redacted.
- require_env's strip warning had gone blind: it knew only the denylist, so
  require_env ["GITHUB_TOKEN"] passed the check and was silently dropped at
  launch. It now knows the allowlist and names --env-passthrough as the fix.
- spec.md:392 claimed "ordinary caller configuration remains intact unless
  explicitly unset", which is no longer true as a class.

Empirically-found coverage gaps: NIX_SSL_CERT_FILE (this box exports it and not
SSL_CERT_FILE, so the allowlist was dropping the only CA bundle present -- Nix
users would have lost TLS), GIT_SSH_COMMAND and the GIT_CONFIG_* family, and
ANTHROPIC_ narrowed from an unbounded prefix that reintroduced the very
open-ended inheritance this change exists to eliminate.

Two ordering traps found by review rather than by tests. remove_tmux_shim_from_path
read TMUX_PROGRAM from the merged map, but TMUX_PROGRAM is denylisted precisely
so it cannot reach Claude while being needed AS INPUT to compute the PATH prune;
a naive filter deletes it first and silently leaves the shim on the child's PATH.
And crates/e2e/src/lib.rs carries an independent environment oracle that omitted
CLAUDE_CODE_CHILD_SESSION, so the full-stack lane never exercised the fix end to
end. Both closed; the oracle stays an independent list rather than importing the
policy it checks.

Policy is now defined once, in crates/protocol/src/v1/launch_environment.rs,
carrying the tables AND the predicates -- shared data with divergent matching
semantics still drifts. Three mirrors deleted (service, client, CLI) and two
source-text drift fences deleted as pointless once there is one copy. The home is
argued in the module doc: a client that must predict which variables the daemon
drops is exercising a v1 clause, not reaching into a service implementation
detail. All three copies were verified byte-identical before merging, so no
silent policy change slipped in.

Sandboxing considered and deferred entirely, recorded in docs/current-state.md: a
microVM handed the parent's environment reproduces this exact hang, so it
addresses a different problem; it would put transcript authority behind a
virtio-fs mount, the PTY behind a proxy layer, and reduce the process-boundary
proof to an assertion about a VM's lifecycle. The correct isolation story is
running the whole stack inside a sandbox, which tools/linux-docker already
demonstrates.

Gate A re-run on the shipped tree: 75/75, 0 failed, driver exit 0. Receipt
evidence/gate-a/receipt-20260727-env-allowlist.json, sha256 65912cf8..., source
digest 0c61ae1e... over 864 files, unchanged across the run. Workspace tests
544 -> 571. fmt clean, clippy -D warnings zero, ruff clean, residue clean.

Known limitation: probe --launch reports a CLIENT-computed removal set. Protocol
v1 carries no field for ResolvedClaudeLaunch::removed_environment_keys, so the
daemon's own answer is unobservable to any client. The two agree only because the
policy is now single-sourced.
`````

<a id="c13"></a>

### 13. Reject agent-team markers that reach the child, not ones merely present

*2026-07-28*

`````text
validate_environment ran before any filtering, so it refused a launch whenever
the caller's SNAPSHOT mentioned an agent-team or teammate name -- including
CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS, which is in TRANSPARENT_EXACT_KEYS and
which the transparent profile already deletes. pmux therefore refused to start
from inside any Claude Code session with agent teams enabled, which is the
ordinary environment for developing pmux itself, over a name the child was
never going to see. Reaching Gate B at all required stripping it in the harness.

The check now runs on the resolved map handed to the child, after the allowlist,
the caller patch and the profile removals. Two rules, because they are different
situations:

  - an AMBIENT marker in the snapshot is allowed only if the policy provably
    strips it; the guard tests delivery, not ambience.
  - an explicitly-set marker is still refused even though the profile would
    strip it anyway. The caller stated an intent that cannot be honoured, and
    silently discarding an explicit instruction is worse than refusing it.

I found that distinction only because the first version -- checking the resolved
map alone -- broke the existing assertion by quietly accepting a request it then
ignored.

Two properties are now structural rather than remembered: the guard reads the
exact map the child receives, so a future policy change that stops stripping a
marker re-arms the refusal automatically; and the new test asserts its own
premise, failing loudly with the reason if the key ever leaves
TRANSPARENT_EXACT_KEYS.

Also pins a boundary the --prompt-file terminator fix created: the strip runs
before the length check, so the terminator is not charged against the caller's
byte budget. A source carrying MAX_PROMPT_BYTES of content plus its terminator
is accepted and delivers MAX_PROMPT_BYTES; one byte of real content past the
budget is still refused. process_boundary.rs's exact-limit expectation moves
from MAX to MAX-1 for an all-newline source, for the same reason.

576 passed, 0 failed, fmt clean.

NOTE: this message was rewritten. The original contained an unescaped backtick
pair around a word, which zsh expanded as command substitution and interpolated
the shell environment -- including live credentials -- into the commit message.
No tracked file ever contained them and nothing was pushed, but the affected
keys were rotated.
`````

<a id="c46"></a>

### 46. Path B is reachable: pmux mints every resource and the caller names none

*2026-08-05*

`````text
`Request::RunStateless` now reaches `Pool::run` instead of an honest refusal.
`crates/service/src/stateless.rs` is the other half of `pool::host`'s seam --
the half that touches a child, a TUI, a transcript and the registry.

`launch_request_for` is a free function of `MintSpec` and nothing else, which
is the form in which "pmux mints every resource" is checkable: there is no
caller string in scope to leak. It fills identity `New { session_id: None }`,
cwd and `config_isolation.root` from the slot tree, model and effort from the
class key's own table entry, `DontAsk`, `denied_tools: ["*"]`, replace-mode
system prompt, `RequireTested`, `Minified`, and an EMPTY environment in all
three directions -- an inherited snapshot is how HOME reaches a child, and HOME
is the fallback source of a config root.

`SessionOwner` splits the registry's one resolver into two, each admitting
exactly one owner and neither having an admits-everything value. A pool
instance is refused to every session-addressed wire method with the byte the
caller gets for a session that never existed, and the owner check runs BEFORE
the generation fence: a stale-generation body names the session, so answering
it would rebuild the oracle the owner check removes. The tombstone
short-circuit is read only for the owner that could have produced it, for the
same reason. The generic idle reaper declines pool sessions at its own
enumeration rather than relying on `expire_idle` refusing -- "the call I made
was rejected" and "I declined to make it" are different statements, and only
the second survives a second reaper.

`HostTurn::sidechain_rows` became `Option<usize>`. The only host that exists
cannot count rows: `TurnResult` publishes the sidechain's tokens and never its
row count, and re-reading the transcript would steal the actor's cursor. Under
the old `usize` such a host had to report `0` -- asserting a fact it never
established, which is the defect this codebase keeps finding. What `None`
costs is now written down: `Pool::commit`'s usage check is unaffected, and the
residue is a sidechain row that carried no usage at all.

Front ends. `pmuxd --path-b-parent` is the enable switch and every other
`--path-b-*` flag is an ERROR without it, checked against what the operator
TYPED rather than against a value differing from a default -- `--path-b-pool-size 15`
is indistinguishable from the default by value and its author is exactly who
needs telling. Refusals happen before the socket is bound, so an operator error
leaves no socket, runtime directory or sidecar behind. `pmux ask --model
--effort PROMPT|--prompt-file` and the MCP `run_stateless` tool carry model,
effort and prompt, and no third thing.

Two defects the wiring turned up. The client's request-timeout table had a
`_ => configured` arm, so a stateless call got 45 seconds against a cold mint
that pays a TUI launch before the model is asked; it now widens like `RunOnce`.
And `shared_manifest_matches_the_closed_v1_surface` compared the manifest
against three string literals, so it passed with the manifest two methods short
of the surface -- the "closed v1 surface" it checked was a copy of the manifest
in a different syntax. Both lists are now derived from `Request`,
`ResponseResult` and `EventPayload` through a wildcard-free match, and the
Python and TypeScript clients carry `run_stateless` rather than a pin that
would have to be widened by hand.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c63"></a>

### 63. An agent the caller pins by version, and a corpus that covered eleven of twelve methods

*2026-08-06*

`````text
DESIGN ONLY. `docs/agent-resource.md` is a build input; nothing here is implemented and no
first-party code changed.

THE SURFACE IS NOT THE MESSAGES API. That API is stateless with CLIENT-HELD history -- the caller
resends `messages[]` every call. Path B is stateless with NO history: `/clear` runs between turns and
the whole isolation argument depends on the transcript being abandoned (`v1.rs:2431-2436`). Path A's
history lives in a real TUI and its append-only JSONL, which is the sole completion authority and
cannot be reconstructed from an array. A `messages[]` parameter would promise continuity neither path
can honour -- the bug class at the API layer. The surface that DOES match is Managed Agents: a
persisted, versioned config; sessions that pin a version. pmux already has sessions, turns and an
event stream; what it lacks is the agent-as-resource split.

IT CONTRADICTS A LIVE INVARIANT AND SAYS SO. `docs/spec.md:664` reads "pmux has no server-side agent
registry and MUST NOT grow one". §4.8 rests on two arguments. The first -- same uid, so a registry
adds zero enforcement -- SURVIVES, and the design concedes it completely: an agent is not a security
boundary, and §6 refuses several attractive features precisely because they would only make sense if
it were. The second -- argv must be a pure function of the request -- is answerable only by a specific
shape: the version is REQUIRED in the request, a stored version is immutable, resolution is a pure
function producing exactly the inline DTO, and the resolved config is echoed with a digest. Without
version pinning it is false and I would not build it. §4.4's wording has to move with it, and that is
an owner decision, not an edit I made.

THE ONE RULE EVERY FIELD CLASSIFIES UNDER: an agent may NARROW what a session may name; it may never
NAME a resource on the session's behalf. So `cwd` stays per-session -- `LiveResourceClaim::directories`
(`native.rs:3393-3399`) enumerates it as one of exactly two directories a session BINDS, and leak 7's
third shape was an intruder cwd standing on a live cell's config root -- but an agent may carry
`containment.workspace_root`, which bounds the cwd and never supplies one, is composed with AND
against `admit_bound_resources` so no value of it can widen admission, and is tested with
`one_directory_contains_the_other` rather than a fresh `starts_with` that symlinks and
`/tmp`->`/private/tmp` already defeated once. `config_isolation` gets the same treatment plus a reason
of its own: a root has a seed disposition, so an agent that NAMED one would make an agent id a
contention key. `environment.snapshot` is deleted from the agent type rather than documented as
must-be-empty, because that note is the bug class.

NO NEW ERROR CODE, AND NO MERGE SURFACE. Both shipped clients hard-reject unknown codes
(`client.ts:309`, `client.py:1074`), so a new one is a three-language lockstep release; `InvalidConfig`
is honest for a missing agent and `IdConflict` already means exactly "your fence does not match"
(`actor.rs:1097`). The update fence is REQUIRED, not optional as in CMA, for the reason
`ClearSessionRequest::expected_transcript_session_id` gives verbatim. Inline and agent-reference are
mutually exclusive and the conflicting-field set is DERIVED by intersecting serialized leaf paths --
CMA ships an `agent_with_overrides` whose `effort` is accepted and silently ignored, which is instance
twenty's shape in the reference API, and a hand-listed conflict set is instance nineteen's.

THE BUG CLASS, INSTANCE TWENTY-ONE, IN THE SHARED CORPUS. `tests/conformance/v1/README.md:16` says
`golden.json` "contains one complete request/result pair for every method". MEASURED against the
manifest it is pinned to: eleven of twelve. `run_stateless` -- the whole of Path B, the method
`pmux ask` reaches, the only producer of `StatelessResult` -- has NO golden pair in any of the three
languages, so it is the one method/result pair no byte-exact cross-language frame pins, while both
clients implement it and both validate the result against no shared vector. The guard cannot see it
because it compares the corpus to a NUMBER: `v1_golden.rs:520/:553/:554`,
`golden-conformance.test.mjs:214` and `test_golden_conformance.py:224` are three hand-written copies
of `11`, none derived from `manifest.methods`. The literal freezes the corpus at the size it had the
day it was written -- deleting an entry reddens it, failing to add one does not, which is exactly how
a method APPENDED to `Request` slips through. All eight golden tests are green today. This is the same
defect the manifest checker in the same directory already fixed for itself with an exhaustive `match`
(`v1_conformance_vectors.rs:126-135` records the history); the fix was applied to one file and not the
other. NOT FIXED HERE -- it touches the corpus and all three client suites, it is not the agent
resource, and it wants its own commit with a per-language mutation proof. It is written up as a
PRECONDITION: derive the count first, or appending four methods ships a corpus covering 11 of 16 with
a green suite, and this design will have produced its own instance twenty-two.

WHAT I REFUSE TO BUILD. Path B must not gain an agent reference -- `RunStatelessRequest`'s own doc is
the argument ("a caller who cannot name a resource cannot alias one"), and concretely an agent id
would make the pool's class key `(model, effort, agent_version)` so `--path-b-warm` could no longer
name a class, and would hand a caller the system prompt the cell refuses BY NAME. Also refused:
per-session overrides, `delete`/`archive` on the wire (same uid, so zero enforcement -- §4.8's own
Argument A applied to this design's surface), server-side `extends` (flatten at create instead), any
discovery, and a vault analogue. The client-side profile stays, as an authoring tool.

FOUR DECISIONS I AM BLOCKED ON, listed in §8: amending §4.8/§4.4; the `--agent` CLI collision, where
`--agent`/`--agent-file` already mean the client-side profile and I recommend renaming those to
`--profile`/`--profile-file` with a refusal that names the new spelling rather than a silent alias;
whether the corpus precondition lands first; and whether an agent may carry `environment.set` values
at all.

Verification plan is 18 rows, each with the assertion AND the mutation that must redden it; row 14
reddens today, before any change. cargo fmt clean, clippy clean first-party (the 4 vendor/rmux-server
warnings are pre-existing and unchanged). No daemons started, no temp roots created; residue
self-test passes and no pmux temp roots are present.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c65"></a>

### 65. An agent the request pins by version, a cwd it may bound but never name, and a digest that lied about what it covered

*2026-08-06*

`````text
Protocol, service, MCP and CLI, on one invariant:

  An agent may narrow what a session may name.
  It may never name a resource on the session's behalf.

Every field classifies mechanically under it. `claude`, `environment.set`/
`unset`, `auth_policy`, `terminal`, `lifecycle`, `retention`, `compatibility`
and `cell` move to the agent; `cwd`, `config_isolation` and `identity` stay
per-session; `environment.snapshot` stays STRUCTURALLY, because
`AgentEnvironmentSpec` deletes the field rather than documenting that it must
be empty -- which is also why a caller's snapshot survives the both-modes
refusal with no exception list.

WHY ARGV IS STILL PURE. `docs/spec.md` 4.8 said "pmux has no server-side agent
registry and MUST NOT grow one", and this contradicts it. The uid argument is
conceded in full and written into the amended section: an agent is NOT a
security boundary, because anything it would refuse the caller can send inline.
The argv-purity argument is answered by four properties, and without all four
this would not be worth building: `AgentRef::version` is REQUIRED so there is no
"latest at start time"; a stored version is immutable; resolution is a pure
function run once at the one start door, whose output nothing downstream can
distinguish from an inline request; and the resolved configuration's digest is
echoed on the response. 4.4 now says "a pure function of the request and of the
immutable version the request names", which is a weaker claim, so it is written
rather than glossed.

DERIVED, NOT LISTED. The both-modes conflict set is computed by intersecting the
serialized LEAF paths of a fully populated `AgentSpec` and `StartSessionRequest`
and reducing to the maximal paths all of whose leaves collide -- both fixtures
`..`-free, so a field added to either type moves the intersection and reddens
the assertion against the production list. `Serialize` and `Deserialize` for
`StartSessionRequest` are written out rather than derived, because the derive
cannot answer the only question the refusal needs answered: five launch-policy
fields are non-`Option`, so "omitted" and "sent at exactly the default" are one
value once a request is typed, and a rule stated over equality-to-default
silently accepts `"cell": "full"` beside an agent whose cell is `minified`. The
CLI's own refusal is derived the same way and checked from clap's argument ids,
so a flag added to `start` and dropped from both classes is red.

THE STORE IS HELD TO THE SOCKET DIRECTORY'S BAR. 0700 directories and 0600 files
from birth, passed to `mkdir(2)`/`open(2)` rather than chmod'd after; an
operator's non-private tree REFUSED at boot naming what is wrong and what would
be right; the mode re-checked on every READ, because a version file is read at
`start_session` time and a file widened between boots is one pmux should not
trust; and never a re-permission. The path component is a minted UUID, never
`spec.name` -- `validate_agent_name` admits `..`, `.` and `a..b`, which is fine
for a map key and a traversal for a path component.

CONTAINMENT IS DIRECTED, AND THAT IS A DEVIATION FROM THE DESIGN. The design
said route `workspace_root` through `one_directory_contains_the_other`. That
predicate is SYMMETRIC, so with a root of `/Users/x/proj` it admits a cwd of
`/Users/x` -- and the field promises "every session's cwd must resolve INSIDE".
`claude_launch::directory_lies_within` is the same resolving walk asked in one
direction, and the test asserts the parent case explicitly.

ACCEPTED-AND-IGNORED IS REFUSED THROUGHOUT. `create_agent` runs the service's
own `validate_v1_terminal_support` and `validate_public_start_retention`, so a
`one_shot` retention or a reserved terminal identity is refused where the caller
can still fix it rather than at a launch that never happens; `cell: minified`
with `require_config_isolation: false` is refused rather than overridden; and
`environment.set` may not name any door in `CONFIG_ROOT_ENV_DOORS`, walked from
the service's own table.

AND A DEFECT IN A CHECK I WROTE. `redaction_hides_values_...` asserted that two
specs differing only in a hidden value stay distinguishable and its message
claimed "a digest computed over the redacted spec would collide here". MEASURED
by deleting the check and taking the digest over the redacted form: it does NOT
collide, because two different values digest to two different digests -- the
assertion passed over the very defect it named, and a second attempt that asked
`config_digest` for the expected value failed the same way, because a mutated
digest function moves both sides identically. The check now computes sha256 over
the canonical serialization itself, which is what a caller in any of the three
languages would do, and the mutation reddens it.

`AgentDescriptor::spec` is opaque on the response and typed with `typed_spec`.
The two halves of the wire contract pull opposite ways on an echoed request
body: a request must refuse an unknown field, a response must tolerate one, and
no client in any of the three languages keeps two decoders for one type.

Refused outright, and recorded in 4.8.3: Path B never gains an agent reference
(it would make the pool class key `(model, effort, agent_version)`, so
`--path-b-warm` could no longer name a class, and hands the caller the system
prompt `RunStatelessRequest` refuses by name); no per-session overrides; no
`delete`/`archive`; no server-side `extends`; no discovery; no vault.

Zero new error codes: both shipped clients hard-reject an unknown `ErrorCode`,
so one would be a three-language lockstep release, and every refusal here puts
its actionable half in `details.recommendation`, which `pmux` renders.

The CLI's `--agent`/`--agent-file`/`PMUX_AGENT`/`PMUX_AGENT_FILE` named the
client-side profile and now name nothing: they are `--profile`/`--profile-file`/
`PMUX_PROFILE`/`PMUX_PROFILE_FILE`, and each retired spelling is refused with
the new one NAMED. A silent alias is exactly how a caller reaches for one
feature and gets the other, and the two disagree about the most consequential
thing a launch configuration can say.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c74"></a>

### 74. A guest that was the wrong OS to reach the keychain, and an `os` that would have vouched for Linux

*2026-08-08*

`````text
A research spike on microsandbox for pmux. Analysis only: no Rust, no
dependency, no manifest touched. One new file.

Do not characterise it from its name. It is not a syscall filter of the
seatbelt family; it is a microVM runtime, and on macOS each sandbox gets
"Its own **Linux kernel**, supplied by microsandbox (built from libkrunfw),
not your host kernel" (docs/security/isolation.mdx:13, byte-verified through
`gh api … | base64 -d`, as is every quote this file leans on).

That decides criterion 1. MEASURED on this host today: `.credentials.json`
is absent and `security find-generic-password -a $USER -s "Claude
Code-credentials"` exits 0, so the credential is in the Keychain, and the
candidate's own isolation page closes the only route to it -- "No host PCI
devices, no host sockets". Both escapes defeat the sandbox rather than
serve it: proxying the keychain call hands the guest the operator's OAuth
token, which is the highest-value item in the blast radius path-b.md
already concedes; and microsandbox's placeholder substitution binds a secret
to an ENVIRONMENT VARIABLE, so it is shaped for the API-key auth whose first
two names -- `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN` -- are exactly what
`SUBSCRIPTION_AUTH_KEYS` strips.

The finding nobody had written down is in criterion 2. `CompatibilityReport`
takes `os` from `std::env::consts::OS` (compatibility.rs:312), a COMPILE-TIME
constant of the daemon. Put the child in a per-instance microVM and the
macOS-built daemon reports `os: "macos"` for a child on Linux; the one
promoted profile matches, `tested: true` is published, and a
`transcript_drain_ms` measured over "456 turns in 189 ... transcripts on
macos/aarch64" is applied to a cell nobody has measured. RequireTested
passes, silently, on a cell it does not describe -- the governing defect in
executable form. Whole-stack-in-the-VM fails the honest way instead: `os`
becomes `linux`, nothing matches, and Path B is dead until a cell is promoted
out of 53 committed attempts.

Criterion 4 is the surprise and is recorded as CLEARED. A ~100 ms guest boot
on a ~4.4 s mint is +2.3%, +0.4 ms/turn amortised at the 250-turn cap, 0.02%
of a 1,955 ms Path B turn. Nobody should reject this for being slow. The
pool cost is memory, and it is a sensitivity table over an unmeasured V, not
a result: 1 to 8 of the 15 slots at a fixed budget.

Recommendation: DO NOT BUILD. current-state.md §9.7 Row S1 stands; this is
its evidence, not its replacement. Build instead the thing the spike found
by accident -- pmux fingerprints `sha256` + `(device, inode)` for the seven
binaries it wrote and checks only `is_absolute()` on the one it launches and
hands the operator's credential to -- and measure where credentialed Claude
Code on Linux keeps its credential, which costs no attempts and is the hinge
the whole Linux answer turns on.

§8 lists seven things this document does NOT establish, starting with the
largest: microsandbox was never installed, built or executed here. A 82/83
gate figure in circulation is not in this tree, so the file cites the 80/81
the tree can defend.
`````

<a id="c95"></a>

### 95. A --strict-mcp-config retracted as "no longer load-bearing" on a descendant-process inventory that cannot see an HTTP endpoint, and a 2.1.226 cell that loads the caller's account MCP connector until the flag it was retracted from is passed

*2026-08-09*

`````text
Claude Code 2.1.226 is structurally compatible with everything pmux ships. All
24 argv spellings derived from claude_launch.rs and sensitive_launch.rs are
accepted, including the two hidden ones no --help lists; `pmux start --cell
minified` reached `state: ready`, which is the composer geometry gate returning
Ready; the post-/clear preamble is the same five rows in the same order; the
menu still marks its selection with fg 153 against 246 and no other attribute;
and CLAUDE_SECURESTORAGE_CONFIG_DIR still pins the un-suffixed keychain item.
Nothing moved between 2.1.223 and 2.1.226, and 2.1.220 could not be re-run
because its binary is no longer installed.

What did move predates 2.1.226 and is what the version gate would never have
caught. A minified cell in a pristine private root, with no local MCP
configuration of any kind, loads an account-level remote MCP connector and
makes an outbound call to it -- identically at 2.1.223 and 2.1.226, and not at
all when --strict-mcp-config is passed. path-b.md retracted that flag on
the ground that no MCP server process spawns; a remote connector is an endpoint
and spawns nothing, so the inventory behind the retraction is structurally
unable to observe the case the claim covers. pmux does not pass the flag.

Also recorded: minified.rs names --strict-mcp-config and --safe-mode as part of
a bundle the derived argv set proves pmux cannot emit; driver_io.rs:142-143
publishes session-dependent row indices as a measured version fact and they are
10-13/(11,2) rather than 4-7/(5,2) today; ultracode is now a bundle alias for
xhigh rather than the separate mode the exclusion reason names; the five-row
preamble is 1899 bytes against a stated 1051-1890; and the free corpus holds
zero rows at 2.1.226, so the drain tool's exit 0 means "nothing to check".

The scan that produced the constant list is derived rather than transcribed:
sixteen production MEASURED sites carry a Claude version, where version-drift.md
named four and estimated a dozen. Twelve of the sixteen are the screen and
/clear group and all twelve are re-measured here.

Zero ledger ordinals: 85 consumed, 15 remaining, before and after. The two
background agents an early probe started are accounted for in section 7 --
output null, no transcript, both reaped.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c101"></a>

### 101. A minified cell whose isolation rested on a process inventory that cannot see an HTTP endpoint, and the launch bundle three source files described and one of them was right about

*2026-08-09*

`````text
Reproduced first at HEAD, on the shipped release binaries: `pmux start --cell
minified` against Claude Code 2.1.226 through a shim that logged the child's
argv and appended `--debug-file`. The live argv was

  --session-id … --model sonnet --effort low --permission-mode dontAsk
  --disallowedTools * --system-prompt-file …

and the child's own startup log held 6 MCP lines, including

  [claudeai-mcp] Fetching from https://api.anthropic.com/v1/mcp_servers?limit=1000
  [mcp-registry] Loaded 294 official MCP URLs (legacy)
  [STARTUP] MCP configs resolved in 33ms

in a pristine 0700 private root with an empty `.claude.json`. The same launch
with `--strict-mcp-config` injected by the shim: 2 lines, `resolved in 0ms`, no
fetch, `state: ready` on both arms. After the fix, pmux emits the flag itself
and the measurement reproduces at 2 lines and `resolved in 1ms`.

`docs/path-b.md` retracted the flag as "NO LONGER LOAD-BEARING" because "no
MCP server process is spawned in any configuration". That measurement -- a 50 ms
descendant-process inventory -- was correct and is untouched. It simply could
not test the sentence built on it: an account-level remote connector is an HTTP
endpoint, spawns nothing, and is invisible to a process table. §0.3 rule 5 in
reverse.

`--safe-mode` was weighed and is deliberately still not passed. Nothing it
closes is measured open -- the compatibility session measured user-scope skill
discovery landing on the private root, with 77 of the operator's 78 skills
absent -- and 2.1.226's own help says it also disables custom themes and
keybindings, i.e. it moves the TUI rendering every screen constant Path B's fast
path trusts. The `docs/path-b.md` §11 item 3 probe covered `ready` and one
answered token; it covered no `/clear`. So its three claims were removed rather
than made true: the `minified.rs` module doc, the `StopHookObserved` refusal
that said "--safe-mode did not hold" about a flag that was never passed, and
`measure_transcript_drain.py`'s `ROW_KINDS` premise.

The class, not the instance. The bundle was a paragraph in three files and an
argv builder in a fourth, which is why all three paragraphs could be wrong at
once. `minified.rs` and `measure_transcript_drain.py` now publish the bundle as
data, and `the_documented_minified_launch_bundle_is_the_argv_a_mint_emits`
drives `launch_request_for` -- the function a live mint calls -- through the same
three steps `start_session` drives it through on a real 0700 slot tree, and
compares both lists element for element in argv order. A fifth code-tree file
naming a `MINIFIED_CELL_FLAGS` spelling is refused by name; `docs/` is out of
scope because those are dated receipts, and this commit says so in the test.

Proved by breaking it five ways, each red with its own message, each restored:
the append removed (both tests, naming the missing flag and the argv it was
missing from), a flag dropped from the Rust list, the same dropped from the
Python tuple (both printing left/right), a sixth file given an opinion, and the
cell condition widened to every session (Path A's argv, named).

`cargo test --workspace`: 69 test binaries, 0 failed. `cargo test -p
pseudomux-service pool::` 90 passed, `--test path_b_pool` 44 passed, `--test
minified_cell` 22 passed. ruff --no-cache clean; `gate-a-residue.sh` passed.

Not established here: that the resulting session carries no MCP TOOLS. The flag
was measured for suppression of the fetch, not for tool delivery -- that needs
the ~29,000-token cache_creation oracle and a paid turn. Managed (policy)
settings under /Library/Application Support/ClaudeCode remain outside the
private root and outside this change, which is why check 6 stays a refusal.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

---

## F. Receipts, evidence and budgets

**The attempt ledger, gate receipts, promotion evidence, the mutation survivor register, and retracted measurements.**

This project spends an irreplaceable budget of real model attempts, so what it can prove is bounded by what it wrote down. The defects are correspondingly about provenance: a receipt that named whatever HEAD happened to be when it was saved, a budget the file's own recount command contradicted by 38 ordinals, two numbers for one quantity and neither with a receipt. Retracted claims are kept struck rather than deleted, because the reason a false measurement was believed is the durable finding.

41 entries.

<a id="c3"></a>

### 3. Gate B attempt 1: ordinal 30 consumed, turn stuck in awaiting_prompt_ack

*2026-07-27*

`````text
First live real-Claude attempt against the frozen Gate A candidate. The atomic
pre-launch reservation worked exactly as designed: ordinal 30 was appended with
status reserved_before_possible_claude_launch before any Claude process could
start, so the budget is correct whether or not the turn succeeded. 30 of 100
consumed, 70 remain.

The attempt failed and the evidence is unambiguous:

  submitting -> awaiting_prompt_ack -> failed (turn deadline elapsed)

Launch and prompt admission both worked against Claude 2.1.220 -- the session
started, the composer was located, the single bracketed paste and the one Enter
were sent, and run_turn returned acceptance. What never arrived was the exact
post-arm typed-user transcript row that acknowledgement requires. The turn sat in
awaiting_prompt_ack for the full deadline in both the campaign run (300 s) and an
independent non-ledger reproduction (90 s).

Decisive supporting fact: Claude wrote NO transcript file at all for either
session UUID. A search of ~/.claude/projects for 41058586-... and 20706978-...
returns nothing, while unrelated sessions written in the same window are present.
So this is not a locator or parser problem -- Claude never recorded the session.

This is a genuine 2.1.215 -> 2.1.220 compatibility finding, not a harness defect.
A zero-cost probe --launch had already reached state=ready on 2.1.220, which
proves the four terminal-geometry constants still hold; the failure is strictly
downstream of composer detection.

Leading hypothesis, and it matches a seam the design review already recorded:
classify_terminal_snapshot tests active_editor BEFORE blocking_screen
(driver_io.rs:83-96), so a modal that coexists with an editor-like region
classifies as Ready rather than needs_input. docs/current-state.md already
carries this as a known seam, and spec.md documents it. If 2.1.220 presents a
trust or auth screen in this cwd, pmux would read it as a ready composer, paste
into it, and then wait forever for an acknowledgement row that cannot exist.

No further attempts were spent diagnosing. The reproduction above used a direct
pmux run, which consumes Claude usage but never touches the ledger.
`````

<a id="c8"></a>

### 8. Untrack Gate A receipts; they live in .context/gate-a/

*2026-07-28*

`````text
Three receipts, 656 KB of regenerable capture output. They are reproduced by
re-running tools/gate-a/run_gate.py against a candidate, so tracking them cost
repository weight without buying anything a rerun could not.

evidence/model-attempt-ledger.ndjson deliberately stays tracked: it is the only
artifact here whose loss cannot be recovered by running something again.
`````

<a id="c11"></a>

### 11. Ledger: record ordinals 31-32, reconcile four detached reservations

*2026-07-28*

`````text
The budget of record had drifted twice over.

The README described a 25-record file at sha ac7878d4; the file was 26 records
at f69c2c55. The miscount came from the last record spelling its ordinal
global_attempt_ordinal rather than global_attempt, so a naive scan skipped it
and read the budget one attempt cheaper than it was. Both spellings are now
documented.

Separately, a driver copied this ledger to a private path on every invocation,
reserved against the copy, and never copied the result back. Each run therefore
restarted from the same base and re-reserved the same ordinal in a file it then
discarded, while its retry loop compared counts against the reset copy and
concluded nothing had been spent. Four campaigns ran where one was authorized.
All four produced zero model tokens -- one was rejected before Claude launched,
three stopped at the folder-trust prompt -- but they are counted anyway. A
reservation consumes its ordinal whether or not Claude produced a result, and
exempting these because they happened to be cheap would make the budget a
measure of luck rather than of attempts.

The hash-chained records are not renumbered into the file: forging chain
entries to tidy the arithmetic would cost more integrity than four ordinals.

Ordinals 31 and 32 are genuine and appended normally. 36 consumed, 64 remain.
`````

<a id="c19"></a>

### 19. Publish the Gate B receipt; the campaign was reproducible only from an untracked dir

*2026-07-28*

`````text
Ten ordinals of irreplaceable spend produced per-attempt artifacts under a
validation root outside this repo. The ledger records RESERVATIONS, not results,
so the nine-grade table and the hash tally could not be re-derived from the
tree -- unlike every Gate A figure. If that directory is lost, the conclusions
are unverifiable. macOS has already wiped earlier candidates out of /private/tmp.

The receipt is the verifier's own --json output over the retained evidence, with
absolute host paths scrubbed so it reads from any checkout. It records: 10
computable gaps, min 0, max 1, ZERO late rows once the 20ms noise band is applied,
4 samples inside the band, and 7 hashes independently reproduced with 0
mismatches.

Re-analysis remains free -- the verifier is offline over published evidence -- so
this can be regenerated whenever the tooling changes, which is how the noise-band
correction was applied to an already-spent campaign at no cost.

Protocol conformance re-verified after stop_hook_at_ms: 44 tests green, including
the golden vectors and the closed-surface manifest.
`````

<a id="c28"></a>

### 28. Phase 1/2 live: every envelope-reachable scenario now has coverage

*2026-07-29*

`````text
Ordinals 44-55, twelve credentialed calls, on a standalone unpolled clone.

  persistent (44-46)  PASS. Turn 3 echoed turn 2's digest byte-identically and
                      that digest reproduces from turn 2's own poem text by
                      independent shasum -- so the tool round trip really
                      happened and state was carried correctly across turns.
  resume (47-48)      PASS. Same session id after a full process restart, and the
                      resumed turns recalled the pre-restart poem and digest
                      exactly. pmuxd re-attached to a session it did not create
                      and found the right transcript position from disk.
  nonascii+hybrid     Ordinal 49 died on SchemaDrift (fixed in <c27>); 50-52
  (49, 50-52)         PASS against the fixed parser. CJK and emoji through the
                      bracketed-paste INPUT path, which no prior turn had ever
                      exercised -- Gate B's nine prompts were pure ASCII asking
                      Claude to WRITE non-ASCII.
  facade (53-54)      PASS through require-tested compatibility, the only gate
                      none of the first 52 ordinals touched.
  deadline (55)       FAILED AS DESIGNED, with code=TurnTimeout. The PRODUCT
                      bounded itself; the envelope's +30s hard bound never had to
                      intervene.

Not run, and documented rather than skipped: cancellation and attach/detach are
unreachable through the phase0 envelope -- --scenario accepts only four values and
phase0.py matrix lists direct rmux control, direct PTY input and attached_stream
as unsupported_by_envelope. Those two runners refuse and spend nothing; the files
are the deliverable for those scenarios.

THE DRAIN QUESTION IS CLOSED, and not by the measurement I built. The first
stop_hook_at_ms samples that have ever existed came back 3 of 3 NEGATIVE
(-106 to -116 ms), which the checker correctly reports as decisive against a
hook-based fast path. But those negatives are the contamination the architecture
review predicted, not evidence about the drain:
last_transcript_activity_at_ms is the last FILE write, and installing the hook
CAUSES writes after it -- the summary row carries the hooks' own durationMs, so it
is written after them, then turn_duration follows. The magnitude is
summary-plus-turn_duration write latency, a property of the instrument.

Had S2 not been re-anchored first, I would now be reading this as a permanent no
on a ~2300ms optimization, from a measurement I designed. The real answer came
free: across 82 turns and four Claude versions on transcripts already on disk, no
model-generated semantic row ever arrives after turn_duration in-turn. The
turn_duration fast path is untouched by these negatives -- different anchor,
in-band, no contamination -- and stays S2/NOT-DONE pending arrival-order
confirmation.

The field itself works: it publishes, the sign survives unclamped, and a negative
is surfaced unmissably rather than averaged away. That was the point of building
it.
`````

<a id="c36"></a>

### 36. Measure Path B through pmux, and retract the latency claim it was sold on

*2026-07-31*

`````text
Nobody had ever run a Path B turn THROUGH pmux. Every prior number came from
pmux's own tests or from driving `claude` directly. Driving pmux end to end --
which spends no ordinal, because the ledger counts phase0 probes and not model
calls -- produced the measurement that settles it:

  Path B  540-575ms median, 528-636ms band  (n=14 spot-check + ~35 matrix turns)
  Path A  535.5ms through the same pmux

PATH B IS NOT FASTER. The ~371ms projection undershot by ~50%. It assumed
deleting the screen ADMIT term freed ~250ms of critical path; it does not.
`completed_at_ms - terminal_candidate_at_ms` equals `drain_ms` to within 1ms in
all 14 samples, and ~300ms of the ~550ms is screen-stability. The graduated
250ms floor already fires and was ALREADY not the binding constraint, so there
was never a ~200ms drain saving available to take. Every latency claim in
earlier drafts is retracted; docs/path-b.md now records the measurement. Future
overhead work belongs on the screen path.

The design doc had pre-registered this: it said in advance that a worse number
would not change the justification, because Path B earns its place on
statelessness and fungibility. That held.

Three findings the smoke test surfaced, all recorded in docs/path-b.md S10:

- The launch bundle does not work as written. Every Path B turn dies at launch
  with UnsupportedClaudeVersion unless `--compatibility allow-untested` is also
  passed: no tested compatibility cell exists for 2.1.220 on macos/aarch64,
  Transparent, Sdk, and `require_tested` correctly refuses. That -- not the three
  flags the doc called inexpressible -- is the real gate on a formal run.
- Concurrent `pmux run` can permanently poison the daemon so it cannot start any
  further rmux sidecar, and `pmux doctor` reports healthy:true throughout. The
  documented health check contradicts the daemon's real state. This blocks the
  pool architecture and is NOT fixed here; it needs its own investigation.
- The three inexpressible flags do not affect COMPLETION (measured across three
  configurations). Making them first-class can be deferred for a
  completion-focused run, but not for any run claiming isolation, because every
  Path B session still spawns MCP servers silently.

phase0 can now express a Path B launch: --denied-tool and --system-prompt-file as
first-class CampaignConfig fields, not extra_args (SAFE_EXTRA_FLAGS is exactly
--debug/--verbose, so anything else is rejected before launch). The replacement
text is read from the file rather than argv, only the file's identity is bound,
the text joins the redaction set, and a credential-shaped document is refused
outright. One deliberate relaxation: launch-option validation accepts both the
legacy 7-name and current 12-name shapes, because 51 of 77 ledger records carry
the old one and requiring 12 would fail an audit of evidence that is merely
older. Unknown names are still refused in both.

The U+FEFF slash-guard fix was source-only and absent from the built binaries,
so a BOM-prefixed /clear still rode through as an ordinary turn. Rebuilt and
verified against the real CLI: BOM+/clear refused, plain /clear refused,
"Say OK." not refused.

cargo test --workspace 672 passed / 0 failed; fmt clean; phase0 243 OK.
Ledger untouched at 77 records, sha 439e4853. ~49 live turns, zero ordinals.
`````

<a id="c53"></a>

### 53. Path B works for someone who never read our argv, and a wave could not tell which daemon it drove

*2026-08-05*

`````text
PROMOTION. `compatibility::PROMOTED_PROFILES` ships one cell -- Claude Code
2.1.220 / macos / aarch64 / transparent / sdk -- and `resolve` searches the
OPERATOR's cells first, so an operator profile for the same identity overrides
it rather than colliding with it. Until now the registry was empty by design,
which meant Path B worked for whoever passed `--tested-claude-profile` and
refused for everyone else. Measured with the flag ABSENT, on real Claude:

    [PROMOTED profile, no --tested-claude-profile on argv, claude 2.1.220]
      served in 4540ms by claude 2.1.220

The drain is measured, not chosen. Max post-answer transcript arrival is 438 ms
over 456 turns in 189 real 2.1.220 transcripts; 1000 ms is 2.28x that and half
the untested fallback nothing ever measured. All 189 arrivals were structural
end-of-turn rows (182 `turn_duration`, 7 `stop_hook_summary`); no semantic row
ever followed an answer. `tools/promotion/measure_transcript_drain.py`
regenerates the receipt, fails on a row kind nobody classified rather than
defaulting, and a unit test binds the shipped constant to the receipt's own
recommendation so the two cannot drift.

THE 132-TOKEN STEP IS THE `/clear` PREAMBLE, AND IT DOES NOT ACCUMULATE. Six
real sonnet/low turns on one instance: 171 cold, then 326 after one, two, three
and five clears. The residue is three messages the rotated transcript carries --
a 245-char `<local-command-caveat>`, a 130-char `<command-name>/clear</...>` and
a 45-char `<local-command-stdout>`, 420 characters -- read off the instance's
own transcripts, at most one caller prompt per file. The hypothesis named the
first two and missed the third. The filler-prompt anomaly is explained too:
`input_tokens` alone is not the turn's input, and a 2709-character prompt
reported `input=2 cache_creation=1230`.

THREE SOFT SPOTS. `HostTurn::sidechain_rows` stays an `Option` -- a host that
cannot count must be able to say so -- but `None` is now a refusal instead of
`unwrap_or(0)`, and production counts: `TranscriptAnalysis::sidechain_rows`
counts every sidechain row of any kind from the walk the engine already makes.
A `Task` subagent whose rows report no usage used to commit with its isolation
claim unmade. The launch-broker layer now exchanges a real launcher frame at
`LAUNCHER_PROTOCOL_VERSION + 1`, which `serve_connection` answers before it
touches the pending map, so it exercises accept, framing and dispatch without
spending a one-use capability -- and its detail string names the one step it
skips. MCP `run_stateless` is driven against a live daemon, with the answer
joined to the child side so a fabricated one fails.

THE SIXTEENTH INSTANCE, AND IT MADE THE MUTATION CAMPAIGN LIE. `cargo test -p
pseudomux-e2e` does not rebuild `pmuxd`; it is another package's bin target. A
mutation making `Pool::commit` refuse EVERY turn was verified against the live
MCP wave and the wave PASSED -- 1 passed, 0 failed -- because it drove the
previous daemon. Thirteen green waves, and none of them could tell. The guard
reads cargo's own depinfo beside each binary, because a hand-rolled "newer than
anything under crates/" rule marks `pmux-rmuxd` stale for an edit it does not
link and cannot be cleared by rebuilding.

Three more of the same class: `quiesced_census` promised a quiesced pool and
summed four counters somebody remembered, now `live == idle`; `trees()` and
`assert_pool_parent_drained` answered "empty" for a directory that does not
exist, so every teardown claim in two files was satisfiable by looking in the
wrong place. And one I wrote myself: a clause in `admissible_here` that no test
could fail, deleted, replaced by one that can.

14 checks mutation-verified, every restore byte-exact by sha256, no survivors.
fmt/clippy clean first-party; 4 pre-existing vendor warnings. Not pushed.
`````

<a id="c54"></a>

### 54. A retracted claim outlives the wrong belief it caused, and path-b.md now describes a shipped thing

*2026-08-06*

`````text
`docs/path-b.md` carried 57 MEASURED claims and three of them were false. They are
corrected, and none is deleted: a reader who remembers a claim needs to see it
struck, and the reason a false measurement was believed is the durable finding.

THE THREE RETRACTIONS.

`CLAUDE_CONFIG_DIR override alone -- Same auth break` was FALSE. The probe moved
two variables at once -- a throwaway config dir AND no API key -- so "Not logged
in" was over-determined and attributable to neither. Claude namespaces the
keychain SERVICE NAME by `sha256(config_dir)[0:8]`; `CLAUDE_SECURESTORAGE_CONFIG_DIR=""`
pins it back. That row made per-cell private roots look impossible.

"Every Path B session currently spawns MCP servers silently" was an INFERENCE
from "pmux cannot pass --strict-mcp-config", never an observation. A complete
descendant inventory of the live `claude` PID at 50 ms across four cells is
exactly `security find-generic-password` and `caffeinate -i -t 300`. No node, no
python, no npx, in any configuration. `--strict-mcp-config` therefore stops
nothing and is no longer called load-bearing.

The `/clear` menu was assumed alphabetical. It is a fuzzy score over
DESCRIPTIONS as well as names -- at `/c` the selected entry is `/cd`, and
`/doctor` is a candidate at `/cl` -- and the highlight is colour-only
(fg=idx153 vs idx246), so it was absent from pmux's data rather than hard to
read.

A NEW SECTION 0 CARRIES THE METHOD, not just the conclusion, because the
confounded probe is the lesson and a future agent hits it before designing one.
Five rules, each anchored to the probe that earned it, including: an unknown
`--effort` spelling is ACCEPTED by 2.1.220, warns on stderr, and pmux never
reads the child's stderr -- so "the CLI did not complain" is not an observation
pmux can make. `--bare` is kept as the counter-example: right conclusion, wrong
probe, now grounded on the bundle's own `rf()` check instead.

TENSE. The pool shipped. The launch bundle table is now the argv
`launch_request_for` produces, not a wish list; the three "inexpressible" flags
are retired with reasons; §10's three end-to-end findings are all closed and
renumbered E1-E3 so they stop colliding with items 7-8. New §12 documents the
product as it is: `(model, effort, prompt) -> tokens` with no id in the result,
the pool and its teardown order, the health tree's four-valued layer, the
promoted 1000 ms drain and its five excluded arrivals, the screen corpus, and
the 24x80 panes a 24x120 request was silently clamped to.

MEASURED, not chosen, and the distinction is kept in both directions: the drain
is measured (438 ms max over 456 turns in 189 transcripts); context across
/clear is measured constant (171 cold, then 326 after one, two, three and five
clears, 15/15 across four runs) with the 420-character /clear preamble as the
whole step; `history.jsonl` never reaches model context (40k seeded,
input_tokens unchanged at 186), so recycle is capacity hygiene. The system
prompt WORDING is CHOSEN and is now labelled so it can never be dressed as
measured.

§13 lists seven things this pass could not reconcile rather than guessing --
including the 571 ms / 535.5 ms Path A anchor that two sections disagree on, and
whether the `--safe-mode` FLAG breaks a cell the way the env var measurably does.

`spec.md` gains the `run_stateless` method it was missing, environment step 7,
and the effort rules stated as pmux's own policy. `current-state.md`'s protocol
counts were 10/10/14/34/18 against a manifest reading 12/12/14/34/23, and its
single test total is replaced by the per-binary harness's 60 targets.
`testing.md` called a five-row launch fixture "the measured 5-row launch
preamble"; the measurement is four rows and the fifth exists so the locator can
corroborate the file.

Docs only. No cargo run, no code touched.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c59"></a>

### 59. Merge the docs reconciliation: three measured claims were false, and one fixture called itself a measurement

*2026-08-06*

`````text
Reconciles every MEASURED claim in path-b.md, spec.md, current-state.md and
testing.md against what was actually measured this week. Three are corrected as
false and retained struck rather than deleted, with the reason each was believed:
the CLAUDE_CONFIG_DIR auth break came from a probe that moved two variables at
once; every-Path-B-session-spawns-MCP-servers was an inference that was never an
observation; and the /clear menu is a fuzzy score over descriptions, not an
alphabetical list.

New path-b.md section 0 puts the confounded-probe method ahead of every
measurement, so a future agent meets it before designing a probe.

testing.md S-28 called a five-row fixture the measured launch preamble. The
measurement is four rows.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>

# Conflicts:
#	docs/path-b.md
`````

<a id="c60"></a>

### 60. Two numbers for one quantity and neither had a receipt, and a directory mtime that was never identity

*2026-08-06*

`````text
`path-b.md` carried 571 ms in §1 and 535.5 ms in §10.1 for the same thing --
"Path A through the same pmux" -- and nothing in the repository distinguished
them. Re-measuring shows why that was the wrong question: neither had an argv,
a harness or a commit behind it, so one of them being right would still have
left a number nobody could regenerate. Both are replaced, along with the
540-575 ms Path B band, by `tools/promotion/measure_turn_latency.py` and two
receipts. Path A costs 1,204 ms median server-side over n=60 against the
zero-latency driver, 646 input gate + 0 generation + 555 commit gate, and
Path B is 741 ms SLOWER at the client clock, not within 40 ms of it. Sweeping
`--drain-ms` 50/250/1000 puts a number on the old conclusion: at every value a
real turn owes, the drain contributes nothing and the screen-stability wait is
the whole cost.

§13's seven unreconciled items are settled, and three did not close the way
they were written. Item 4 asked for a mint into a warm private root, which
pmux structurally refuses for a minified cell, so there is no such thing to
time -- what a cold root does cost is one 477 KB `cache/changelog.md` per root
and no measurable readiness-window penalty over n=10/n=10. Item 3's
`--safe-mode` FLAG does not break a cell: 5/5 started and answered against a
5/5 control, so it and `CLAUDE_CODE_SAFE_MODE` are not interchangeable. Item 7
recorded that `ultracode` "warns on stderr and falls back to the default";
reading that stderr from outside pmux -- no product change needed -- shows it
does neither. It is a recognised spelling the CLI's own warning omits, the
match is case-insensitive, and it is its own tier: 195 of 210 pairwise
comparisons above the default's own output-token distribution. Item 5's
2 s sampling blind spot is closed by mechanism rather than by more samples --
Claude REPLACES `.claude.json`, 25 writes produced 25 inodes and 0 torn reads
in 8,764 samples, with a positive control that caught 36 torn reads out of 407
against an in-place rewrite. Item 6 needed no new instrument at all:
`TurnTimings` already stamps `turn_duration_observed_at_ms` and its post-marker
partner, and the latency tool's refusal to publish a total containing an
unclassified `*_at_ms` field is what forced them to be read. 20/20 markers,
0/20 rows after one, 552 ms of commit gate spent after the marker had arrived.

`phase0_self_tests` was a real defect, not load. 2 of 12 isolated runs red,
always the same capture abort, and the cause was not the one the debt row
named: the capture's own Git queries run under `GIT_OPTIONAL_LOCKS=0` and write
nothing. An external ~6 s workspace poller adds and removes `.git/index.lock`,
which moves the Git DIRECTORY's mtime -- 14 times in 30 s with nothing of ours
running -- and that mtime was being recorded as identity. A directory's mtime
is when its entry set last moved, which is a fact about whoever moved it. It is
gone from directory identities; files keep theirs and are bound by sha256
besides; producer and validator move together so a regression is refused rather
than silently reintroduced. 1 abort in 20 before, 0 in 30 after, and 16/16
green cell runs across two interpreters.

`release_full_stack_e2e` is satisfiable as written and the report saying
otherwise was stale. The cell DOES set `PMUX_E2E_TYPESCRIPT_DIST_DIR`, three
earlier cells stage that directory, and the `PMUX_POOL_REAL_CLAUDE` half was
fixed in <c57>. Five consecutive reproductions of the cell exactly as the
driver runs it: 5/5 green, 528-539 s, `9 + 10 + 21 passed; 0 failed; 0
ignored`. What is left is C10 and only C10, now 2 in 12 rather than 2 in 7, and
0 of 5 does not retire it.

Two more of the class, both in guards that have reported success on every gate
run there has ever been. `prepare_validation_root` promises "the documented
validation tree owner-private, or refuse" over a hand-written three, and
twenty-one cells build into a fourth: a pre-created 0755 `cargo-target` drew no
refusal at all, which is the exact case the docstring exists to catch. The set
is now derived from the manifest with the documented three as a floor.
`per_binary_tests.sh` ended its kind mapping in a bare `continue`, so an
unclassified target kind was dropped from a set its footer then called every
one of them; and separately it never supplied `PMUX_E2E_BIN_DIR`, so one of the
61 targets was permanently red and the coverage sentence was unreachable. Both
fixed by derivation, and the report now earns its own claim for the first time:
every one of the 61 test targets passed in isolation, 1031 cases, 0 ignored.

Every check added or changed was proven by mutation and restored byte-exact.
No Rust changed. `cargo fmt` clean, clippy clean first-party, ruff clean,
shellcheck clean, residue audit passed after the E2E runs.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c76"></a>

### 76. A tree that did have the newer receipt, behind the one ignore rule the search could not see

*2026-08-08*

`````text
The spike said the 82/83 figure "does not appear in this tree" and cited 80/81
instead. It does appear, at .context/gate-a-mutants/dead-code-pass/stdout.log:85,
and the reason git grep missed it is that .context/ is the last line of .gitignore.

The method reported a negative it could not establish: absence from git grep is
absence from tracked files, not absence from the tree. Both receipts carry the
same single deliberate red; only the cell count moved.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c77"></a>

### 77. A ledger whose own recount command falsified the budget it published, and a pool that erased a root under a child it never counted

*2026-08-08*

`````text
Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c78"></a>

### 78. A budget the ledger's own recount command contradicted by 38 ordinals, deleted rather than corrected

*2026-08-08*

`````text
`evidence/README.md` published "47 of the authorized 100 global attempts are
consumed; 53 remain" and, two lines later, the command to check it. Run
verbatim, that command returned `77 records, 5 through 81`: by the file's own
arithmetic, 81 + 4 detached = 85 consumed and 15 left, against a hard ceiling
that no restart, runner or failed call resets. Every doc that restated the
figure ranked work against it -- `docs/instrument-fix-plan.md:65` ordered a
whole fix plan by "~53 remaining irreplaceable ordinals".

The same document refuses to pin a SHA-256, in the paragraph immediately above,
because "a stale digest that looks authoritative is worse than none". So the fix
is not 15. Nowhere in the tree now writes a record count, a last ordinal, a
consumed count or a remaining count for this file:

- `phase0_lib.summarize_attempt_ledger` derives all of them on the call. It
  counts through `ORDINAL_SPELLINGS`, ONE tuple now shared with
  `_recognized_prefix_last`, because the second copy of that list is what makes
  a scan stop at ordinal 29; it takes the ceiling from the records' own
  `global_attempt_ceiling` rather than from a constant; and it refuses rather
  than reports on a non-contiguous ledger, an unrecognized ordinal spelling, or
  consumption already past the ceiling. That closes item 5 of
  `docs/instrument-fix-plan.md`'s "what this plan does not fix".
- `phase0.py budget --ledger …` prints it, and is listed in
  `tools/phase0/README.md` under a test that derives the command table from
  argparse.
- `test_run_gate.py::test_the_evidence_readme_states_no_budget_figure_and_its_command_derives_one`
  runs in Gate A. It scans the README for every shape such a figure has actually
  been written in -- each shape checked against the verbatim sentences that
  shipped stale, so a shape that stops covering the defect fails too -- then
  runs the command the README itself prints and compares it to a count taken
  independently.

The four detached reservations are still real and still uncounted by the file:
ordinals 5-81 are contiguous with one record each, and
`evidence/gate-b-drain-calibration.json` still carries five rows at ordinal 31.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c91"></a>

### 91. A promotion that would fit 2.1.223 a 250 ms drain from one arrival, and a 2,344-turn free corpus that is 219 rows

*2026-08-08*

`````text
Measured answer to "is our process for every Claude Code update optimal?": no,
and not for the reason the arithmetic suggested.

The free evidence is not there. The corpus holds 219 `system/turn_duration`
rows in total across both roots, not 2,344 -- counted by row-version, by
file-level admission and by raw text match. Every one of the 219 carries
`entrypoint: cli`; zero appear on `sdk-ts`, which is 98.8% of the 169,237
versioned rows. "turn_duration did not exist before 2.1.207" is an entrypoint
confound, not a version fact. And 178 of the 186 reachable arrivals at 2.1.220
come from pmux's own campaign directories: the corpus that promoted 2.1.220 for
free was the residue of the Gate B campaign that had just been paid for.
Re-analysis does not replace a campaign at a new version, because at a new
version there are no cli turns to re-analyse.

The per-version pin measures noise. Between 2.1.215 and 2.1.220 the maxima
differ by 100 ms; the within-version half-split |max(A)-max(B)| has p95 216 and
176 respectively. Subsampling 2.1.220 to 2.1.215's n=36 puts 2.1.215's 338 ms
at the 65th percentile of the resulting max distribution, and a permutation
test on the difference gives p = 0.730.

The pin is also unsafe when thin. Run today, the tool recommends 250 ms for
2.1.223 from its single arrival -- 188 ms below an arrival already observed one
version earlier, and below `POST_MARKER_CATCH_WINDOW_FLOOR_MS = 438`, the
constant that exists to keep that arrival catchable. E[max] fits
76.4*ln(n)+27.2 at R^2 = 0.9975, so a bound fitted from 36 arrivals is 31% low
and from one is 87% low, in the direction that truncates answers.

0 of 226 arrivals across four versions exceed 1000 ms, and the pooled estimator
-- the tool's own margin and step over every observed version -- returns exactly
1000 ms. Widening the key from one version string to a range changes no number.
1000 ms prices at one expected truncation per 336,786 unmarked arrivals.

Gate B's 13 ordinals bought 10 drain samples against the corpus's 189 at the
same version, with `late_row_attempts` 0 and a maximum gap of 1 ms. What it
buys uniquely is the launch bundle being accepted and the hash oracle -- neither
of which any transcript can answer, and neither of which the version key
currently protects, while a dozen screen constants measured on 2.1.220 are
keyed to no version at all.

Evidence and a proposal. No protocol change is implemented, and section 7 names
what this did not establish -- including that the briefing's turn table does not
reconcile and that nothing was measured about 2.1.223's launch bundle.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c96"></a>

### 96. A drain whose only receipt was one version's own fit, promoted as a bound over four, and a tool whose exit 0 meant "nothing to check" at exactly the version nobody has measured

*2026-08-09*

`````text
The shipped 1000 ms is unchanged, because the pooled estimator returns it exactly. What was
missing is that nothing said so. `PROMOTED_PROFILES`' provenance read "max post-answer
transcript arrival 438 ms over 456 turns in 189 Claude Code 2.1.220 transcripts ... 1000 ms is
2.28x that maximum", and the only committed receipt was a `--version 2.1.220` run -- i.e. a
per-version fit that happened to agree. docs/version-drift.md sec.3.3 measured that fit and
found it measuring noise (between-version spread 100 ms against a within-version p95 of
176-216 ms, permutation p = 0.730), and sec.3.5 measured which way it errs on a thin corpus:
87% low at n=1. Running the tool on 2.1.223 today recommends 250 ms, below the 438 ms already
observed one version earlier and below POST_MARKER_CATCH_WINDOW_FLOOR_MS = 438.

`--version` is now repeatable and the recommendation is pooled over every version named. One
pass over the corpus, each file read once and offered to every version that names it, so the
per-version admission rule is unchanged and `evidence/promoted-profile-2.1.220-macos-aarch64.json`
regenerates with the same `recommended_transcript_drain_ms`, `claude_version`, `os`, `arch` and
reachable maximum (`files_scanned` 1195 -> 1146 is the host's transcripts having been cleaned up,
as sec.1.5 predicted). The new receipt,
`evidence/pooled-transcript-drain-macos-aarch64.json`, is 425 files, 1,336 turns, 226 reachable
arrivals, max 438 ms at 2.1.220, recommendation 1000 ms -- and it publishes
`per_version_recommendations_not_to_be_shipped: {2.1.207: 250, 2.1.215: 750, 2.1.220: 1000,
2.1.223: 250}`, because the gap between those numbers is the whole argument and a receipt that
hid it would be asking to be re-fitted.

The price is measured rather than asserted. `full_drain_binds_on` reproduces sec.5's figure from
the corpus -- 385 `cli` turns that reached a terminal candidate, 166 with no `turn_duration`
marker, 0.431 -- because a turn that already has its marker owes only
TURN_DURATION_DRAIN_FLOOR_MS, so that share is exactly what a wider bound would cost. 1250 ms
would spend 250 ms on 43% of turns to move the expected first truncation from one in 337,000
unmarked turns to one in 4.5 M. Recorded, not taken.

`every_promoted_drain_is_the_pooled_bound_and_not_a_per_version_fit` re-derives the
recommendation from the receipt's own `recommendation_basis.margin` and `.rounded_up_to_ms`, so
no Rust constant repeats the Python ones. It refuses a receipt pooled over fewer than two
versions, refuses a named version that contributed no arrival, refuses a receipt in which no
per-version fit is strictly below the pooled bound -- without that one the word "pooled" is
decoration -- and refuses a `drain_provenance` that does not quote the receipt's own 438, 1000,
385 and 166. Proven able to fail: moving the drain to 1250 ms, replacing "166 of 385 cli turns"
with prose, and rewriting the receipt to name one version each turn it red; restoring each
returns it green.

`--bound-ms` turns the reading into a check. Exit 4 when a reachable arrival exceeds the drain
already shipped -- reproduced with `--bound-ms 400` against 2.1.220, which names the 438 ms
arrival. Exit 5, distinctly, when there was NOTHING to check: `--version 2.1.226 --bound-ms
1000` exits 5 on this host because the corpus holds zero 2.1.226 rows, and until now that ran as
exit 0. "We checked" and "there was nothing to check" were the same exit code at exactly the
version where the difference decides a promotion.

Exit 2 and exit 3 are unchanged in behaviour and now name the trigger they are: reproduced at
2.1.201 (`pr-link/None` 101, `system/compact_boundary` 2, `system/model_refusal_fallback` 10) and
by flipping `since_candidate > 0` to `< 0`, which turns 2.1.223 red with `{"system/api_error": 9}`.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c97"></a>

### 97. An exact-version key that spent 13 ledger ordinals per patch release to pin the one quantity that does not move, and a pool that halted on one of the seven refusals its own comment described

*2026-08-09*

`````text
The key is now a range: `2.1.220..=2.1.226`, macos/aarch64, transparent/sdk.
`validate_exact_version` is gone -- a validator that threw its parse away, which is how a
version comes to be compared as a string three lines later. `ClaudeVersion` keeps the three
components, `VersionRange { floor, tested_through }` is inclusive at both ends, and
`TestedCompatibilityProfile::matches` asks it for containment.

The floor is where the evidence starts: 2.1.220 holds the drain receipt, the Gate B campaign
and the screen/preamble measurements, and docs/version-drift.md sec.3.1 measured 2.1.201 and
earlier at ZERO reachable `cli` arrivals -- unestablished, not safe, so the range does not open
backward. The ceiling is where the evidence stops: docs/2.1.226-compatibility.md measured the
24-spelling launch bundle accepted with zero rejections, the post-/clear frame at 2 rendered
rows below the cursor, the 5-row preamble identical in rows and order, the local-command menu's
foreground-only selection, the --effort vocabulary byte-identical, and `pmux start --cell
minified` reaching `state: ready`. The drain was NOT measured at 2.1.226, because the corpus
holds zero 2.1.226 rows; that is what the pooled bound is for, and `range_provenance` says so
where an operator reads it.

A range never spans a minor. `VersionRange::new` refuses a floor and a ceiling on different
`major.minor` lines, and because the bounds share a line, ordered containment refuses another
line for free -- there is no second `same_line` clause in `admits` to forget. That is trigger 5
as a predicate. Once the key is a range, "duplicate" means OVERLAPPING and not equal, so
`insert` refuses `2.1.223..=2.1.230` beside `2.1.220..=2.1.226` and admits `2.1.227..=2.1.230`.
An operator's profile gains an optional `claude_version_tested_through`; absent means the exact
version, which is what every profile written before the field meant and still means.

`RepromotionTrigger` makes the five conditions values instead of five sentences in a document.
Each variant carries the FILE and the SYMBOL that detects it;
`every_repromotion_trigger_names_a_detector_that_exists` opens each file and fails when the
symbol is not in it, and `detector()` carries no wildcard, so a sixth trigger stops the crate
compiling until somebody says where it is found. Two of the five point into Python, which is
the point: triggers 1 and 2 are detected by measure_transcript_drain.py for 0 ordinals, and a
Rust-only binding would have silently stopped covering them.

TRIGGER 3 did not exist. A child that refused a launch flag and a Claude that was merely slow
produced the identical `NeedsInput` refusal. `startup_screen_diagnostics` gains one structural
boolean. The marker is MEASURED on this host at 2.1.223 and 2.1.226 byte-identically: `claude
--pmux-probe-sentinel doctor` prints `error: unknown option '--pmux-probe-sentinel'` on stderr,
exits 1 with empty stdout, and the commander exits before the subcommand runs -- so the probe
that established it executed nothing and spent no ordinal. The screen is still never
reproduced; the test asserts the flag name does not appear in the refusal.

TRIGGER 4 half-existed, and the half that was missing is the house bug class.
`clear_selected_wrong_local_command` tested `reason == "wrong_local_command"` -- one literal --
under a doc that already claimed the general thing: "it means pmux's model of the composer no
longer matches the installed Claude, and every other instance is typing /clear into the same
composer." That is true of six other refusal reasons and none was tested for. A cleared
preamble carrying a metadata record type pmux has never seen, a system row whose subtype is not
`local_command`, a line the parser cannot parse, a row kind it does not recognise, a third user
row, or more rows than the preamble has ever had -- each is Claude writing a preamble that is
not the one driver_io.rs:184 was MEASURED against, each is a fact about the INSTALLED Claude,
and each quarantined one instance while the pool minted the next one into the identical drift.

The thirteen literals are now `AssertEmptyRefusal`, whose `is_a_version_drift_signal` is a
wildcard-free match, so a new reason cannot be added without answering the question. Seven halt
the pool and carry WHICH reason, so the operator is sent to the part of the preamble that
moved. Seven do not, each with a stated reason rather than a default: a byte budget checked
before any parse can fire on a large leaked file; `clear_command_missing` is a deadline
expiring and is indistinguishable from a slow clear; `preamble_not_settled` is a stalled
writer; `unexpected_clear_echo` is an identity fact; and a prompt, a turn marker or a semantic
row are content, which is a leak, and a leak is one instance. One wire value changed
deliberately: the byte-budget site reported `row_budget_exceeded` while publishing `bytes` and
`byte_budget`, and is now `byte_budget_exceeded`.

Reproduced at HEAD's classification by narrowing `is_a_version_drift_signal` back to
`WrongLocalCommand` alone: `a_preamble_that_moved_is_a_repromotion_trigger_and_a_leak_is_not`
and `every_preamble_mismatch_halts_the_whole_pool_and_not_only_a_mis_selected_command` both
fail with `["wrong_local_command"]`, and
`a_successor_carrying_metadata_that_is_not_preamble_is_refused` -- which drives a whole
rotation, preamble read and all -- fails on the trigger it should have carried. Restoring
returns all three to green. Also proven able to fail: renaming a detector symbol reddens the
binding test; collapsing the range to one version reddens
`a_version_no_promoted_cell_names_is_refused_and_the_refusal_says_what_to_do` with "admits one
version, so the range key buys nothing"; comparing versions as strings reddens
`versions_are_ordered_numerically_and_unparseable_ones_are_refused`.

Two fixtures were derived rather than re-written. `refused_pool_claude` named `2.1.223`, which
stopped being a refusal the moment the range reached 2.1.226 -- a health test asserting against
a refusal the daemon no longer issues -- and now runs one patch above the promoted ceiling
through `admit_claude_version` itself. `a_promoted_cell_admits_this_platform_with_no_operator_profile`
and `a_version_no_promoted_cell_names_is_refused...` assert EVERY patch inside the range, not
its endpoints: a containment predicate that admits its endpoints and nothing between them
passes a two-endpoint test and refuses most of the range in production.

`cargo test --workspace`: 1116 passed, 0 failed, 50 ignored.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c98"></a>

### 98. A drain corpus that existed only because a Gate B campaign had just been paid for, and the `cli` transcripts every ordinary Path B turn writes and the pool erased four lines later

*2026-08-09*

`````text
docs/version-drift.md sec.2.2: 178 of the 186 reachable post-answer arrivals behind the shipped
1000 ms came out of pmux's own campaign directories under ~/pmux-drain-campaigns and
/private/tmp. Eight came from anywhere else on that host. The 2.1.220 profile was built "for
free" only because a campaign had just been run at 2.1.220 -- the transcripts were free, the
turns that wrote them were not. And sec.2.1 is why re-analysis cannot stand in: the
`turn_duration` marker is a `cli`-entrypoint feature, ZERO of the corpus's 169,237 versioned SDK
rows carry one, and at a brand-new version there are no `cli` turns to read at all --
`--version 2.1.226 --bound-ms 1000` exits 5, "nothing to check", on the host that shipped
2.1.226.

A Path B cell IS a `cli` cell. Every ordinary turn already writes the evidence a promotion
needs, and `Pool::destroy` erased it. It is now mirrored first, in the window between the
process being proven reaped and `erase_tree` -- the only window in which the file exists and
nothing is writing to it, and off the turn path, so it costs no turn latency.

WHAT IT RETAINS IS DERIVED, NOT JUDGED. Not the transcript: a mirror pruned to
`evidence::RETAINED_ROW_FIELDS`, which is exactly `measure_transcript_drain.py`'s `FIELDS_READ`
-- entrypoint, isMeta, isSidechain, promptId, subtype, timestamp, type, version. Choosing eight
fields that look safe is a judgement nobody can check; taking the eight the only consumer reads
is a fact, and `the_retained_fields_are_the_ones_the_measurement_tool_reads` opens the Python
file to establish it. The tool now PRUNES every row to `FIELDS_READ` on the way in, so that
constant is load-bearing in the tool rather than decorative: a reader added below that needs a
ninth field gets None and the measurement visibly changes instead of the list quietly becoming a
lie. No prompt and no completion can be retained, and not because they look sensitive -- because
nothing measures them.

The mirror is not an approximation. Mirroring the 189 transcripts behind
evidence/promoted-profile-2.1.220-macos-aarch64.json through this field set produced 271,497
bytes -- 1,437 bytes per transcript -- and running the tool over the mirrors reproduced that
receipt's `post_answer_arrivals`, `recommended_transcript_drain_ms`, `full_drain_binds_on`,
`partition_balances` and 456-turn count IDENTICALLY. The tool cannot tell the difference,
because it never reads a field the mirror dropped. That measurement is also where the 64 MiB
budget comes from: ~46,000 retained transcripts, two orders of magnitude past the 425 behind the
shipped bound.

Where: `<socket parent>/path-b-evidence/`, beside `logs/` and `agents/` and derived through the
same `daemon_sibling_dir` those two go through, owner-only at every level and 0600 per file. It
is outside the pool parent, held to the rule `--path-b-retain-dir` already had -- and that rule
is now ONE rule applied to both, because the reason it gave ("evidence must outlive the tree it
is taken from") was already exactly as true of a corpus written from a config root the next line
erases. `--path-b-evidence-dir` moves it, `--path-b-no-evidence` turns it off and wins over an
explicit directory rather than being ignored beside one, and the daemon publishes the answer in
`configuration.path_b.evidence_dir`, so "on by default" is a claim the running daemon answers.

Bounded at 64 MiB by deleting oldest-first, by MTIME: a session uuid carries no order, so a
name-ordered prune would delete an arbitrary file rather than the least useful one. The test
sets mtimes explicitly through `utimensat` rather than trusting sixteen writes in a loop to land
on different timestamps, which on a 1-second-granularity filesystem would make the ordering
assertion a coin flip.

`socket_parent` is split out of `validate_socket_path` so the derivation is pure: `resolve_path_b`
runs BEFORE the socket directory is created, deliberately, so a pool refusal leaves no socket
directory, no runtime directory and no rmux sidecar behind it, and a derivation that had to
create something first would have inverted that order.

Proven able to fail: making `Pool::destroy` skip retention reddens
`ordinary_path_b_traffic_retains_its_own_drain_evidence_and_no_content`; adding `message` to
`RETAINED_ROW_FIELDS` reddens both the cross-language field check and the redaction assertion,
the latter printing the caller's prompt back out of the mirror. The off switch is asserted
against the directory that already holds one mirror, so it cannot pass with the feature deleted.
The pool's test double now writes a transcript in the shape Claude Code writes one, because a
double that wrote only `history.jsonl` would have exercised the retention path against an empty
directory forever.

`cargo test --workspace`: 1122 passed, 0 failed, 50 ignored. gate-a-residue.sh passes.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c99"></a>

### 99. A refusal that named the global real-Claude ceiling while testing this file's own numbering, and the four detached reservations it therefore would have spent twice

*2026-08-09*

`````text
`phase0.py budget` adds `DETACHED_GLOBAL_ATTEMPTS` back before comparing
against the ceiling; `reserve_attempt` did not. Its predicate was the bare next
ordinal, under a refusal reading "global real-Claude attempt ceiling is
exhausted" -- the message naming the total, the test naming the file. Against
the ledger at ordinal 81 and a ceiling of 100 the guard would still have handed
out 19 ordinals (82..100) while the command `evidence/README.md` sends every
reader to reported 15 remaining. Four attempts past a hard ceiling, spent
believing the tool agreed, and the planning guard one frame up had the same
arithmetic. All three sites now derive the total through one
`global_attempts_consumed_through`, because a second copy of `+ detached` is
what let them drift. The new case pins the reservation boundary to
`summarize_attempt_ledger`'s own `remaining` at both ends -- reserves at one
left, refuses and appends nothing at none left -- with the ordinals computed
from the constant, so it cannot be satisfied by writing today's numbers down.
Reverted to HEAD's predicate it fails with `BudgetExhausted not raised`.

The constant's comment also called four "the only such number", and it is not.
`evidence/turn-latency-2.1.220-macos-aarch64.json` is a committed receipt for
`measure_turn_latency.py --driver-environment operator` against the operator's
real Claude 2.1.220: 22 `pmux turn` and 22 `pmux ask` samples, `zero_latency:
false`, and `measure_path_b` refuses to record a sample whose text came back
empty. Stamped 2026-08-06, seven days after this ledger's last record, and it
reserved nothing; the real lanes behind `PMUX_POOL_REAL_CLAUDE` in
`pool_concurrency.rs` and `cross_cell_contamination.rs` reserve nothing either.
So "enforced at reservation time" holds only for work routed through
`tools/phase0`, while the sentence above it calls the ceiling a total across
all campaigns. Nothing here re-prices D4: were those turns counted, consumption
would already be past the ceiling and `budget` would refuse rather than report,
which is the owner's call and not this commit's. What changes is that the
constant no longer claims to be a census it is not.

No ordinal was spent reaching this. The ledger is byte-identical.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c100"></a>

### 100. A range promoted through 2.1.226 on a drain its own provenance said was never measured there, now measured at 70 ms over four real turns, and the warm-pool mint that runs before pmuxd installs its SIGTERM handler

*2026-08-09*

`````text
Eleven real model turns through the shipped release binaries against the
installed Claude Code 2.1.226 -- ten Path B, one Path A, Sonnet 5 at low,
medium and high. Every one returned the model's actual answer, checked as a
nonce plus an arithmetic result so a cache, a template or a neighbouring
transcript could not have produced it.

`PROMOTED_PROFILES` already ships `2.1.220..=2.1.226`, so the product was
never refusing; what was missing was the measurement its own `range_provenance`
admits to: "NOT measured at 2.1.226: the drain, because the corpus holds zero
2.1.226 rows". There are rows now. `measure_transcript_drain.py --version
2.1.226 --bound-ms 1000` exits 0 rather than 5, over 4 `cli` turns and 7
transcripts: max post-answer arrival 70 ms against the 1000 ms the profile
ships, every turn carrying its `turn_duration` marker, no unclassified row
kind. The per-version fit is 250 ms and is published as not-to-be-shipped,
which is the thin-corpus failure version-drift.md sec.3 describes. n = 4; this
does not retire the pooled bound and is not offered as doing so.

P4 is no longer unproven against a live Claude: `Pool::destroy` mirrored all 7
transcripts, 46 rows, exactly the eight `RETAINED_ROW_FIELDS` and no prompt or
completion text, and the drain reproduces identically from the mirror alone.

`/clear` clears: the post-clear transcript carries the five measured preamble
rows and no trace of the turn before it, so the 228 -> 361 `input_tokens` step
is the preamble and not leaked context. The recycle measures 682-860 ms,
median 792, against a recorded 703-756 median 730 -- inside this instrument's
own 60-90 ms granularity, and nowhere near the ~1700 ms a 1000 ms drain would
predict, because the minified fast path is what a clear on this cell pays.

The reliability defect is not about Claude. `shutdown_signal()` calls
`signal(SignalKind::terminate())` inside a future `serve_until` only polls
after `NativeService::start` has minted the entire declared warm set, so for
the whole width of that mint SIGTERM has its default disposition: exit 143, one
slot epoch tree left per instance the mint reached, a stale socket, and a log
holding only the raw startup record because the appender's `WorkerGuard` is
never dropped. The next start then refuses -- "the pool never adopts a tree it
did not create" -- with nothing in the log saying why. A daemon with no warm
set shows no window. No process is leaked; the first counter that said
otherwise was matching this session's own shell command line, and sec.9 records
that correction rather than deleting it.

Also reported, not fixed: three sites describe a minified launch bundle
containing `--strict-mcp-config` and `--safe-mode`, neither of which pmux
emits or contains anywhere as argv -- including the comment above
`ROW_KINDS`'s "load-bearing column" and a refusal that reads "so --safe-mode
did not hold" about a flag that was never passed.

Gate A on the settled tree, six phases, gate_b not run: 61/62, sole red
`gate_f/linux_docker_self_tests` (debt row C6, reproduced exactly and read
rather than assumed), `source_unchanged: true` with an identical digest at both
ends. A first attempt reported 60/62; the extra red was this session's own
`cpython-313` bytecode cache and is disclosed in sec.8.1.

Ledger byte-identical at 85 consumed / 15 remaining: `pmux ask` reserves
nothing, so the true count of real turns is 11 higher than the file knows, and
nothing here re-prices D4.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c105"></a>

### 105. The 2.1.226 half of the promoted range, restated as the sentence a run of the promotion path generated rather than one written beside it

*2026-08-09*

`````text
`range_provenance` has now been wrong in both directions. It described a launch
bundle pmux does not emit (`<c95>`), and then, once
`docs/2.1.226-acceptance.md` had measured the drain at 2.1.226, it went on
reading *"NOT measured at 2.1.226: the drain, because the corpus holds zero
2.1.226 rows"* -- understating its own evidence, which is the same defect as
overstating it, and which that document flagged and deliberately left for its
own commit. Both are only reachable while the sentence and the measurement are
separate artifacts that nothing compares.

`evidence/promotion-2.1.226-macos-aarch64.json` is a run of
`tools/promotion/promote_claude_version.py` against the installed 2.1.226:
verdict `promotable`, nine checks passed, five real Sonnet 5 turns at low and
high effort, every one through `pmux ask` and therefore through a
`SessionCell::Minified` cell -- the cell no phase0 campaign has ever launched.
Every graded reply exact; the four-grade suite served by pid 37130 across a
`/clear` per turn; sidechain and cache zero on all five results; the pool never
halted; nothing of the target binary surviving and no epoch tree left.

The drain at 2.1.226, over the daemon's own evidence mirror: 5 reachable
post-answer arrivals, max 223 ms against the pooled 1000 ms bound, no
unclassified row kind. The per-version fit is 500 ms and is published under
`per_version_recommendations_not_to_be_shipped` and used by nothing -- the
tool's three runs at this version fitted 250, 500 and 500 ms within half an
hour on one host, and the smallest is below
`POST_MARKER_CATCH_WINDOW_FLOOR_MS = 438`.

`every_promoted_range_is_the_sentence_its_promotion_receipt_generated` is what
keeps the two together: it requires the receipt to exist for the version the
range is tested through, to carry `verdict: promotable` against a real Claude
with every check passed, and to hold this exact string. Proved red both ways:
one number changed in the shipped sentence, and the receipt's verdict set to
`rehearsal`.

No ledger ordinal was spent -- `pmux ask` reserves none. The digest is
unchanged at 439e48533a77679d15bcc24a5a555366dcf426131cc8a0ae1e2c105afb167153,
`consumed: 85, remaining: 15`, and `phase0.py budget` now reports the five turns
this receipt cost under `real_turns_outside_the_ledger`.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c110"></a>

### 110. The Path B verdict, with two of the five criteria NOT MET and a clippy error five parallel reviewers read past because none of them ran the gate

*2026-08-10*

`````text
Gate A at `<c109>`, on a settled tree: **61/62 in 34.9 minutes**, sole red
`gate_f/linux_docker_self_tests`, which ran 111 tests and failed exactly the one
that IS debt row C6. `source_unchanged: True`, and the digest is fresh rather
than inherited -- `fcf329ec...82376cd` over 950 files before, after, and again
when recomputed independently once the run had finished.

Criterion 4 is MET. Criteria 1 and 5 are NOT, and the document says so in the
heading rather than in a footnote.

Gate A at `<c108>` was **60/62**, not the 61/62 this round was briefed with:
`rest.starts_with(|c: char| c == '-')` is `manual_pattern_char_comparison`, and
the `gate_a/rust_clippy` cell runs clippy with `-D warnings`, which makes it an
error rather than the warning a bare `cargo clippy` prints. `git show
<c106>:...` does not contain the line; `git show <c108>:...` has it at 706.
Twenty-nine findings and five coverage statements were produced over that tree
and not one of the five reviewers ran the command that was already failing.

Of the 29: **28 confirmed as real, 1 not adjudicated, 0 refuted as non-defects.**
Five carry corrections -- the refusal-constructor count is 5 of 7 and not 6 of
8; `daemon_lost` is a failure and not a refusal, so its absence from a table
headed *what `ask` refuses* is not the defect the table's real omission is; the
U+0085 superset errs toward the caller and narrowing it would err toward the
instance, which the finding did not say; the MCP tool description is less
informative than the finding credits it with, not more.

**One suggested fix is refuted outright.** The head proof does accept any
non-empty prefix -- reproduced here, gate accepted the head `"W"` for `What is 2
plus 2?` and `(pastes, enters)` came back `(1, 1)`. But the proposed repair,
"a head shorter than the row means the composer did not wrap, so require
equality", is contradicted by a measurement already in this tree: the recorded
2.1.226 wrapping render is 114 characters on a pane with 118 available, because
the wrap broke at a word boundary and ate the space. Applying it would refuse
every prompt long enough to wrap. The correct repair needs a width model, and
column count and character count part company on the CJK prompts the live
verification already sends.

Three of the five clauses of `rendered_prompt_head_is_proven` were re-mutated
here after the gate: `!empty_cursor_position`, the revision/rows-changed pair
and `same_editor_geometry` each disabled in turn leave `pseudomux-service --lib`
at 415 passed, while the control -- disabling the head clause -- reddens two.
`gate_b`'s mutation cell carries
`scope_does_not_cover=crates/service/src/driver_io.rs`, so the number could
never have covered them. No completed `gate_b` exists at this head: the only
artifact from the Evidence phase is an 86-second enumeration at `<c107>` that
tested no mutant, and the last real receipt is 39 commits back at 93%.

Five of the 29 are rediscoveries of rows already open in
`docs/repo-review.md` -- the MCP surface at 13 of 16, the digest's missing
`evidence/`, the moved `TESTING.md`, the dead `.dockerignore` rule, the CLI
matrix at 11 of 13. The register is not the bottleneck; working it is. And the
register's own citations have rotted, which is the house bug class aimed at the
place defects go to be remembered.

The document is deliberately NOT listed in `docs/path-b.md` §0.0, and that
is finding 5.2 seen from the other side: adding a row inserts a line into
`docs/path-b.md` and moves all 37 unchecked citations that the unscanned half of
`docs/` holds into it. Close the scanning gap, then promote this file.

Ledger `consumed 85, remaining 15`, digest `439e4853...f167153`, identical
before and after. No live turn was spent.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c118"></a>

### 118. The register's own citation for a line `<c108>` deleted, and criterion 5 rewritten around what a total grader measured rather than what a partial one could reach

*2026-08-10*

`````text
`docs/path-b-verdict.md` §1 criterion 5 was written against a grader that could
check 62 of 132 citations. It now records what a total one found: all 130 in the
six linted documents graded, 47 line citations of a linted document repaired in
the half of `docs/` nothing had ever scanned, four grader defects that had each
been narrowing the coverage the name promised, and the one thing left — 38 of 55
gradable citations in source that do not land on what they name, with the
pairwise-anchor change that would halve the noise written, measured and
reverted for costing more than it bought.

The five rediscovered rows and the digest row are closed in place with the
commit that closed them, rather than left open under a paragraph saying the
register is not the bottleneck.

`docs/repo-review.md` was itself the example: `:98` cited `bin/pmux/src/cli.rs`
for a `strip_suffix` line `<c108>` deleted, and the claim it carried — that
`pmux` does one normalization step the facade does not — stopped being true in
the same commit, since both call `normalize_cli_prompt` now. Three abbreviated
`` `:NNN` `` citations are written out in full, and two of the three were rotted
the moment they could be checked: the MCP tool-name literal and the `cell` enum
had both moved.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c122"></a>

### 122. The 94 floor enforced for the first time against the scope that fails it, and a disposition for all 136 survivors keyed on something other than the line number that moved for 100 of them

*2026-08-10*

`````text
`scope=full` at `<c121>`: 1,653 enumerated, 504 unviable, 1,086 caught, 63
missed, 1,149 decided, **94%**, exit 0 in 10,443 s. Evidence `run.bbUDg3`.
`docs/path-b-verdict.md` §4 item 2 called this the single highest-value
measurement left; §7 is it, and §4 item 2 is annotated CLOSED rather than
rewritten. The prior run of the same scope scored 88 with 136 survivors, 105 of
them in the two files the gate cell excludes for wall time. So the 94 floor had
never been asked the question it claims to answer. It is asked now and it is met.

**Every survivor is dispositioned, and completeness is enforced rather than
claimed.** `evidence/mutation-survivor-register.json` holds 141 rows -- 65
KILLED, 16 EQUIVALENT, 47 ACCEPTED, 13 REMOVED -- one per mutant that survived
either run. `scripts/mutation_register.py check` runs inside
`scripts/gate-a-mutants.sh` on every run at either scope and refuses one that
produced a survivor the register does not hold, or where a mutant the register
calls closed survived again. It does not refuse a survivor since caught: those
print as `retired_survivor=` so the row can be pruned, because refusing them
would make closing a survivor break the gate. Both refusals were proved against
the real outcomes before the checker was trusted -- five rows removed names those
five and exits 1 with the score still printed; the floor raised to 99 refuses on
the score with the register census still printed.

**The key is not `file:line:column`, and the rot is measured rather than
feared.** Of the 136 survivors this register ratchets from, 123 still exist at
this head and **100 of those are at a different line** -- a register keyed on the
tool's own name would have lost 100 rows to two commits of test-writing, which is
exactly the change that closes a survivor. The key is
`(file, function, genre, replacement, occurrence)`. A derived "where did the
clause go" hint was written and deleted: it matched on file, genre and operator,
so it offered `prompt_glyph_split` as the successor of a `validate_prompt`
clause. A field promising where a clause went and delivering any new function
with the same operator is this repository's own bug class, in the instrument
built to enumerate it.

**Five survivors nobody had seen, and what they measure.** Five mutants the prior
run counted as caught are missed here with no edit between the two touching any
of them, and that run's own logs name a real-PTY or real-rmux test as the SOLE
catcher of every one: three by the soak, one by the resize test, one by
`a_terminal_is_created_at_the_requested_geometry_and_not_the_rmux_default` --
which `docs/testing.md`'s list of three drifting tests does not name, so the list
is four. One of the five is not bookkeeping: `admit_claude_version`'s
`POOL_CELL == SessionCell::Minified` read as `!=` skips
`require_tested_for_minified_cell` for POOL admission, so the pool admits a
Claude version no promoted profile covers into the minified cell. It had a
catcher that was never really testing it.

**The floor is now per scope, and neither tier is aspirational.** `gate` keeps
94, the constant it has always been against a measured 95.50. `full` gets 93,
and the script does not hold that number -- it reads
`recorded_at.floor_percent` out of the register, beside the survivor list that
explains it, so the tree states the floor once instead of twice.
`PMUX_MUTANTS_MINIMUM_SCORE` may raise either floor and is refused below it;
both refusals exit 2 before a single mutant is built. 93 and not the measured 94
because the drift above is worth five mutants and the headroom at 94 is exactly
five: `floor_percent` is the same run with every mutant whose only failing test
was a measured drifter counted as missed, which is 17 of 1,086 and lands on 93.
What actually ratchets `full` is not the floor but the register, which refuses a
new survivor BY NAME whatever the score does.

63 survivors remain, all written down. 19 are one root cause -- `NativeService`
holds a concrete `Arc<PrivateRuntime>` and the only integration tests that build
one are `#[ignore]`d, so `wait_for_turn` read as `<` returns `DaemonLost` on the
first iteration of every turn with the whole suite green. The fix for that bucket
is a seam, not a test. 13 are marked `cheap` and are not closed here for one
stated reason: the floor has to come out of one named `outcomes.json` and each
round of closing costs another three-hour re-measurement.

`tools/phase0/phase0_lib.py` refused the new file rather than miscounting it --
the budget scan stops on a receipt it cannot classify, which is the one behaviour
a budget can have. It is classified by the `schema` field it declares rather than
by sniffing for a distinguishing key, because "this document happens to hold
`failing_conditions`" is a statement about a schema nobody wrote down.

Nine `docs/testing.md` line citations that this document's own edits moved are
relocated, each verified as identical text at the old line in HEAD and the new
line here. `docs/repo-review.md`'s three are left alone: that document is pinned
at HEAD `<c82>`.

cargo test --workspace: 70 binaries, 1202 passed, 0 failed, 51 ignored.
cargo clippy --workspace --all-targets -- -D warnings: clean.
ruff check + ruff format --check over the workspace: clean.
gate-a driver self-tests 56 passed; phase0 self-tests 261 passed.
shellcheck + bash -n on the gate script: clean. Residue audit: passed.
`git diff <c121> -- $FULL_GLOBS` is empty, so the tree the number describes and
the tree it is committed against are the same tree in every file it is about.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c123"></a>

### 123. A gate that owned the whole tree for three hours given a checkout of its own, and a receipt that names the commit it graded rather than leaving a reader to assume HEAD

*2026-08-10*

`````text
`run_gate.py` hashes the source before and after a run and reports
`source_unchanged`; `cargo-mutants` copies the tree it finds. So the two long
gates -- ~35 minutes and ~3 hours -- have owned the working tree against all
editing, which is the largest wall-clock cost in this project.
`scripts/gate-in-worktree.sh` puts the run somewhere else: `git worktree add
--detach` at an explicit commit, the gate there with `{worktree}`,
`{artefacts}`, `{validation}` and `{commit}` substituted, the checkout removed
when it ends. Nothing about what a gate measures changes; only where it runs.

The receipt is the point. A receipt from `run_gate.py` names no commit, and
this repository keeps paying for that: a 61/62 receipt quoted as current seven
commits later, a mutation score from thirty-six commits back briefed as this
tree's. This one carries `describes_commit`, `tree_sha`, `describes_head` and a
`reader_warning` sentence printed as the run's last line, and it hashes every
artefact, so the Gate A receipt inside it is identified by content and not by a
path something else can overwrite.

**Two things about WHERE a checkout sits change what the gate measures, and
both were measured rather than reasoned.** A fresh checkout has no
`clients/typescript/node_modules`, which `docs/testing.md` requires to already
exist: the first run of this script reddened four `gate_a` typescript cells for
a reason that was about the checkout. That is what `--prepare` is for, beside
`--release-build`, and with `cd clients/typescript && npm ci` those four are
green. Then `tools/linux-docker/evidence.py` opens every absolute path
component with `O_NOFOLLOW` and refuses any carrying setuid, setgid or the
sticky bit -- `/private/tmp` is mode 1777 -- so a run rooted under `/tmp` came
back with `gate_f/candidate_envelope_self_tests` at `FAILED (failures=14,
errors=9)` and `gate_f/linux_docker_self_tests` beside it, every message
reading *"JSON evidence parent has unsupported special mode bits"*. The chain
is now checked before the checkout, so that costs one second instead of fifty
minutes. It also settles the residue question by subtraction rather than by a
rule naming a directory: the audit scans one level under `/tmp`, and no
worktree can be there.

Refusals, each proved by running it: a work root inside the repository; a work
root whose ancestors carry special mode bits or a symlink; an unexpanded
`{placeholder}`, the rule the gate driver already applies to its manifest; a
commit that is not one; a missing `--receipt`. Exit 1 is the gate command's own
failure and exit 2 is the runner failing to start, and the receipt is written
either way.

`finalize` runs on INT and TERM as well as EXIT and reaps the gate child before
removing the checkout, because bash runs no EXIT trap for an uncaught TERM and
a killed run would otherwise leave a checkout registered and no receipt. That
path is not hypothetical: one run here was interrupted, and the receipt it left
records `exit_status 2`, `worktree_removed true`, no artefacts.

`bash -n` and `shellcheck` are clean, and both cells that run them now name
this file -- as does `docs/testing.md` section F, which
`test_run_gate.py::test_the_testing_document_names_the_shell_scripts_the_gate_lints`
holds to those two cells.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c127"></a>

### 127. Nine mutants that flipped between two runs of the same 1653, every one of them decided by a test that needs a real rmux rather than by the code it mutates

*2026-08-11*

`````text
A second full-scope mutation run at `<c125>`, evidence `run.7OrHM9`, read out of
its own `outcomes.json`:

    enumerated=1653 unviable=504
    caught=1085 missed=64 decided=1149
    mutation_score_percent=94 minimum=93

94 against the `full` floor of 93 and against `gate`'s 94. The register REFUSED
the run -- five mutants it called KILLED survived and two survivors had no row
-- and that refusal is the most useful thing the measurement produced.

Keyed the way the register is keyed rather than by line, the two runs enumerate
the SAME 1653 mutants and exactly nine flipped: five caught->missed, four
missed->caught. The five that flipped out are precisely the three "regressions"
plus the two undispositioned rows. And for every one of the nine, the sole
catcher in the run that caught it was one of three tests needing a real rmux
sidecar or a real PTY, or a bare timeout:

    bounded_soak::repeated_real_rmux_cycles_remain_resource_bounded_and_leave_no_residue
    private_runtime::a_terminal_resize_after_creation_is_delivered_and_not_silently_clamped
    private_runtime::a_terminal_is_created_at_the_requested_geometry_and_not_the_rmux_default

No product regression is in that list. The clearest case is `unix_now_ms`:
`Ok(0)` and `Ok(1)` are one function replaced by two different constants, and
between the runs the pair SWAPPED which of them a real-runtime test happened to
catch. The catcher decided the disposition, not the code.

All five are ACCEPTED with that measurement written into the row; the register
is 143 rows and `check` passes at `undispositioned=0 regressed=0`.

`floor_percent` stays 93 and stops being a carried number. It is the score with
every mutant counted missed whose only catcher is a measured drifter or a
timeout -- 15 of the 1085, giving 93 -- and the wider rule over whole
integration binaries selects the same 15 and the same 93, so the floor does not
depend on which derivation is used. The previous 93 came from a name-based rule
over one run; this one comes from a measured flip set across two.

One row is closeable and was deliberately not closed. `unix_now_ms` is a free
`pub(crate)` function of no arguments -- no seam problem at all -- and two clock
reads bracketing one call kill every constant replacement of it at once. Writing
that test edits `native.rs`, which is inside the gate's own `FULL_GLOBS`, and
would rot the measurement the row is recorded against.

`docs/path-b-verdict.md` gets section 8: this measurement, the live adversarial
run at 2.1.226 with every guard watched firing, the 2.1.227 drift, the eight
`gate_b` cells no receipt has ever covered, and what the done-gate does not
measure. Section 4 gets a re-ordered and re-costed list, in which the rmux issue
drafts stop carrying an open question: 0.10.0 exists, shipped 2026-08-05, and
both reported defects are still in its published source.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c128"></a>

### 128. The eight cells no receipt had ever covered, run at a named commit, and the gate mutation number that was guessed stale in the safe direction turning out to be 97

*2026-08-11*

`````text
`gate_b` had not been run since `docs/path-b-verdict.md` was written. Section 4
item 2 said so, section 5 said so, and the criteria script said so by name --
`8 manifest cell(s) were graded by no receipt named here` -- which is why
criterion 4 was NOT MET at 62 of 70 cells with no red cell anywhere in it.

It is run now, at `<c127>`, in its own pinned worktree: **8/8, exit 0**, 6,247 s.
The first attempt cost ninety seconds instead of two hours because the driver
fails closed on unresolved tool placeholders and named all four --
`{cargo_fuzz}`, `{nightly_cargo}`, `{nightly_rustc}`, `{cargo_mutants}` -- before
running a cell.

Its mutation cell retires the last stale number in section 7. Evidence
`run.ar6ndL`, `scope=gate`:

    enumerated=740 unviable=103
    caught=620 missed=17 decided=637
    mutation_score_percent=97 minimum=94

97 against 94, from a floor defended against a measured 95.50. Section 7.5
guessed that closing thirteen `v1.rs` serializer survivors had left this number
"stale in the safe direction"; that guess was right and is now a measurement.

With both receipts the done-gate reports `cells_executed=70` and criterion 4
**MET**, `NOT DONE 3/5, not met: 1, 3`.

And it will not stay MET. A pinned receipt is bound to a commit and a tree hash,
so this commit -- which changes only this document -- leaves both receipts
describing an ancestor and takes criterion 4 back to NOT MET with no cell having
changed. That is the binding working rather than failing, and section 8.4 states
it rather than leaving the next reader to discover that the document recording a
MET verdict is the thing that ended it. `--commit <c127>` re-reads the verdict
for the commit the gates actually graded.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c131"></a>

### 131. The register row the fix could not close until there was a commit to name, and the count in its title going from one character to four

*2026-08-11*

`````text
`scripts/path_b_done.py` refuses a CLOSED row whose `closed_by` is not a commit
in this repository, which is the right rule and the reason this is a second
commit rather than part of the first.

`verdict-1b-trailing-nel-is-deleted` is CLOSED by `<c130>`. Criterion 1 goes
from NOT MET -- `an OPEN defect in the Path B path` -- to **MET**, with
`defect_register_open=0`, `defect_register_closed=3`,
`defect_register_letters_reconciled=4` and `survivor_register_files_drifted=0`:
nothing in the mutation gate's `FULL_GLOBS` was touched by either commit, so the
143-row survivor register still describes this tree.

The row's title is rewritten rather than kept, because it carried the claim the
measurement refuted. It said U+0085 was *"the one character whose treatment
depends on where in the prompt it stands"*. It was four -- U+0009, U+000B,
U+000C and U+0085 -- and the number was never measured; it was the number of
characters the person writing the row was looking at. A register the done-gate
reads is the last place a count should be inferred, and the row now says so.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c132"></a>

### 132. The nine ordered checks run against 2.1.227, and the per-version drain fit landing at 250 ms again — 188 ms below the floor the catch window would need

*2026-08-11*

`````text
`tools/promotion/promote_claude_version.py --claude .../2.1.227`, verdict
`promotable`, every one of the nine checks passed, five real Sonnet 5 turns
(four grades at low, one at high) through `pmux ask` on one unchanging pid.
The ledger is byte-identical before and after -- `pmux ask` reserves nothing --
so `phase0.py budget` now reports 54 real turns outside it rather than 49.

The number worth reading is in `per_version_recommendations_not_to_be_shipped`:
2.1.227 fits **250 ms**, against `POST_MARKER_CATCH_WINDOW_FLOOR_MS = 438` and
the shipped pooled bound of 1000. Three runs at 2.1.226 fitted 250/500/500. A
fourth version's thin corpus has now landed under the floor twice, which is
`docs/version-drift.md` P1 reproducing rather than being argued: the drain this
receipt asserts against is READ from
`evidence/pooled-transcript-drain-macos-aarch64.json`, and the fit is published
to be looked at and not shipped.

Five reachable post-answer arrivals at 2.1.227, max **52 ms**, median 25 --
measured over the daemon's own evidence mirror, because the free corpus cannot
answer it: `measure_transcript_drain.py --corpus ~/.claude-1/projects --version
2.1.227` exits **5**, nothing to check, exactly as it did for 2.1.226 the day
that version shipped.

Committed alone. The profile that widens the range is a claim about what pmux
supports; this file is the measurement it would rest on, and the two are worth
being able to revert separately.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c133"></a>

### 133. The promoted range widened to a version whose every calibrated property was measured first, and a citation four lines from where the same edit moved it

*2026-08-11*

`````text
`PROMOTED_PROFILES` now reads `2.1.220..=2.1.227`, and `range_provenance` is the
sentence the receipt GENERATED rather than one written beside it --
`every_promoted_range_is_the_sentence_its_promotion_receipt_generated` binds the
two, proven able to fail twice: bumping the shipped ceiling to 2.1.228 and
moving one number in the sentence from 52 ms to 53 each turned it red, and both
were restored.

WHAT WAS MEASURED BEFORE ANYTHING WAS WIDENED, in `docs/2.1.227-compatibility.md`

Every version-keyed instrument, run at 2.1.226 and 2.1.227 within one hour on
one host, and **not one of them disagreed**: `claude --help` byte-identical at
73 options; 66 flag probes over a set derived from the two files that build a
Claude argv, every one accepted and the negative control rejected at both; the
effort array, the `{med:"medium"}`/`{ultracode:"xhigh"}` tables and the
unknown-value warning template identical in both bundles; `pmux start --cell
minified` reaching `state: ready` at both; the child's real argv, read from
`ps -Eww` rather than composed, identical modulo uuid and paths; a 24x80 PTY
replay of that argv giving the same cursor, the same two rendered rows below it,
the same `#afd7ff`/`#949494` menu selection, and the same five-row 1935-byte
rotated preamble; and `--strict-mcp-config` still removing the same outbound
fetch of the caller's account connector list.

The deltas are all in the derivation, not in the product. The MEASURED-site scan
went 16 -> 25 -> **44**, and the first version of that scan reported 31 because
it split production from test at the FIRST `#[cfg(test)]` -- which in
`claude_launch.rs` gates a production constant a thousand lines above `mod
tests`, so the `--effort` vocabulary, one of the four sites `version-drift.md`
sec.3.6 names by hand, was silently graded as a test. The flag probe's first
version read every accepted short flag as rejected because `-p` is a substring
of `--pmux-probe-sentinel`. Both are this repository's own bug class inside the
tools written to look for it.

THE CITATION THIS COMMIT MOVED, AND ONE IT INHERITED

The new `range_provenance` is four lines shorter than the old, so everything
below it in `compatibility.rs` moved up by four. `path_b_doc_citations`
caught `version-drift.md`'s `TestedCompatibilityProfile` citation immediately.
It could not catch the two in `bin/pmux/src/cli.rs`, which cite the same file
and are not in a linted document -- and those two were ALREADY off by three
before this commit, pointing at the `if` rather than at the message they quote.
Both are repointed at the lines that hold the strings.

Not established: a minor step, which is what trigger 5 exists for and what no
patch step tests. One release moving nothing is one datum.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c142"></a>

### 142. A hand-written receipt that named whatever HEAD happened to be when it was saved, for numbers produced by a working tree that was not that commit

*2026-08-11*

`````text
`evidence/screen-veto-cost-2.1.227-macos-aarch64.json` carried
`"commit": <c139>`, which is what `git rev-parse HEAD` returned at the moment
the file was written. The pmuxd that produced its 4,415 frames was built from
the working tree, which at that moment was <c139> plus every uncommitted change
this work consists of -- so the field named a commit whose code never ran, and
`<c139>` is precisely the commit whose behaviour the run refutes.

`scripts/gate-in-worktree.sh` exists because "a gate receipt names no commit,
and read beside a repository is silently taken to describe HEAD" -- and its own
README lists two occasions this repository quoted a receipt at the wrong commit.
This is the same defect in a receipt written by hand, which no driver was going
to catch.

Replaced by a `provenance` object that says what was measured (an uncommitted
tree), the commit that tree became (<c140>), the HEAD the run started from, and
a reader warning; the two binary sha256s were already there and are the actual
identity of the code that ran. `not_established` gains the fact that the
measured daemon predates <c141>.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c145"></a>

### 145. A survivor register that wrote KILLED for twelve mutants one campaign happened to catch, and the gate-scope campaign four hours later that missed four of them

*2026-08-12*

`````text
`KILLED` in `evidence/mutation-survivor-register.json` means CLOSED, and
`mutation_register.py check` refuses a closed row that comes back. The register
recorded at `<c143>` moved twelve rows there -- ten `ACCEPTED`, two
`EQUIVALENT` -- because the full-scope campaign caught them. `gate_b`'s own
mutation cell, at the commit carrying that register, reported
`register_regressed=4`: `pool/evidence.rs:96:47`, `pool/mod.rs:1311:48`,
`pool/mod.rs:1311:33` and `pool/mod.rs:957:48` all survived the gate-scope run
of the SAME tree. Scope changes which files are mutated and not which tests
run, so the only difference between the two runs is load and ordering -- and
three of the four are in the campaign's own drifter-only list, caught by a
real-resource target and nothing else.

All twelve are restored, each carrying a sentence naming the campaign that
caught it and why that is not a closure. The rule, stated once: a row moves to
KILLED when a named test kills it, not when a run happens to catch it. Both
`outcomes.json` re-checked against the corrected register --
`register_undispositioned=0`, `register_regressed=0`, in `full` and in `gate`.

Also recorded: `{python}` is the gate driver's own `sys.executable` and nothing
checks it can import what the cells import, which produced two red `ruff` cells
saying "No module named ruff" twenty-six cells into a run whose PATH `ruff` had
just passed; and the pinned-worktree runner's missing disk pre-flight, which
killed a second run at cell 39 with "No space left on device" after ten
product-shaped red cells.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c146"></a>

### 146. A currency check that compares whole files and cannot see a test at all, and the one KILLED row a deleted test made false while criterion 1 stayed green

*2026-08-12*

`````text
`check_survivor_disposition` decides the survivor register's currency by
`git diff --name-only` over `FULL_GLOBS`: one comment moved in `driver_io.rs`
and all 144 rows are declared stale. MEASURED on the certification that just
finished -- 11,765 s (3 h 16 m) over 1,661 mutants at `<c143>`, re-verifying a
window whose 48 hunks touch 17 functions and 75 mutants.

The worse half is not the granularity. The check watches only the product globs,
so a test change is invisible to it. Reproduced rather than argued, in a
throwaway clone: delete
`an_agent_start_emits_the_agent_and_omits_every_path_the_agent_supplies`, apply
the register's KILLED `<impl Serialize for StartSessionRequest>::serialize` `*`
-> `/` mutant by hand, and the gate's own three test packages run 34 targets and
853 tests green -- the row is false -- while `path-b-done.sh --only 1` reports
criterion 1 MET at `survivor_register_files_drifted=0`.

`docs/register-currency.md` states the three rules that replace it, the fallback
for the 21 of 1,661 mutants that have no function (every one a module-level
`const` initializer, at eight sites), and what forces a full run anyway. It also
records three things the reproduction cost: the gate does not preserve the
per-mutant logs the catching test is recovered from, so the register's own
campaign can no longer be distilled; a filtered run whose baseline the filter
narrowed derives a 20 s timeout from a 1.5 s baseline and reports false
timeouts; and `bounded_soak` flaked under four-way parallel load and scored the
surviving mutant as caught, which is the one-directional error the register's
own floor derivation names, observed in the act.

For the window this document sizes, the escalation trigger fires and the
three-hour run was right. That is the point of writing the trigger down.

No behaviour changed.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c147"></a>

### 147. A currency check that called all 144 register rows stale for one moved comment, and the eighty-three KILLED rows that now name the test whose deletion would make them false

*2026-08-12*

`````text
`scripts/path_b_done.py` decided whether the survivor register still described
the tree by running `git diff --name-only` over `FULL_GLOBS` and refusing on any
answer. One comment moved in `driver_io.rs` declared the whole register stale and
demanded the full campaign again -- 11,765 s, measured -- and the check could not
see a test change at all, which is the thing that actually falsifies a KILLED
row. `docs/register-currency.md` §4.1 had already built that hole in a clone:
delete one test, and criterion 1 reported MET at zero drift over a row that had
stopped being true.

`scripts/register_currency.py` decides it per row instead. Rule 1 intersects
`cargo mutants --list --json`'s item spans at the judged commit with `git diff
-U0`'s hunks -- 4.4 s, not the 86 s the design estimated, and skipped entirely
when no mutated file moved. Rule 2 reads `caught_by` off each KILLED row and
invalidates on a change inside the catching test's own span, which is why adding
a test still costs nothing. Rule 3 carries the seven escalations §5 states plus
the four this implementation's own limits add, and the refusal prints the
filtered command that would refresh exactly the stale set -- derived by that
command, not copied into it.

The same clone, the same deletion: NOT MET, 13 rows in one function, naming the
test, its target and the remedy. One word changed in one comment inside
`active_editor`: 3 rows, one function, 6.4 s to decide and about four minutes to
re-decide. All 83 KILLED rows now name a catcher, distilled from a 274-mutant
filtered run over their 35 functions -- 70 by `--lib`, 8 by `fake_uds`, 5 by
`v1_wire`, none undetermined and none by a measured drifter.

Three things this found on the way. `cargo mutants -F` let six `StructField`
mutants of a function nobody named through a filter built for one other function,
so every receipt records what the filter reached beyond what it was given. A
filtered run's own baseline can be red for its own reasons -- `bounded_soak` lost
its `rmux.sock` at cycle six -- so the baseline is attempted twice and every
attempt goes in the receipt. And inserting a line into `driver_io.rs` rotted six
Path B citations, which turned that baseline red and stopped the run: the guard
that refuses to grade a red tree, working.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c148"></a>

### 148. A filtered run that handed cargo-mutants one shared target directory and graded 101 of 291 mutants against the previous mutant's binary, and the thirty-five KILLED rows that named the wrong test because of it

*2026-08-12*

`````text
The acceptance test for `docs/register-currency.md`: the register recorded at
`<c125>`, the new rules applied across `<c125>..<c143>`, and only the stale
set re-decided -- at the one commit where the full-scope campaign's answer also
exists. It found a defect, and the defect was in the harness this work shipped
rather than in the rules it was written to test.

`scripts/mutation_refilter.py` exported `CARGO_TARGET_DIR` into the environment
`cargo mutants` inherits. That tool copies the tree once per worker and needs
each copy to own its `target/`; one shared directory makes four workers
fingerprint the same package path into the same place, and cargo then reports
`Fresh pseudomux-service` for a source it has just rewritten. 101 of 291 mutants
were graded that way, 99 of them reported CaughtMutant, and three of those
survive a hand-applied patch with 472 lib tests passing. `run.bbUDg3`, a
full-scope campaign, has 0 of 1,653 -- `scripts/gate-a-mutants.sh` sets that
variable for its probe and its candidate build and for nothing else.

Withheld from that one environment, and guarded: `rebuilt_in` reads each
per-mutant log for its own crate's `Compiling` line, the crate derived from the
mutated path against the workspace manifest, and the tool refuses rather than
writing a receipt for mutants the tests never saw.

`evidence/mutation-filtered-run-killed-rows.json` was produced before the fix and
is the only source of `caught_by` for the register's 83 KILLED rows, so it was
re-derived: 28 of its 274 outcomes were wrong and 35 of the 83 rows named the
wrong test. No disposition moved and no KILLED row's mutant survived.

With the harness fixed the filtered answer agrees with the campaign on 287 of
291 mutants, and on 290 of 291 against the register after its own drift audit,
in 38 m 27 s against 3 h 16 m 05 s. All four disagreements run one way -- the
campaign recorded caught, this run records missed -- three are mutants that audit
had already reverted, and the fourth, `ScreenShape::of` with `revision != 0` read
as `== 0`, survives 31 targets and 854 tests applied by hand: a thirty-seventh
survivor the campaign counted as caught.

The rules themselves named 1 of the 21 register rows that had genuinely stopped
being true over that window, and escalated twelve times, which is what covered
the other 20. Every one of the 21 is a survivor row that had since been caught --
none is a KILLED row surviving again -- and 18 of them fell to eight test
functions one commit added. Rule 2 watches the test that decided a KILLED row; a
survivor row names no test, and a test being ADDED is what falsifies it. That
hole is now counted on every run as
`survivor_register_rows_a_new_test_could_falsify` instead of being left in a
document, and section 9 measures what closing it would cost.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c149"></a>

### 149. A census receipt one commit added to `evidence/` and no turn budget could classify, and a README sentence whose two ratios read as the ledger figure that document refuses to print

*2026-08-12*

`````text
Gate A at `<c148>` came back 59 of 62 with two cells red beyond the deliberate
Linux one, and both were about this work rather than about the product.

`evidence/mutation-enumeration.json` landed at `<c147>` without a line in
`NO_TURN_RECEIPT_SCHEMAS`, so `real_claude_turns_outside_the_ledger` refused to
classify it and took `phase0_self_tests` and `gate_driver_self_tests` down with
it. It is a `cargo mutants --list` over committed source and reaches no model,
which is exactly what that tuple records, so it is named there.

`evidence/README.md` gained the sentence "287 of 291 agree with that campaign as
written and 290 of 291 with the register after its own drift audit", and
`test_the_evidence_readme_states_no_budget_figure_and_its_command_derives_one`
scans that whole file for `N of M` because the file once published "47 of the
authorized 100 global attempts are consumed" against a ledger at 85. The shape
is the point and the sentence carried it, so the sentence says all but four and
all but one instead.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c153"></a>

### 153. A receipt for 70 graded cells written where the run that produced it is reaped, and the criterion that answered cells_executed=0 without naming the file it wanted

*2026-08-12*

`````text
`bash scripts/path-b-done.sh --gate-a-receipt .context/gate-a/receipt-<c149>.json
--gate-a-receipt .context/gate-a/receipt-<c149>-gate-b.json` reports criterion 4
NOT MET with `cells_executed=0` over receipts recording 62 cells / 61 passed and
8 cells / 8 passed, `source_unchanged: true`, sole red the deliberate Linux cell.
The refusal is right: only a pinned-worktree receipt carries `describes_commit`
and finds its inner gate receipt by digest, and a bare `run_gate.py` receipt names
no commit at all. What was missing is the pinned receipt. The certification passed
`--receipt` an ephemeral path, `gate-in-worktree.sh` wrote exactly where it was
told, and the file died with the work root; none exists on disk for that commit.
`$TMPDIR/pinned-227/` and `$TMPDIR/pinned-gate/` hold seven more pinned receipts
from earlier runs, and SIX OF THE SEVEN already name artefacts that no longer
exist -- so this is not a near miss that happened once.

`--receipt` now defaults to `.context/gate-a/pinned-receipt-<label>-<commit>.json`,
and a path that cannot outlive the run it describes is refused before the checkout.
The temporary roots are ASKED FOR rather than listed: `tempfile.gettempdir()` once
as the environment stands, then again with `TMPDIR`, `TMP` and `TEMP` removed and
its cache reset, which is what refuses `/tmp` on a host whose shell points at
`/var/folders`. `--work-root` is the third, and it is this run's own. Each refusal
was driven until it fired, and each names the root it matched rather than claiming
a general durability test:

    --receipt $TMPDIR/pinned/gate-a.json  -> under /private/var/folders/.../T
                                             (the temporary directory this
                                             environment names)
    --receipt /tmp/pinned/gate-a.json     -> under /private/tmp (the platform
                                             default temporary directory), with
                                             TMPDIR set elsewhere
    --receipt $WORK_ROOT/gate.x/tree/r.json -> under --work-root, whose checkout
                                             this run removes, with a --work-root
                                             under $HOME so nothing about the path
                                             is temporary except that
    --receipt $REPO/docs/receipt.json     -> inside the repository at a path git
                                             does not ignore

That last one is not fastidiousness: `path-b-done.sh` exits 2 with no verdict at
all from a dirty tree, so a receipt written at a tracked path costs criteria 2 and
5 as well as itself. It is asked of `git check-ignore`, so the same file under
`.context/` is accepted -- the rule is about tracking, not about a directory name.

The evidence moves with the receipt. `run_gate.py`'s receipt and the two logs are
copied to `<receipt>.evidence/`, every copy compared to its original by digest, and
the COPIES are what the pinned receipt hashes; `origin` keeps the work directory's
path so a reader who kept it can tell the two apart. Measured end to end: a real
run at `<c152>`, work directory deleted afterwards, all three recorded digests
recomputed from the durable copies. When the copy cannot be made the receipt still
lands with `evidence_durable: false` and `evidence_fault` naming the failure, and
`reader_warning` says the files will not outlive the work directory. Driven by an
artefact its producer left at mode 000.

Criterion 4's refusal now prints a remedy: the path a pinned run for the judged
commit would write, the phases nothing graded, the command that would produce them,
and any pinned receipt already on disk for this commit that was not named. The path
is obtained by RUNNING `gate-in-worktree.sh --print-receipt-path` with the same
`--commit` the printed command carries, so the naming convention has one author;
`--phase` is derived from the missing cells, so it narrows as receipts accumulate.
The two preparations are not derived and are named as the third copy they are:
nothing declares them in a form a program can read.

`tools/gate-a/tests/test_pinned_worktree.py` drives all of it over a one-commit
repository it builds under `target/` -- not under a temporary directory, because
`--work-root` refuses a sticky path component and `--receipt` now refuses a
temporary one. It is here rather than under `scripts/tests`, which
`crates/service/tests/register_currency_self_tests.rs` makes a `pseudomux-service`
test target and therefore a cost paid once per mutant.

Two defects found while building it. An artefact at mode 000 took the receipt from
"evidence not durable" to "not parseable JSON" -- `shasum` and `wc` both refused it
and the fields went out as `"sha256": "", "bytes": ` -- so the second failure hid
the first; digests and sizes are now `null` or a value, never empty. And the
copy-verification loop compared two empty digests as equal when neither side could
be read, which would have passed an unreadable artefact off as an intact copy.
Separately, `sha256_of` in `gate_receipts` could raise an uncaught `OSError` on a
present-but-unreadable artefact and end the run in a traceback rather than a
verdict.

NOT FIXED HERE, and it predates this work: `cargo test --workspace` is red at
`<c152>` on `pseudomux-rmux --test vendor_server_patch::every_gate_lane_runs_the_
derived_regression_module_and_no_file_restates_a_name`. Proven against a pristine
checkout of `<c152>` in a pinned worktree carrying none of these edits, receipt at
`.context/gate-a/pinned-receipt-vendorcheck-<c152>.json`. Two documents from the
draft work name a vendored patch regression the scan grants to exactly two files.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c155"></a>

### 155. A verdict document whose newest section called itself final, and the four commits at which its own workspace suite was red

*2026-08-12*

`````text
Section 11 records the third end-to-end certification. It is written under §10.5's
rule, unchanged: the Gate A and `gate_b` verdicts are NOT in this file, because a
pinned receipt is bound to a commit and a tree hash and any commit carrying a Gate
A number is a commit that receipt no longer describes. Run the script.

§11.1 is the finding. `crates/rmux/tests/vendor_server_patch.rs` excepted two
written-down files from its no-file-may-name-a-regression scan, and the two
upstream-facing documents `<c150>` and `<c151>` landed each spell one. Neither
commit ran the gate, so `cargo test --workspace` was red at `<c150>`, `<c151>`,
`<c152>` and `<c153>` and nothing said so. That is §0's lesson from the other
end: §0 was five reviewers who never ran the gate; this was four commits that never
ran it, and a drafting session that lands prose turns out to be able to redden a
test cell.

§11.5 is the question this round exists for, answered narrowly rather than
flattered: everything the repository carries is reproducible from a clone, and
criterion 4 is not, because `.gitignore:20` ignores `.context/` and the runner
refuses a receipt path git does not ignore -- a tracked receipt dirties the tree
and the done-gate gives no verdict at all from a dirty one. A pinned receipt is
therefore by construction never in the repository. What `<c153>` bought is
durability on the host that ran it, not repository visibility, and the section
says that instead of the wider claim.

§11.3 records the currency check answering in seconds, and why: nothing under
`FULL_GLOBS` changed across the eleven commits since `<c143>`, every `caught_by`
names a target whose derived sources are untouched, and there are no
`undetermined` catchers -- the one row kind that a change to any test-package file
would have staled. A demand for the full campaign would have been a finding about
the granularity rule; it is recorded as the thing that did not happen.

`## 10. The final certification of 2026-08-12` is now `## 10. The second
certification of 2026-08-12`, and the header's `last measured 2026-08-11` is now
2026-08-12. A section that grows a successor did not get to be the last one, and a
header that dates the document a day before its newest measurement is the same
promise-more-than-the-predicate shape aimed at a date.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c156"></a>

### 156. A certification section written in the past tense about a pinned run no receipt on this host records, and the nine durability self-tests that fail in the one cell that runs them

*2026-08-12*

`````text
§11.4 read "Run in pinned worktrees at the commit this section lands as ... so
both landed at the runner's own default". Four checks say otherwise, and all four
agree: no receipt under `.context/gate-a/` names `<c155>` or `<c154>`; the newest
one names `<c152>` and is the `vendorcheck` probe; the only worktrees under
`$TMPDIR/gate-worktrees` are two for `<c149>`, a commit that predates the file
§11.1.1 is about; and the raw gate receipts beside them were written before
`<c153>` added that file at all. The sentence was past tense about a run that
produced no artefact, at a commit that did not exist when the bytes were typed.

That is the house bug class aimed at a document's own tense, and §10.5 had walked
up to it and stopped one step short. §10.5 says no commit can carry its own Gate A
NUMBER. The statement that would have caught this is stronger: no commit can carry
any claim about its own gate run, including that one happened -- at the instant a
section is written there is no commit for a receipt to describe. §12.4 is what a
section may say instead, and it is an imperative and a derivation rather than a
report.

§11.1.1's repair is kept and is now measured by a second person rather than
inherited. In a real `git worktree` under `$TMPDIR`, `test_pinned_worktree.py` goes
from eight failures and one error at `<c155>` to nine passes. §11.1.1 also claimed
"sixty-five of sixty-five after", which the probe does not support and which this
commit removes: a bare checkout reddens four `test_documented_surface` cells that
want built binaries, and so does an idle-machine run whose `target/debug` a
concurrent workspace build is rewriting underneath it. Sixty-five is the main tree
on an idle machine, and the sentence now says which number belongs to which tree.

§11.7's "and then by the gate" is gone for the same reason as §11.4's tense.

Re-measured, none of it trusted from §11: `cargo test --workspace --no-fail-fast`
1226 passed / 0 failed over 73 binaries; `cargo fmt --all --check` clean; clippy
`-D warnings` exit 0; `ruff check` and `ruff format --check` clean; the residue
audit passed over 8 candidate executables. The register's currency answered
filtered and empty in seconds -- 0 drifted files, 0 stale rows, 0 stale functions,
0 `undetermined` catchers -- and the reason is re-derived rather than re-quoted:
the two files this commit touches are a markdown document and a Python test, and
neither is under `FULL_GLOBS` nor a source of any Cargo test target.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c160"></a>

### 160. A pre-push review that refutes its own round's only blocker, and the pinned receipt that refuses on a commit rather than on a digest

*2026-08-13*

`````text
Twenty-four findings from four parallel reviewers, re-run here at <c159>. Seventeen
confirmed, five overstated, two refuted. Verdict: READY, no blocker.

The blocking finding is a real display name in two test fixtures and a wrong severity
call. Its other two facts are already published: `America/New_York` against 160 commits
all stamped -0400, and `Claude Max` against
`docs/2.1.226-compatibility.md`'s `"subscriptionType": "max"`. The novel content is
one first name, in a tree that deliberately publishes a username 2,401 times.

The mechanism offered for its urgency is wrong. `path_b_done.py` compares
`source_digest` only on the branch for a bare receipt read against the tree in front of
it; a `pmux.pinned-worktree-run.v1` record takes `:940-1005`, which checks
`describes_commit`, `tree_sha` and artefact digests and never hashes the live tree.
Reproduced: the refusal at HEAD names the commit, not a digest. So a scrub is not
specially expensive -- any commit invalidates a pinned receipt, which is what the two
docs-only commits already did. The rule is re-pin last, not scrub first.

The established `<HOME>` count is 86 lines and 2,401 occurrences; every figure in
it was a `git grep -c`. Recommendation: leave the 2,385 in `evidence/` (provenance in
measured receipts) and `docs/2.1.226-compatibility.md` (load-bearing input to a
published sha256); scrub seven prose and template occurrences that carry no evidence.

C10 is genuinely invisible to criterion 1, whose register scope is derived from the
verdict document's own §1 letters -- and `docs/linux-handoff.md:1032-1040` already says
so, naming the §9.4 that holds C10. Downgraded from discovery to disclosed gap.

MEASURED at HEAD: `cargo test --workspace` exit 0, 72 result lines, 1,226/0/51;
linux-docker 111 with the one deliberate red; criterion 1 MET; criterion 4 MET at
<c156> with 70/70 cells. The <c156>..<c159> delta is two files, both `docs/`, named
by no manifest cell, driver or test.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c161"></a>

### 161. A commit log that names one defect per message and dies at the squash, and a redaction map that would have been a literal list written on the host with nothing left to find

*2026-08-13*

`````text
The 160 messages on this branch are unusual: each names THE DEFECT FOUND rather
than the change made -- "A modal classifier whose ten spare phrases no screen in
the suite could reach", "A prompt beginning `!` that switched the composer into
bash mode and ran the rest as a shell command on the host". The diffs are
recoverable from the tree; the reasoning that found them is only in the log, and
the squash for publication deletes all of it. `docs/defect-log.md` is that log,
every subject and body verbatim with its date, before anything is squashed.

GROUPED, AND THE GROUPING IS READ OUT OF THE MESSAGES. The repository already
names one class and machine-checks the count: 17 of the 160 messages use the
phrase "bug class", 7 number the instance in words from nineteen through
thirty-three, and `test_every_statement_of_the_bug_class_counter_spells_the_
same_ordinal` holds four Rust sites and the last such heading to one number.
That is section A, quoted from the tree. The other six sections come from the
subjects' own recurring vocabulary -- `gate` 446, `pool` 274, `prompt` 203,
`receipt` 172, `drain` 141, `evidence` 138, `transcript` 117, `composer` 81,
`ordinal` 71 -- which clusters into what a caller types, when a turn is over,
what the pool holds, what a cell can reach, what was written down, and what does
the measuring. Every entry is filed once, under the defect its subject names
FIRST; most subjects name two and no cross-listing is offered, because a second
class hand-assigned to 160 subjects is the set section A is about.

THREE SUBSTITUTIONS, DECLARED IN THE DOCUMENT, APPLIED BY A COMMITTED PROGRAM.
Hand-editing evidence is forbidden here and has already caught a receipt naming
whatever HEAD happened to be when it was saved; what makes a scrub different in
kind is that the transformation is committed and can be re-run.
`tools/defect-log/generate.py` is it, and re-running it produces the same bytes.

  1. Machine-specific identifiers -> `<HOME>`, `<REPO>`, `<WORKSPACES>`,
     `<USER>`, `<TMPDIR>`, structure-preserving so a path stays a path.
  2. Commit hashes -> this document's own entry ordinals. A token is rewritten
     only if git resolves it AND the commit is one of the 160: 56 distinct
     hashes substituted, and the 26 left alone are sha256 prefixes, session
     uuids, upstream rmux hashes and references that were already dead. No
     replacement hash is invented.
  3. Line numbers dropped from the 6 citations of a linted Path B document,
     keeping the path. `path_b_doc_citations.rs` fails the build on such a
     citation, and an archive reproducing six of them would arm that guard
     against a file nobody can edit. The document set and the suffix-resolution
     rule are read out of `docs/path-b.md` §0.0, the same table the guard reads.
     The other 148 distinct `path:line` citations, all into source, are untouched.

NOT ONE NEEDLE IS WRITTEN DOWN. `tools/defect-log/machine.py` asks the running
machine for all six -- home, login name, checkout path, the distance from one to
the other, the worktree's name, and the temporary directory -- and returns them
longest first, so a shorter needle cannot half-substitute a longer one. The
generator and the check share that one derivation, because two derivations of
one map is two maps and the second is the one that goes stale. `pseudomux` is
deliberately not derivable from it although it is a path component of this
checkout: it is also the crate namespace the log names on hundreds of lines, so
the ancestors between home and root are taken as ONE needle rather than one
each. `macos`, `aarch64` and `macOS-15.7.7` are not looked for -- the
compatibility profile is keyed on them -- and neither is `smithers`, a shipped
product module.

THE TEMPORARY-DIRECTORY RULE WAS FOUND BY RUNNING THE CHECK, NOT BY REASONING.
Taking both spellings of the temporary directory reported seven offences, every
one of them `/private/tmp` and every one substantive -- including the residue
audit that never observed a leaked root because BSD `find` will not descend a
symlink named on the command line, which is a finding about that exact path. A
temporary directory identical on every host of a platform names the platform.
The needle is now the difference between the configured one and the platform
default, which on this host is the hashed private path and nothing else.

The map table in the document DESCRIBES its inputs instead of spelling them,
because the file is scanned for them: a table that spelled its own inputs would
be the one live instance of the shape the checker refuses, sitting inside the
paragraph that declares it.

VERIFIED, not asserted. All 160 subjects, dates and bodies compared
byte-for-byte against `git log` put through the same map; entries numbered 1..160
exactly once; the index in commit order; the map idempotent over the messages;
zero derived identifiers surviving. Six checks in
`tools/gate-a/tests/test_redaction.py`, which runs in `gate_f/gate_driver_self_
tests`, each proved able to fail by mutating the artefact it guards and restored
byte-exact by sha256: the home path planted in the log, the placeholders stripped
from every quoted message, a nested placeholder, the log truncated to a stub, the
derivation narrowed to nothing, and the map ordered shortest-first. The
generator's three refusals over its class table were driven the same way.

Two checks were rewritten after being run rather than shipped as written. The
placeholder-presence check read the whole file and was satisfied by the map table
naming all five, so it now reads only the quoted messages. The scope sentence
promised a cross-listing this does not produce, and says so instead.

RECORDED, NOT FIXED, and it predates this: `cargo test --workspace` is red at
HEAD on `nothing_cites_a_path_b_document_by_line_number`, over eight line
citations in `docs/pre-push-review.md` -- the file HEAD itself added, in the
commit whose message reads "MEASURED at HEAD: `cargo test --workspace` exit 0".
Reproduced in a pinned worktree at a pristine checkout of that commit. This
change does not touch it and adds none of its own: the log contributed six
offences before rule 3 and contributes zero after. It is the same shape as the
two upstream documents that had been failing that scan since the day each landed.

Verified here: 71 test targets ok and that one failure, unchanged either way;
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`ruff check --no-cache`, `ruff format --check --no-cache` and the gate-a driver
self-tests all clean; `scripts/gate-a-residue.sh` passes at
candidate_executables=8 with no cache or `__pycache__` residue left behind.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c166"></a>

### 166. A finding that published the address it existed to keep unpublished, an archive whose tail boundary moved with the catalogue it bounded, and a preamble whose counts of itself no rule in the tree reproduced

*2026-08-13*

`````text
`docs/pre-push-review.md` §1.16 says three commits publish a non-noreply
address and that this is the one finding "cheap now and impossible later" -- and
then prints the address, in the paragraph reporting it. §1.1 of the same
document had already named this exact shape for the display name: "Quoting it
here would have republished the one thing the finding is about, in the document
that reports the finding -- which is how a review outlives the fix it
recommended." The batch that fixed §1.1 read the residue tables for PATHS and
never read the findings' own prose, so the one carrier that was a sentence
rather than a path survived. Elided now. The address is in no tracked file:
`git grep` for it is empty, where before it was one hit that no scan looking for
this machine's identifiers could ever have produced, because it is not this
machine's identifier -- it is the author's.

The same paragraph named its three commits by hash, and the squash it is about
destroys every hash on this branch. They are named by their ordinal in
`docs/defect-log.md` instead -- entries 40, 41 and 54 -- which is rule 2 of that
document's own declared map, applied here by hand and said so. MEASURED, for
scale: 555 occurrences of in-range commit hashes stand in 37 tracked files, and
only the two registers `scripts/path_b_done.py` validates will turn a gate red.

THE RESIDUE TABLE'S ONE OWNER DECISION IS NOW LABELLED AS ONE. The row keeping
`docs/2.1.226-compatibility.md` §4.1 gave as its reason that the arithmetic is
"unverifiable without the literal input". That reason stopped being true one
commit ago, when §4.1 was rewritten to state the input, print the command and
work the same digest through a second machine-free root. What is left is that
the digest IS the home path, one truncated sha256 away, published beside the
recipe -- a derived value that no map asked of the environment can catch, since
a derivation cannot know which functions of an identifier somebody wrote down.
Deleting it is a hand-edit of a `DATED RECEIPT`. The row says so and elides the
literal, so §4.1 is the only place it stands and the decision has one subject.

THE ARCHIVE COULD NOT BE BROUGHT FORWARD, AND THE REASON WAS ITS OWN BOUNDARY.
`carries_only_files_born_after` admitted a trailing commit as archive
maintenance if every path it touched was born after THE LAST CATALOGUED COMMIT.
That boundary moves every time the catalogue grows. The first time it moves past
the archive's own birth -- which is what cataloguing the five commits since
requires -- the archive's files stop being born after it, so the commit that
does nothing but regenerate the log is refused and the log can never be brought
forward again. The fix is one word of derivation: the boundary is the commit
that ADDED the file this generator writes, asked of git, and that does not move.
MEASURED both ways at this head: `<c161>` is the birth, `<c165>` (which
touches `README.md`) is refused, `<c162>` (which touches only the generator) is
admitted.

THE GENERATOR NOW SAYS WHAT THE SQUASH DOES TO IT. `origin/main..HEAD` stops
holding this history at the moment of the event the archive exists because of,
and the old refusal for that case read `1 commits in origin/main..HEAD against
166 classified`, which describes a miscount. It now names the squash and says
what to run instead, and the range is an argument, so the archive stays
re-derivable from the preserved pre-squash tip rather than becoming a program
that structurally cannot run.

THE PREAMBLE COUNTED 160 MESSAGES BESIDE 165, AND ITS OTHER NUMBERS WERE WORSE.
Four sites said 160. The vocabulary census -- `gate` 446, `pool` 274, twenty
figures in all -- is not reproducible by any counting rule tried against the
messages or against the document: whole-word and substring, over the messages
and over the whole file, every one disagrees with every figure. Nor is
"Seventeen ... over 21 lines; seven of those ... five of them". And the example
offered for the ordinal notation, `<c103>`, occurs exactly once in 9,900 lines:
on the line offering it as an example of what a message body contains. A number
typed into prose beside a growing catalogue is this log's own section A, sitting
in the paragraph that defines it. All of them are measured at generation time
now, under the rule the document states, from the messages that run catalogued;
the term SELECTION stays editorial and is labelled as such; the ordering is by
count; the example is the ordinal the messages cite most often, smallest first
where they tie, which makes it real as well as deterministic. Interpolated
numbers have widths nobody knows when the prose is written, so the paragraphs
carrying them are re-flowed to the width the rest of the document uses.

`classes.txt` gains rows 161-166: the archive itself (F), the generator defect
above it (G), the substitution map and the sealed ledger (G), the map whose
scope was two remembered locations (A), the worked refusal example (A), and this
commit (F).

PROVED ABLE TO FAIL, not asserted: the squash refusal fires on a short range and
prints the range and both counts; the birth boundary discriminates the two
commit shapes above in opposite directions; the example ordinal `<c152>` stands
on 8 lines of the regenerated document where `<c103>` stood on 1.

`cargo test --workspace --no-fail-fast` exit 0, 72 targets, 1,226 passed, 0
failed, 51 ignored, both citation graders ok. `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, `ruff check
--no-cache`, `ruff format --check --no-cache` all 0. Python: gate-a 79,
evidence_common 74, phase0 261, all green. `gate-a-residue.sh` passed,
candidate_executables=8. The generator is byte-deterministic across three
consecutive runs.
`````

---

## G. Instruments, gates and tooling

**The Gate A/B drivers, verifiers, harnesses, citation graders, mutation machinery, handoffs and upstream reports.**

An instrument defect is worse than a product defect: it publishes a wrong number someone believes, or blames the product for something it did not do. Two verifier defects here would have printed a loud and entirely false "pmux dropped a byte" on a clean run; a per-binary harness reported that every one of its targets passed while enumerating zero of them. Read this group as the reason the other six can be trusted at all.

43 entries.

<a id="c2"></a>

### 2. Gate A passes 75/75; characterize pmux overhead; fix 4 defects the capture found

*2026-07-27*

`````text
First complete end-to-end execution of the 75-cell deterministic manifest.
Receipt at evidence/gate-a/receipt-20260727.json, sha256
303d92a79b54614f6f5253936010bf69ce739ae07bb71795ef64d7e9f635ecb4, source digest
identical before and after the run, host macOS-15.7.7-arm64, all eight release
binaries frozen at mode 0500 with recorded digests.

Fuzz ran at its mandated spec for the first time: 50,000 iterations across
transcript_jsonl, transcript_cursor and native_frame, zero crashes. The previous
maximum ever executed was 5,000. That was the last never-executed deterministic
lane.

tools/gate-a/run_gate.py is a new minimal driver that RECORDS rather than gates:
continue-on-failure is the default and every cell's outcome is captured. The
existing 4,304-line candidate_envelope.py structurally cannot emit a failing
receipt -- it raises on the first non-zero cell before the report is written --
which is why this project had produced no Gate A number at all. The new driver is
533 lines with 26 self-tests, and it diagnosed five defects on its first run.

Captures scored 12/75, then 70/75, then 75/75. What they found:

- {cargo} resolved through the ~/.cargo/bin/cargo symlink to the rustup shim,
  which dispatches on argv[0], so every cargo cell died with "unexpected argument
  '--all'". 59 of 63 first-capture failures. Fixed by resolving the parent while
  preserving the invoked name; regression ToolNamePreservationTest.
- libFuzzer deadly signal in transcript_cursor at execution 47,755. The target
  asserted no framed line ever ends with CR, but cursor.rs:188 strips exactly one
  trailing CR -- CRLF normalization and nothing more -- so a line whose source
  ended in several CRs legitimately still ends in CR. transcript_properties.rs
  models the carriage return as a boolean per record, and spec.md's CRLF
  normalization governs the prompt path, not the transcript path. The tracked
  fuzz target over-specified a contract the cursor never promised. It surfaced
  only because the lane ran twice: the first run added 1,223 corpus units.
  Assertion corrected, crash seeded as a tracked corpus entry, and minimized into
  cursor_strips_exactly_one_trailing_carriage_return.
- cargo-fuzz creates fuzz/artifacts and fuzz/target relative to its own manifest
  regardless of CARGO_TARGET_DIR, and the final residue cell correctly failed on
  them -- Gate A was structurally unpassable whenever fuzz ran. Now pruned, but
  fail-loud if non-empty, since a file there is a crash written outside the
  evidence root.
- The driver never applied umask 077 although docs/testing.md:124 mandates it, so
  tsc emitted 0644 into the validation stage and dist-stage verify rejected the
  tree, failing all three TypeScript cells. Fixed; regression UmaskTest.

Performance is now characterized rather than asserted. Measured against
pmux-test-claude, which has zero model latency, so every number is pmux's own
overhead: launch 37.6 ms p50, execution 21.0, completion 170.0 (of which 150 is
the configured drain), close 32.4, turn total 191.0. Non-drain overhead is 41 ms
p50. Real-world cost is 41 ms plus a 500 ms editor-fence floor plus the drain, so
about 2.5 s with the conservative fallback and about 3.1 s against the 2,354 ms
drain observed with real Claude. pmux's own machinery is roughly 1.5% of a turn;
the rest is deliberate, calibratable margin. The record stays a diagnostic with
no host-speed threshold, and declares the four boundaries it cannot observe.

One defect is open, not fixed: C8 in docs/current-state.md. pmux-rmuxd's
owner_eof_reaps_a_hup_term_ignoring_pane_tree_and_surfaces_lease_loss failed once
inside the capture with ECHILD from /bin/ps and passes 3/3 in isolation. The
75/75 receipt therefore covers a run in which that test happened to pass. It is
recorded with its reproduction conditions rather than re-run until green.
`````

<a id="c10"></a>

### 10. Gate B calibration suite: nine graded prompts and an offline verifier

*2026-07-28*

`````text
The 24 live turns captured so far were all effort=low, no tools, trivial
prompts, and all showed a zero late-arrival gap. That is absence of evidence on
exactly the cases that matter. These nine prompts grade from a trivial control
up to 40-line CJK/emoji poems with three sequential shell calls, and make
Claude hash its own poem text so verify_calibration.py can recompute the digest
independently. A mismatch means the bytes pmux read are not the bytes Claude
hashed, so the hash is a checksum over the whole pipeline, not just a timer.

Two defects in the verifier were found by review before it ever ran, both of
which would have printed a loud and entirely false 'pmux dropped a byte'
verdict on a correct run:

  - the newline byte-variants were applied to the transform's OUTPUT. Appending
    a newline does not commute with reversal -- reverse(P + "\n") is
    "\n" + reverse(P), neither candidate -- so the heredoc-fed reversed
    grades (prompts 05 and 06) were unreproducible by construction.
  - text blocks were joined with "\n"; crates/claude/src/engine.rs:843
    concatenates them with an empty separator. A chunked terminal message, which
    is exactly what the long grades provoke, hashed to a digest Claude could not
    have reported. The existing test asserted the wrong separator and so
    defended the bug.

The hash test is one-sided by nature: a match is strong evidence, a mismatch is
inconclusive because a live model cannot be forced to feed a shell exactly the
bytes it later reprints.
`````

<a id="c12"></a>

### 12. Grade attempts by prompt content hash, not argv position

*2026-07-28*

`````text
prompt_suite_index is assigned by CLI argument order, so it identifies a
position rather than a prompt. Resuming a campaign at grade 03 -- which is
exactly what happened after a permission stall consumed ordinal 36 -- restarts
the numbering at 1, and every attempt silently acquires the label of a prompt it
never ran. The published by-grade table was shifted two grades: 08 and 09 read
'attempts=0' while both had in fact succeeded.

The checker already detected this and appended a note saying the label 'may not
describe what was actually sent' -- but notes are written at eight sites and
rendered at none outside --json, so a wrong table printed silently while the
evidence to correct it sat in the same record. Content hash is now authoritative
and the index is a fallback that says so; grade_source records which was used.

Campaign result, attributed by content: 9 grades, one sample each, output
scaling from 4 to 2385 tokens across 0 to 3 sequential tool calls plus CJK and
emoji. Late-arrival gap never exceeded 1 ms against a ~2350 ms drain. Hash
verification: 7 requested, 7 independently reproduced, 0 mismatches.
`````

<a id="c14"></a>

### 14. Gate C Linux handoff, and correct the ledger's two ordinal spellings

*2026-07-28*

`````text
Gate C is cut from the macOS thread but is not abandoned: it will be run on a
Linux server by an agent starting cold. docs/gate-c-linux-handoff.md is written
for that reader -- what macOS evidence does and does not transfer, the D6 and C6
preconditions, the lane's entry points, the traps that would otherwise be
rediscovered by spending irreplaceable budget, and what result should make them
stop and report rather than push through. It anchors every citation to commit
5326287 and tells the reader to trust the quoted strings over the line numbers
if HEAD has moved.

The document went through three passes: draft, independent critique, repair.
Both first-pass drafts opened by certifying themselves -- 'Everything checks
out', 'Verified every citation' -- and each carried five or more verified
errors, including a de-scope total stated as 1,214+450+45=1,664 (it is 1,709), a
line-count table published as 16,195 that sums to 15,961, and the claim that the
campaign discharged 'no CJK or emoji prompt was ever sent'. It did not: all nine
prompt files are pure ASCII English asking Claude to WRITE CJK and emoji, so the
non-ASCII response path is tested and the non-ASCII input path is untouched.
Self-certification was worth nothing; the independent pass was worth a lot.

Also corrects a claim I committed two commits ago. evidence/README.md said
ordinal 30 was the ONLY record spelling the field global_attempt_ordinal. It is
14 records: the spelling changed AT ordinal 30 and every reservation since uses
it, so it is the current format and global_attempt is the legacy one. A scan
that knows only the old spelling stops at 29 and reports the budget fourteen
attempts cheaper than it is -- the same class of error the note was written to
prevent, one ordinal after writing it.
`````

<a id="c15"></a>

### 15. Instrumentation fix plan: 8 defects in the tools that judge pmux

*2026-07-28*

`````text
Seven defects were found on 2026-07-28 and four were in the validation tooling,
not in pmux. An instrument defect is worse than a product defect: it publishes a
wrong number someone believes, or blames pmux for something it did not do. Both
verify_calibration defects would have printed 'pmux dropped a byte' on a clean
run.

An adversarial audit across six dimensions raised 52 findings; 19 survived
three-lens refutation and collapse to 8 distinct defects. Four burn an ordinal or
corrupt published evidence on the next live run:

  - the source-identity fence compares the WHOLE identity, including .git
    ctime_ns, so an unrelated editor git-poll rewrites a paid, SUCCESSFUL Claude
    turn to status=failed and discards its drain sample. This is what cost
    ordinal 32 today.
  - late-row classification is gap <= 0 in BOTH implementations, while
    v1.rs:1313-1319 defines the rule as a band of one actor poll interval (20ms,
    actor.rs:80). One noise millisecond deletes the absence-of-evidence banner
    and republishes it as a measured 1ms lower bound with 1999ms of headroom.
  - a grade that requested three hashes and delivered one is tallied 'match';
    there is no expected-label check, so the transform proofs that are the entire
    reason grades 05 and 06 exist are not counted as absent, merely not counted.
  - expects_hash is read off the entry that did not produce the grade, so a real
    proof-of-work failure is filed as 'this prompt did not ask for a hash'.

Section 4 is the part that matters most and is deliberately not optimistic. Four
things stay unverified after every fix: the band constant is asserted from a
constant the audited product self-reports; the 'second independent
implementation' reproduced the first one's bug rather than catching it, so a
shared misreading of the protocol is invisible to both; the hash oracle recomputes
a digest over the poem text pmux itself captured, so it proves internal
consistency of one artifact and not fidelity to what Claude emitted; and the
late-arrival distribution is empty by construction, so no amount of better
statistics yields a defensible drain number -- the missing work is a prompt that
PROVOKES a late row.

Not yet applied. This is the work list.
`````

<a id="c16"></a>

### 16. Verifier: noise band, partial-hash state, and the right expects_hash entry

*2026-07-28*

`````text
Three of the four FIX NOW items from docs/instrument-fix-plan.md. All three are
the same class: the number was defensible and the presentation invited a wrong
conclusion.

LATE-ROW BAND. Classification was gap <= 0. crates/protocol/src/v1.rs:1313-1319
states the real rule: the difference straddles zero by a few milliseconds --
negative by the parse-and-analyze interval, positive by the interval between the
confirming poll's stability measurement (monotonic) and the completion timestamp
read (wall clock) -- and a gap within one actor poll interval of zero reads as
no late rows. The interval is 20ms (actor.rs:80).

This is not theoretical. Re-running the 2026-07-28 campaign under the corrected
rule flips the published conclusion:

  before: no-late-row=6 late-row=4, headroom 1999ms, no banner
  after : no-late-row=10 within-noise-band=4 late-row=0, headroom null, banner

Four +1ms clock artifacts had been suppressing the absence-of-evidence banner and
republishing themselves as a measured 1ms lower bound with 1,999ms of apparent
proven margin -- an invitation to cut transcript_drain_ms on noise. The honest
reading of that campaign is zero late rows in 10 of 10 attempts, and the banner
that says so now prints. I reported the 1ms as 'the first non-zero gap ever
measured'; it was measurement noise.

Band gaps get their own bucket rather than being folded into zero, so 'we saw
nothing' and 'we saw only noise' stay distinguishable. headroom_ms is null unless
the max exceeds the band, because headroom against an unmeasured worst case is
not headroom.

PARTIAL HASH. hash_overall was 'match' if every hash PRESENT verified, with no
expected-count check anywhere. A grade-06 reply carrying one correct
SHA256(poem): scored a full match while both transform proofs were absent --
they were not counted as missing, they were not counted. Prompts now declare
expected_labels, parsed by the same rule that reads the reply, and a strict
subset is 'partial'.

EXPECTS_HASH. Read off suite_entry, which is None whenever grading fell back to
the index, so an index-graded reply with no hash at all was filed as 'this
grade's prompt did not ask for a hash' -- a positive claim about a prompt the
tool had just admitted it could not identify. One edit to the prompts directory
would have turned every missing hash in the tree into not_applicable at once.

Also suppresses p95 below 20 samples: nearest_rank(_, 95) is index
-(-95*n//100)-1, which is the last element for every n <= 19, so p95 and max were
one number printed twice and read as two independent statistics.

The corrected banner caught a bug I introduced with it: the old text said
'observed a zero or negative gap', which stopped being true once band gaps
counted as no-late-row. It now names the band and how many were inside it.

48 tests pass, ruff clean.
`````

<a id="c18"></a>

### 18. Gate the claim, not the environment: five instrument fixes and two dispositions

*2026-07-28*

`````text
Applies the remaining FIX NOW and section-2 items from
docs/instrument-fix-plan.md, plus the C8/C9 dispositions and the Gate B record.

THE SOURCE FENCE (plan 1.1). _verify_candidate_unchanged compared the WHOLE
source identity, which embeds revision_identity.repository_control: nine .git
nodes with ten stat fields each, including ctime_ns. Any external git status
moves it, so a ~1.3s window failed ~30% of the time and an ~11s window
essentially always -- AFTER reservation, so each failure spent an irreplaceable
ordinal. Two changes, and only one is a trade:

  - GATE THE CLAIM. Compare what the evidence actually asserts: the content
    digest, file count, the digest tool's own identity, and the revision facts a
    reader would reconstruct the tree from. repository_control is still RECORDED,
    just not gated on. revision_identity_sha256 is excluded too -- it is a hash
    OF repository_control, so keeping it would have quietly reintroduced the same
    sensitivity. To slip through, a mutation must leave content, commit,
    porcelain status, binary diffs and tool digests all identical, in which case
    every claim the evidence makes is still true.
  - SEPARATE THE FACTS. The post-command check no longer sits inside the try that
    owns pmux's verdict, so a moved timestamp can no longer rewrite a completed
    turn to status=failed, null its result binding, discard its usage and drop
    its drain sample. That is what happened to ordinal 32: Claude replied, pmux
    published outcome=completed, and the campaign threw it away. It is recorded
    as post_command_source_check and still stops the campaign -- a genuinely
    changed source invalidates the candidate for LATER attempts, not the one
    already published.

Three validators had to learn the new field, including the prior-campaign chain:
a resumed campaign must accept the shape its predecessor wrote. The summary's
error and source check are now cross-checked against the attempt's own outcome,
so the summary cannot disagree with the artifact it summarizes about why an
ordinal was spent.

A DRIFT FENCE CAUGHT A DEFECT I INTRODUCED TWO COMMITS AGO.
drain_calibration_from_timings does not hard-code the late-arrival field; it
DISCOVERS it as the one name TurnTimings carries beyond the five it knows, so a
rename fails loudly instead of silently. stop_hook_at_ms created a second unknown
name and made that discovery ambiguous -- the tool could have computed every gap
in every future campaign from the hook timestamp instead of the transcript one,
with no error and no reason to question it. Naming it in KNOWN_TURN_TIMING_FIELDS
restores the invariant. The test needed no change; it was right.

NOISE BAND, both implementations. v1.rs:1313-1319 defines no-late-rows as a gap
within one actor poll interval (20ms, actor.rs:80) of zero, and
summarize_drain_calibration printed that rule while contradicting it with
gap <= 0. headroom_ms is now null when the maximum is inside the band, because
headroom against an unmeasured worst case is not headroom -- publishing the full
configured drain there presented an absence as proven margin, the exact reading
its own interpretation string warns against. Two tests asserted that old
behaviour and were updated deliberately.

BOUNDED_PROCESS. drain_deadline is assigned min(deadline, ...), so
 was true by construction and every post-exit expiry
was labelled drain_timeout. phase0_lib derives timed_out = (reason == 'timeout'),
so a command that burned its entire lifetime envelope published timed_out: false
on the one field used to decide whether pmux hung. Now mirrors managed_process's
ordering and names the bound that actually bound.

SILENT DETECTION. verify_calibration wrote eight notes sites and rendered none
outside --json, which is how the grade-misattribution defect survived being
detected. Notes, grade_source and uncomputable-gap reasons now appear in the
default output, the header partitions so burnt ordinals cannot vanish, and
partial/missing hashes exit nonzero. phase0.py prints the campaign error and
echoes it to stderr, so a run that spent ordinals no longer reports a bare
status=failed that reads as a pmux failure.

DISPOSITIONS. C9's wall-clock bound is made deterministic. C8 is documented as an
explicitly unsupported boundary: its failure was ECHILD from /bin/ps -- an
inherited SIGCHLD disposition that auto-reaps children -- and NOT a timing bound.
I conflated the two in review; the third test, in pmux-launcher, is the one
actually measured at ~3.3ms against a 2s bound. Both dispositions close rows that
more green runs could never close, since docs/testing.md:110-112 makes a flaky
gate command a gate failure.

DOCS. Gate B's nine grades and the 7/7 hash oracle, with the precision that all
nine prompt files are pure ASCII asking Claude to WRITE CJK and emoji -- so the
non-ASCII RESPONSE path is tested and the INPUT path is not, and any limitation
about non-ASCII prompts stands. Two DESIGN-DEBT entries: the Stop-hook fast path
and its precondition, and NeedsInput failing fast when no input channel exists.
testing.md gains the environment preconditions, including why validation cannot
run in a git-polled workspace.

cargo 580 passed / 0 failed. evidence_common 48. phase0 167 with one
environmental flake: test_audit_reports_reserved_crash_without_artifact_as_incomplete
fails ~40% in THIS workspace because workspace_revision_capture reads the git
control identity that Conductor rewrites every ~10s. Same root cause the fence
fix addresses, and the reason Gate A must run in an unpolled clone.
`````

<a id="c20"></a>

### 20. C9: a pre-connect regression hung the gate command instead of failing it

*2026-07-28*

`````text
The review that never ran found this, and found it the way I asked: by breaking
the product in a scratch tree and observing what the test did. That review also
confirmed the C9 disposition's central claim -- making HOOK_CLIENT_IO_TIMEOUT
effectively unbounded (5s -> 86400s) still FAILS with 'the relay is unbounded',
and a hook that gives up too early still FAILS in 2.5s. The lower bound is a
genuine product gate and is not load-sensitive, which is the whole point of
replacing the wall-clock upper bound.

But accepted_rx.await sat ahead of every assertion. Its sender is owned by the
stall task, parked in accept(), and a oneshot::Receiver only errors when its
sender is DROPPED -- so a hook that exits before ever connecting leaves that
await pending forever. Everything in main.rs before send_hook_payload runs
pre-connect: path validation, the stdin read, the size limit, the JSON decode,
and a sibling test exists precisely because that path is live.

Measured in a scratch copy: a bail-before-connect ran past 180s with the child
already exited and reaped -- no panic, no output, no verdict. docs/testing.md
makes a flaky gate command a gate failure; a gate command that never returns is
worse, because it yields nothing to judge. It also falsified this test's own doc
comment, which promises 'a hook that gave up without ever waiting trips the lower
bound'. It did not; it hung.

Moved after the assertions, the same regression trips the lower bound in 1.79s
with the intended message. The accept confirmation still runs -- it just no
longer preempts the checks that can actually report.

10/10 green on cargo test -p pmux-hook.

Also corrects the C9 citations in both docs, which were short by exactly four
lines; one pointed into the body of a different test entirely. The third moved
again as a result of this edit, which is its own small lesson about citing line
numbers in a file you are still changing.

RECORDED, NOT FIXED: HOOK_SELF_DEADLINE in the test is a hand-copy of
HOOK_CLIENT_IO_TIMEOUT in main.rs with nothing tying them together, so the
accepted band is [4s, 120s] and a 5s -> 60s regression passes 6/6. A 60s hook
would be SIGKILLed by Claude Code long before returning, which is the signalled-
death mode two assertions here were added to catch, and the harness never applies
that pressure. The fix is a drift fence in the same crate, not a wall-clock upper
bound -- reintroducing one would be the original C9 mistake.
`````

<a id="c21"></a>

### 21. Gate A 75/75 with a valid source identity, and the setup that took four runs

*2026-07-28*

`````text
PASS 75/75, driver exit 0, source_unchanged: TRUE, 13.0 min, receipt sha256
32c9ccc6. Run in a standalone clone at 2279ea9, alone, with nothing else in
flight.

source_unchanged: true is the part that makes this the receipt of record. Three
earlier captures ran inside the Conductor workspace, where phase0_lib's identity
comparison embeds .git ctime_ns and an external git poll moves it every ~10s; the
last of those came back false and was discarded as evidence, because a capture
whose inputs moved underneath it proves nothing about the tree it describes. The
clone measured zero drift over 24s, and the phase0 test that failed 2 of 5 in the
workspace passed 5 of 5 there -- the environmental diagnosis verified by
experiment rather than asserted.

Getting to 75/75 took four captures and every failure was mine, not the code's:
69/75 because a fresh clone has no node_modules so tsc is absent and the e2e lane
cascades off it; then 71/75 twice on typescript-dist, which must EXIST and be
EMPTY. I had previously broken that cell by pre-populating it, so this time I
deleted it and got ENOENT; then re-created it without emptying and got the
original error back, because typescript_external_build populates that directory
even after stage_prepare has already failed -- so a failed capture poisons the
next one. Three wrong directions on one undocumented invariant.

Both preconditions are now in the Linux handoff, together with the rule that
actually governs: Gate A hashes the whole tree and must run alone on a frozen
one. Four captures in this project have been invalidated by a concurrent writer,
twice by my own verification commands.

The receipt covers all seven phases on Darwin arm64 and is archived untracked at
.context/gate-a/receipt-20260728-final.json -- regenerable by re-running the
driver, unlike the attempt ledger.
`````

<a id="c22"></a>

### 22. Handoff: say which files moved after its citations were verified

*2026-07-28*

`````text
The document anchors every file:line to 5326287 and already tells the reader to
trust quoted strings over line numbers if HEAD has moved. HEAD is now df33615 and
30 files changed in between -- the Stop-hook timestamp, the instrument fixes, the
C9 disposition, and the docs themselves -- so that instruction is now load
bearing rather than precautionary. It names the changed source files explicitly,
so a cold reader knows where to be careful instead of discovering it one wrong
line number at a time.
`````

<a id="c23"></a>

### 23. Say that the fix plan was applied, and record the three deferrals in the repo

*2026-07-28*

`````text
The header still read 'NOT YET APPLIED -- this is the work list, not a record of
work done'. All eight defects were fixed hours later, so a cold reader -- which
is exactly who this file is for -- would have treated finished work as
outstanding and possibly redone it against line numbers that no longer resolve.

Three findings were deliberately not fixed and existed only in commit messages.
A finding that lives only in a commit message is a finding nobody will find, so
they are now in the document: HOOK_SELF_DEADLINE has no drift fence and accepts a
5s -> 60s regression 6/6; matched_variant is computed and never rendered, so a
byte-exact hash match and one that only survived NFC normalization read
identically; and source_digest.py:1309 still aborts on a control-identity move
within a single capture, the narrower half of the window section 1.1 fixed.

Section 4 is flagged as the part that stays live: it says what remains unverified
AFTER every fix, and applying them changed none of it.
`````

<a id="c24"></a>

### 24. Apply the validated pre-push review: 4 blocking, 5 high, 7 medium, 5 low

*2026-07-29*

`````text
A 14-agent review produced .context/review/FINAL-REVIEW.md -- nine dimensions,
then four independent validators that kept only CONFIRMED or OVERSTATED findings.
Verdict was PUSH-AFTER-FIXES. The product code validated CLEAN: three Fable
reviewers enumerated every path to a commit and found no way to return before
work is done, and the ledger integrity was re-derived independently. Every
blocker was in commit prose, one harness writer, or evidence documentation.

BLOCKER 1 was a credential leak in a commit message, fixed separately: the
message contained a backtick pair around a word, which zsh expanded as command
substitution and interpolated the shell environment -- including live keys --
into the message. Rewritten, old objects pruned, nothing was ever pushed and no
tracked file ever contained them. The affected keys need rotating regardless.

BLOCKER 2: evidence/README.md understated the irreplaceable budget by 10
attempts and contradicted itself. Now matches the ledger, verified by re-deriving
it: 39 records, ordinals 5-43, plus 4 detached reservations = 47 consumed, 53
remain. One reviewer's proposed fix text was itself wrong (43 records) and a
validator caught it.

BLOCKER 3: the Gate A receipt of record was named in zero tracked files while
the docs called a superseded receipt "current" at a path that never existed. Now
cited with its sha256, and -- the precision a validator added -- stated to attest
commit 2279ea9, NOT HEAD, with .context/ noted as gitignored so it does not
travel with a push.

BLOCKER 4, the only one in executable code and a hole I created then failed to
close: _publish_failed_attempt omitted post_command_source_check while
_validate_attempt_outcome required it, so ONE failed attempt made the entire
evidence root unauditable -- and pmux_start_failed, the Linux agent's most likely
first failure, goes down exactly that path. Fixed across both outcome writers,
both summary writers and all five validators including the prior-campaign chain
used on resume. The reviewer verified the new test by falsification: restored the
pre-fix writer in a scratch tree and confirmed the test fails, then deleted the
surface assertion and confirmed it STILL fails via audit_campaign.

HIGH: the handoff described C8 and C9 as open flaky tests when both were
dispositioned, which would have sent the Linux agent to redo finished work. It
now agrees with current-state and keeps all three tests straight -- C8 is ECHILD
from /bin/ps in pmux-rmuxd, C9 was the wall-clock bound in pmux-hook, and the
~3.3ms-vs-2s measurement came from a third test in pmux-launcher. Also: the
orphaned Gate B receipt is now cited, the citation-freshness list is complete,
spec.md's normative permission-bypass citation is corrected, and line-number
drift was repaired across the doc set.

MEDIUM/LOW: stop_hook_at_ms now reaches both the TypeScript and Python clients --
a published field no client could read was a half-finished feature, and the whole
point of that field is that a future measurement reads it. Both mirror
last_transcript_activity_at_ms exactly, and neither clamps or unsigns the value,
because the SIGN is the answer. The Docker ownership tests were UNCONDITIONALLY
red because the fixture had no .git; they now build a hermetic one-commit repo
with explicit author identity, since rev-parse --verify HEAD^{commit} writes to
stderr on a commitless repo and that is fatal to the capture. Stale test counts,
the never-correct de-scope total, and the Gate B receipt's missing provenance are
fixed.

RECORDED, NOT FIXED, per D9: the analyze() per-poll cost (with the validator's
corrected figure -- the reviewer overstated it 2.4x), the ~270ms drain sampling
granularity, the transient-unterminated-record behaviour, and the performance
receipt's measurement basis. These are "could be better", not "a caller hits
this", and abandoning that discipline at the last step under push pressure is how
a freeze becomes a rewrite.

Verified: cargo 580/0/17 and fmt clean, phase0 168 (+1 for blocker 4),
linux-docker ownership 7 (was unconditionally red), python client 35, typescript
50/0, ruff clean. Gate A must now be re-run: docs/ is inside the source digest,
so every one of these edits invalidates the 2279ea9 receipt.
`````

<a id="c25"></a>

### 25. Gate A 75/75 on the tree being pushed, and the fourth typescript-dist trap

*2026-07-29*

`````text
PASS 75/75, driver exit 0, source_unchanged TRUE, 10.8 min. Receipt sha256
3dcb2bd58e3ffbbbc70049879b9dcbb042fa598c25f48051674077d014839f44, archived at
.context/gate-a/receipt-<c24>.json, captured in a standalone unpolled clone at
commit <c24> with nothing else in flight.

This receipt attests <c24>, the PARENT of HEAD. It cannot attest HEAD: docs/ is
inside the source digest, so the commit that records a receipt necessarily
invalidates it. The only delta is documentation -- git diff <c24>..HEAD --stat
shows no source, test, or manifest change -- and current-state.md now says this
explicitly rather than implying HEAD is attested. That imprecision was itself a
review finding, and I nearly re-committed it one sentence after fixing it.

The capture took two attempts on a precondition I have now hit from all four
directions across six captures of the same tree:

  pre-populated            -> "prepare requires an empty root"
  absent                   -> ENOENT
  stale from a failed run  -> "prepare requires an empty root" again, because
                              typescript_external_build populates that directory
                              even after stage_prepare has already failed, so a
                              failed capture poisons the next one
  wrong mode               -> "root mode must be 0700"

The last is new and was mine: I chmod'd the validation root and assumed the child
inherited it, but mkdir -p uses the umask. The earlier passing run only worked
because I had chmod'd that directory explicitly. None of the four messages names
the directory it means, and a fresh Linux server with a different umask will hit
the mode one immediately -- so the handoff now gives all three properties as one
copy-pasteable block instead of prose.

Everything substantive was green in every attempt: source_unchanged true, and all
Rust, fuzz, process-boundary, protocol, conformance, Python and shell cells
passing. All four failures were the same self-inflicted staging chain, never the
code.
`````

<a id="c26"></a>

### 26. Phase 0: make the drain question free to answer, and harden the live runners

*2026-07-29*

`````text
Every live turn ever run was scenario one-shot -- the ledger holds {one-shot: 14}
and nothing else. Resume, non-ASCII input, deadline and facade have zero live
coverage, and those are where completion state is CARRIED rather than derived
fresh, which is where a wrong answer is most plausible. Phase 0 is the free work
that makes the next ~9 ordinals answer two questions instead of one.

DRAIN SIGN. verify_calibration.py now computes the signed difference
stop_hook_at_ms - last_transcript_activity_at_ms. Consistently positive means
Claude flushed before firing Stop and the fast path
(stop_hook_observed || stable_for_ms >= drain) is sound, recovering ~2300ms of the
~2350ms drain against 41ms p50 of non-drain overhead. A single negative means Stop
can precede the final write, so completing on it would TRUNCATE a turn, and the
question closes permanently in the safe direction.

The sign survives end to end -- a reviewer traced it and then mutation-tested it:
inserting max(0,..) breaks 4 tests, suppressing the negative banner from default
output breaks 2, reporting absent as 0 breaks 4. Absent is reported as
uncomputable WITH THE REASON, never as zero. Against the retained Gate B evidence
it correctly says the run "says NOTHING about whether Claude flushes the
transcript before firing Stop" and that "an open question forbids the fast path
exactly as a negative observation would" -- those 10 attempts predate the field.

FACADE COVERAGE was the thinnest of any untouched scenario, mentioned in one test
file versus 4-15 for the others. Spending a credentialed call to discover what the
deterministic double would catch for free is waste, so facade_blackbox.rs now
covers the wire behaviour: the fixed 24x120 cell, all six permission-mode
spellings, all six efforts, prompt integrity for unicode/multiline/padded input,
stdin-vs-positional precedence, the exact 1 MiB boundary, and that a non-completed
outcome is never labelled success. Proven behavioural by mutation: mis-mapping
BypassPermissions to DontAsk -- the exact mode the facade runner will use live --
fails the test.

LIVE RUNNERS, untracked under .context/campaigns/. A reviewer ran them against a
STUB phase0 in /tmp, proving the whole reserve-then-copy-back protocol without
spending anything, and found two must-fixes that are now closed:

  - the deadline runner reported its PASS shape when phase0 failed BEFORE
    reserving. A nonzero exit is that campaign's expected shape, but it is also
    what a source-identity mismatch looks like -- which launches nothing. It now
    requires the ledger to advance by exactly one, because "the ordinal appears
    once" is mechanically checkable and asking the operator to check it is how a
    false closure happens.
  - a live exit 0 with fewer appended records than attempts still returned 0. That
    is the precise signature of the bug that ran four campaigns while reporting
    nothing spent: the accounting was printed and never enforced. Now enforced.

Also: permits_fast_path read true from a ZERO-ONLY sample set, though the tool's
own prose says a zero does not establish which came first. It now requires at
least one positive and no negative, else None -- the same "not yet observed" state
as no samples at all.

And a citation that originated with me and propagated into three files:
phase0_lib.py:1234 is blank; APPROVED_EFFORTS is enforced at :1350-1351.

SCOPE CORRECTION, recorded rather than worked around: cancellation and
attach/detach are NOT reachable through the phase0 envelope -- --scenario accepts
only {one-shot, persistent, resume, claude-p-one-shot} and phase0.py matrix lists
direct rmux control, direct PTY input and attached_stream as
unsupported_by_envelope. Those two runners refuse, exit 2, spend nothing, and
document what envelope change would unblock them. The files are the deliverable
for those scenarios.

BUDGET RECONCILIATION. The ledger has 39 records ending at ordinal 43, so it
numbers the next reservation 44. My accounting says 47 consumed because it counts
four detached reservations that were real credentialed calls but are not ledger
records. Both are true for different questions: the ledger governs NUMBERING, the
conservative count governs the BUDGET, so spending stops at ordinal 96 rather
than 100.

Verified: runners selftest 36/36, verify_calibration 83, phase0 187, cargo
580/0/17 and fmt clean, ruff clean. Ordinals untouched at 47/53 -- no agent spent
one, which was the instruction that mattered most.
`````

<a id="c29"></a>

### 29. Gate A 75/75 on the SchemaDrift gate and the Phase 1/2 tree

*2026-07-30*

`````text
PASS 75/75, driver exit 0, source_unchanged TRUE, 19.7 min, on a standalone
unpolled clone at <c28> with nothing else in flight. Receipt archived at
.context/gate-a/receipt-<c28>.json.

First capture to certify the payload-proven stop_hook_summary allowlist -- the only
change in this whole effort that touches the completion-authority path -- together
with the five new tests, the tracked fixture of the row that killed ordinal 49,
the corrected citations, the system-subtype taxonomy, and the re-anchored S2 row.

Four typescript-dist preconditions were all satisfied before launch this time
(exists, empty, mode 0700, npm ci done), so it passed first try rather than after
four captures. Those preconditions are in the Linux handoff.

As before, the receipt attests the PARENT of HEAD: docs/ is inside the source
digest, so the commit recording a receipt necessarily invalidates it. The only
delta is documentation -- git diff <c28>..HEAD --stat shows no source, test or
manifest change.
`````

<a id="c31"></a>

### 31. Gate A 75/75 on the api_error gate and arrival instrumentation

*2026-07-30*

`````text
PASS 75/75, driver exit 0, source_unchanged TRUE, 19.9 min, standalone unpolled
clone at <c30>, nothing else in flight. Receipt archived at
.context/gate-a/receipt-<c30>.json.

Certifies the two changes on the completion-authority path: the api_error
non-terminal gate, which stops pmux completing a turn mid-retry during ordinary
network flakiness, and the arrival instrumentation, which is measurement only and
structurally cannot contaminate what it measures.

As always the receipt attests the PARENT of HEAD -- docs/ is inside the source
digest, so the commit that records a receipt necessarily invalidates it. The only
delta is documentation; git diff <c30>..HEAD --stat shows no source, test or
manifest change.
`````

<a id="c32"></a>

### 32. Correct two coverage rows that understated what ordinals 44-55 bought

*2026-07-30*

`````text
Both rows still listed resume, Unicode input, deadline and facade as 'entirely
untouched' after all four had live coverage, and next-step 4 still said '53
attempts' against a 51-record ledger. Understating coverage is an unusual
direction to be wrong in, but these are exactly the false-but-authoritative
sentences the handoff exists to prevent -- a cold reader would re-spend
irreplaceable ordinals on scenarios already covered.

They now distinguish three states that were previously collapsed into one:
covered live (resume, persistent, non-ASCII input, facade, deadline);
unreachable-and-documented (cancellation and attach/detach, refused by the
envelope, with the runners themselves as the deliverable); and genuinely open
(replay, which was neither covered nor explained and is the one real hole).
`````

<a id="c42"></a>

### 42. Keep the screens pmux discards, and four checks that could not have failed

*2026-08-04*

`````text
Path B drives the real Claude Code TUI, so every input and completion gate is a
claim about terminal geometry that Claude can change without notice. Two such
claims have already been wrong and neither was findable by reading the code: the
composer gate measured from the bottom of the GRID rather than the end of Ink's
FRAME and survived four review rounds, and the /clear menu's selection is
rendered in foreground colour alone, which the plain-text snapshot discarded
entirely. Both died to a live capture. This adds the machinery that finds that
class offline and in bulk.

RECORD/REPLAY. crates/service/src/screen_corpus.rs records every TerminalSnapshot
and StyledScreen to versioned NDJSON stamped with Claude version, OS, arch and
geometry. Off unless PMUX_SCREEN_CORPUS_DIR is set; when on, frames go to a
bounded channel drained by a dedicated OS thread and a full queue DROPS rather
than blocking the 25 ms poll it is observing. Two hook lines in driver_io.rs at
the existing gated_snapshot/gated_styled_screen choke points; the frame is
borrowed and only cloned once a recorder exists, so the disabled path allocates
nothing. screen_corpus_replay.rs is the standing test over the checked-in seed.

PROPERTIES. screen_properties.rs generates screens across every axis the two
known bugs touched and asserts properties, not outputs. The load-bearing one is
that appending blank rows below the frame must not change the verdict: against
the pre-fix expression it fails on the FIRST generated case and shrinks to a
19-row grid with the frame at rows 7-11. Four of its ten checks catch that
mutation. proptest was already a dev-dependency at the locked version; nothing
was added to the lock file.

PASTE INJECTION. Nobody had tested a prompt containing \e[201~. Both guards hold
-- validate_prompt refuses ESC/NUL/controls, and the wire encoder refuses them
again independently -- so this is a confirmed-mitigated risk and NOT a live
defect. Proving it needed a real PTY: with both guards removed, a caller sending
"benign\e[201~/logout\r" gets "benign" pasted and /logout\n delivered as
KEYSTROKES. paste_injection.rs holds that shut against 55 hostile inputs and 512
generated ones, and lifts the wire encoding into pseudomux_rmux::
bracketed_paste_payload so it is testable without a live rmux.

FOUR CHECKS THAT COULD NOT HAVE FAILED, all found by mutating what they check:

  1. Replaying the corpus through the BROKEN composer gate passed. Every
     geometry invariant was conditional on the classifier's own verdict, so a
     classifier that stops saying Ready satisfies all of them by having no cases
     left -- which is exactly what the bug did. CorpusFrame::expect_ready is the
     unconditional half, set only where the verdict was established without
     consulting the classifier.
  2. The styled round-trip compared a recovered screen against a fixture built
     with the same constructor, so a constructor that dropped the padding flag
     dropped it on both sides and compared equal. It now asserts is_padding() and
     row_text() against literals.
  3. Nothing asserted the slash-command guard. /clear carries no ESC, so it
     survives both control-character guards and would be submitted EXACTLY --
     satisfying the refuse-or-submit-exactly property while handing Claude Code a
     command.
  4. tools/screen-corpus/per_binary_tests.sh enumerated its targets through an
     inline Python one-liner whose quoting the shell broke. It enumerated zero
     targets and reported "every test binary passed in isolation". It now
     enumerates from the source tree and refuses to report a result on an empty
     enumeration.

The local-command exercise is scoped and NOT run; tools/screen-corpus/
local_command_geometry.md says so and says why it is far smaller than 85
commands: ControlCommand is a single-variant enum with no payload, so /clear is
the only slash command pmux can ever type, and the question worth wall-clock is
only the menu geometry AROUND /clear.

Zero model turns were spent. Nothing here needs one.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c45"></a>

### 45. Merge the screen corpus: every discarded screen is now evidence

*2026-08-05*

`````text
Two hooks at the existing `gated_snapshot`/`gated_styled_screen` choke points,
so the screens pmux already reads are recorded instead of dropped, and the
corpus replays against the parsers without a Claude on the box.

`crates/rmux/src/lib.rs` re-exports both sides of the merge: `ControlPlaneFault`
from the fault classification, `bracketed_paste_payload` from the injection work.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c49"></a>

### 49. The per-binary harness covers thirteen packages, and doctor names the layers nobody reported

*2026-08-05*

`````text
The harness enumerated a HAND-WRITTEN array of six packages against a workspace
of thirteen. Every `bin/` package was absent, so "every one of the N test
targets passed" was a true sentence about a set that did not include `pmux`,
`pmuxd`, `pmux-mcp`, `claude-p`, `pmux-hook`, `pmux-launcher` or `pmux-rmuxd` --
the same defect its own header warns about, one level up: a report whose scope
is narrower than its sentence. Targets now come from `cargo metadata`, and the
count is cross-checked against the root manifest's `members` so a short list
REFUSES rather than reports. 33 targets became 60.

It also builds every target once before the loop. MEASURED on this host: the
first eight targets took fifty minutes and the whole workspace then built in two
minutes forty, because each per-target invocation was linking and
first-executing a fresh binary and macOS stalls in dyld the first time it runs
one. The loop still runs one `cargo test` per target -- that is what makes the
results isolated -- but each is now a freshness check and a test run.

`doctor_is_turn_free_and_reports_healthy_and_unhealthy_boundaries` asserted
`healthy` for a fake daemon whose diagnosis carried no health layers, which is
exactly the sentence the layered surface exists to make unsayable. Its fixture
now sends a complete tree, built from `HealthLayerName::ALL` so a layer added
later joins it rather than going stale, and the case it used to be is now an
assertion of its own: a daemon that answers `diagnose`, passes its runtime
probe, holds no sessions and reports NO layers is `unproven`, and `doctor`
names all eight unreported layers rather than folding them silently. An
operator told `unproven` with no reason cannot act.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c55"></a>

### 55. The gate linted the directories somebody listed, and the residue audit never looked inside /tmp

*2026-08-06*

`````text
Gate A reported green over a set of things to check that was, in five separate places,
a list a person had typed rather than a set derived from the tree. Every widening here
is the same edit: replace the list with the derivation, and let a test assert the
derivation is not empty.

**`ruff` named seven directories.** `check` and `format --check` both ran against
`clients/python tools/evidence_common tools/package-smoke tools/phase0
tools/linux-docker tools/gate-a tools/gate-a-candidate`. Both now run against
`{workspace}`. MEASURED: 34 files before, 36 after -- `tools/promotion/measure_transcript_drain.py`
and `tools/screen-corpus/seed_corpus.py` were in the tree, are python, and were linted
by nothing. Neither cell carried `PYTHONDONTWRITEBYTECODE`, which is what
`scripts/gate-a-residue.sh` then fails the run on, so both now carry it.

**`test_the_real_manifest_supplies_pythondontwritebytecode_per_cell` passed in the
dangerous case.** Its predicate was `len(carriers) == 1`. A new python cell added
WITHOUT the guard leaves the count at one and passes; a cell added WITH the guard trips
"the bytecode-residue guard moved" and fails. It was forbidding the protection and
requiring its absence. The rule the residue scan actually needs is derived from the
cells: `{c.id for c in cells if c.argv[0] == "{python}"}` equals
`{c.id for c in cells if c.env["PYTHONDONTWRITEBYTECODE"] == "1"}`, with a third
assertion that no cell sets the name to anything but `"1"` -- because two of those three
sets being equal is satisfiable by a cell that sets it to `"0"`.

**`shell_syntax` and `shellcheck` named three of seven scripts.** `tools/linux-docker/{run,inside,suite}.sh`,
which `docs/testing.md` itself names in Gate F, and `tools/screen-corpus/per_binary_tests.sh`,
added this week, were never parsed and never linted by the cell that reports
"shell_syntax ok". The new test walks `SOURCE_ROOT_DIRS` for `*.sh` under the driver's own
`SOURCE_SKIP` and asserts both cells name exactly that set, so the eighth script cannot
enter the tree unlinted. `docs/testing.md`'s two command lines are widened to match.

**Five `gate_f` cells were restored.** `docs/testing.md:712-716` requires
`evidence_common`, `package-smoke`, `phase0`, `gate-a-candidate` and `gate-a` self-tests,
and the manifest carried none of them: 75 cells where the README published 80. A cell
count test now pins the per-phase shape as a tripwire on shrinkage, since
`tools/linux-docker/gate-a-manifest.json` has already been left behind by one trim.

**The residue audit read ten `/tmp` prefixes of thirty-two.** Every prefix belonging to a
`bin/` blackbox test -- `ph-`, `pl-`, `prd-`, `pmd-`, `clp-`, `pmcp-`, `pmux-cli-` -- was
absent, as was every prefix added after the list was written: `pmux-pool-wave-`,
`pmux-containment-`, `pmux-spellings-`. The prefixes are now derived from the test sources
by three rules (a direct `.prefix("...")` in a file that also names `tempdir_in("/tmp")`,
a `PathBuf::from("/tmp").join(format!("...` , and the literals passed to a fixture
constructor for the `.prefix(variable)` form that hides the literal behind a helper --
`CancellationFixture::start("pmux-geometry", ..)` is six of the thirty-two). The original
ten survive as `FLOOR`, a LOWER bound: a regex that silently stops matching would narrow
the scan back to nothing while still printing "passed", so a derivation that loses a
known-good prefix exits 2 rather than reporting a result.

**And that scan had never observed a leaked root on the platform it was written for.**
On macOS `/tmp` IS a symlink to `private/tmp`, and BSD `find` without `-H` does not descend
through a symlink named on the command line: the expression matched the symlink itself,
failed `-type d`, and returned empty for every pattern on every run -- underneath a call
site whose comment read "including macOS where /tmp resolves to /private/tmp". A whole
wave of leaked pool roots sat inside the set it did not look at.

**Two refusals moved to before the first cell.** The driver used to `mkdir(exist_ok=True)`
the validation root and leave its children to whichever cell reached them first. An
operator who pre-created the tree under an ordinary umask got FOUR red cells --
`typescript_stage_prepare`, `typescript_stage_verify`, `typescript_tests` and
`release_full_stack_e2e`, the last after five minutes of E2E -- every one of which reads
as a product failure and every one of which was one mode bit on a directory this driver
owns. `prepare_validation_root` creates the root and its three documented children at
0700 and REFUSES a wider existing mode rather than chmod-ing it, because silently
widening-then-narrowing hides a validation root somebody else can already read. The
second is `require_release_depinfo`: `crates/e2e/tests/pool_concurrency.rs:237` proves the
candidate is not stale by reading the `<binary>.d` cargo wrote, so a release directory
assembled by copying only the eight executables fails all nineteen pool tests six minutes
in, with nineteen identical panics about a missing `.d`. It is derived from the
directory's own executables, so a ninth binary cannot enter the candidate without its
depinfo. The README's example `--release-dir` is corrected to `$PWD/target/release`,
which is what `candidate_envelope.py` actually passes.

**One citation in `test_verify_calibration.py` was a hand-restated line number.** It said
`crates/service/src/v1/actor.rs:83` while the tool's own comment said the same thing in
its own source; Path B moved `poll_interval` to line 85 and the copy in the test, which
nothing else reads, stayed behind. Both citations that had one are now read out of
`verify_calibration.py`'s source through `sole_citation`, which asserts the file is cited
exactly ONCE in the text it is given -- taking the first of several would quietly check a
different claim and pass while doing it. The four stale line numbers the tool emits
(`actor.rs:83`, `v1.rs:1357-1359`, `driver_io.rs:1613-1616/1723/1670-1672`) are corrected
to the lines that carry those statements today.

**Nine rustdoc intra-doc links pointed at private items.** Comment-only, and validated by
the `rustdoc` cell passing under `RUSTDOCFLAGS=-D warnings`; each says in prose why the
item is not linked. `private_dir.rs` gains the one that was simply unqualified.

`ruff format` over the widened set is the remainder of the diff.

Verified: `tools/gate-a/tests` 31 tests OK; `tools/phase0/tests` 243 tests OK;
`ruff check`/`ruff format --check` over `.` clean; `bash -n` and `shellcheck` over all
seven scripts clean; `cargo fmt --all --check` clean; `cargo doc --locked --workspace
--all-features --no-deps` under `RUSTDOCFLAGS=-D warnings` clean but for the four
pre-existing `vendor/rmux-server` dead-code warnings.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c57"></a>

### 57. A phase the driver could not start, and a green report over forty-nine tests that never ran

*2026-08-06*

`````text
Five defects in the gate machinery itself. Three are the standing bug class: a
guard whose message promises more than its predicate tests, or a check whose
set-of-things-to-check is hand-written where it could be derived.

1. `gate_b` COULD NOT EXECUTE -- six cells, no receipt.

    gate-a driver error: placeholder {cargo_fuzz} is unresolved; pass --tool cargo_fuzz=<path>
    EXIT=2

Resolved, not deferred to a louder refusal. The refusal was already loud and
already before the first cell (`_plan` expands everything up front), so failing
"earlier" would have left the phase exactly as unrunnable. `cargo-fuzz` is
version-pinned by the gate that runs it -- `cargo_fuzz_version` asserts the exact
string `cargo-fuzz 0.13.2` and `scripts/gate-a-fuzz.sh:56` refuses anything else
-- so it is installed under the workspace, and `scripts/gate-a-fuzz.sh:14` and
`candidate_envelope.py:1605` BOTH already default to
`.context/tools/cargo-fuzz/bin/cargo-fuzz`. This driver was the third reader of
that path and the only one that did not know it. It is checked before PATH, for
the reason the pin exists: two cells measuring two different binaries is the
outcome the version assertion is there to prevent. An explicit `--tool` still
outranks both. Verbatim, after:

    [1/6] gate_b/transcript_properties ok 9822ms
    [2/6] gate_b/actor_model ok 14916ms
    [3/6] gate_b/client_protocol_properties ok 2656ms
    [4/6] gate_b/protocol_framing_properties ok 35175ms
    [5/6] gate_b/cargo_fuzz_version ok 35ms
    [6/6] gate_b/production_fuzz ok 75181ms
    PASS 6/6 cells passed, 0 failed, 6 executed

And the refusal that remains now names EVERY unresolved placeholder in the
selected phases rather than the first. `gate_b` needs four and is budgeted at
four hours: one per run is one gate attempt per missing `--tool`. Scanning
continues past an unresolvable name inside one value, and across every value in
a cell, so `production_fuzz` reports `{nightly_cargo}` and `{nightly_rustc}`
together.

2. `typescript_tests` WAS VACUOUS. Its argv hand-listed three `.test.mjs` files
while `clients/typescript/package.json` runs `node --test tests/*.test.mjs`.
Measured, with a deliberately failing `zz-mutation.test.mjs` beside them:

    GATE CELL (hand-list)  exit=0  tests 50  pass 50  fail 0
    GLOB       (npm test)  exit=1  tests 51  pass 50  fail 1

The cell keeps its list -- a gate argv is literal by design, and `npm test`
builds into `clients/typescript/dist`, which `scripts/gate-a-residue.sh:237`
forbids -- and gains the derivation the two list-based shell cells already had.
Both halves: the candidate cell in `tools/gate-a/tests`, and the Linux suite's
identical hand-list at `suite.sh:433` in `tools/linux-docker/tests`. With the
mutation file present both now fail, naming it.

3. `per_binary_tests.sh` HAD THE DEFECT IT WAS WRITTEN TO FIX, one level down.
Enumerating every target is not running every test: each `cargo test` ran without
`--include-ignored`, so the report printed "every one of the 61 test targets
passed in isolation" while 49 cases never executed -- among them all nineteen of
`pseudomux-e2e --test pool_concurrency`, `0 passed; 0 failed; 19 ignored`, which
is where the only real failure lives. `cargo test --workspace --all-targets --
--ignored --list` counts those 49 exactly. Scope and sentence both:

    scope complete: 1029 test cases ran across all 61 targets, 2 target(s) failed; NOT claiming isolation coverage

1029 = 980 + the 49. The two failures are `pseudomux-e2e/full_stack` (7 failed,
absent `PMUX_E2E_TYPESCRIPT_DIST_DIR`) and `pseudomux-e2e/pool_concurrency`
(5 failed, absent `PMUX_POOL_REAL_CLAUDE`) -- the two unmet harness preconditions
the previous commit recorded, which the old scope could not see. The claim is
printed only when the counts earn it: unknown scope (a target with no
`test result:` line, or a case still not run) exits 2, a failed target exits 1,
and the sentence carries the executed and ignored counts itself rather than
saying "none ignored" because control reached it.

4. `scripts/gate-a-residue.sh` printed `candidate_executables=%d` from
`${#required_binaries[@]}` -- "our literal has eight entries" written as "we
found eight executables". Derived from the directory now, by the same predicate
`run_gate.py:require_release_depinfo` already uses on the same directory, with
the eight names kept as a FLOOR exactly like the /tmp prefix floor thirty lines
above. Against a synthetic candidate directory of ten, with a ninth executable
left running:

    BEFORE  exit=0  Gate A residue audit passed.  candidate_executables=8
    AFTER   exit=1  candidate process residue for .../ninth-binary: 42907 .../ninth-binary

5. `docs/testing.md:719` requires `tools/linux-docker/tests` in Gate F and no
cell ran it. Added as `linux_docker_self_tests`, and the omission itself is now
derived: `test_gate_f_runs_every_unittest_directory_the_doc_requires` parses the
Gate F block of `docs/testing.md` and compares it to the manifest's `gate_f`
cells, because nothing compared the two. `test_the_real_manifest_cell_count_...`
stops restating the counts and reads them from the README line that publishes
them, so a cell added or removed is one number in one place.

The suite that cell runs was red, and its message was arithmetic:
`test_runner.py:284` carried a literal `{gate_a: 42, gate_d: 11, ...}` against
41 and 10 on disk, so the test failed on a stale count and NEVER REACHED the
projection drift it exists to detect. Those three literals -- the phase counts,
and `len(observed) == 97` at `:350` -- are derived now: phase membership from the
candidate, projection size from the candidate's own cells plus a declared
fifteen-name container-only set that is cross-checked against the `expected`
list built beside it. The failure now names the seven drifted cells one by one.

That failure is debt row C6 and it is NOT repaired here. The repair was
rehearsed mechanically on a scratch copy of the tree: it makes this test pass at
96 gates and then fails two OTHER tests in the same file -- `:651`
`test_package_framing_property_and_shellcheck_gates_are_exact`, whose
hand-written `required` list demands the two `*_package_artifact` cells that
`docs/current-state.md` row 38 records as unsatisfiable and the old
`candidate_envelope_tests` name; and `:821`, whose ordering forbids
`release_full_stack_e2e` in phase A because the container has no release
binaries until D. Both are Gate C decisions that would mean editing test
expectations, `docs/gate-c-linux-handoff.md` §3.4 orders C6 after D6, and this
is not the commit to make them in. The cell is added anyway, on this driver's
own stated principle: a failing Gate A number is far more informative than
another missing one. It is now one named red cell in the receipt instead of a
lane the gate never looked at.

`tools/linux-docker/tests/test_bounded_runner.py` spawned `bounded_runner.py`
with a constructed `env=` carrying only `PATH`, dropping the
`PYTHONDONTWRITEBYTECODE` its parent ran under; the runner imports `evidence`
and `source_digest` from beside itself, so every run left
`tools/linux-docker/__pycache__/{evidence,source_digest}.cpython-313.pyc` in the
tracked tree and the residue audit failed the gate on three findings. Measured
by bisection -- `test_docker_ownership.py:369`'s constructed env runs only
`git` and writes nothing. The guard is restored and the property is now
OBSERVED, not asserted about a dict: the cache directory beside the runner,
before and after.

Delete-the-check, per check. Each production rule deleted, its target run, the
failure recorded, the rule restored and the file `sha256`-matched.

  1 workspace cargo-fuzz resolution deleted -> test_a_workspace_pinned_tool...
    errors `UnresolvedPlaceholder: placeholder {cargo_fuzz} is unresolved`.
  2 `_plan` fault collection deleted -> `2 of 3 selected cells cannot be
    expanded` absent; the refusal names one cell.
  3 `expand_cell` accumulation deleted -> `{nightly_rustc}` unnamed.
  4 `expand` intra-value accumulation deleted -> `{third_tool}` unnamed.
  5 the new gate_f cell deleted -> the doc derivation reports
    `+ 'tools/linux-docker/tests'`.
  6 README counts desynced from the manifest -> the published-shape test fails.
  7 `PYTHONDONTWRITEBYTECODE` deleted from the child env -> `{'evidence...pyc',
    'source_digest...pyc'} is not None : the runner created .../__pycache__`.
  8 a name dropped from CONTAINER_ONLY_GATES -> the expected/declared
    cross-check fires.
  9-11 on a C6-repaired scratch copy where the projection test passes at 96
    gates: an extra legal-named gate -> `97 != 96`; two adjacent gates swapped
    -> the ordered comparison; one gate renamed -> membership. Each fires its
    own assertion and no other.
  12 `--include-ignored` deleted, guard kept -> `scope incomplete: 1 test cases
    ran across 1 of 1 targets; NOT claiming isolation coverage`, exit 2.
  13 both deleted -> `every one of the 1 test targets passed in isolation:
    1 test cases ran, 1 ignored` -- the original defect, and the sentence now
    carries the count that refutes it.
  14 a FLOOR binary removed from the candidate directory -> `candidate directory
    ... has no pmux-hook (derived 10 executables); refusing to report a result`.
  Mutants 12-14 ran against a two-file scratch cargo workspace and a synthetic
  candidate directory; 9-11 against an rsync'd copy of this tree. Nothing was
  mutated in place except 1-8, each restored byte-exact.

Measured after: `tools/gate-a/tests` 35 OK; `tools/phase0/tests` 243 OK;
`tools/evidence_common/tests` 48 OK; `tools/gate-a-candidate/tests` 20 OK;
`tools/linux-docker/tests` 109 tests, 1 failure (C6), 142 s;
`ruff check`/`format --check` over `.` clean; `bash -n`/`shellcheck` over all
seven scripts clean; `cargo fmt --all --check` clean; `cargo clippy --locked
--workspace --all-targets --all-features -- -D warnings` clean first-party
(the 4 pre-existing `rmux-server` vendor warnings remain); the residue audit
green with `candidate_executables=8`; no daemon, temp root or leaked pool
parent left behind.

`gate_f` end to end through the driver, 9 cells where there were 8:

    [5/9] gate_f/gate_driver_self_tests ok 7993ms
    [6/9] gate_f/linux_docker_self_tests FAILED exit_status 139922ms
    FAIL 7/9 cells passed, 2 failed, 9 executed failed: package_smoke_self_tests, linux_docker_self_tests

`package_smoke_self_tests` fails on `No package metadata was found for
setuptools`, confirmed identical in a clean `<c56>` worktree: an unmet host
precondition, not a regression, and not touched here.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c58"></a>

### 58. Five waves demanded a variable no gate can pass, and a mode drift the umask had already applied

*2026-08-06*

`````text
Re-ran the whole Gate A attestation -- all 7 phases, all 81 cells, through
`tools/gate-a/run_gate.py` -- rather than reasoning about it. It found two
defects, both of the class this tree keeps finding: a guard whose message
promises more than its predicate tests.

  run 1  36.1 min  FAIL 79/81  release_full_stack_e2e, linux_docker_self_tests
  run 2  30.8 min  FAIL 80/81  linux_docker_self_tests
  run 3  30.5 min  FAIL 78/81  release_full_stack_e2e, phase0_self_tests,
                               linux_docker_self_tests

Run 3 is the same tree as run 2. Its two extra cells are INTERMITTENTS, chased
to a rate and a named field rather than re-run until green, and recorded as
debt rows C10 and C11. The deterministic verdict is 80/81; the only reliably
red cell is C6, which is red on purpose. No run has yet been observed at
81/81, and this commit does not claim one.

`source_unchanged: true` over 926 files on every run, on macOS-15.7.7-arm64,
Darwin 24.6.0. `gate_b` executed for the first time in this project's history:
6/6, `production_fuzz` included. The one remaining red cell is debt row C6 and
is deliberately red; see the last section.

-- 1. The real lane promised a gate and asserted one instead ----------------

`crates/e2e/tests/pool_concurrency.rs:432`. The doc comment on `real_wave`
says the lane "is `#[ignore]`d AND gated on `PMUX_POOL_REAL_CLAUDE` naming the
executable". The code `.expect(..)`ed it. Those are not the same thing, and
the difference is a whole gate cell.

`release_full_stack_e2e` runs `-p pseudomux-e2e --all-targets` under
`--include-ignored` and supplies `PMUX_E2E_BIN_DIR` and
`PMUX_E2E_TYPESCRIPT_DIST_DIR` -- both of which work; `full_stack` passed
10/10 inside the same cell. It does not supply `PMUX_POOL_REAL_CLAUDE`,
because no gate can: the value is a host path, the lane spends `2 *
concurrency` real model turns, and the variable is not in the driver's
`ENVIRONMENT_ALLOWLIST` (`run_gate.py:53`), so nothing exported around the
gate reaches a cell. Measured, verbatim:

    test result: FAILED. 14 passed; 5 failed; 0 ignored; ... in 146.17s
    thread 'two_concurrent_real_callers' panicked at
      crates/e2e/tests/pool_concurrency.rs:434:14:
    PMUX_POOL_REAL_CLAUDE must name the Claude executable for the real lane

Five identical panics, nine minutes into the cell. The cell could not pass on
any host and had not been able to since the pool tests landed -- the receipt
of record predates them, which is why `current-state.md` still recorded this
cell as "8 passed / 0 failed".

The sibling real lane in the same crate already does the right thing:
`cross_cell_contamination.rs:2258` skips loudly and returns when
`PMUX_CONTAMINATION_REAL_CLAUDE` is absent. `Lane::real` now does the same --
`Option<Self>`, a `SKIPPED:` line naming the variable, and `let Some(lane) =
.. else { return }` at the three construction sites. Nothing below the gate
changed: with the variable set, every one of the five runs exactly as before,
and that arm was executed to prove it (`claude --version` off
`<HOME>/.local/bin/claude`, zero model turns).

    test two_concurrent_real_callers ... SKIPPED: set PMUX_POOL_REAL_CLAUDE
    to the Claude executable to run the pool's real lane. ... ok

Two regressions, both non-`#[ignore]`d so they run in the default suite and in
the cell:

  `the_real_lane_is_gated_on_its_variable_rather_than_asserting_it` states an
  EQUALITY -- the lane yields an instance exactly when the variable is set --
  so neither "always panic" nor "always skip" survives it. The patterns carry
  a condition rather than `Some(_)`, so what is asserted is a usable lane: a
  version measured off the binary, and a promoted lane that really dropped
  `operator_profile`.

  `every_real_lane_test_names_and_reaches_its_gate` is derived from the file's
  own source: every `#[ignore]` that promises real model turns must name the
  variable, every construction of the lane must be bound through it, and the
  variable may be spelled in exactly one code line -- the constant. Its
  needles are assembled with `concat!` because, spelled literally, it reported
  ITSELF as an ungated construction, which is a true statement about the file
  and a useless one about the lane.

-- 2. A mode-drift test the umask decided ----------------------------------

`tools/linux-docker/tests/test_source_digest.py:191`. It `chmod(0o700)`'d a
directory `setUp` had just created, then asserted the digest moved. Under an
ordinary 0022 umask that is a real change. Under `umask 077` -- which
`testing.md:124` requires of EVERY gate command and `run_gate.py:635` sets for
every cell -- `mkdir` already yields 0700, the chmod is a no-op, and the
digest does not move. Measured both ways on the same tree:

    umask 022  ok
    umask 077  AssertionError: 'acb1330c0d9de4c4...' == 'acb1330c0d9de4c4...'

So the test had never run correctly in the only environment the docs mandate,
and it was invisible outside the gate: running the suite by hand shows 1
failure, the cell shows 2. Its verdict was decided by the ambient umask rather
than by the digest it names -- and in the other direction too, since it never
asserted the mode it produced.

The drifted mode is now derived from the directory's actual mode
(`current ^ 0o070`, which flips group bits and leaves owner traversal intact
under every umask), and the recorded mode is asserted to equal that derived
value rather than a constant that happened to match.

This is §7.2 defect 4 one level down: that one was the driver not applying
`umask 077`, this one is a test that cannot survive it.

-- Delete the check, run its target, restore, verify ------------------------

Six mutants, no survivors. Every restore verified `sha256` byte-exact against
the pre-mutation hash.

  1  `source_digest.py:654` -- directory mode removed from the aggregate.
     Target fails under BOTH 0022 and 0077. This is the measurement that
     shows the repaired test tracks the digest and not the umask: before the
     repair it passed under 0022 and failed under 0077 regardless of whether
     the digest hashed directory modes at all.
  2  `Lane::real` -- the original `.expect(..)` restored. Target
     `the_real_lane_is_gated_..` panics at `:454` with the original message,
     with the variable UNSET.
  3  `Lane::real` -- always `None`. Target passes with the variable unset and
     FAILS with it set (`right: true`). 2 and 3 together are why the check is
     an equality: each arm catches what the other cannot.
  4  `real_wave` -- `let lane = Lane::real().unwrap();`. Target
     `every_real_lane_test_..` names the line back:
     `a real-lane construction is not bound through its gate:
      let lane = Lane::real().unwrap();`
  5  `five_concurrent_real_callers`'s `#[ignore]` reworded to "set the
     real-lane variable". Target: `an #[ignore] promises real model turns
     without naming PMUX_POOL_REAL_CLAUDE: ...`
  6  a second literal spelling of the variable in code. Target:
     `PMUX_POOL_REAL_CLAUDE appears as a literal in 2 code lines; exactly one
     -- the constant -- may spell it`

-- What was re-measured, not assumed ---------------------------------------

`eight_concurrent_callers_against_three_slots_cold_swap_rather_than_starve`,
10 isolated runs against the release candidate: 10/10 PASS, 7.23-8.28 s. Every
run served 9 and refused 15 (15 at the cap), no-decision 0, killed 0; 3-5
launches for 9 served calls; 2-3 instances serving more than one caller;
rounds all in [1.94 s, 2.46 s]. Before the pool fix this failed 10/10 with
rounds [2.3 s, 337-810 us, 337-810 us]; the second and third rounds are now
real work rather than instant refusals.

`per_binary_tests.sh` after its scope fix: 61 targets across 13 packages,
**1031 test cases ran**, 1 target failed. The 49 ignored cases the old scope
excluded were re-counted directly (`--ignored --list` over the workspace) and
are still 49, so the old "every one of the 61 test targets passed in
isolation" was a sentence about 982 of 1031 cases. `pool_concurrency` now
reports 21 passed (19 + the two new checks) where it previously failed; the
one remaining red target is `pseudomux-e2e/full_stack`, which panics with
`PMUX_E2E_TYPESCRIPT_DIST_DIR is required for cross-client E2E`
(`full_stack.rs:2626`) because this harness stages no dist directory. That is
a real precondition, honestly reported and exited 1 on; the gate cell stages
it and the same target passes 10/10 there.

`typescript_tests` vacuity, re-measured by mutation with a failing
`zz-mutation.test.mjs` beside the three named files:

    gate cell argv (hand-list)   exit=0   tests 50   pass 50   fail 0
    package glob (npm test)      exit=1   tests 51   pass 50   fail 1

The cell keeps its literal argv by design, and what closes the gap is the
derivation: `test_run_gate.py::test_the_typescript_cell_runs_every_test_file_
the_package_globs` and `test_runner.py::test_the_typescript_gate_runs_every_
test_file_the_package_globs` both go red and both name `zz-mutation.test.mjs`.
The exact `gate_f/gate_driver_self_tests` argv exits 1 under the mutation and
0 without it, so the GATE is what goes red, not the cell. The file was removed
and `clients/typescript/tests` verified byte-exact
(`dac3ea3f6781ab234dcabe1431fbf8f10fd8f51c30fe5fdb8b72c6fcb1bc120f`).

-- What is still red, and why it stays red ---------------------------------

`gate_f/linux_docker_self_tests`: `109 tests`, `FAILED (failures=1)`, and the
one failure is `test_linux_manifest_is_the_exact_ordered_candidate_projection`
-- debt row C6, the Linux manifest never re-projected after the candidate was
trimmed. Not repaired here, and the reason was re-confirmed by reading the two
tests rather than by repeating the prior claim: `test_runner.py:638-650` is a
literal `required` list that asserts `typescript_package_artifact`,
`python_package_artifact` and `candidate_envelope_tests` are present in the
Linux manifest, and `:808-821` asserts an ordering in which
`release_full_stack_e2e` follows `release_build`. Re-projecting makes the
first test pass and those two fail. Both are Gate C decisions about
unsatisfiable package cells and container phase ordering, and editing them to
make a projection green is exactly the move this tree forbids.

`docs/current-state.md` §7.1 claimed **PASSES** over a 75-cell manifest. The
manifest is 81 cells and the verdict is 80/81; the heading and a leading note
now say so, and the four receipts it lists are marked as attesting a smaller
manifest than the one that runs today.

Two intermittents, chased rather than re-rolled, now debt rows C10 and C11.

C10 `fifteen_concurrent_callers_survive_children_killed_mid_clear`. 2 failures
in 7 whole-target sequences at HEAD, 10/10 green in isolation, and 4/4 green in
a scratch worktree at `<c55>` -- the commit before the cold-swap fix -- which
failed `..._cold_swap_rather_than_starve` 3/4 in the same four runs, the defect
`<c56>` fixed. So this is a regression of the pool fix, not old flakiness. It
is a NON-RECOVERY, not a refusal: after the mid-clear kill the census reads
`registered_instances: 13` against `instance_terminals_present: 9` with
`idle: 13`, `clearing: 0`, daemon `Faulted/Fail`, and two callers hold a
permanent `DaemonLost / private rmux lease was lost during prompt submission`
that survives retry. Four idle instances whose sidecar is gone stay registered
and keep being handed out. NOT repaired here, deliberately: `clearing: 0` at
the moment of failure means the new admission wait is not the proximate cause,
the fix belongs in the destroy/reap path, and a guess at a pool fault-recovery
path is how a 2-in-7 intermittent becomes a permanent one.

C11 `test_source_identity_is_byte_for_byte_canonical_linux_runner_digest`
erroring `workspace revision changed across source capture`. 1 of 3 isolated
runs of 1.5 s each, on a tree whose `git status --porcelain` was byte-stable
throughout. The moving field was isolated exactly -- snapshot, run one `git
status`, snapshot again, and only `git_dir.ctime_ns`/`mtime_ns` differ:
`_repository_control_snapshot` puts the `.git` DIRECTORY's mtime in an
identity, and `workspace_revision_capture`'s own Git queries move it, so the
capture aborts on a change it caused itself. Already a named follow-up --
`phase0_lib.py:1195-1197` calls out `source_digest.py:1309` as "a second,
narrower window with the same cause and it is still open" -- so what was
missing was the rate, not the diagnosis. NOT repaired here: this is the
host-Git apparatus decision D6 de-scopes, `run_gate.py:41-43` already refuses
to import it, and D6 deletes `_repository_control_snapshot` outright.

Residue audit run last, after every E2E run: `Gate A residue audit passed.`,
`candidate_executables=8`. One leaked pool parent was found and reaped -- an
orphaned `target/debug/deps/path_b_pool-..` reparented to PID 1, 2h39m old,
predating this session. The residue audit does not catch it by design: its
scope is the candidate executables in the release directory, and a debug test
harness is not one. Worth knowing before reading "residue audit passed" as
"no leaked test processes".

Verification: `tools/gate-a/tests` OK, `tools/phase0/tests` OK,
`tools/evidence_common/tests` OK, `tools/gate-a-candidate/tests` OK,
`tools/package-smoke/tests` OK, `tools/linux-docker/tests` 109 tests 1 failure
(C6), `ruff check` 36 files / `ruff format --check` 35 files clean over `.`,
`bash -n` and `shellcheck` over all seven scripts clean, `cargo fmt --all
--check` clean, `cargo clippy --locked --workspace --all-targets
--all-features -- -D warnings` clean first-party (the 4 pre-existing
`rmux-server` vendor warnings remain).
`````

<a id="c67"></a>

### 67. Three instances recorded, twelve matrix rows, and a design document that stopped saying nothing here is built

*2026-08-06*

`````text
`docs/current-state.md` gains §9.15, §9.16 and §9.17 -- the eleven-of-twelve corpus, the check whose
message named a defect it could not catch, and the transport that flattened three refusals that
named a field -- and a new invariant 0 in §10: an agent may narrow what a session may name and may
never name a resource on its behalf, with the four properties that keep `spec.md` §4.4 true and the
explicit statement that an agent is NOT a security boundary.

`docs/testing.md` gains AGT-01..AGT-12 and CLI-12 is rewritten for the renamed profile flags,
including the retired-spelling refusal.

`docs/agent-resource.md` was the build input and said "Status: DESIGN. Nothing here is built."; it
now says what shipped, records the four §8 decisions as taken, and lists four deviations with their
arguments -- the directed containment predicate, the opaque echoed spec, the presence-exact
serializer pair, and `--from-profile`. §9.3 records the one thing the design asserted that turned out
to be false: row 17's claim that a digest over the redacted spec would collide.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c71"></a>

### 71. A re-warm counted with a lower bound, and eighteen survivors left open under a reason nobody checked

*2026-08-07*

`````text
THE POOL SCOPE IS MEASURED, AND SO IS THE WHOLE SCOPE. `pool/**` was the one scope with no complete
number and the module where the highest-consequence defects have lived. Two complete runs of
`scripts/gate-a-mutants.sh` at the cell's own settings, both on an idle machine, 5,285 s and 5,099 s:

  BEFORE  702 enumerated, 102 unviable, 600 decided, 561 caught, 39 missed -- 93.50%
                                                    pool/**: 233 decided, 19 missed -- 91.85%
  AFTER   702 enumerated, 102 unviable, 600 decided, 573 caught, 27 missed -- 95.50%
                                                    pool/**: 233 decided,  4 missed -- 98.28%

Complete means `end_time` non-null AND the outcome counts summing to `total_mutants` AND that
equalling the run's own `enumerated_mutants`. Both checks are needed: `outcomes.json` counts the
mutants that got an OUTCOME, so a run stopped at 623 of 1,588 writes `total_mutants: 623` and sums
perfectly against itself, which is how a composition of two partial runs once read as a measurement.
The floor moves 85 -> 94: 1.5 points under the measurement, which is three times the measured noise.

INSTANCE THIRTY-ONE. Section 9.23 closed with "every one of them needs a live pool actor under
`tokio` with slots in specific states, which is a harness this pass did not build. They are the next
agent's work and they are the reason the floor is not higher." `crates/service/tests/path_b_pool.rs`
IS a live pool actor under `tokio` -- deterministic host, driven clock, queueing spawner, real
filesystem, 35 tests, 2,057 lines, cited three rows up in section 4 -- and it had been for the whole
branch. Thirteen of the eighteen needed no harness; they needed a test that OBSERVES something the
existing ones do not. A reason for not doing work is a claim about the tree, checkable exactly like a
closure claim, and nobody had checked it. It also set the floor.

  * A RE-WARM COUNTED WITH A LOWER BOUND. `emptying_a_classes_idle_set_mints_a_replacement_
    immediately` asserted `spawner.pending() >= 1` under "a checkout that emptied a class's idle set
    queues a re-warm". On that path the pool queues TWO things -- the post-answer clear and the
    re-warm -- so THE CLEAR ALONE SATISFIES THE BOUND. The predicate could not tell a queued re-warm
    from no re-warm, let alone from a spurious one, which is why all three `should_rewarm` mutants
    lived under it. Exact counts now, three configurations: dry class beside a free slot (2 queued),
    dry class at the budget (1), class still idle (1).
  * NO TEST HAD EVER MADE A MINT FAIL. `Script::mint_failures` was READ by the double's `mint` and
    written by nothing, in a file whose header says every hard-to-provoke edge "is exercised here by
    telling the double to produce it". So `Pool::abandon_mint` deleted clean. With it deleted the
    instance stays `Reserved` forever: a pool of two whose Claude is misconfigured is permanently
    full after two requests. Both arms covered -- proven-reaped releases the slot and erases the
    tree, may-be-live leaks the slot and KEEPS it.
  * RETENTION HAD ONLY ITS POSITIVE CASE. `1286` marks every instance quarantined, so a healthy
    recycled instance's whole config root is moved to the operator's evidence directory and kept
    forever; `1098` reclassifies a clear that positively typed nothing; `1109` drops `BeginDestroy`
    out of the quarantine path, stranding the instance in `Quarantined` with `Reaped` refused as an
    illegal edge out of it. One test now reads zero, zero and one from one retention directory.
  * THE PRODUCTION SPAWNER HAD NO TEST AT ALL. Every pool test substitutes a queueing spawner --
    which is what makes "the caller never waits on the clear" observable -- so `TrackedSpawner::spawn`
    dropping its future passed the whole suite. What it drops is every post-answer `/clear`.

TWO NEEDED THE HARNESS, AND THEY ARE THE TWO THAT MATTERED. `Pool::check_invariants` is a `pub`
checker with dozens of callers and no test that could fail: every call site asserts `Ok`, forty of
them inside one mixed sequence, and `-> Ok(())` satisfies all forty. `Pool::abandon_unpublishable` is
the teardown for an instance that is "neither idle nor being torn down", which its own doc calls a
capacity leak with no diagnostic. Both answer for states the pool REFUSES to enter, so both need a
planted state, and `PoolState` and `Pool::state` are private to `crate::pool`.
`pool/mod.rs::tests::live` is a real `Pool` with that state reachable. The variant table is READ OUT
OF THE ENUM DECLARATION, and a well-formed pool closes the test so it cannot be satisfied by a
checker that refuses everything.

FOUR ARE EQUIVALENT, WITH THE PREMISE WRITTEN AS A TEST RATHER THAN AN ARGUMENT. `mod.rs:702 <->=`,
`mod.rs:903 <->=` and `mod.rs:903`'s guard `-> true` all widen a capacity test conjoined with
`free_slot(..).is_some()`, and `free_slot` skips exactly the slots `capacity` subtracts.
`a_free_slot_is_never_offered_while_the_pool_is_at_its_budget` enumerates every pool state for
`pool_size` 1..4, so the day the implication stops holding those three become real. `mod.rs:1308
&&->||` needs an `Idle -> Idle` edge; `the_machine_has_no_edge_from_idle_back_to_idle` is that half
written out. Both say in their own docs that they close NO mutant.

EVERY CLOSURE PROVEN BY RUNNING THE MUTANT. 75 mutants over every site ever recorded as surviving:
all 15 CAUGHT, and all 18 of the previous pass's pool closure claims CAUGHT too -- checked site by
site against `caught.txt`, because the first draft of that sentence said "seventeen of the eighteen"
after attributing `mod.rs:1130` to a claim nobody had made.

"ANY TEST THAT GOES FLAKY UNDER LOAD" WAS AS FAR AS ANYBODY HAD LOOKED. Three mutants flipped
between runs that were all quiet, and opening the log of the run that said CAUGHT names the cause in
a minute each:

  v1.rs:1561 exhaustive       <- bounded_soak.rs::repeated_real_rmux_cycles_..._leave_no_residue
                                 "cycle 13 retained ...": rmux.sock still present
  agent.rs:1209 advance_head  <- driver_io.rs::tests::a_preamble_that_lands_after_the_anchor_...
                                 preamble_not_settled after waited_ms: 802
  pool/mod.rs:1130            <- private_runtime.rs::a_terminal_resize_after_creation_...
                                 private rmux sidecar exited unsuccessfully: exit status: 1

All three hold a wall clock or a real sidecar, none of the three mutants can reach any of them, and
the mutation loop runs four full suites in parallel. The error is one-directional -- a spurious
failure is always read as a caught mutant -- so every score here is an over-estimate, by exactly
three mutants between the two complete runs. That is the second reason the floor is 94 and not 95.

`0o600`, NOT `0o300`. The new `erase_tree` fixture closes a slot directory to deny `stat`, which is
the fixture instance twenty-nine got wrong: `0o300` is `-wx` and GRANTS the search bit `stat` needs.
The premise is asserted before anything depends on it, so it fails as a broken fixture rather than
passing vacuously.

AND ONE IN THIS WORK'S OWN SCAFFOLDING, CAUGHT BY ITS FLOOR. The new `declared_violations` scan found
ONE variant of six -- `rustfmt` writes `Name { field: T }` with a space and `Name(T)` without, and
the matcher tested `starts_with('{')` against a leading space. `assert!(declared.len() >= 6)` refused
instead of reporting full coverage over a set of one.

Every one of the 27 remaining survivors falls in a named class and there is no unclassified gap left
in this scope: 16 serde length hints, 4 equivalent pool mutants, 2 `#[cfg(not(unix))]` twins, and 5
individually argued.

This commit also carries the phase-0 and package-smoke work and the rustdoc private-link fixes that
were in the tree when the gate ran; `docs/current-state.md` and `crates/protocol/src/v1.rs` are
edited by both and cannot be split.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c73"></a>

### 73. A hundred and two unviable mutants that were a compile log, and six dependencies a `#[path]` made load-bearing

*2026-08-08*

`````text
THE UNVIABLE LIST IS NOT A DEAD-CODE CANDIDATE LIST, and the brief that sent this pass to it
said it was. Every one of the 102 `Unviable` outcomes in `outcomes.json` carries exactly one
phase result -- `{"phase": "Build", "process_status": {"Failure": 101}}` -- and no `Test`
phase at all; the count is 102 of 102, checked rather than sampled. `Unviable` is rustc
rejecting the mutated source, almost all of it `Ok(Default::default())` against a type with
no `Default`. It is never a statement about reachability. A mutant that survives because
nothing reaches the code is recorded MISSED, and §9.23 already classifies all of those with
no unclassified gap. Recorded here so the next reader does not spend the same afternoon.

DEAD CODE IN THIS TREE IS ENUMERABLE RATHER THAN SEARCHABLE, because rustc already covers
most of it: `dead_code` fires for every private item and for every item of a `bin` crate, and
the workspace compiles with zero first-party warnings (4 in `vendor/rmux-server`, pre-existing).
So it can only survive in four places, and each was enumerated:

  * **`pub` items of the six library crates** -- 910 declarations scanned for references
    anywhere in `crates/ bin/ tools/ tests/ fuzz/`. Two have no reference outside their own
    definition line, and both are test helpers. Fourteen more have no non-test call site
    anywhere -- `driver_io.rs:1429 with_timings`, `machine.rs:82 owns_a_root`,
    `machine.rs:616 idle_is_proof_carrying`, `rmux/src/launch.rs:70 patched`, and ten others.
    That is public-for-testing, not dead, and it is named here rather than narrowed:
    `crates/service/tests/` is an external consumer of `pseudomux-service`, so `pub` is what
    those items need.
  * **Items under a `dead_code` suppression** -- 11 sites. The dead set is the INTERSECTION
    over every target that compiles the module, never the union. Stripping all six module-level
    allows and compiling `--all-targets --message-format json` gives, per item, the set of
    targets calling it dead: `bin/pmux/tests/support/mod.rs` is dead in four of its five
    includers and wholly live in `process_boundary`; `process_support/mod.rs` is wholly live
    in `bounded_soak`; `actual_daemon.rs` peaks at 6 of 7. **Exactly two items are dead in
    every includer** -- `TestTranscript::failing_arm` and `assert_error_code`, both in
    `crates/service/tests/support/mod.rs`, 6/6 targets -- and both are deleted, with the now
    unused `ErrorBody` import. The allows were restored byte-exact before anything else ran.
  * **`cfg`-gated bodies** -- 21 `#[cfg(not(unix))]` and 2 `#[cfg(windows)]`. Left; below.
  * **Manifests** -- below.

SIX PACKAGES' `sha2` IS NOT UNUSED, AND NEITHER TOOL CAN SEE WHY. `cargo machete` reports it
unused in the dev-dependencies of `claude-p`, `pmux-rmuxd`, `pmux-mcp`, `pmuxd`, `pmux-hook`
and `pmux-launcher`. All six `#[path = "../../../tests/support/candidate_binary.rs"]`-include
a module OUTSIDE the package directory machete scans, and that module is where `sha2` is used
(`tests/support/candidate_binary.rs:7`, `:248`). MEASURED by deleting the line: `cargo check
-p pmuxd --tests` gives `bin/pmuxd/tests/../../../tests/support/candidate_binary.rs:7:5:
error[E0432]: unresolved import 'sha2'`; restored byte-exact, and `Cargo.lock` with it, since
the removal rewrote it and putting the manifest back does not put the lock back. In the other
direction `cargo +nightly udeps --workspace --all-targets` reports "All deps seem to have been
used" and misses all four real ones. Neither tool is sufficient; the pair plus a read is.

WHAT THE MANIFESTS ACTUALLY CARRY THAT NOTHING NEEDS. `crates/e2e` declares `serde` and no
file in the package names it. `tools/crash-harness` declares `serde`, `serde_json` and `uuid`
and uses none. `crates/service` and `crates/client` each declare one crate in BOTH tables --
`sha2` and `tokio`, identical `{ workspace = true }` spellings -- where a normal dependency is
already visible to that package's tests, so the second line states nothing. And `crates/e2e`
put five test-only crates in the normal table: it is consumed by `pseudomux-service` as a
dev-dependency, and a dev-dependency's `[dependencies]` are what the consumer pays for, so
`pseudomux-client`, `pseudomux-protocol`, `tempfile` and `tokio` move to `[dev-dependencies]`
where a reader can tell them from `libc`/`serde_json`/`sha2`/`uuid`, which `src/` links.

DUPLICATE VERSIONS: TWO, NEITHER IN A SHIPPED BINARY. `getrandom` 0.3.4 (via `rand_core` <-
`proptest`) beside 0.4.2 (via `tempfile`, `uuid`) -- `cargo tree -i getrandom@0.3.4 -e normal`
prints "nothing to print", so it is dev-only. `syn` 2.0.117 beside 3.0.0 (via `async-trait`);
in the normal graph but only as a proc-macro build input, so it costs a compile and links
nothing. Features were read too: `clap`'s `env` is used at 15 `#[arg(env = ...)]` sites,
`uuid`'s `v4` and `serde` both, `tracing-subscriber`'s `env-filter` and `json` both at
`pmuxd/src/main.rs:916,921`, and `tokio`'s `full` is not over-broad -- `io-std` (`pmux-mcp`
stdio), `process`, `signal`, `fs` and `net` are all reached, leaving only `parking_lot`, which
is tokio's own locking choice and not ours to pick.

VENDOR, REPORTED AND NOT TOUCHED: `vendor/` is 311,685 lines over 643 `.rs` files, 84% of this
tree's Rust files and 73% of its Rust lines, and it pulls 19 of the graph's 116 third-party
crates on its own -- `chrono`, `regex`, `bincode`, `compact_str`, `signal-hook` and 14 more,
none reachable except under a vendored `rmux-*`. pmux runs a patched rmux; it is not ours to prune.

WHAT WAS NOT DELETED, so the next agent does not re-derive it:

  * **The `#[cfg(not(unix))]` fallbacks (21 sites).** §9.23 class 1 keeps two of them as
    unkillable survivors, and that is the right call, but the reason under it is now stronger
    than the comment at `claude_launch.rs:947-951`, which says the branch "exists so the crate
    still compiles where the `cfg(not(unix))` fallbacks elsewhere in this file compile." There
    is no such place. MEASURED, by regating `pub mod attach` in `crates/service/src/lib.rs` to
    a predicate false on this host and compiling: `pseudomux-service` fails with four
    `error[E0433]: failed to resolve` at `native.rs:2024, :2051, :2054, :4330`, all four
    ungated references to `crate::attach`, which `lib.rs:4` gates on `unix`. Restored
    byte-exact. So the configuration those 14 service fallbacks serve cannot build, and the
    5 in `pmuxd` inherit that through its dependency. The 2 in `pmux-launcher` are NOT covered
    by that proof -- `pseudomux-rmux` has no ungated `attach` use -- though `process_boundary.rs`
    reaches `libc::waitpid/kill/getsid` from an ungated module, which is an argument and not a
    compilation. Deleting them is a portability-posture decision with a live citation in
    `docs/current-state.md`, and it would move the mutant enumeration; it is not this pass's.
  * **`MessageBlock::ToolResult` (`v1.rs:2487`), the one wire variant with no producer.**
    `map_message_block` (`actor.rs:3828`) is exhaustive over `ClaudeBlock`, which has no
    tool-result arm, so the daemon can never emit it. It is still not dead: §9.5 row 42 already
    VETOED deleting seven producerless `ErrorCode` variants on the fact that a closed
    deserialize union in three languages makes removing a variant exactly as breaking as adding
    one, and `MessageBlock` is the same shape -- `clients/typescript/src/client.ts:391`
    hard-codes all four `kind` values in a runtime `requireEnumField`, and
    `clients/python/pmux_client/protocol.py:398` mirrors the union.
  * **The `serialize_struct` field accumulator.** Thirteen of this run's 22 survivors are its
    arithmetic, whose only consumer is a length HINT `serde_json` discards. Unobservable, but
    `serialize_struct` requires the argument, so there is nothing to delete.
  * **`docs/current-state.md` §9.3 rows 22-41 and R1-R4, and §9.5 rows 42-56.** Rows 28, 30 and
    31 are the duplication and test-only-fossil candidates a dead-code pass finds first; they
    are adjudicated, graded and dated. Not re-opened.

AND ONE CHECK WHOSE SET IS HAND-WRITTEN. `shared_manifest_value_enums_match_the_rust_string_enums`
(`v1_conformance_vectors.rs:457`) builds a 23-entry `BTreeMap` by hand. Each entry's VALUES are
derived -- `wire_values!` carries an exhaustive match that a new variant breaks -- but the SET
OF ENUMS is a literal, and nothing derives it. Deriving it here from `v1.rs` finds 33
string-valued wire enums and tagged unions; `Request`, `ResponseResult`, `EventPayload` and
`ErrorCode` are pinned by `methods`/`results`/`events`/`error_codes`, which leaves six unpinned,
and `MessageBlock` is the only one of the six a client validates at runtime. Row 34 closed
saying "All 17 nested value enums are pinned"; the set was counted, not derived, and the one
runtime validator it was opened to protect is outside it. Named, not fixed: closing it is new
test code, not a deletion.

BOTH GATES RE-RUN ON THE COMMITTED TREE, detached, quiet machine:

  Gate A   82/83, 1 failed: `gate_f/linux_docker_self_tests` -- the C6 manifest divergence,
           unchanged and untouched. `source_unchanged: true` over 936 files, receipt and
           residue cell both clean, and `scripts/gate-a-residue.sh` re-run by hand after.
  Mutation 702 enumerated, 102 unviable, 600 decided, 578 caught, 22 missed -- 96.33%,
           exit 0 against the floor of 94, 4,909 s. The enumeration and the unviable set are
           the run of record's, mutant for mutant, differing only by the 18 lines `v1.rs`
           moved since `<c70>`.

THE FIVE-SURVIVOR DELTA IS NOISE, AND ONE OF THE FIVE PROVES IT. All five are in §9.23's
documented flip set: three accumulator lines, `agent.rs:1432 sync_parent_directory -> ()`, and
`claude_launch.rs:964 resource_key -> Default::default()`. That last one is the
`#[cfg(not(unix))]` twin: it is not compiled on this host, its mutant is byte-identical to the
baseline, and it came back CAUGHT. A mutant that provably changes nothing cannot be detected,
so that verdict is a flaky test attributed to a mutant -- which is exactly the one-directional
error §9.23 names, now with the sharpest instance available. The score is an over-estimate by
at least that one, which is why the floor sits at 94 and not at the measurement.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c75"></a>

### 75. A brief that promised three drifted cells and thirteen were measured, and a debt row citing a line that moved nineteen

*2026-08-08*

`````text
`docs/linux-handoff.md` (817 lines), for a Linux agent who was not here. Six required sections:
what is done and what its scope boundary is, C6 precisely, the macOS-specific measurements that
must be re-derived rather than ported, what not to redo, the method, and the open items.

THE BRIEF THAT COMMISSIONED IT ASSERTED SEVEN THINGS THE TREE DOES NOT SUPPORT, and §0 records
each with the measurement that decided it rather than transcribing them into a document a Linux
agent would then trust. Two would have sent the reader to the wrong file on the first morning.

* C6's divergence is THIRTEEN names, not three -- measured by running the test, 6 in the Linux
  manifest that the candidate does not have and 7 the other way, `97 - 6 + 7 = 98`. It decomposes
  into four renames the Linux side never followed (mechanical), two unsatisfiable
  `*_package_artifact` cells, three cells the candidate gained from the mutation work, and one
  PHASE conflict -- `release_full_stack_e2e` is in BOTH manifests, `gate_a` in the candidate and
  `D` in Linux, which is the opposite of what the brief said and decides how it gets fixed.
* `test_docker_ownership.py:369` does not write `.pyc`. Disproved both directions: 110 tests with
  `PYTHONDONTWRITEBYTECODE=1` leave 0, the same module without it leaves 3, and the literal `env=`
  dict at `:325` is unchanged in both -- so it cannot be the cause. Every subprocess it is passed
  to is `git`. The gate cell already sets the variable (`phase-manifest.json:732`). Nothing to fix.
* The bug-class counter reads THIRTY-THREE, not thirty-four, and this repository machine-checks it:
  `test_every_statement_of_the_bug_class_counter_spells_the_same_ordinal` is green against four
  Rust sites and the last `### THE BUG CLASS, instance ...` heading. Run it rather than counting.
* 82/83 is real and the sandbox spike was wrong to call it absent -- it is at
  `.context/gate-a-mutants/dead-code-pass/stdout.log:85`, and `.context/` is the last line of
  `.gitignore`, so `git grep` cannot see it. Provenance recorded: `git_head=<c72>` with seven
  modified files that are exactly the seven `<c73>` committed. That their CONTENTS matched is
  marked UNVERIFIED; no diff was stored.
* 96.33% re-derived from the receipt rather than repeated. `service 618` is not reproducible at
  this HEAD by any scoping; `--list` says 637 three ways, and 618 is marked UNVERIFIED.

AND THE DEBT ROW FOR C6 CARRIES THREE ROTTED LINE NUMBERS, which is the bug class inside the row
that records the bug class. `test_runner.py:277` is at `:296`, `:651` at `:634`, `:638-650` at
`:637-649`; `:284`/`:350` name nothing relevant since the 2026-08-06 repair made those literals
derived. `docs/current-state.md:589` already says why -- "A name is greppable; a line number is a
claim nothing checks" -- so this file gives the measured numbers and tells the reader to grep the
symbol.

THE SECTION WORTH THE READER'S TIME IS 3, and its first item was found rather than inherited.
Exactly one shipped code path is `#[cfg(target_os = "linux")]`: the process birth token at
`process_boundary.rs:521`, the only `/proc` reference in the product. It has never been compiled
here, and the 96.33% is structurally blind to it for the reason §9.23 class 1 already states about
`cfg` -- cargo-mutants does not evaluate `cfg`, so the mutant is byte-identical to the baseline.
It is also WEAKER than its macOS twin: `fine: 0` against `pbi_start_tvusec`, a token coarsened to
clock ticks. Debt row C2 dispositions itself with "no supported platform is affected today", and
that sentence expires on Linux; C3's window is "one 25 ms poll gap", now to be read against tick
granularity rather than microseconds. C2/C3/C4 are handed over as live rows, not deferred ones.

The rest of §3 separates mechanism from value for each measurement: the firmlink alias family has
no Linux analogue while the `(st_dev, st_ino)` walk that answers it is POSIX; the keychain's
`sha256(config_dir)[0:8]` service name is Keychain-specific and is the hinge the whole Linux answer
turns on; the 438 ms drain is a macOS corpus; 24x80-against-a-requested-24x120 was fiction once
already and is upstream of the 85/85 composer corpus. §3.2 records the one arrangement in which
`RequireTested` passes on a cell it does not describe -- `CompatibilityReport.os` is
`std::env::consts::OS`, a compile-time constant of the daemon -- and says to let the refusal happen
instead.

All 80 path:line citations machine-checked to resolve and be in range; every quoted string checked
verbatim against its source; all arithmetic re-derived. The seven UNVERIFIED claims are listed by
name in §5 rather than by a count that could go stale. `docs/current-state.md` §7.1 and
`gate-c-linux-handoff.md` §3 are both recorded as needing updates and neither was edited -- they
are owned elsewhere, and this file is not their replacement.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c79"></a>

### 79. Fifty-seven line citations into the two phase0 files the budget fix grew, re-anchored to what each one resolved to before it

*2026-08-08*

`````text
Adding `summarize_attempt_ledger`, `ORDINAL_SPELLINGS` and `phase0.py budget`
inserted 93 lines into `tools/phase0/phase0_lib.py`, 23 into
`tools/phase0/phase0.py` and 119 into `tools/phase0/tests/test_phase0.py`, and
five documents cite those files by line. Left alone, every citation past the
insertion point would have pointed somewhere new.

The mapping is `difflib`'s over each file at `<c77>` against this tree, and
the check is content equality: for every remapped pair, the line now at the new
number is byte-identical to the line that was at the old one. 0 mismatches over
all 57.

**This is a shift, not a verification.** It preserves what each citation
resolved to at `<c77>`; it does not claim that was the right line. Several
plainly were not -- `phase0_lib.py:5030-5034` is introduced as "the comparison"
and lands on `class CampaignRunner:`, and `test_phase0.py:39` is introduced as a
`# noqa: F401` breadcrumb and lands on `PERMISSION_MODES,`. Those were stale
before this commit and are stale after it, at a different number. Debt row 36's
citation lint is what would close them; `docs/gate-c-linux-handoff.md` already
says the durable thing to do is `rg` the quoted text.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c82"></a>

### 82. A review that refuted nothing over a hundred and eight findings, replaced by one that killed nine of twenty-eight and reproduced the two it could not

*2026-08-08*

`````text
`docs/repo-review.md` at `<c76>` opened by reporting 108 findings confirmed, 0
refuted, 0 unjudged, produced by a pass in which two verifiers confirmed 22 of 22
blind. A 0% kill rate is a transcription of the finders, not an adjudication of
them, so the file is replaced rather than annotated: two rankings beside each
other give a reader no way to tell which one was checked.

Re-measured 31 of that version's published claims against this HEAD. 19 upheld as
stated, 5 upheld with a wrong number or a rotted line corrected here, 2
downgraded, 2 refuted outright, 3 already closed by this session's fixes.
Kill-or-correct rate 9 of 28 still-open, 32%, and the document says so in its
second section rather than in a footnote.

The orchestrator handed this pass an EMPTY adjudicated set and empty tallies. The
document states that where a reader will hit it, and the rates it publishes are
its own re-measurements over the subset it could reach -- not the pipeline's.
Seventy-seven claims were not re-reached; they are named as not re-adjudicated
rather than folded silently into the count.

Both Path B pool races were reproduced fresh with a probe crate over the public
`Pool` API, deleted after the run: `pool/mod.rs:1069` panics `no entry found for
key` when shutdown removes a slot under an awaited clear, and shutdown erases a
root under a launching child with `mints=1 destroys=0 leaked=0`. `stateless.rs:450`
takes no `start_guard`, so an `ask` in flight at SIGTERM is the shipped
interleaving, not a contrived one.

Two refutations, stated so nobody re-reports them. `README.md`'s "no flag at
all" meant `--tested-claude-profile` and is exactly what `docs/testing.md:1067`
row S-41 asserts in capitals. `forbidden_flag_count` is redundant rather than a
widened attestation -- the double's early `Err` kills the child and reddens the
lane, so the eight flags in its list are caught by a stronger guard than the
assertion. And the mutation-score story is downgraded because
`docs/linux-handoff.md:174` already records the byte-identical-mutant-CAUGHT
phenomenon as the reason the floor is 94 and not the measurement.

Every `path:line` carried forward was re-resolved before publication. Five of the
old version's had rotted in the interval, which is itself the finding in section
5: 0 of 499 doc citations point past end of file, and a hand-graded sample of 13
puts 5 of them at something other than what the prose says.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c83"></a>

### 83. A review ranked on thirty-one claims it re-measured by hand because the other seventy-seven never arrived, replaced by one merged with all hundred and eight and their adjudications

*2026-08-08*

`````text
The previous version was written when the probe phase failed to launch and the
set arrived empty. Its author refused to rank on missing data and re-measured 31
claims by hand instead, which was the right call and is why that work is carried
forward here rather than discarded.

This version has the set: 108 findings re-probed with a reproduction artifact
each, every critical/high survivor adjudicated. 16 OVERSTATED, 11 DOWNGRADED,
27 of 108 killed or corrected -- 25%, against the 0% of the pass that started
this. 10 duplicates merged, so 98 distinct defects. Five are high and none is
critical.

The two disagreements are resolved in the open. The previous version's top two
MUST FIX rows -- the pool's shutdown races -- are medium here, because
`start_session_owned` holds `start_guard` for its whole body and
`NativeService::shutdown` takes that guard before it can reach `pool.shutdown`,
which narrows the window to the tail of `Pool::mint`; I read that fence rather
than taking the adjudicator's word for it. Both its refutations survive. Its
`FORBIDDEN_FLAGS` row named three omitted flags where one matters, and is
corrected.

Every path:line was re-resolved at this HEAD. Six had rotted in the four commits
since the last version, which is section 5's own subject.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c94"></a>

### 94. A launcher refusal bound whose stopwatch spent 348 of its 350 milliseconds sha256ing the harness's own candidate, and three documents that recorded the remaining 4 ms as 600x headroom

*2026-08-08*

`````text
`socket_and_token_validation_fail_before_broker_use_and_are_bounded` asserted
`started.elapsed() < Duration::from_secs(2)` over a region that also held
`launcher_binary()` and `assert_candidate_unchanged()`. Each of those sha256s
the whole candidate through `tests/support/candidate_binary.rs:245`, so the
region measured, per iteration, two full passes over the 4.3 MB debug binary.

Measured here at HEAD `<c93>`, macOS/aarch64, quiet host:

  one hash          174 ms      (4.27 MB at ~24 MB/s, sha2's soft compress)
  region as written 353 / 366 / 350 ms   for the three cases
  the launcher      4 ms

That is 5.7x headroom against the 2 s bound, not the 600x the docs claimed --
and 98.8% of what the bound gated was this harness. `gate_d/launcher_process`
passed throughout only because `PMUX_TEST_BIN_DIR` aims it at the 1.2 MB
release binary: the same test, differing in nothing but what it hashed.

Reproduced before fixing, with 60 bounded self-terminating spinners (load
average ~60, all reaped): the assertion failed 3/3 at
`process_blackbox.rs:397`. The sibling
`stalled_broker_read_uses_the_shipped_ten_second_deadline_and_redacts_token`
failed 2/2 at `:420` for the same reason -- its region held the same two
hashes, 10.355 s against a 13 s bound.

The timed region now holds one statement. `timed_refusal` takes the candidate
already resolved and cannot reach `CandidateBinaries` at all, and `ChildGuard::
wait` stamps `Output::completed` before it re-hashes, so the stalled test reads
the child's runtime rather than the hashing after it. Both verifications
survive; they happen on either side of the clock instead of inside it.

  region after      4.0 / 4.2 / 5.3 ms   and 10.010 s
  under the same load  3/3 and 2/2 green

The bound is also no longer arbitrary. The token case now points at a socket a
listener holds open and never answers, which is the one case here whose socket a
launcher validating in the wrong order could reach; a launcher that connected
first would pay the shipped ten-second read deadline
(`bin/pmux-launcher/src/main.rs:48`) and the listener is asked afterwards
whether anything ever connected. That is the name's first clause, which nothing
tested before.

Proven able to fail, each restored: the token case given a valid token refused
in 10.0097 s over the 2 s bound; with that assertion removed the same run
reported `a refusal reached the broker socket: Ok((UnixStream ...))`; and the
stalled test's `elapsed`, read from the new field, trips a deliberately
inverted `< 10 s` at 10.299 s.

`docs/testing.md:229`, `docs/current-state.md:1568` and
`docs/gate-c-linux-handoff.md:927` each recorded "about 3.3 ms -- roughly 600x
headroom" for this assertion. 3.3 ms was the launcher; the assertion never
measured it alone. All three are corrected with the measurement and re-anchored
to the lines the fix moved.

Not fixed, measured and named:
`bin/pmux-hook/tests/process_blackbox.rs:303-306` times its stalled relay
across the same two hashes -- the 5.8 MB `pmux-hook` binary, so ~470 ms at the
24 MB/s measured above, inside a region that reported 6.464 s. Its
assertion is a *lower* bound, so hashing can only make it pass -- a hook exiting
at 3.53 s clears a bound that says 4 s. That is a precision loss, not a flake,
in a test with its own C9 adjudication record, and it belongs to its own change.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c104"></a>

### 104. A promotion no one could repeat without improvising, whose only campaign envelope has never once launched the minified cell its gate exists for and still spelled a flag the product renamed

*2026-08-09*

`````text
`require_tested_for_minified_cell` (`v1/registry.rs:143`) gates exactly one
thing, `SessionCell::Minified`, and `tools/phase0` cannot configure one:
`_forwarded_launch_args` forwards six options and `--cell` is not among them.
So no phase0 campaign has ever exercised the cell the gate protects -- including
the one that promoted 2.1.220 -- while `tools/phase0/README.md:620` said a
minified cell "adds three flags to that shape and nothing else".

Forwarding `--cell` would be worse than the omission, which is why the omission
is now a written decision rather than an oversight: seven of the nine graded
prompts instruct the model to run `shasum -a 256`, and a `denied_tools: ["*"]`
cell cannot. (The brief's stronger claim -- that a fabricated digest passes the
oracle -- is NOT reproduced: `verify_calibration.py:592` recomputes each digest
from the poem text pmux captured, and both `mismatch` and `missing` are
`failing_conditions`. The defect is that those grades cannot run at all, one
reserved ordinal each.)

`tools/promotion/promote_claude_version.py` is the acceptance session's checks,
in order, with their pass criteria, as a program. Nine checks; the four free
ones run first so a version that fails one costs nothing. Every turn goes
through `pmux ask`, so every turn is a minified cell, and every graded prompt is
answerable by reasoning alone -- nonce plus arithmetic, an exact unicode echo,
an ordered long reply -- because that is Path B's whole contract. Reuse is pid
identity, not latency. The drain is READ from
`evidence/pooled-transcript-drain-*.json`'s own `recommended_transcript_drain_ms`
and `--bound-ms` is deliberately not an option; the per-version fit is published
under the tool's own `not_to_be_shipped` key and never used. Three live runs at
2.1.226 fitted 250, 500 and 500 ms while the pooled 1000 ms bound held, which is
`docs/version-drift.md` sec.3.2 reproducing on this session's own numbers.

The check set is derived: the tool reads `RepromotionTrigger::detector`'s ids
out of `compatibility.rs` and REFUSES to run unless its checks cover exactly
them, and `every_repromotion_trigger_is_exercised_by_the_promotion_path` is the
other half of that binding. Its probe values name no flag `stateless.rs`'s
workspace scan reserves to four files: the valueless ones are read from
`claude_launch.rs::MINIFIED_CELL_FLAGS`.

Two more defects the derived launch-surface check found, neither of which any
existing test could:

- `--agent-file` was renamed to `--profile-file` and kept only as a HIDDEN
  spelling that refuses by name, and this envelope went on emitting it -- and
  `claude-p` declares no profile option at all. A campaign configured with a
  profile could not launch through either entrypoint and would have discovered
  it one ordinal after reserving one. `test_forwarded_launch_options_reach_pmux`
  asserted the old pair and passed, because the fake pmux accepts any argv.
- `phase0.py budget` now prints `real_turns_outside_the_ledger`, scanned from
  the receipts in `evidence/`, so "this file is not a complete census of real
  Claude turns" is a number (44 today, from the committed turn-latency receipt)
  rather than a paragraph. Reported beside `consumed`, never folded into it:
  D4 is the owner's. A receipt the scan cannot classify stops the count.

Proved red first, each restored: a trigger no check exercises and a trigger
renamed in the Rust both refuse the tool (exit 2); an instance that answers
something other than the reply its prompt specified fails `grades_answer`; one
that returns the prior turn's nonce fails `context_did_not_survive_recycling`;
a warm instance that never reaches `idle` fails `minified_cell_is_admitted`;
restoring `--agent-file` reddens three launch-surface assertions; deleting
`--cell`'s reason reddens two. Positive control for the pid baseline: two
concurrent runs, the later one's baseline holding the earlier one's pid.

Cost: 15 real Sonnet 5 turns across three runs of the new path, low and high
effort, all through minified cells. The ledger is byte-identical -- `pmux ask`
reserves nothing, which is the defect the census above now states in numbers.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c124"></a>

### 124. Five criteria a person checked by reading, made a script that reads the set out of the document stating them and refuses a sixth it cannot measure

*2026-08-11*

`````text
The owner's five criteria for Path B live in `docs/path-b-verdict.md` section 1.
They have been "verified" three times by an agent reading the tree and writing
a paragraph, and the three answers disagreed. `scripts/path-b-done.sh` runs
them: five functions, each of which READS EVIDENCE rather than asserting, and
exit 0 only when all five hold.

**The set of criteria is not in this script.** `criteria_in` reads the ordinal
and title of every `###` heading under section 1, and `bind` refuses -- exit 2,
before a single check runs -- if that set is not the set implemented here, in
either direction. Proved both ways: a sixth heading added to the document is
refused by the count word in the section's own title ("The five criteria" over
six headings), and with the count word corrected it is refused again, by name,
as a criterion nothing measures. The verdict text after each heading's em dash
is a dated record and nothing here reads it.

What each criterion reads:

1. `evidence/path-b-defect-register.json`, new here, bound letter-for-letter to
   the `(a)`-`(d)` list criterion 1 itself publishes -- a fifth letter in the
   document with no row is a refusal -- plus the mutation survivor register,
   held to `mutation_register.validate`, to `scope=full`, to an ancestor head,
   and to `git diff` over the mutation gate's own `FULL_GLOBS` being empty
   since that head. One row is OPEN (a trailing U+0085 is still deleted from a
   caller's prompt, and which behaviour ships is the owner's call), so criterion
   1 is NOT MET and names it.
2. The `cargo test` commands the adversarial document's own "Verification at
   this commit" tables name -- seven of them, derived, and a table naming none
   is fatal. The `fmt`/`clippy`/`ruff`/residue rows in those tables are Gate A
   cells, which criterion 4 reads a receipt for rather than running twice.
3. `claude --version` against `PROMOTED_PROFILES` parsed out of the Rust, then
   the promotion evidence for that version, required to hold every check
   `tools/promotion/promote_claude_version.py` defines -- so a check added to
   the tool makes old evidence insufficient rather than silently unexercised --
   and the minified-cell one is found by what the tool says each check is FOR.
4. A Gate A receipt, or a `scripts/gate-in-worktree.sh` receipt that names its
   commit. Required cells are the manifest's, read out of the judged commit; a
   red cell is admissible only if BOTH criterion 4's own section and an open
   `docs/current-state.md` row 9.4 name it. Either side alone over-derives, and
   that is measured: section 4 also names `gate_f/phase0_self_tests`, and row
   C10 also names `release_full_stack_e2e`. Together they leave exactly the
   Linux cell.
5. The citation grader, run.

**Fail closed.** Missing or stale evidence is NOT MET, never skipped. `--only`
exits 3 and is never a verdict. A verdict from a tree that is not the judged
commit is refused outright, because criteria 2 and 5 run tests against the tree
in front of them.

**Its first run found two drifts the document records as MET.** Claude Code on
this host is **2.1.227** -- the symlink moved at 19:32 on 2026-08-10 -- and
`PROMOTED_PROFILES` covers 2.1.220 through 2.1.226, so criterion 3 is NOT MET
and no promotion evidence exists for what is installed. And `gate_a/rust_fmt`
is red: measured with the pinned-worktree runner at four commits, clean at
`<c118>` and red at `<c119>`, `<c120>` and `<c122>`. Three commits, three
sessions, and the report that landed them said clippy was clean, which it was.

Every refusal above was demonstrated by breaking the evidence, watching the
script name the criterion, and restoring: a register row deleted, a register
citation rotted to a name `<c114>` removed, the promoted range widened with no
evidence behind it, a receipt read against a tree it does not describe, and one
`path:line` citation in `docs/path-b-adversarial.md` moved five lines.

`cargo test --workspace`: 70 binaries, 1202 passed, 0 failed, 51 ignored.
`cargo clippy --workspace --all-targets -- -D warnings`, `ruff check` and
`ruff format --check`: clean. Gate A residue audit: passed.
`tools/phase0/phase0_lib.py` gains one name, because `phase0.py budget` refuses
an evidence receipt it cannot classify as spending turns -- which it did, to
this one, the first time it was run.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c125"></a>

### 125. The seven rustfmt hunks three commits of survivor-killing left in the one file the mutation gate mutates most, red in a cell whose reports only ever named clippy

*2026-08-11*

`````text
`cargo fmt --all --check` has been red since `<c119>` and stayed red through
`<c120>` and `<c122>` -- measured at four commits with the pinned-worktree
runner, clean at `<c118>`. Every one of those sessions reported
`cargo clippy --workspace --all-targets -- -D warnings` clean, and it was; the
cell that was red was `gate_a/rust_fmt`, which no report checked because no
report ran it.

All seven hunks are in `crates/service/src/driver_io.rs`, all inside its test
module -- the fixtures and assertions written to kill the composer and
transcript survivors. `cargo fmt --all` is the whole repair; it changes 21
lines and no token.

It was left unrepaired last session because `driver_io.rs` is inside the
mutation gate's `FULL_GLOBS`, so touching it rots the survivor register's
currency check. That cost is paid here: this commit is the head a full-scope
re-measurement is being taken at.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c143"></a>

### 143. Three intra-doc links from public documentation to items rustdoc cannot reach, and a receipt for twenty-four real turns that the turn budget refused to classify

*2026-08-11*

`````text
Gate A at <c142> was 58/62, against a 61/62 baseline whose sole red is the
deliberate Linux cell. All three new failures were mine, and two of them were
one defect.

`gate_a/rustdoc` runs with `RUSTDOCFLAGS=-D warnings`, which `cargo clippy
--all-targets` does not imply and which nothing else in the local loop runs.
`TerminalScreenState` linked `[`blocking_screen`]` and `to_json` linked
`[`diagnostic_u64`] -- both private -- and one link named
`tests::every_rendering_decision_site_is_registered`, which exists only under
`#[cfg(test)]`. `screen_geometry` already states the rule in its own doc
comment: a private item is named in plain code, not as an intra-doc link.

`phase0/real_claude_turns_outside_the_ledger` keys on a witness each receipt
carries and has NO default, so a receipt it cannot classify stops the count
instead of being silently read as zero -- which is the failure mode a budget
cannot have. It stopped on two, and it was right about both:
`screen-veto-cost-2.1.227-macos-aarch64.json` spends 24 real Sonnet 5 turns
through `pmux ask`, which reserves no ordinal, and
`mutation-filtered-run-native-seam.json` committed at <c139> spends none and
had never been classified either. The veto receipt now declares
`schema: pmux.screen-veto-cost.v1` and a reader counts `turns.total`; the
filtered mutation run joins NO_TURN_RECEIPT_SCHEMAS.

`real_turns_outside_the_ledger` is 78, from 54. The ledger itself is untouched
and byte-identical: none of these turns reserved an ordinal, which is the whole
reason this census exists.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c150"></a>

### 150. Three drafts whose defects a maintainer can still reproduce at 0.10.0, and the one draft whose code quote upstream demoted to a test

*2026-08-12*

`````text
The three unfiled `.context/rmux-issue-drafts/` were written against 0.9.0/0.9.1
and had gone two weeks unfiled. `docs/rmux-upstream-state.md` decides each one
against upstream source obtained this session, not against `vendor/`.

Drafts 01 and 02 are STILL VALID and were REPRODUCED, compiled and run, against
the pristine 0.10.0 crates: a scratch crate on `rmux-client = "=0.10.0"` fails
this tree's own fragmentation regressions with `unknown attach-stream message
tag 27`, and passes once line 694 alone is bounded; upstream `rmux-server`
0.10.0, given only the minimal preclosed-attach regression appended to its own
test module, captures `[]` where `y\r` was written. Both files are byte-
identical at `main` HEAD, and `crates/rmux-client/src/attach.rs` has had no
commit at all since v0.9.0.

Draft 03 survives as a question -- the two ambiguous doc sentences are unchanged
at 0.10.0 -- but its implementation section is stale: `revision_for` is now
`#[cfg(test)]`-only, and a second writer, the surface-stream frame builder,
raises the same registry. That also corrects `docs/repo-review.md:471-472`,
which says all three defects survive byte-identically; two do, and the third's
code quote does not.

Also recorded: no undocumented vendor patch on either crate, two stale directory
prefixes inside `vendor/rmux-server/PMUX-PATCH.md` that its own gate cannot see
because the gate matches names and not paths, and an upgrade sizing whose
unbudgeted item is that pristine `rmux-server` 0.10.0 does not compile its unit
tests in the `--no-default-features` cell this repository validates.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c151"></a>

### 151. A draft that predicted `unknown attach-stream message tag 13` from the one byte value the decoder accepts as a valid tag, and the two upstream reports whose repros now run against the published 0.10.0 crates

*2026-08-12*

`````text
Both drafts were written against 0.9.x and carried three claims that no longer
survive contact with the release a maintainer would read them at.

The client draft's reproduction table predicted that a residual `\r\n` is read
as a new frame header and errors with tag 13. `13` is `RENDER_TAG`
(`rmux-proto` 0.10.0 `src/attach.rs:18`), so the tag guard at `:329` returns
`Ok(None)`, the tail goes to the incremental decoder, and the stream
desynchronises with no error at all -- measured here: with the second frame's
payload set to `b"\r\n"` and a third complete frame sent afterwards, the client
emitted 66 bytes, never delivered the third frame, reported no decode error,
and failed only at EOF. The draft now leads with the symptom that is
unconditional and silent -- a payload completed out of the previous read's
bytes -- and states the loud/quiet split by what the orphaned tail's first byte
happens to be.

The server draft said "reproduced with `--no-default-features`". That cell no
longer builds upstream at 0.10.0, so the sentence would have sent a maintainer
to a broken build; it is gone, and the repro is stated in the default cell it
was actually run in.

Every number was re-measured this session against pristine upstream, not
against the local tree:

* `crates/rmux-client/src/attach.rs` md5 `ccddf857...` at 0.9.0, 0.9.1, 0.10.0
  and `main` (`1f4571e7`), buggy slice at `:694`, bounded sibling at `:709`.
* `crates/rmux-server/src/pane_io.rs` md5 `ac809b29...` at 0.10.0 and `main`;
  `TryAttachRead::Closed` at `:461-469`, the two dispatches at `:480` and
  `:628`, `wire.rs:145-150`; same shape at 0.9.0 `:402` and 0.9.1 `:451`.
* Client repro: a 60-line standalone test on `rmux-client = "=0.10.0"` from
  crates.io fails 10/10 with a payload of nine `A`s where `y\x1b[?1049l` was
  sent, then `unknown attach-stream message tag 121`. The one-line fix makes it
  pass 10/10 and leaves the crate's own 160 tests unchanged.
* Server repro: appended to upstream's own `src/pane_io/tests.rs` at 0.10.0,
  `left: []` against `right: [121, 13]`, 10/10, with `forward_attach` returning
  `Ok(())`. The deferral patch quoted in the draft takes `cargo test --lib
  pane_io::` from 206 passed/1 failed to 207 passed/0 failed; it covers the
  burst loop only, which the draft says.

Each draft now stands alone for a reader who has never heard of this project:
no name, no architecture, one generic sentence of context, and a reproduction
that needs only a published crate.

`.context/` is gitignored, so the filing copies live in `docs/upstream-issues/`.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c152"></a>

### 152. A revision documented as advancing on every mutation, whose registry holds one fingerprint per pane and learns nothing from an interval no capture observed

*2026-08-12*

`````text
`rmux-sdk` 0.9.0/0.9.1/0.10.0/`main` `src/snapshot.rs:25-27` calls `revision` "a
daemon-derived counter that changes whenever the captured pane state mutates".
`PaneSnapshotRevisionRegistry` (`rmux-server` 0.10.0
`src/handler_pane/snapshot.rs:372-411`) stores one `{fingerprint, revision}` pair per
pane and is written only by the three capture paths at `handler_pane/snapshot.rs:144`,
`handler/pane_stream_capture.rs:156` and `:249` — two of which did not exist at 0.9.1,
where the single writer was `:131`. No pane mutation reaches it.

Draft 03 changes shape from the interval question to a documentation report, because
that is what the evidence supports: upstream's own tests already assert the implemented
semantics exactly — `:629-633` "returning to prior content is still a new transition",
`:637-667` "an identical reset publishes a strictly newer revision" — and the design
comment at `:192-201` says the surface and snapshot revisions "are documented as one
shared monotonic counter". Nothing misbehaves; only the prose promises more.

The demonstration is one consumer's two captures run twice over the same pane history,
`(1, 1)` and `(1, 3)`, differing only in whether a second producer materialised a
capture in between. It passes on a pristine `rmux-server` 0.10.0 tree from
`static.crates.io`, default features.

The draft's stale implementation section is gone: `revision_for` is `#[cfg(test)]`-only
at 0.10.0, and "reached only from `handle_pane_snapshot_inputs`" was false. Added: the
two published input lists disagree with each other and neither matches
`compute_snapshot_fingerprint`'s eight arguments (`:333-341`) — the sdk's `:45` folds
four of them into "the underlying process state", the proto's `:771` omits `cols` and
`rows` — and four of those eight are not fields of `PaneSnapshotResponse` at all, which
is why the revision advances between two responses equal in every field a client reads.

49 of 49 `path:line` citations re-checked against the crates the draft names. Every rmux
file cited is byte-identical between 0.10.0 and `main` `1f4571e7`. The two tracked
references to the old filename follow the rename, and `docs/linux-handoff.md:725` no
longer sends a reader to `.context/` for drafts that are tracked.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c157"></a>

### 157. A census whose seventeen categories close exactly to the 585,839 tracked lines, and the two cfg(test) scanners whose braces desynced on a char literal

*2026-08-13*

`````text
Assigns every git-tracked line at <c156> to exactly one semantic category and
shows the parts summing to the whole. Measured with a Rust lexer that reconciles
line-for-line to `wc -l` and a module-graph resolver that follows `#[path]`,
`name.rs`/`name/mod.rs` and `include!`, gated on two invariants: every `#[test]`
in a `src/` tree lands inside a detected `#[cfg(test)]` span, and no non-item
span exceeds three lines.

Headline: first-party production Rust is 47,795 lines, 8.2% of the checkout.
`crates/service/src` at 52,244 is 47.8% test code, so its production half is
27,252. The test-to-production ratio is 2.51:1 in code lines. `vendor/` is 53.9%
of the repository and 1,351 of its lines were written here; the product build
reads 157,341 of the 315,530 vendored lines and compiles 129,551.

Three prior scans disagreed on the `#[cfg(test)]` split by up to 29%. The cause
was the same in both losing cases: matching braces on raw text, so a `'{'` char
literal truncated the span. Two further failure modes are recorded that neither
scan reported -- `#[cfg(test)]` on a struct field has no `;` or `{` and swallows
the next `impl` block, and a generic return type's comma terminates the item
early.

Also corrects three counts whose predicate was narrower than their message:
`#[ignore]` is 51 attributes not 70 grep hits (19 are in doc comments), so
default-run tests are 1,226 and reconcile to the stated 1,224 within two;
`#[cfg(not(unix))]` is 22 attributes not 23 grep hits; and rmux-server's gate
runs 80 of 3,159 test functions, not 80 of the 1,180 that `grep '#[test]'`
finds in a crate that writes `#[tokio::test]`.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c158"></a>

### 158. A handoff rewritten from the tree rather than from its predecessor, and the C6 decomposition that summed seven names out of six and seven

*2026-08-13*

`````text
`docs/linux-handoff.md` was written 2026-08-08 against `<c74>` at 75 commits and last
touched for citation repair. At `<c157>`, 158 commits, it held zero mentions of
`path-b-done`, the survivor register, `SessionRuntime`, the seam, `Unrecognised`,
`register_currency`, `gate-in-worktree` or the 70-cell manifest -- the apparatus that now
defines what a Linux lane inherits for free. Replaced entirely rather than edited, because
the previous rewrite inherited seven wrong premises from its brief and the cheapest way not
to repeat that is to open the code.

Every claim re-derived on this host at `<c157>` and labelled MEASURED, INFERRED or
UNVERIFIED. 50 `path:line` citations, each re-read before commit; the four Path B citation
rules pass, and this file is not in the linted set, which the header says so a reader knows
nothing in the build will catch a rotted number here.

What re-derivation changed against the working notes it was built from:

* C6's thirteen drifted names decompose into **four rename pairs, three cells the Linux lane
  never acquired, and two unsatisfiable `*_package_artifact` entries** -- not the "five
  renames, two new cells, two unsatisfiable" the notes carried, which sums to seven out of a
  set of six and a set of seven. Re-derived from the two manifests and the
  `CONTAINER_ONLY_GATES` frozenset, independently of the failing test.
* `tools/linux-docker/source_digest.py` is 2,063 lines, not the 2,026 both the notes and
  debt row 24 state.
* `crates/service/src/driver_io.rs` is 7,150 `#[cfg(test)]` lines and `native.rs` 5,341, by
  the tree's own `cfg_test_regions` -- the census's 7,204/5,361 came from a different span
  rule, and a third analyst's hand-rolled matcher had already desynced on a `'{'` literal.
* The source digest is not quotable in prose: it is a function of every tracked file, so
  this commit moves it. The command is given instead of the value.
* `docs/gate-c-linux-handoff.md` §3.2 says the candidate is 75 cells and that all 75 are in
  the Linux manifest; it is 70 and seven are absent. Its §4 starting sequence and its
  fifteen container-only names still hold, and the file now says which half to trust.

The done-gate is stated as a commit, not a mood: 5/5 at `<c156>`, 4/5 at `<c157>`, the
difference being one docs commit with no pinned receipt. The finish line is restated as the
five criteria on Linux, with a note that criterion 1 reads
`evidence/path-b-defect-register.json` and never opens `docs/current-state.md` §9.4 -- so a
green criterion 1 on Linux would not re-argue C2, C3 or C4, whose dispositions are macOS
arguments that expire there. That is the house bug class aimed at the done-gate, written
down next to the gate rather than left for the reader to find.

Verified: `cargo clippy --workspace --all-targets -- -D warnings` 0, `cargo fmt --all
--check` 0, `ruff check --no-cache` clean, `cargo test -p pseudomux-service --test
path_b_doc_citations` 4/4, `scripts/gate-a-residue.sh` 0, no `__pycache__` residue.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c159"></a>

### 159. A handoff whose own six numbers were each wider or narrower than the command that produced them, and the conservative Linux birth token that already exists in a test

*2026-08-13*

`````text
Every claim in `docs/linux-handoff.md` was re-run against the tree. All 133
`path:line` citations resolve and were read; six numbers did not survive.

`scripts/gate-a-mutants.sh:194` was cited for a subtraction that happens at
`:175` and is refused at `:186`; `:194` is the full scope's assignment and
subtracts nothing. The "unreachable ordered assertion" was pinned to
`tools/linux-docker/tests/test_runner.py:843`, which is in a different test that
is green today and which goes red if C6 is repaired by moving the cell to phase
A; the unreachable one is `:393`, three lines under the set assertion that
fails. `grep -c "THE BUG CLASS, instance"` returns 16 and 15 of them are
headings -- which is the defect `docs/current-state.md` §9.28 records its own
self-check catching on its second run. `OAUTH_FILE_SUFFIX` occurs five times in
the 2.1.227 bundle and is assigned three. The 44 published by
`docs/2.1.227-compatibility.md` §2 and the 48 here are two denominators, not
four new sites: that scan was `crates/*/src/**.rs`, the same rule reads 46 over
it today, and the real drift is two. `cargo test --workspace` prints 72 `test
result:` lines, 66 binaries and 6 doc-test targets, not 73 binaries.

Two findings the pass produced rather than corrected. The birth token is
implemented twice: `crates/e2e/tests/full_stack.rs:4741` maps only
`ErrorKind::NotFound` to "no token" and propagates every other error, where
production's `crates/rmux/src/process_boundary.rs:523` is `.ok()?` and collapses
`EACCES`, a `hidepid=` mount and a missing `/proc` into the permissive `None`
that debt row C2 asks to be made conservative -- so the tree holds both
dispositions in two crates and ships the weaker. And
`crates/rmux/src/process_boundary.rs:781` asserts a live birth token under
`#[cfg(any(target_os = "linux", target_os = "macos"))]` in the crate's own
`#[cfg(test)]` module, so `cargo test -p pseudomux-rmux` exercises the `/proc`
arm on Linux with no sidecar, no `--ignored` and no credential.

New §4.7: all 51 ignored tests, 44 of them credential-free, are the live-runtime
layer, and a green `cargo test --workspace` on Linux is thin on the operating
system rather than free of it -- two `lifecycle_faults` tests launch a real
sidecar in the default run. The header no longer pins a line count this file's
own edits move.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c162"></a>

### 162. A generator whose own commit put the range one ahead of its class table, so the archive that exists because history is about to be rewritten could not be re-run against the history that carries it

*2026-08-13*

`````text
`docs/defect-log.md` says its generator is "deterministic and re-runnable" and
the generator's own docstring repeats it. The commit that landed both made it
false: `RANGE` is `origin/main..HEAD`, the class table has 160 rows, and the
archive's own commit is the 161st. Running it at the head that carries it exits
on `161 commits in origin/main..HEAD against 160 classified`.

That is the house bug class in the file that catalogues it -- a sentence
promising more than the predicate under it -- and it is the same shape as the
verdict section written in the past tense about a run no receipt records: at the
instant the claim is typed there is no commit for it to be true of.

Neither obvious repair is available. Pinning the endpoint to a hash does not
survive the squash the archive exists because of. Growing the table by a row
saying "here is the archive" files a commit that names no defect, in a document
whose whole premise is one defect per message.

So the tail is DERIVED. A commit after the catalogue is admitted as part of the
archive only if every path it touches was born after the catalogue closed --
`git log --diff-filter=A` over each path, bounded by the last catalogued commit.
Nothing lists the archive's own files, which is the point: a file added to it
later needs no edit here, and a commit touching one product file alongside them
is refused rather than silently dropped from the range. Measured both ways: the
archive commit is admitted, an ordinary product commit is not, and 161 catalogues
back to 160.

The document reproduces byte-identically at this head --
`95874381fc2fc307fcd05758dc27e70b9c29f93951629899ae10cd4041567fc7` before and
after -- which is the property that was being claimed and could not be exercised.

`ruff check --no-cache`, `ruff format --check --no-cache`, `cargo fmt --all
--check` and the gate-a driver self-tests clean; `scripts/gate-a-residue.sh`
passes at candidate_executables=8.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````

<a id="c163"></a>

### 163. A substitution map whose needles were resolved paths against emitters that record unresolved ones, and the sealed ledger a scrub would have forged rather than redacted

*2026-08-13*

`````text
The scrub that redacted `docs/defect-log.md` fixed the file that existed. The next
campaign writes the paths back, so the map now runs at the point of generation:
`tools/evidence_common/portable_paths.py` holds the one derivation --
`tools/defect-log/machine.py` becomes its re-export, and the generator reproduces
the 9,487-line log byte-for-byte through it -- and every emitter of a committed
receipt renders its whole document at the single point it becomes bytes.

THE DEFECT THE TITLE NAMES, found by running the renderer rather than by reading
it. `measure_transcript_drain.py` was driven against a corpus under a temporary
`HOME` and the root came back out of the receipt untouched. The needles were
`pathlib.Path(...).resolve()`; the emitters record what they were given. On macOS
`/var` and `/tmp` are symlinks into `/private`, so `<TMPDIR>` -- the one root that
is always the hashed per-user path -- could never have matched an emitter that did
not resolve, which is most of them. `_spellings` now derives both, and
`scripts/path_b_done.py` had already written the sentence: "two spellings of one
directory are the ordinary case here".

THE LEDGER IS NOT REDACTABLE AND THAT IS THE POINT. `evidence/model-attempt-
ledger.ndjson` carries 2,365 of the tree's 2,391 remaining occurrences and every
one of them is inside a digest. MEASURED: every record seal and every chain link
in the committed file verifies, and with the map applied to a copy, not one does.
Substituting a placeholder there does not redact the record, it forges it, and
`artifact_directory` is re-opened and audited by `_reconcile_prior_usage_locked`
before the next reservation is allowed. So the emitter is unchanged, the reason is
written beside the file in `evidence/README.md`, and the exemption is DERIVED
rather than listed: a file may keep this machine's paths only if every record it
holds is sealed, checked against the file. `test_the_sealed_exemption_is_real_and_
not_a_story` re-runs the forgery on every gate.

The budget arithmetic is indifferent, which was the constraint. `phase0.py budget`
prints `consumed 85 / ceiling 100 / remaining 15` before, after, and over a
redacted copy of the ledger.

THE GATE RECEIPTS KEEP THEIR ABSOLUTE PATHS AND NOW SAY WHY IN THEMSELVES. There a
path is a handle: `path_b_done.py` opens `artefacts[].path`, re-hashes it, and
compares the gate receipt's `workspace` with the pinned runner's `worktree`. Those
two are written by two processes in two different checkouts -- the driver runs
inside the pinned worktree, the runner beside it -- so each would render its own
`<REPO>` and two spellings of one directory would stop comparing equal. Both
receipts gain `paths_are_absolute_because`.

EMITTERS WIRED, derived by scanning for the writers rather than from a list, and
confirmed against which committed artefact each produces: `measure_turn_latency`,
`promote_claude_version`, `measure_transcript_drain`, `verify_calibration`,
`mutation_refilter`, `mutation_register`. Three were RUN and their receipts read
back: `verify_calibration --json` over the real gate-B evidence tree now writes
`<HOME>/pmux-validation-.../gate-b-evidence`; `measure_turn_latency` against the
double writes `<REPO>/target/release/pmuxd`; `promote_claude_version --driver-
environment double` writes `<REPO>/target/release/pmux-test-claude`. Zero offences
in all three.

The nine committed receipts were re-rendered by the same committed program, not by
hand: `git ls-files evidence` now carries no identifier from this machine outside
the sealed ledger, 2,411 occurrences down to 2,391. The 26 that remain are prose
and a captured Claude fixture, which are the scrub's, not the emitters'.

WHERE THE IMPORTS SIT AND WHY. `measure_transcript_drain.py` is cited by line from
three linted documents, the highest at 608; `verify_calibration.py` from two
unlinted ones, the highest at 857. Both bootstraps sit below those lines, so no
sentence moved off the line it names. `every_citation_in_a_path_b_document_lands_
on_what_it_names` stays green; the one citation this change did move --
`evidence/README.md:72-73`, 23 lines further down -- is repointed.

Structure-preservation is a contract and is tested as one: `_validate_campaign_
contract` reads a recorded binary path back for its file NAME and its shared parent
directory, and `<REPO>/target/release/pmux` answers both where a digest or an
elision would not.

Every new test was proved able to fail and every mutation restored byte-exact by
sha256: `_spellings` narrowed to the resolved form (red, and it is the found
defect); the map ordered shortest-first; `render` reduced to a basename; the drain
emitter's render removed; a home path planted back into a rendered receipt; a
placeholder substituted into a placeholder; the seal check made unconditional at
its first statement; the artefact set narrowed to one file; every placeholder
stripped from every committed receipt.

That last one caught one of my own checks being vacuous, which is the same defect
the defect log's check had. `test_a_rendered_receipt_shows_the_map_was_applied`
first read every tracked file, and `evidence/README.md` explains what the
placeholders are and therefore spells all of them -- so with every receipt
stripped the check was still GREEN off the prose. It reads receipts only now, and
the mutation is red.

MEASURED at this tree, all exit 0: `cargo fmt --all --check`, `cargo clippy
--workspace --all-targets -- -D warnings`, `ruff check --no-cache`, `ruff format
--check`, `shellcheck`, `bash -n`, `scripts/gate-a-residue.sh`. Python: gate-a
driver 76, scripts 48, evidence_common 65, phase0 261, candidate envelope 20,
package smoke 36, all green; linux-docker 111 with the one failure it already had,
`test_linux_manifest_is_the_exact_ordered_candidate_projection`, measured red
before this change. `cargo test --workspace --no-fail-fast`: 71 targets ok, and
`path_b_doc_citations` at its pre-existing single failure --
`nothing_cites_a_path_b_document_by_line_number`, eight offences in
`docs/pre-push-review.md`, untouched by this work. Same 71/1 as before it.

NOT ESTABLISHED. The phase0 reservation writer records paths that three modules
cross-check and that `_validate_public_file_identity` requires to be absolute;
making it portable is a campaign-contract change and is not attempted here. No
live campaign was run, so `mutation_refilter` and the promotion tool's real-Claude
path were exercised only through the double. `mutation_register` records no
absolute path today, so its render is a no-op that will matter the first time it
does.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
`````
