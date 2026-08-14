# Code census

**Scope.** Every git-tracked line in the repository at `f4622a9` (branch
`N0xMare/plan-pmux-architecture`), assigned to exactly one semantic category. Measured
2026-08-13 on `aarch64-apple-darwin`, tree clean before and after.

**Denominator.**

    git ls-files | wc -l                        -> 982 files
    git ls-files -z | xargs -0 wc -l | tail -1  -> 585,839 lines

Every table below closes to that number or to a stated subset of it. Where a category count
would otherwise omit a directory, the omission is named.

**Method.** Rust line classification and the production/test split come from a purpose-built
lexer (`/tmp` scratch, not committed) that char-scans each file tracking line comments (doc
`///` `//!` vs plain), nested block comments, normal/byte/C strings, raw strings with hash
counts, char literals vs lifetimes, and backslash-escaped newlines. Its per-file line totals
sum to 145,342 for non-vendor `.rs`, i.e. identical to `wc -l`, and its blank count (8,171)
matches `git ls-files '*.rs' | grep -v ^vendor/ | xargs awk 'NF==0' | wc -l` exactly. On top
of that, `#[cfg(test)]` item spans are found by bracket matching over the comment- and
string-blanked text, and the Rust *module graph* is resolved (including `#[path = "..."]`,
`name.rs`/`name/mod.rs`, and `include!`) so that files which are test-only by virtue of their
declaration site are found even though they contain no `#[cfg(test)]` themselves.

Two invariants gate the split, and both hold:

- every one of the 656 `#[test]`-family attributes in a `src/` tree falls inside a detected
  `#[cfg(test)]` span — zero outside;
- no span attached to a non-item (a struct field, a statement, a match arm) exceeds three
  lines, and no span runs past EOF.

---

## 1. The census

| # | Category | Lines | % of repo |
|---|---|---:|---:|
| A | Product code — first-party Rust | 47,795 | 8.16% |
| B | Product code — vendored fork Rust (`vendor/`) | 145,486 | 24.83% |
| C | Product code — client bindings (Python + TypeScript) | 5,933 | 1.01% |
| D | Tests — first-party Rust | 97,547 | 16.65% |
| E | Tests — vendored fork Rust | 166,199 | 28.37% |
| F | Tests — client bindings | 5,181 | 0.88% |
| G | Dev tooling — gate/evidence machinery (Python) | 40,802 | 6.96% |
| H | Dev tooling — its own tests (Python) | 19,924 | 3.40% |
| I | Dev tooling — shell gate scripts | 4,532 | 0.77% |
| J | Documentation (Markdown, non-vendor) | 24,555 | 4.19% |
| K | Evidence and receipts (JSON/NDJSON emitted by gates) | 14,738 | 2.52% |
| L | Fixtures, conformance vectors, gate manifests, client configs | 3,841 | 0.66% |
| M | Lockfiles (all, incl. vendor) | 6,501 | 1.11% |
| N | Cargo manifests (all, incl. vendor) | 1,038 | 0.18% |
| O | Vendor prose and packaging metadata | 257 | 0.04% |
| P | Prompt corpora (`.txt`) | 500 | 0.09% |
| Q | Misc — licences, Dockerfile, ignores, checksums, C fixtures, fuzz corpus | 1,010 | 0.17% |
| | **TOTAL** | **585,839** | **100.00%** |

Sum check: 47,795 + 145,486 + 5,933 + 97,547 + 166,199 + 5,181 + 40,802 + 19,924 + 4,532 +
24,555 + 14,738 + 3,841 + 6,501 + 1,038 + 257 + 500 + 1,010 = **585,839**.

Roll-ups from the same partition:

| Roll-up | Lines | % |
|---|---:|---:|
| All product code (A+B+C) | 199,214 | 34.0% |
| All test code (D+E+F+H) | 288,851 | 49.3% |
| All documentation and receipts (J+K+O) | 39,550 | 6.8% |
| All dev tooling (G+I) | 45,334 | 7.7% |
| Machine-generated / carried config (L+M+N+P+Q) | 12,890 | 2.2% |
| Vendored fork, all of it (B+E + its share of M,N,O) | 315,530 | 53.9% |
| Everything pmux wrote (585,839 − vendor 315,530 + 1,351 patch) | 271,660 | 46.4% |

**Category definitions, so the predicates are not narrower than the labels.**

- **A** is `crates|bin/*/src/**` minus `#[cfg(test)]` spans, minus the two files that are
  test-only by declaration, minus `crates/e2e/src` (a test double), minus
  `tools/crash-harness/src` (a measurement instrument outside the workspace).
- **B** is `vendor/*/src/**` minus `#[cfg(test)]` spans, minus every file reached only from a
  `#[cfg(test)]`-gated `mod` declaration; it includes `vendor/rmux-server/build.rs` (39).
- **C** is `clients/python/pmux_client/**` (2,710) + `clients/typescript/src/**` (3,223).
- **D** is inline unit tests + integration-test targets + first-party test support, in Rust.
- **E** is the same three for the vendored fork.
- **G/H** is all `*.py` outside `clients/`, split by `tests/` dir or `test_` filename.
- **K** is `evidence/*.json|ndjson`; `evidence/README.md` (670) is counted in **J**.

---

## 2. First-party Rust, in detail

Denominator: `git ls-files '*.rs' | grep -v '^vendor/'` → **129 files, 145,342 lines**.

| Bucket | Lines | Code | Doc cmt | Plain cmt | Blank | cmt/code |
|---|---:|---:|---:|---:|---:|---:|
| Production | 47,795 | 32,007 | 10,460 | 2,739 | 2,589 | 0.41 |
| Inline unit tests (`#[cfg(test)]` + 2 declaration-gated files) | 32,743 | 25,875 | 3,613 | 1,463 | 1,792 | 0.20 |
| Integration tests (`tests/` targets) | 59,669 | 49,797 | 4,158 | 2,251 | 3,463 | 0.13 |
| Test support, harnesses, fuzz targets, instruments | 5,135 | 4,545 | 166 | 97 | 327 | 0.06 |
| **TOTAL** | **145,342** | **112,224** | **18,397** | **6,550** | **8,171** | 0.22 |

Sum check: 112,224 + 18,397 + 6,550 + 8,171 = 145,342.

**Test-to-production ratio: 2.51 : 1 in code lines** (80,217 test-ish code vs 32,007
production code); 2.04 : 1 in physical lines. Production is **32.9%** of first-party Rust
by physical lines and **28.5%** by code lines.

Per package:

| Package | Production | Inline unit | Integration | Support | Total |
|---|---:|---:|---:|---:|---:|
| crates/service | 27,252 | 24,992 | 24,856 | 1,754 | 78,854 |
| crates/e2e | 0 | 0 | 11,321 | 1,339 | 12,660 |
| crates/protocol | 4,314 | 0 | 5,958 | 0 | 10,272 |
| bin/pmux | 3,407 | 1,705 | 4,128 | 669 | 9,909 |
| crates/claude | 3,839 | 1,665 | 4,315 | 0 | 9,819 |
| crates/client | 1,856 | 502 | 2,727 | 0 | 5,085 |
| crates/rmux | 2,318 | 659 | 1,636 | 0 | 4,613 |
| bin/pmux-mcp | 1,614 | 1,247 | 1,091 | 0 | 3,952 |
| bin/pmuxd | 1,719 | 1,586 | 614 | 0 | 3,919 |
| bin/claude-p | 505 | 126 | 1,549 | 0 | 2,180 |
| bin/pmux-rmuxd | 676 | 99 | 565 | 0 | 1,340 |
| bin/pmux-hook | 169 | 138 | 433 | 0 | 740 |
| bin/pmux-launcher | 126 | 24 | 476 | 0 | 626 |
| tools/crash-harness | 0 | 0 | 0 | 569 | 569 |
| fuzz | 0 | 0 | 0 | 544 | 544 |
| tests (workspace root) | 0 | 0 | 0 | 260 | 260 |
| **TOTAL** | **47,795** | **32,743** | **59,669** | **5,135** | **145,342** |

`crates/service` alone is **57.0% of all first-party production Rust** and 54.3% of the
first-party Rust tree.

### The `service/src` = 52,244 figure, resolved

The number is real but is not a production count. `crates/service/src` decomposes as:

    27,252  production
    23,232  inline #[cfg(test)] spans
     1,496  src/native/tests/seam.rs  (declared `mod seam;` at native.rs:4760,
                                       inside the #[cfg(test)] mod tests block at 4728)
       264  src/source_scan.rs        (declared `#[cfg(test)] mod source_scan;` at lib.rs:19-20)
    ------
    52,244

**47.8% of `crates/service/src` is test code.** The two largest files invert on this
measure: `driver_io.rs` is 11,410 lines of which 7,204 are `#[cfg(test)]` (4,206 production),
and `native.rs` is 10,068 of which 5,361 are `#[cfg(test)]` (4,707 production). The
production heavyweight is not `driver_io.rs` but `v1/actor.rs` (4,193 production lines,
3,422 of them code).

Exactly two files under a `src/` tree are entirely test code without containing a
`#[cfg(test)]` themselves; both are in `crates/service`, both are found only by resolving the
module graph, and together they are 1,760 lines. A path-based `src`/`tests` split misses both.

### Test functions

    1,277  #[test] / #[tokio::test] / #[rstest] / #[proptest] attributes (671 in src, 606 in tests/)
       51  #[ignore] attributes, all attached to one of the above
    -----
    1,226  default-run test functions

`git grep -c '#\[ignore'` reports **70**; 19 of those hits are inside doc comments discussing
ignore policy. 51 is the count in code. 1,226 is within 2 of the 1,224 the project quotes as
passing; the residual 2 is not explained here (no `cargo test` was run) but it is not platform
gating — zero `#[test]` attributes sit inside a `cfg` block this host cannot compile.

---

## 3. The vendored fork

`vendor/` is 661 files and 315,530 lines — **53.9% of the repository**.

### How much of it pmux wrote: 1,351 lines (0.428%)

    /usr/bin/diff -rq ~/.cargo/registry/src/index.crates.io-*/rmux-server-0.9.0 \
        vendor/rmux-server -x .cargo-ok
      -> Only in vendor/rmux-server: PMUX-PATCH.md
         src/pane_io.rs differs, src/pane_io/tests.rs differs

    /usr/bin/diff -ru ... | grep -c '^+[^+]'  -> 1,199      (pane_io.rs +384, tests.rs +815)
    /usr/bin/diff -ru ... | grep -c '^-[^-]'  ->    44

    rmux-client: PMUX-PATCH.md new, and src/attach.rs differs by ONE line:
      -  decode_attach_data_frame(&read_buffer[consumed..])
      +  decode_attach_data_frame(&read_buffer[consumed..bytes_read])

    1,199 + 1 + 119 + 32 (two PMUX-PATCH.md) = 1,351 authored
    315,530 - 1,351                          = 314,179 carried byte-identical

### What the fork is made of

| | rmux-server | rmux-client | Total |
|---|---:|---:|---:|
| Production Rust | 133,376 | 12,071 | 145,447 |
| `build.rs` | 39 | 0 | 39 |
| Inline `#[cfg(test)]` | 28,405 | 3,936 | 32,341 |
| Test-module files (reached only from a `cfg(test)` declaration) | 118,169 | 5,360 | 123,529 |
| `tests/` dirs | 8,409 | 1,920 | 10,329 |
| **Rust total** | **288,398** | **23,287** | **311,685** |
| Non-Rust (locks, manifests, prose, tunnel configs) | 2,049 | 1,796 | 3,845 |
| **Grand total** | **290,447** | **25,083** | **315,530** |

**The vendored fork is 53.3% test code by line.** Its own test-to-production ratio is
1.14 : 1 — less than half pmux's 2.51 : 1.

### What the build actually compiles

`Cargo.toml` excludes both vendor crates from the workspace and re-enters them through
`[patch.crates-io]`, and `bin/pmux-rmuxd` takes `rmux-server` with `default-features = false`
(the default feature is `web`). Measured from cargo's own dep-info, which lists every file the
compiler read:

    tr ' ' '\n' < target/debug/deps/rmux_server-*.d | grep vendor/rmux-server \
      | sed 's/:$//' | sort -u | xargs wc -l   -> 356 files, 149,203 lines
    (the release .d gives the identical set)

| | Files | Lines read | of which production |
|---|---:|---:|---:|
| rmux-server compiled into the product | 356 | 149,203 | 122,851 |
| rmux-server never read by the compiler | 204 | 130,747 | 10,525 |
| rmux-client compiled into the product | 29 | 8,138 | 6,700 |

All of `vendor/rmux-server/src/web/` (13,637 lines) is feature-gated off; the only compiled
path containing "web" is `src/handler/web_request_identity.rs`. **pmux depends on 129,551
production lines of vendored code and carries 315,530 lines to get them — a 2.4x carry
ratio.**

### What of it is tested here

`tools/linux-docker/suite.sh` is the only place vendor tests run:

    rmux_vendor_standalone_tests:            cargo test --manifest-path vendor/rmux-client/Cargo.toml
                                             --all-targets --all-features        (everything)
    rmux_server_vendor_patch_regressions:    cargo test --manifest-path vendor/rmux-server/Cargo.toml
                                             --lib --no-default-features pane_io::tests::

A comment-aware count of `#[test]`/`#[tokio::test]`/`#[rstest]`/`#[proptest]` attributes over
`vendor/rmux-server/{src,tests}` gives **3,159 test functions**. The name filter selects the
80 under `pane_io::tests::` (74 in `src/pane_io/tests.rs`, 6 in
`src/pane_io/tests/persistent_overlay.rs`). **2.5% of rmux-server's test functions ever
run**, and its entire `tests/` directory (24 files, 8,409 lines, 203 test functions) is
compiled by `clippy --all-targets` and executed by nothing.

(A bare `grep -rc '#\[test\]'` reports 1,180 for the same tree, because this crate writes
most of its tests as `#[tokio::test]`. Using that as the denominator would put the coverage
at 6.8% instead of 2.5% — the same predicate-narrower-than-the-message failure, this time in
the measuring command.)

That is defensible policy — you regression-test your patch, not upstream — but the gate is
*named* `rmux_server_vendor_*` and its predicate is `pane_io::tests::`. The name is wider
than the predicate.

---

## 4. Non-Rust mass

Non-vendor, by extension. `git ls-files | grep -v '^vendor/'` → 321 files, 270,309 lines.

| Ext | Files | Lines | Role |
|---|---:|---:|---|
| `.rs` | 129 | 145,342 | see §2 |
| `.py` | 45 | 65,823 | 43,512 tooling + 22,311 its own tests |
| `.md` | 34 | 24,555 | 20,996 in `docs/`, 979 root README, 2,580 elsewhere |
| `.json` | 23 | 18,526 | 14,661 evidence + 2,438 vectors + 1,324 manifests + 103 client config |
| `.sh` | 10 | 4,532 | gate scripts |
| `.lock` | 3 | 3,356 | root 1,941, fuzz 386, crash-harness 1,029 |
| `.ts` | 5 | 3,223 | TypeScript client |
| `.mjs` | 6 | 2,794 | TypeScript client tests |
| `.toml` | 18 | 544 | manifests |
| `.txt` | 24 | 500 | phase0 prompt corpora |
| (no ext) | 9 | 431 | Dockerfile 213, licences 207, fuzz corpus 11 |
| `.c` | 2 | 242 | process fixtures for the Python tooling |
| `.dockerignore` | 2 | 234 | |
| `.ndjson` | 3 | 85 | ledger + capture corpora |
| `.sha256` | 2 | 81 | |
| `.gitignore` | 2 | 22 | |
| `.jsonl` | 3 | 19 | transcript fixtures |
| `.typed` | 1 | 0 | PEP 561 marker |
| **TOTAL** | **321** | **270,309** | |

270,309 + 315,530 = 585,839.

**Python is the second language at 65,823 lines** — larger than every Rust crate's `src/`
except `service`, and it carries 19,924 lines of its own tests, i.e. it is maintained as
product. It is the gate runner (`tools/gate-a`, `tools/gate-a-candidate`), the real-Claude
attempt ledger (`tools/phase0`, 17,484), the Linux container suite (`tools/linux-docker`,
13,718), the evidence emitters (`scripts/*.py`, 4,592) and the promotion tooling. Any
statement of the form "pmux is N lines of Rust" understates the maintained surface by a
quarter.

---

## 5. The branch, decomposed

    git diff --numstat main...HEAD | awk '{i+=$1;d+=$2;n++} END{print n,i,d}'
      -> 1053 files, 583,854 insertions, 15,352 deletions   (156 commits)

| | Insertions | Share |
|---|---:|---:|
| Carried vendor (315,532 diff lines − 1,351 authored) | 314,181 | 53.81% |
| Gate-emitted evidence receipts (15,408 − 670 README) | 14,738 | 2.52% |
| Lockfiles (root 695, crash-harness 1,029, fuzz 386, npm 51) | 2,161 | 0.37% |
| **Authored work** | **252,774** | **43.29%** |

Deletions tell the same story: 5,210 of the 15,352 are `apps/` moving into `bin/`, a rename
accounted as churn.

The honest headline for this branch is **~253,000 authored lines**, not 584,000 — of which
145,342 Rust (47,795 production), 65,823 Python, 24,555 Markdown.

---

## 6. Spot-check log

Three analysts measured in parallel. Every number below was re-derived here.

| Claim | Source | Verdict |
|---|---|---|
| 129 non-vendor `.rs` files, 145,342 lines | all three | **held**, exact |
| 982 tracked files, 585,839 lines | analyst 3 | **held**, exact |
| `service/src` inline test = 24,732 (incl. `seam.rs`) | analyst 1 | **held** to within 4 lines (I measure 23,232 + 1,496 = 24,728) |
| `service/src` inline test = 20,030 | analyst 2 | **refuted** — 14% low |
| `service/src` inline test = 16,579 | analyst 3 | **refuted** — 29% low |
| `native.rs` cfg(test) = 5,361 | analyst 1 | **held**, exact |
| `native.rs` cfg(test) = 2,551 | analyst 2 | **refuted** (brace matcher desynced on a `'{'` char literal) |
| Production Rust 47,791 / code 32,008 / doc 10,460 / cmt 13,199 | analyst 1 | **held** (I get 47,795 / 32,007 / 10,460 / 13,199) |
| Test-to-production code ratio 2.51 : 1 | analyst 1 | **held** (2.506) |
| tokei's columns omit doc comments: 126,879 vs 145,342 | analyst 1 | **held** — tokei reports code 112,251 / comments 6,539 / blanks 8,089, an 18,463-line hole against my 18,397 doc-only lines. tokei's blank count (8,089) is also wrong; `awk 'NF==0'` says 8,171 |
| Exactly two src files test-only by declaration, 1,760 lines | analyst 1 | **held** — confirmed by full module-graph resolution, not grep |
| 4 compiler warnings, all in vendor, zero first-party | analyst 2 | **held** — `cargo check --workspace --all-targets` reproduces exactly those 4 |
| `#[cfg(not(unix))]` = 23, not 21 | analyst 2 | **held as a grep count**, but 1 of the 23 is inside a doc comment; the real attribute count is **22** |
| 40 never-compiled cfg blocks, ~250 lines | analyst 2 | **held** — I measure 40 blocks / 257 lines, and zero `#[test]` inside them |
| jscpd: 129 sources, 45 clones, 845 lines, 0.58% | analyst 2 | **held**, exact, at `--max-lines 60000` |
| Duplication split 684 test / 152 prod / 54 mixed | analyst 2 | **held**, exact |
| `SCAN_SKIPPED_DIRECTORIES` defined 3x, one with `vendor` and two without | analyst 2 | **held** |
| claude-p ↔ pmux `--env` clone | analyst 2 | **held with a nuance** — `bin/claude-p/src/main.rs:291-366` vs `bin/pmux/src/cli.rs:1643-1723` differ in 4 hunks (the env lookup is injected in one and `std::env::var_os` in the other, plus two comments); "duplicated verbatim" overstates it, "the same validation maintained twice" does not |
| `tools/crash-harness` referenced by nothing executable | analyst 2 | **held** — 3 docs only, not a workspace member |
| 7 of 9 `tools/` subdirs in `phase-manifest.json` | analyst 2 | **held** |
| Vendor authored = 1,351 lines (0.428%) | analyst 3 | **held**, exact, by diff against a pristine crates.io 0.9.0 |
| 356 files / 149,203 vendor lines compiled; 204 / 130,747 not | analyst 3 | **held**, exact |
| `docs/current-state.md` = 337,207 bytes | analyst 3 | **held**, exact |
| Only 80 of rmux-server's 1,180 test functions run (6.8%) | analyst 3 | **numerator held, denominator refuted** — 1,180 is `grep '#\[test\]'`, but this crate writes most tests as `#[tokio::test]`. Comment-aware count is 3,159, so the figure is **2.5%** |
| `rg -n 'target_os' bin/ clients/ crates/ fuzz/ tests/` = 30, against a doc that says 25 | analyst 2 | **held**, exact |
| `cargo machete` reports zero unused dependencies | analyst 2 | **held** |
| `pmux-test-claude` is in the Gate A residue floor | analyst 1 | **held** — `scripts/gate-a-residue.sh:182-195` names "those eight" and lists it |
| Python 65,823 = 43,512 prod + 22,311 test | analyst 3 | **held**, exact |
| Branch decomposition 252,774 authored | analyst 3 | **held**, exact |
| 70 `#[ignore]` attributes → 1,207 default-run | analyst 1 | **refuted** — 19 of the 70 grep hits are inside doc comments. 51 real attributes, 1,226 default-run, which reconciles to the stated 1,224 within 2 |
| 6 historical evidence files | analyst 2 | **partly refuted** — 5 files have zero references from any `.rs`/`.py`/`.sh`/`.json`; `turn-latency-double-macos-aarch64.json` is read by `tools/phase0/tests/test_phase0.py` |
| 3,604 lines of docs no executable reads | analyst 2 | **refuted as stated** — the correct set is 7 files / 3,464 lines (it omits the three `docs/upstream-issues/` drafts and wrongly includes `linux-handoff.md`) |

**Where the analysts disagreed, the cause was always the same.** Analysts 2 and 3 matched a
`#[cfg(test)]` block's braces on raw text, so a `'{'` char literal or a brace inside a string
desynced the count and the block was truncated. Analyst 1's method (blank the strings and
comments first, then match) is correct, and the invariant that catches the failure is cheap:
*every `#[test]` in a `src/` file must land inside a detected `cfg(test)` span.* Analyst 1
reports that under the naive method, 91 of the 132 test attributes in `driver_io.rs` fall
outside every span; I did not reproduce that specific figure, but I did reproduce the
mechanism, and my own first attempt failed the same invariant until the string and comment
blanking was in place.

I hit two further failure modes analyst 1 did not report, and both silently inflate the test
count rather than deflate it:

- `#[cfg(test)]` on a **struct field** (`crates/claude/src/cursor.rs:82`) has no `;` or `{`,
  so a scanner looking for one runs on to the next `impl` block and swallows 190 lines. In
  `crates/claude` alone that is a 180-line overcount (1,845 vs the correct 1,665).
- `#[cfg(test)] async fn f(...) -> Result<(), E> {` terminates early at the comma inside
  `Result<(), E>` unless generics are excluded from comma termination.

---

## 7. What the shape means

### Is the repo unusually large for what it does?

The product is seven binaries that drive Claude Code CLI instances inside rmux panes. The
first-party production code for that is **47,795 lines of Rust plus 5,933 lines of client
bindings — 53,728 lines, 9.2% of the checkout.** That is a normal size for what pmux does.
Nothing about the product is bloated.

The checkout is 585,839 lines because of three multipliers stacked on top of that core:

1. **the vendored fork: 315,530 lines, 53.9%** — 5.9x the product code, to carry a
   1,351-line patch;
2. **first-party tests: 97,547 lines, 16.7%** — 2.04x the first-party production Rust;
3. **gate machinery: 45,334 lines of Python and shell plus 14,738 lines of receipts, 10.3%**
   — 1.1x the product code.

So the honest sentence is: *pmux is a 54k-line product carried inside a 586k-line
repository, and 90.8% of the repository is verification apparatus, vendored dependency, or
the record of having verified.* Whether that is too much depends entirely on the three
sections below, and my answer differs for each.

### The test-to-production ratio: defensible, with one inert region

**2.51 test code lines per production code line** (80,217 : 32,007), 1,226 default-run test
functions, one test function per 39 production lines. For a project whose thesis is that an
agent-written control plane can be *certified*, that ratio is the thesis. I would not cut it.

Three observations qualify that.

**The ratio is not uniform, and its distribution is right.** `crates/protocol` — the wire
contract — has zero inline tests and 5,958 lines of integration tests plus 2,438 lines of
language-independent conformance vectors: the contract is tested from outside, which is the
only way a three-language binding set can be kept honest. `crates/service` runs 1.83 : 1 and
holds 57% of production. `bin/pmux-launcher` has 126 production lines and 476 lines of
black-box process tests, a 3.8 : 1 ratio on the smallest binary — which is correct, because
it is a setuid-adjacent process boundary where the interesting behaviour is entirely
observable from outside.

**The genuinely inert region is small and identifiable.** 51 tests carry `#[ignore]`, 45 of
them in `crates/e2e` and `crates/service` gated on a real Claude or a real daemon. Those are
not inert — they are the ones the gate runs deliberately. What *is* inert:
`tools/crash-harness` (1,637 tracked lines including a private 1,029-line lockfile pinning
108 packages), which is not a workspace member, appears in none of the gate cells, is named
only in three prose paragraphs, and therefore is not compiled by any CI. It still builds
(`cargo check --offline --manifest-path tools/crash-harness/Cargo.toml --all-targets` →
Finished, 0 warnings), so it is dormant, not rotted. It depends on `pseudomux-service` by
path, so an API change would break it silently. That is 1,637 lines of latent maintenance
with a zero-line owner. Either wire it into a gate cell or delete it.

**The duplication in the test mass is real but is the cheap kind.** jscpd at a 15-line
threshold finds 45 clones and 845 duplicated lines, 0.58% of the tree; 684 of those lines are
test↔test and only 152 are production↔production. Copy-paste in test scaffolding is often
deliberate (an independent oracle must not share code with the thing it checks). The one
production clone worth acting on is the `--env` KEY=VALUE parser and
`validate_environment_name`, maintained in two places
(`bin/claude-p/src/main.rs:291-366` and `bin/pmux/src/cli.rs:1643-1723`) for a surface whose
own comments say the value is withheld from error text "because it may be a secret."

The one place where "deliberate independent oracle" has already cost something: the e2e test
double's `TEST_TRANSPARENT_EXACT_KEYS` (10 entries) has drifted from the protocol's
`TRANSPARENT_EXACT_KEYS` (11), missing `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS`. And
`SCAN_SKIPPED_DIRECTORIES` exists three times with two different contents — the copy in
`crates/service/tests/path_b_doc_citations.rs` excludes `vendor`, and the two older copies do
not, so `crates/rmux/tests/vendor_server_patch.rs` walks all 661 vendored files on every run.
That is this project's own bug class in miniature: the newer file wrote down *why* `vendor`
must be excluded, next to two copies that never learned it.

### The comment ratio: carrying its weight, with a rot risk that is already realised

Production code runs at **0.41 comment lines per code line, and 79% of those comments (10,460
of 13,199) are doc comments.** By contrast, integration tests run at 0.13 and test support at
0.06 — commenting here is a production-code habit, not a repo-wide tic. Density concentrates
exactly where the contracts are: `crates/rmux` 0.66, `crates/protocol` 0.60,
`crates/service` 0.47, against `bin/pmux-rmuxd` 0.05 and `bin/pmux-launcher` 0.00.

The provenance character is real but concentrated. Segmenting production comment lines into
contiguous blocks: **2,031 blocks / 13,199 lines, of which 266 blocks (13.1%) cite a
`docs/`, `evidence/`, `scripts/` or `tools/` path or a measurement term — and those 266
blocks cover 4,799 lines, 36.4% of all production comment lines.** Citing blocks average 18.0
lines against 6.5 overall. A line-level count would have reported 3% and understated the
phenomenon twelvefold; the correct unit is the block, because a provenance paragraph cites
once and then explains for seventeen lines.

Is it carrying its weight? **Mostly yes, and it is partly machine-checked** —
`crates/service/tests/path_b_doc_citations.rs` (1,095 lines) validates `path:line` citations
inside the docs, so the pointers cannot silently rot even if the prose does.

But the liability is not hypothetical, and the repository already contains a worked example.
`docs/gate-c-linux-handoff.md:587` states that a `target_os` census yields "25 today";
`docs/path-b-verdict.md:404` records "a target_os census of 25 against a measured 30"; the
measured value today is 30, with 40 never-compiled blocks totalling 257 lines. The correction
was written down *next to* the uncorrected claim rather than *into* it. That is the failure
mode of provenance comments at scale: they accumulate as strata rather than being amended,
and the reader has no way to tell which stratum is current. The same shape appears in the
brief that commissioned this census — "21 `#[cfg(not(unix))]`" was true once and is 23 (22 in
code) now.

Two cheap mitigations, both consistent with what the project already does: make the citation
test also assert that any comment quoting a *count* names the command that produced it, and
date-stamp measurement comments so a reader can tell 2.1.220 claims from 2.1.227 claims
without reading `docs/version-drift.md`.

### The vendored fork: the most expensive line in the census, and the one with the clearest fix

pmux carries **315,530 lines to own 1,351.** The costs are measurable:

- **53.9% of the repository, 53.8% of the branch's insertions.** Every mass statistic about
  this project is dominated by code nobody here wrote. That is not just cosmetic: it is why
  "583,854 insertions" is a misleading number and why any reviewer's first impression of the
  repo is wrong.
- **130,747 lines are never read by the compiler in a product build** — 204 of rmux-server's
  560 src files, including all 13,637 lines of `src/web/`. pmux depends on 129,551 production
  lines and carries 315,530: a **2.4x carry ratio**.
- **166,199 lines of vendored tests** — 53.3% of the fork — of which the gate executes 80
  test functions out of 3,159.
- **The upgrade cost is the real one.** Moving to rmux 0.10.0 means re-auditing a diff over
  315,530 lines to protect a 385-line patch in `pane_io.rs` and a one-line patch in
  `attach.rs`. The 130,747 uncompiled lines are pure re-audit surface with zero runtime
  benefit.

There is a cheaper arrangement, and the repository is already 90% of the way there. The three
`docs/upstream-issues/` drafts (679 lines, all written 2026-08-12, all with zero executable
references) are the patches written up as upstream bug reports. The patch is small, it is
documented in two `PMUX-PATCH.md` files, and the client half is a one-line slice-bound fix
that is plainly an upstream bug. **Upstream the patches; then depend on rmux from crates.io
and delete `vendor/` entirely.** That removes 314,179 carried lines — 53.6% of the
repository — and converts the fork's maintenance from "re-audit 315k lines per release" to
"bump a version". Until upstream lands, the intermediate step is to stop vendoring the
`web` feature tree and the 161 test-module files: a `cargo vendor --no-delete` with a
filtered manifest, or a checked-in patch file applied at build time, keeps the same 1,351
lines of intent and drops six figures of carried mass.

If neither is acceptable, the honest thing is to say so in `README.md`, because right now a
reader's first measurement of this project is 53.9% wrong.

### What a new contributor faces

The load-bearing reading list for understanding the product is much shorter than the tree
suggests:

| To understand | Lines |
|---|---:|
| `docs/spec.md` (normative for behaviour) | 2,224 |
| root `README.md` | 979 |
| `crates/protocol/src` (the wire contract, zero inline tests, 0.60 cmt/code) | 4,314 |
| `crates/service/src` production | 27,252 |
| the other five crates + seven binaries, production | 16,229 |
| **Total to read the product end to end** | **50,998** |

That is 8.7% of the checkout. The other 91.3% is either someone else's code, the tests, the
machinery that runs the tests, or the record of having run them.

Two traps are worth naming for a newcomer:

- **`crates/service` is 78,854 lines and 65% of it is tests.** A contributor who opens
  `driver_io.rs` (11,410 lines) sees a file that is 63% `#[cfg(test)]`, and
  `native.rs` (10,068) is 53%. Neither file's size tells you anything about its complexity.
  The largest single production unit in the repo is `v1/actor.rs` at 4,193 lines.
- **Nothing tells you where the module graph goes.** Two files under `src/` are test-only by
  declaration, and `crates/e2e/src/bin/pmux-test-claude.rs` (1,277 lines) is a *test double*
  that nonetheless builds as a real binary and appears in the Gate A residue floor alongside
  the seven product binaries. A newcomer counting binaries gets eight.

The genuinely good news for a contributor: `cargo check --workspace --all-targets` and
`cargo clippy --workspace --all-targets` emit **exactly four warnings, all four in
`vendor/rmux-server`, zero in first-party code**, and `cargo machete` over the 16 non-vendor
manifests reports "didn't find any unused dependencies". There is no dead-code debt to learn
around.

### What the shape reveals about the project's history

**156 commits over 17 calendar days** (2026-07-27 → 2026-08-12), producing ~252,774 authored
lines: about **14,900 authored lines per day**, sustained. That is not a human cadence, and
the artefacts show it in ways worth noticing:

- **Documentation grows like code, not like documentation.** `docs/current-state.md` has 43
  revisions and grew from 69,042 bytes on 2026-07-27 to 337,207 bytes at HEAD — roughly 5 KiB
  per working day, still accelerating. `docs/` is 20,996 lines across 21 files at 75 bytes
  per line, so its *byte* mass (1.58 MB) is about twice what its line count suggests relative
  to code. Any figure quoted about `current-state.md` decays within a week; the "321 KB"
  figure in circulation was true on 2026-08-07 and is 16 revisions stale.
- **The receipts have started to outlive their readers.** Of 17 tracked `evidence/` files, 11
  are opened by a `.rs`, `.py` or `.sh`; 5 (4,250 lines, 28.8% of the receipt mass) are cited
  only by prose in `docs/`. Two of those five are the `mutation-filtered-run-*` receipts that
  `docs/register-currency.md` exists to date. This is the beginning of an archive, and an
  archive with no retention rule grows monotonically.
- **Prose has started to outlive its readers too.** 7 documents totalling 3,464 lines have
  zero references from any executable file: `agent-resource.md` (998),
  `sandbox-spike.md` (690), `repo-review.md` (652), `2.1.227-compatibility.md` (445), and the
  three `upstream-issues/` drafts (679). The spike and the review are legitimately
  historical. The upstream issues are not stale — they are unfiled.
- **`apps/` → `bin/` shows up as 5,210 deletions**, i.e. a third of all deletions on the
  branch are one rename. The branch has almost no other subtraction: 10,142 deletions across
  1,053 files outside that rename. This project adds; it very rarely removes. That is
  consistent with everything above — the inert crash-harness, the unreferenced receipts, the
  three copies of `SCAN_SKIPPED_DIRECTORIES`, the correction written next to the uncorrected
  claim. **The one habit this repository does not yet have is deletion**, and at 14,900
  authored lines per day that is the habit with the highest compounding value.
- **One thing the owner may not have noticed:**
  `vendor/rmux-client/src/attach_unsupported.rs` (92 lines) is reachable from no `mod`
  declaration anywhere in the crate — it is an orphan file in the *published* 0.9.0 archive,
  compiled by nothing. It is a small proof that the vendored tree contains upstream slop that
  pmux is paying to carry and to re-audit.

---

## 8. Limits

- **The 2-test gap.** 1,226 default-run test functions found statically vs 1,224 quoted as
  passing. No `cargo test` was run (read-only measurement), and the gap is not explained by
  platform gating (zero `#[test]` inside a cfg block this host cannot compile).
- **Macro-generated tests are not counted.** The counter finds literal `#[test]`-family
  attributes; a `macro_rules!` that emits test functions would be undercounted. Not audited.
- **Feature-gated test code is scored as production.** The span finder requires a bare `test`
  token in the `cfg` predicate; a hypothetical `#[cfg(feature = "test-utils")]` would land in
  the production column. `git grep` finds no such gate, but workspace features were not
  enumerated.
- **The vendor compiled/carried measurement reads existing `target/deps/*.d` artefacts**
  rather than a fresh build. Debug and release `.d` files agree on the same 356-file set and
  every listed path exists, but a stale artefact could shift 149,203 slightly.
- **Category C/F (client bindings) split by directory** (`src/` vs `tests/`), not by module
  resolution; TypeScript and Python have no `cfg(test)` analogue, so a test helper living in
  `src/` would be scored as product. Spot-checked: all five `.ts` files under `src/` are
  product and all six `.mjs` files are tests.
- **`crates/e2e/src` (1,339) and `tools/crash-harness/src` (569) are classified as test
  support**, on the strength of `publish = false`, dev-dependency-only manifests, their own
  header comments, and `tools/crash-harness` being outside the workspace — not on a
  dependency-graph proof that no shipped binary links them. `crates/e2e`'s
  `pmux-test-claude` binary *is* a real build artefact and does appear in the Gate A residue
  floor; "not production" here does not mean "not shipped."
- **The 4-line residual on `crates/service`** between this measurement (23,232 inline
  `#[cfg(test)]` lines) and analyst 1's (23,236) was not chased. It is 0.008% of the crate.
- **`vendor/` was split by the same tooling as first-party code**, but its module graph is
  large and uses `#[path]` and `include!` heavily; 560 of 560 rmux-server src files and 50 of
  52 rmux-client src files resolve, and the two that do not were classified by hand
  (`connection/tests.rs`, 667 lines, `include!`d from a `#[cfg(all(test, unix))]` block →
  test; `attach_unsupported.rs`, 92 lines, orphan → production).
