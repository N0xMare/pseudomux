#!/usr/bin/env python3
"""Rebuilds the seed screen corpus from the checked-in terminal captures.

The corpus this writes is the FLOOR, not the corpus. Real corpora come from
`PMUX_SCREEN_CORPUS_DIR` on a live session (see `crates/service/src/screen_corpus.rs`);
this exists so `crates/service/tests/screen_corpus_replay.rs` has something to
check on a fresh clone, and so the geometry the 2.1.70 captures pin is asserted
by the same machinery that will check every later recording.

Run it from anywhere:

    python3 tools/screen-corpus/seed_corpus.py

It is idempotent and overwrites its two outputs.

Both paths below are resolved from THIS FILE, not from the working directory,
and it takes no arguments. It used to do neither: `CORPUS_DIR` and `FIXTURES`
were relative, and argv was ignored entirely. Run from one directory up, the
fixture glob matched nothing, `os.makedirs` created a fresh `crates/…/corpus`
wherever you happened to be, and the script wrote a corpus containing the
2.1.220 frame and NONE of the five 2.1.70 captures -- silently, with an exit 0
and a cheerful count. That is the house bug class in a tool: a program whose
output says it did the job it names, from an input set it never checked it
found.
"""

from __future__ import annotations

import argparse
import glob
import json
import os

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
CORPUS_DIR = os.path.join(REPO_ROOT, "crates/service/tests/corpus")
FIXTURES = os.path.join(REPO_ROOT, "crates/service/tests/fixtures/claude_2_1_70_*.txt")
EXPECTED_FIXTURES = 5
SCHEMA = 1


def snapshot_frame(
    site: str,
    rows: list[str],
    cursor_row: int,
    cursor_col: int,
    expect_ready: bool | None = None,
) -> dict:
    cols = max((len(row) for row in rows), default=1)
    frame = {
        "kind": "snapshot",
        "site": site,
        "captured_unix_ms": 0,
        "revision": 1,
        "rows": len(rows),
        "cols": max(cols, 1),
        "cursor": {
            "row": cursor_row,
            "col": cursor_col,
            "visible": True,
            "style": 0,
        },
        "visible_text": "\n".join(rows),
    }
    # Set ONLY where the answer was established without consulting the
    # classifier. Every other corpus invariant is conditional on the
    # classifier's own verdict and goes vacuous when it stops saying Ready;
    # this is the unconditional half. See `CorpusFrame::expect_ready`.
    if expect_ready is not None:
        frame["expect_ready"] = expect_ready
    return frame


def write(path: str, stamp: dict, frames: list[dict]) -> None:
    lines = [json.dumps(stamp, ensure_ascii=False)]
    lines += [json.dumps(frame, ensure_ascii=False) for frame in frames]
    with open(path, "w", encoding="utf-8") as handle:
        handle.write("\n".join(lines) + "\n")


def main() -> None:
    argparse.ArgumentParser(
        description="Rebuild the seed screen corpus from the checked-in captures.",
        allow_abbrev=False,
    ).parse_args()
    os.makedirs(CORPUS_DIR, exist_ok=True)

    # 1. The five verbatim 2.1.70 screen captures.
    #
    # These are text-only: the capture recorded no cursor, so one is
    # RECONSTRUCTED at the empty-composer position the same way
    # `claude_2_1_70_captures_pin_the_prompt_glyph_prefix_and_bottom_offset` in
    # crates/service/tests/actor_model.rs reconstructs it. The stamp says so.
    # A reconstructed cursor cannot be evidence about where Claude puts the
    # cursor; what these frames DO pin is the row geometry around it, which the
    # capture recorded for real.
    frames = []
    captures = sorted(glob.glob(FIXTURES))
    # Fail closed on the input set. The five captures ARE the 2.1.70 half of
    # this corpus; producing the file without them is producing a corpus that
    # pins nothing about the version it is named for.
    if len(captures) != EXPECTED_FIXTURES:
        raise SystemExit(
            f"expected {EXPECTED_FIXTURES} 2.1.70 captures at {FIXTURES}, "
            f"found {len(captures)}"
        )
    for path in captures:
        with open(path, encoding="utf-8") as handle:
            rows = handle.read().split("\n")
        composer = max(index for index, row in enumerate(rows) if "❯" in row)
        glyph_col = rows[composer].index("❯")
        frames.append(
            snapshot_frame(
                f"fixture.{os.path.basename(path)}",
                rows,
                composer,
                glyph_col + 2,
                # The cursor is placed at the empty-composer position by
                # construction, so on these five the expectation is a statement
                # about the reconstruction, not about Claude. It still binds:
                # a classifier that refuses this geometry is refusing the shape
                # all five 2.1.70 captures render.
                expect_ready=True,
            )
        )
    write(
        os.path.join(CORPUS_DIR, "claude-2.1.70-captures.ndjson"),
        {
            "schema": SCHEMA,
            "claude_version": "2.1.70",
            "os": "unknown",
            "arch": "unknown",
            "rows": 0,
            "cols": 0,
            "recorded_unix_ms": 0,
            "label": (
                "2.1.70 verbatim screen captures; cursor RECONSTRUCTED at the "
                "empty-composer position because the captures are text only"
            ),
        },
        frames,
    )

    # 2. The measured 2.1.220 post-/clear frame.
    #
    # This is the screen the composer gate got wrong: Ink painted four rows at
    # the TOP of a 24-row grid and left rows 8-23 literally blank, so the
    # composer's distance to the bottom of the grid is 18 and its distance to the
    # end of the frame is 2. Measuring at the grid made a provably empty composer
    # unfindable.
    rows = [""] * 24
    rows[4] = "─" * 78
    rows[5] = "❯ "
    rows[6] = "─" * 78
    rows[7] = "  ? for shortcuts"
    write(
        os.path.join(CORPUS_DIR, "claude-2.1.220-post-clear.ndjson"),
        {
            "schema": SCHEMA,
            "claude_version": "2.1.220",
            "os": "macos",
            "arch": "aarch64",
            "rows": 24,
            "cols": 80,
            "recorded_unix_ms": 0,
            "label": (
                "MEASURED post-/clear frame: rule/composer/rule/footer at rows "
                "4-7 of a 24-row grid, cursor (5,2), rows 8-23 length zero, "
                "byte-identical for 285s across ~4,250 samples"
            ),
        },
        # MEASURED as a provably empty composer, independently of any
        # classifier: the composer glyph is at (5,0), the cursor at (5,2), and
        # nothing is typed. This is the frame the composer gate refused.
        [snapshot_frame("input_gate.pre_paste", rows, 5, 2, expect_ready=True)],
    )

    print(f"wrote {len(os.listdir(CORPUS_DIR))} corpus files to {CORPUS_DIR}")


if __name__ == "__main__":
    main()
