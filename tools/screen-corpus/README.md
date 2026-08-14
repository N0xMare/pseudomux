# Screen corpus tooling

Three things live here. Two of them have been run; the third has not, and this
file says which is which.

## `seed_corpus.py` — RUN

Rebuilds `crates/service/tests/corpus/*.ndjson` from the checked-in terminal
captures. Idempotent. Run from the repository root:

```bash
python3 tools/screen-corpus/seed_corpus.py
```

The seed corpus is the FLOOR. Real corpora come from recording a live session
(below), and both are checked by the same standing test,
`crates/service/tests/screen_corpus_replay.rs`.

## `per_binary_tests.sh` — RUN

Runs every test target in the workspace isolated, one `cargo test` per target,
and prints a line each.

```bash
bash tools/screen-corpus/per_binary_tests.sh
```

Per binary and not one aggregate because the process-spawning blackbox binaries
are load-sensitive: the same `cargo test --workspace` has produced 845/27,
859/13 and 872/0 on this host, so an aggregate total is not a stable number and
chasing one is chasing load.

The first version of this script enumerated its targets from `cargo test
--message-format=json` through an inline Python one-liner whose quoting was
broken by the shell. It enumerated ZERO targets and reported "every test binary
passed in isolation". It is now enumerated from the source tree and **refuses to
report a result when it finds no targets**, which is the property that failure
should have had.

The same defect then reappeared one level down. Enumerating every target is not
the same as running every test: each `cargo test` ran without
`--include-ignored`, so the report printed *"every one of the 61 test targets
passed in isolation"* while **49 test cases never executed** — including all
nineteen of `pseudomux-e2e --test pool_concurrency`, which reported
`0 passed; 0 failed; 19 ignored` and is where the only real failure lived. Every
target now runs `-- --include-ignored`, the executed and skipped case counts are
accumulated from the same `test result:` lines the table prints, and the
coverage sentence carries those counts. A target that emits no `test result:`
line, or a case that still did not run, means the scope is unknown, and unknown
scope **refuses the claim** (exit 2) rather than printing it. A target that
failed exits 1. The sentence is only printed when the counts earn it.

## Recording a live corpus — RUN THIS WHEN A NEW CLAUDE CODE SHIPS

Recording is opt-in and off by default. It costs no model turns: the frames are
whatever the session was already going to render.

```bash
export PMUX_SCREEN_CORPUS_DIR="$PWD/.context/screen-corpus"
export PMUX_SCREEN_CORPUS_CLAUDE_VERSION="$(claude --version)"
export PMUX_SCREEN_CORPUS_LABEL="2.1.221-smoke"
mkdir -p "$PMUX_SCREEN_CORPUS_DIR"
# ... start pmuxd and run whatever turns you were going to run ...
```

Each process writes `pmux-screens-<unix_ms>-<pid>.ndjson`. To promote a
recording into the standing suite, copy it into
`crates/service/tests/corpus/` and then **add `expect_ready` to the frames whose
verdict you established independently** — see below, because a corpus without
those is a corpus that cannot catch the composer bug.

### Why `expect_ready` is not optional

MEASURED: replaying the seed corpus through the pre-fix composer gate PASSED.
Every geometry invariant is conditional on the classifier's own verdict — "a
frame classified `Ready` has exactly two rendered rows below the cursor" — and a
classifier that stops returning `Ready` satisfies all of them by having no cases
left. That is exactly what the composer bug did.

`expect_ready` is the unconditional half. Set it only where the answer was
established WITHOUT consulting the classifier (you looked at the screen; the
composer was empty; nothing was typed).

## `local_command_geometry.md` — NOT RUN

The plan for the free local-command exercise, and the reason it is smaller than
it first appears. See that file.
