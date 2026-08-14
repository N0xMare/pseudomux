"""Independent drain-calibration verifier for a published Phase 0 evidence tree.

This is deliberately a *second*, standalone implementation, not a caller of
`phase0_lib.py`. `phase0_lib.py` already computes and publishes its own
`drain_calibration` block (see `drain_calibration_from_timings` and
`summarize_drain_calibration` in `tools/phase0/phase0_lib.py`); this script
recomputes the late-arrival gap straight from `TurnTimings` on its own, and
recomputes each attempt's reported hash straight from the poem text pmux's
own result captured, so a bug shared between "the tool that measured it" and
"the tool that checks the measurement" is far less likely.

It reads only already-published evidence:

- `<evidence-root>/attempt-<uuid>/reservation.json` for `prompt_suite_index`
  and `cell.effort`;
- `<evidence-root>/attempt-<uuid>/outcome.json` for `status` and
  `public_result_binding.{timings,compatibility}`;
- `<evidence-root>/attempt-<uuid>/pmux-{run,turn,claude-p}.stdout.{json,ndjson}`
  for the raw `TurnResult` (`final_blocks`/`text`), which
  `public_result_binding` deliberately excludes (phase0_lib.py's evidence
  envelope never reinterprets assistant/transcript content; see
  `tools/phase0/README.md`, "The Python code does not ... open or parse
  Claude JSONL").

It does not verify manifest hashes, ownership, or tamper-evidence -- that is
`phase0.py audit`'s job. This tool assumes the tree it is pointed at has
already been trusted (or run through `audit`), and only asks three questions
of it: did the reported hash really come from the reported poem, how late did
the transcript keep moving after pmux thought the turn was done, and -- when
the turn timed Claude's `Stop` lifecycle hook -- had the transcript already
stopped moving by the time that hook fired.

The second question is asked against the drain the commit gate actually
REQUIRED, not the one `compatibility.transcript_drain_ms` configured. On a
graduated build those differ by 1,750 ms (`graduated_drain_ms` in
crates/service/src/v1/backend.rs; every line number this tool prints for that
function is resolved at import by `cite`, not written down), and the required
value is derived here from each attempt's own published timings -- see
`derive_required_drain`.

Alongside the answers, this tool states how many attempts it did NOT check:
how many had no hash independently recomputed over them, so a truncated or
entirely empty reply from those attempts would have graded exactly as a
complete one. That is an observation about coverage, not a failure -- some
prompts ask for no hash by design -- but it is printed unmissably, because
"nine attempts, no mismatches" and "seven attempts checked" are different
sentences.

Only the first two can fail this tool. The third is an OBSERVATION about
Claude's flush/hook ordering, not a claim about pmux: a negative sample
forbids a future optimization and is reported unmissably, but it is not a
defect in the evidence under audit, so it never changes the exit status. See
`failing_conditions`.

Usage:

    python3 tools/phase0/verify_calibration.py \\
        --evidence-root /absolute/private/evidence

See `tools/phase0/README.md` ("Drain calibration prompt suite and verifier")
for the prompt suite this pairs with and the hash-extraction contract below.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
import unicodedata
from collections import Counter
from pathlib import Path
from typing import Any, Mapping, NamedTuple, Sequence

# ---- Source citations, resolved rather than restated -------------------------
#
# Every line number this tool PRINTS -- and every line number it writes down
# about product source at all -- is computed here, at import, by searching the
# cited file for the text the sentence is about. A restated line number is
# correct exactly once. The re-arm citation in `NEGATIVE_HEADROOM_BANNER` and
# the stop-hook absence clause were both true when they were written and both
# pointed at unrelated code by the time anyone re-read them; the actor poll
# interval was off by two lines in the DEFAULT output while a self-test named
# for exactly that citation passed, because the number it checked was the copy
# in a comment and not the one being printed. A citation nobody re-measures has
# already rotted. The anchor text is the thing worth naming, so the anchor is
# what is written down and the number is derived from it -- which is also why
# `test_verify_calibration.py` refuses a hand-written product line number
# anywhere in this file.
REPO_ROOT = Path(__file__).resolve().parents[2]

# What a citation renders as when it cannot be resolved. It deliberately does
# NOT contain `path:<digits>`, so nothing downstream -- including this project's
# own citation self-tests -- can mistake a failed lookup for a located line.
UNRESOLVED_CITATION = "{path} (LINE UNRESOLVED, {reason}: {anchor!r})"

_SOURCE_CACHE: dict[str, list[str] | None] = {}


def _source_lines(path_part: str) -> list[str] | None:
    """The cited file's lines, or None when this checkout does not have it.

    Cached because a banner cites one file three times and re-reading a
    6,000-line source per citation would be import-time work for nothing.
    """

    if path_part not in _SOURCE_CACHE:
        path = REPO_ROOT / path_part
        try:
            _SOURCE_CACHE[path_part] = path.read_text(encoding="utf-8").splitlines()
        except OSError:
            _SOURCE_CACHE[path_part] = None
    return _SOURCE_CACHE[path_part]


def cite(
    path_part: str,
    anchor: str,
    *,
    after: str | None = None,
    through: str | None = None,
) -> str:
    """`path:line`, or `path:start-end`, for the line that holds `anchor`.

    `anchor` must occur on EXACTLY ONE line of the searched region; two matches
    refuse the same way zero do. That is not pedantry -- `state.last_change =
    Instant::now()` appears twice in `driver_io.rs`, once inside
    `read_observed_range` and once at an arm boundary, the two lines are
    byte-identical, and a citation that silently took the first would point at
    the wrong mechanism while looking perfectly healthy. `after` names the line
    that opens the region to search (the enclosing `fn`, say), which is how the
    ambiguity is resolved without anybody writing a number down.

    `through` extends the citation to the first line at or after the anchor that
    holds it, for the multi-line ranges a sentence sometimes needs.

    On any failure -- absent file, absent anchor, ambiguous anchor, absent
    `after`, absent `through` -- this returns a citation with NO line number and
    the reason in it, rather than a number that might be wrong. The tool still
    prints its report; the self-tests are what refuse.
    """

    lines = _source_lines(path_part)
    if lines is None:
        return UNRESOLVED_CITATION.format(
            path=path_part, reason="file not in this checkout", anchor=anchor
        )
    start_index = 0
    if after is not None:
        opens = [index for index, line in enumerate(lines) if after in line]
        if len(opens) != 1:
            return UNRESOLVED_CITATION.format(
                path=path_part,
                reason=f"{len(opens)} lines match the opening {after!r}",
                anchor=anchor,
            )
        start_index = opens[0]
    hits = [
        index
        for index, line in enumerate(lines)
        if index >= start_index and anchor in line
    ]
    if len(hits) != 1:
        return UNRESOLVED_CITATION.format(
            path=path_part, reason=f"{len(hits)} matching lines", anchor=anchor
        )
    first = hits[0] + 1
    if through is None:
        return f"{path_part}:{first}"
    closes = [
        index + 1
        for index, line in enumerate(lines)
        if index >= hits[0] and through in line
    ]
    if not closes:
        return UNRESOLVED_CITATION.format(
            path=path_part,
            reason=f"no line at or after the anchor holds {through!r}",
            anchor=anchor,
        )
    return f"{path_part}:{first}-{closes[0]}"


# The `graduated_drain_ms` gate function, named in the graduation verdicts and
# in the drain block: what a turn owes when no end-of-turn marker was seen.
GRADUATED_DRAIN_MS_CITATION = cite(
    "crates/service/src/v1/backend.rs", "pub const fn graduated_drain_ms"
)

# The clause in `TurnTimings::stop_hook_at_ms`'s doc comment that says an absent
# field is expected, so "uncomputable" is never read as "something broke".
STOP_HOOK_ABSENCE_CITATION = cite(
    "crates/protocol/src/v1.rs",
    "Absent on any turn where no Stop hook was observed",
    through="turn that ended before one arrived.",
)

# The three lines that are, together, the re-arm safety property named by
# `NEGATIVE_HEADROOM_BANNER`: the quiet window is measured from the last BYTE,
# it is re-stamped on every nonzero read, and a poll that read nothing returns
# before the re-stamp. The middle one is the reason `after` exists.
STABLE_FOR_MS_CITATION = cite(
    "crates/service/src/driver_io.rs",
    "let stable_for_ms = protocol_milliseconds(",
    through="state.last_change.elapsed().as_millis(),",
)
REARM_CITATION = cite(
    "crates/service/src/driver_io.rs",
    "state.last_change = Instant::now();",
    after="fn read_observed_range(",
)
QUIET_POLL_RETURN_CITATION = cite(
    "crates/service/src/driver_io.rs",
    "if read_len == 0 {",
    through="return Ok((Vec::new(), metadata.identity));",
)

# The actor's default poll interval, whose value `ACTOR_POLL_INTERVAL_MS` below
# repeats and whose location the noise-band line prints.
ACTOR_POLL_INTERVAL_CITATION = cite(
    "crates/service/src/v1/actor.rs", "poll_interval: Duration::from_millis("
)

# `APPROVED_EFFORTS`, the tuple this tool repeats rather than imports.
APPROVED_EFFORTS_CITATION = cite("tools/phase0/phase0_lib.py", "APPROVED_EFFORTS = (")

# The graduated floor constant, whose value `TURN_DURATION_DRAIN_FLOOR_MS` below
# repeats.
TURN_DURATION_DRAIN_FLOOR_CITATION = cite(
    "crates/service/src/v1/backend.rs", "pub const TURN_DURATION_DRAIN_FLOOR_MS: u64 ="
)

# The one field `TurnTimings` (crates/protocol/src/v1.rs) publishes beyond the
# five names phase0_lib.py already knows (`KNOWN_TURN_TIMING_FIELDS`,
# phase0_lib.py:188-210). Named here explicitly -- rather than discovered
# dynamically the way phase0_lib.py does it -- because an independent checker
# should assert what it expects to see, not adopt whatever the tool under test
# decided to call it. If this field is ever renamed upstream, every gap in this
# report becomes "uncomputable" with that reason spelled out, which is a loud,
# safe failure rather than a silently wrong number.
LATE_ARRIVAL_FIELD = "last_transcript_activity_at_ms"

# `TurnTimings::stop_hook_at_ms` (crates/protocol/src/v1.rs): the instant
# Claude's `Stop`/`StopFailure` lifecycle hook was observed. Named explicitly
# here for the same reason as `LATE_ARRIVAL_FIELD`, and NOT treated as a
# late-arrival field: phase0_lib.py:194-202 lists it in
# `KNOWN_TURN_TIMING_FIELDS` precisely so the late-arrival field stays
# discoverable as "the one name beyond that set". Nothing in pmux decides
# completion from it; it is pure measurement.
STOP_HOOK_FIELD = "stop_hook_at_ms"

# `TurnTimings::turn_duration_observed_at_ms`: the instant the in-band
# `turn_duration` end-of-turn marker row was first observed for this turn.
# Its PRESENCE is this tool's only published witness that the marker was seen,
# and `graduated_drain_ms` (`GRADUATED_DRAIN_MS_CITATION`) keys the
# graduated floor off exactly that fact. Named explicitly for the same
# independence reason as `LATE_ARRIVAL_FIELD`.
TURN_DURATION_MARKER_FIELD = "turn_duration_observed_at_ms"

# `TurnTimings::drain_ms`: the stability the commit actually PAID -- the
# `stable_for_ms` the drain gate was satisfied at. It is an upper-bound witness
# on the required drain, never the required drain itself, and that asymmetry is
# the whole of `derive_required_drain` below.
OBSERVED_DRAIN_FIELD = "drain_ms"

# `TURN_DURATION_DRAIN_FLOOR_MS` (`TURN_DURATION_DRAIN_FLOOR_CITATION`), used by
# `graduated_drain_ms` (`GRADUATED_DRAIN_MS_CITATION`):
#
#     if turn_duration_seen && TURN_DURATION_DRAIN_FLOOR_MS < configured_drain_ms
#         { TURN_DURATION_DRAIN_FLOOR_MS } else { configured_drain_ms }
#
# Repeated here rather than imported, for the same independence reason as
# `LATE_ARRIVAL_FIELD`, and with the same caveat: a build whose floor differs
# from this constant will be described wrongly by this tool, and nothing in
# this tooling can currently detect that. The `graduated` verdict below is
# therefore stated as an inference FROM THE PUBLISHED TIMINGS -- an attempt
# that committed at less stability than its own configured drain graduated,
# whatever the constant says -- and the floor is only used to name the value.
TURN_DURATION_DRAIN_FLOOR_MS = 250

# `pmux-{label}.stdout.{json,ndjson}` is written by
# `CampaignRunner._execute_reserved_command` (phase0_lib.py:5415, writing at :6227-6230) for
# every turn-producing public command; `label` is one of these three
# (phase0_lib.py:5902 `_execute_one_shot`, :5952 `_execute_claude_p_one_shot`,
# :6111/:6139 `_execute_persistent`'s `_turn_command` calls). `pmux-start`
# and `pmux-close` use the same naming convention but never carry a turn
# result, so they are deliberately excluded.
TURN_RESULT_LABELS = ("run", "turn", "claude-p")

# `APPROVED_EFFORTS` (`APPROVED_EFFORTS_CITATION`). Repeated here (not imported)
# for the same independence reason as `LATE_ARRIVAL_FIELD` above.
APPROVED_EFFORTS = ("low", "medium")

# A bare `SHA256: <hex>` line has no label and means "the poem itself".
# `SHA256(reversed): <hex>` and `SHA256(upper): <hex>` are the two
# transformation grades in tools/phase0/prompts/
# (05-poem-hash-reverse-transform.txt, 06-poem-hash-triple-transform.txt).
HASH_LINE = re.compile(r"^SHA256(?:\(([A-Za-z0-9_]+)\))?:\s*([0-9a-fA-F]{64})\s*$")

TRANSFORMS = {
    "poem": lambda body: body,
    "reversed": lambda body: body[::-1],
    "upper": lambda body: body.upper(),
}


# `SessionActorConfig::default().poll_interval` (`ACTOR_POLL_INTERVAL_CITATION`).
# Repeated here rather than imported, for the same independence reason as
# `LATE_ARRIVAL_FIELD`. CAVEAT, and it is not small: this is asserted from a
# constant the audited product self-reports, and `SessionActorConfig` is
# overridable. A campaign run with a different poll interval gets a band that is
# wrong in the direction that silently reclassifies samples. Nothing in this
# tooling can currently detect that.
ACTOR_POLL_INTERVAL_MS = 20

# nearest_rank(samples, 95) returns index -(-95*n//100)-1, which equals n-1 --
# the maximum -- for every n <= 19. Below this many samples p95 is not a
# percentile, it is the max under another name.
P95_MEANINGFUL_MIN_SAMPLES = 20

PROMPT_HASH_REQUEST = re.compile(r"SHA256(?:\(([A-Za-z0-9_]+)\))?\s*:")

# How each attempt's grade label was established. `prompt_sha256` is the only
# one that identifies a PROMPT; `prompt_suite_index` identifies a position in an
# argv list, which a resumed or subset campaign renumbers from 1. The two used to
# render as the identical string, so a guessed label was indistinguishable from
# an established one in the default output.
GRADE_SOURCE_LABELS = {
    "prompt_sha256": "graded by prompt content hash",
    "prompt_suite_index": "graded by index, not content",
    "none": "no identifiable prompt",
}

# Every discovered attempt directory lands in exactly one of these. The header
# used to print `discovered (successful, incomplete, fatal errors)`, which is not
# a partition: a `failed` attempt was in none of the three, and a directory with
# no readable reservation.json was in no row of the whole report. Those are burnt
# ordinals, and they vanished.
ATTEMPT_BUCKETS = ("successful", "failed", "incomplete", "unreadable", "fatal")


def expected_hash_labels(prompt_text: str) -> set[str]:
    """Which hash labels a prompt actually requires.

    Matches the same shape `HASH_LINE` accepts in a reply, so the demand and
    the proof are read by one rule. A bare `SHA256:` is the untransformed poem
    and normalises to "poem", the identity entry in `TRANSFORMS`. Labels the
    checker cannot verify are still returned: a prompt asking for a transform
    this tool does not implement should surface as an unknown label rather
    than silently reduce what the grade is required to prove.
    """

    return {(label or "poem") for label in PROMPT_HASH_REQUEST.findall(prompt_text)}


class HashLine(NamedTuple):
    label: str
    reported_hex: str
    raw_line: str


class VerifyError(RuntimeError):
    """This attempt's evidence could not be read or parsed at all."""


# ---- Prompt suite -----------------------------------------------------------


def load_prompt_suite(prompts_dir: Path) -> list[dict[str, Any]]:
    if not prompts_dir.is_dir():
        raise VerifyError(f"prompts directory does not exist: {prompts_dir}")
    files = sorted(prompts_dir.glob("*.txt"))
    if not files:
        raise VerifyError(f"no *.txt prompt files found in {prompts_dir}")
    suite = []
    for index, path in enumerate(files, start=1):
        text = path.read_text(encoding="utf-8")
        suite.append(
            {
                "index": index,
                "path": path,
                "grade": path.stem,
                "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                # A prompt that mentions "SHA256" is asking for a reported
                # hash; used only to tell "no hash requested" (grades 01, 02)
                # apart from "a hash was requested but the reply had none".
                "expects_hash": "SHA256" in text,
                # WHICH hashes, not just whether any. A bare `SHA256:` in the
                # prompt means the poem itself (label "poem", matching
                # TRANSFORMS); `SHA256(reversed):` names a transform. Without
                # this the checker could only ask "did every hash present
                # verify", which scores a grade that delivered one of three
                # required proofs as a full match.
                "expected_labels": frozenset(expected_hash_labels(text)),
            }
        )
    return suite


# ---- Raw pmux stdout -> final message text ----------------------------------


def find_turn_result_artifact(attempt_dir: Path) -> tuple[Path, str] | None:
    for label in TURN_RESULT_LABELS:
        for suffix in ("json", "ndjson"):
            candidate = attempt_dir / f"pmux-{label}.stdout.{suffix}"
            if candidate.is_file():
                return candidate, suffix
    return None


def parse_turn_result(raw: bytes, suffix: str) -> dict[str, Any]:
    """Recover the `TurnResult` object from raw (already redacted) pmux
    stdout, independently of `phase0_lib.parse_public_output`/
    `public_result`, mirroring the same two output-format rules those
    functions apply (phase0_lib.py:4455-4491)."""

    text = raw.decode("utf-8")
    if suffix == "json":
        value = json.loads(text)
        if not isinstance(value, dict):
            raise VerifyError("pmux JSON stdout is not an object")
        return value
    if suffix != "ndjson":
        raise VerifyError(f"unsupported raw stdout suffix: {suffix}")
    records = [json.loads(line) for line in text.splitlines() if line.strip()]
    result_indexes = [
        index
        for index, record in enumerate(records)
        if isinstance(record, dict) and record.get("type") == "result"
    ]
    if result_indexes != [len(records) - 1]:
        raise VerifyError(
            "pmux NDJSON stdout does not end with exactly one result record"
        )
    data = records[-1].get("data")
    if not isinstance(data, dict):
        raise VerifyError("pmux NDJSON result record has no object data")
    return data


def final_text_from_turn_result(result: Mapping[str, Any]) -> str:
    """`final_blocks` (`MessageBlock::Text` -> `{"kind": "text", "text": ...}`,
    `MessageBlock` in crates/protocol/src/v1.rs) is the transcript-derived
    terminal message, split into whatever text blocks it actually had; joined
    back into one string in order. Falls back to the `text` field, which is
    always present, if `final_blocks` carries no text block for any reason.

    The blocks are concatenated with an EMPTY separator, matching
    `final_text_blocks.concat()` in crates/claude/src/engine.rs. Joining
    with "\\n" instead would insert separators Claude never emitted whenever a
    terminal message carries two or more text blocks, so the "independent"
    digest would become a checksum over this function's guess rather than over
    what pmux read -- and would report the difference as a pmux defect. The
    long grades (prompts 07 and 09) are the most likely to be chunked.
    """

    blocks = result.get("final_blocks")
    texts = []
    if isinstance(blocks, list):
        for block in blocks:
            if (
                isinstance(block, dict)
                and block.get("kind") == "text"
                and isinstance(block.get("text"), str)
            ):
                texts.append(block["text"])
    if texts:
        return "".join(texts)
    text = result.get("text")
    return text if isinstance(text, str) else ""


# ---- Hash-extraction contract ------------------------------------------------
#
# Every prompt in tools/phase0/prompts/ that asks for a hash requires the
# final reply to be exactly: the poem's lines, one blank line, then one hash
# line per computed hash (`SHA256: <hex>` or `SHA256(<label>): <hex>`), the
# last of which is the final line of the reply. This function is the
# reference implementation of that contract's parser: it consumes hash lines
# from the bottom of the text upward until a non-matching line is hit, treats
# everything above that (minus one blank separator line, if present) as the
# poem/body text.


def extract_hash_lines_and_body(
    final_text: str,
) -> tuple[str, list[HashLine], dict[str, Any]]:
    lines = final_text.splitlines()
    while lines and lines[-1].strip() == "":
        lines.pop()
    collected: list[HashLine] = []
    index = len(lines) - 1
    while index >= 0:
        match = HASH_LINE.fullmatch(lines[index].strip())
        if match is None:
            break
        label = match.group(1) or "poem"
        collected.append(HashLine(label, match.group(2).lower(), lines[index]))
        index -= 1
    collected.reverse()
    body_lines = lines[: index + 1]
    separator_present = bool(body_lines) and body_lines[-1].strip() == ""
    if separator_present:
        body_lines = body_lines[:-1]
    extra_trailing_blanks = 0
    while body_lines and body_lines[-1].strip() == "":
        body_lines.pop()
        extra_trailing_blanks += 1
    meta = {
        "separator_present": separator_present,
        "extra_trailing_blanks": extra_trailing_blanks,
    }
    return "\n".join(body_lines), collected, meta


def candidate_texts(text: str) -> list[tuple[str, str]]:
    """A small, explicit, documented set of text variants to try before
    calling a hash unreproducible. A live model cannot be forced to feed a
    shell command exactly the bytes it later reprints -- `echo` and a heredoc
    append a trailing newline, `printf '%s'` does not, and terminal/JSON round
    trips can renormalize combining characters in the Unicode/emoji/CJK grade.
    This offers the raw extracted text, the same text plus one trailing
    newline, and (only when it would differ) the NFC-normalized form of both.
    It never silently strips or rewrites the reported hash or the poem text.

    These are TEXT variants, not encoded bytes, because a caller applying a
    transformation must vary the input and then transform, never the reverse:
    `reversed` does not commute with appending a newline. See
    `verify_hash_lines`.
    """

    variants = [("exact", text), ("trailing_newline", text + "\n")]
    nfc = unicodedata.normalize("NFC", text)
    if nfc != text:
        variants.append(("nfc", nfc))
        variants.append(("nfc_trailing_newline", nfc + "\n"))
    return variants


def candidate_encodings(text: str) -> list[tuple[str, bytes]]:
    """`candidate_texts` encoded to UTF-8, dropping variants that collapse to
    bytes an earlier variant already produced.
    """

    seen: set[bytes] = set()
    encoded: list[tuple[str, bytes]] = []
    for label, value in candidate_texts(text):
        data = value.encode("utf-8")
        if data in seen:
            continue
        seen.add(data)
        encoded.append((label, data))
    return encoded


def verify_hash_lines(
    body: str, hash_lines: Sequence[HashLine]
) -> list[dict[str, Any]]:
    checks = []
    for hash_line in hash_lines:
        transform = TRANSFORMS.get(hash_line.label)
        if transform is None:
            checks.append(
                {
                    "label": hash_line.label,
                    "reported": hash_line.reported_hex,
                    "match": False,
                    "matched_variant": None,
                    "recomputed_example": None,
                    "reason": "unknown_transform_label",
                }
            )
            continue
        # Vary the poem text first, then transform each variant -- never
        # transform once and vary the result. Appending a newline does not
        # commute with `reversed`: reverse(poem + "\n") == "\n" + reverse(poem),
        # which is neither reverse(poem) nor reverse(poem) + "\n". Feeding a
        # multi-line poem on stdin via a heredoc always LF-terminates the last
        # line, so transforming first made the reported digest for the
        # `reversed` grades (prompts 05 and 06) unreproducible by construction,
        # and reported that as a pmux transcript defect.
        matched_variant = None
        recomputed_example = None
        seen: set[bytes] = set()
        for variant_label, variant_text in candidate_texts(body):
            data = transform(variant_text).encode("utf-8")
            if data in seen:
                continue
            seen.add(data)
            digest = hashlib.sha256(data).hexdigest()
            if recomputed_example is None:
                recomputed_example = digest
            if digest == hash_line.reported_hex:
                matched_variant = variant_label
                break
        checks.append(
            {
                "label": hash_line.label,
                "reported": hash_line.reported_hex,
                "match": matched_variant is not None,
                "matched_variant": matched_variant,
                "recomputed_example": recomputed_example,
                "reason": None,
            }
        )
    return checks


# ---- Late-arrival gap ---------------------------------------------------------


def compute_gap(timings: Mapping[str, Any]) -> tuple[int | None, str | None]:
    """`last_transcript_activity_at_ms - terminal_candidate_at_ms`, signed and
    unclamped, exactly as documented on `TurnTimings::last_transcript_activity_at_ms`
    in crates/protocol/src/v1.rs:
    "Read a difference within one actor poll interval of zero as 'no late
    rows'; only a clearly positive difference means the drain window did
    work." This function does not decide what "one actor poll interval" is;
    it only returns the signed millisecond difference and lets the report
    label it.
    """

    candidate = timings.get("terminal_candidate_at_ms")
    if not isinstance(candidate, int) or isinstance(candidate, bool):
        return None, "terminal_candidate_at_ms_absent"
    if LATE_ARRIVAL_FIELD not in timings:
        return None, f"{LATE_ARRIVAL_FIELD}_not_published"
    last_activity = timings[LATE_ARRIVAL_FIELD]
    if not isinstance(last_activity, int) or isinstance(last_activity, bool):
        return None, f"{LATE_ARRIVAL_FIELD}_not_an_integer"
    return last_activity - candidate, None


# ---- Required (effective) drain ------------------------------------------------
#
# `compatibility.transcript_drain_ms` is what was CONFIGURED. On a graduated
# build it is not what the commit gate required. `SessionActor`'s turn loop
# (crates/service/src/v1/actor.rs, `let full_drain_ms =`) computes what the
# FULL cell owes as
#
#     graduated_drain_ms(self.compatibility.transcript_drain_ms,
#                        analysis.turn_duration_seen)
#
# so a turn that saw the `turn_duration` end-of-turn marker owed 250 ms, not
# 2,000. That value is still the whole requirement for a Full-cell turn, which
# is every turn a campaign can currently launch: the minified cell is reachable
# only through `NativeService::select_minified_cell`, which has no protocol
# request variant. A minified-cell turn layers two more steps on top of it
# (`offered_drain_ms` from `minified_drain_ms`, and the earned re-check at
# `confirmation.drain.satisfies(offered_drain_ms)`) and can commit at a SHORTER
# requirement -- see `below_graduated_floor`, which refuses to describe such a
# turn rather than reporting the floor it did not owe.
#
# Publishing headroom against 2,000 credits the run with margin from a
# mechanism that was not running: on the ordinal-70 near-miss the tool reported
# 1,648 ms of headroom (2,000 - 352) for a turn whose gate required 250 ms and
# whose transcript kept moving for 352 ms -- i.e. margin that, by that framing,
# was NEGATIVE. Worse, every field in the old report was identical whether or
# not graduation was on, so a regression that silently disabled it was
# invisible here.
#
# What follows never trusts a build flag it cannot see. It infers from the
# attempt's own published timings.

# Every value `derive_required_drain` can return in its verdict slot. A run's
# verdict is the SET of these across its successful attempts, so "some turns
# graduated and some did not" can never collapse into one word.
GRADUATION_STATES = (
    "graduated",
    "below_graduated_floor",
    "not_graduated_no_marker",
    "floor_not_binding",
    "graduation_indeterminate",
    "unknown",
)

GRADUATION_STATE_LABELS = {
    "graduated": (
        "the marker was observed AND the turn committed at less stability than "
        "its own configured drain, so the gate cannot have required the "
        "configured value -- graduation is PROVEN by this attempt's own timings"
    ),
    "below_graduated_floor": (
        "the marker was observed but the turn committed at LESS stability than "
        "the graduated floor itself, which the graduated gate cannot do. Some "
        "shorter proof than graduation admitted this commit -- the minified "
        "cell's fast path is the one this tree can produce -- and the published "
        "timings carry no cell field to say which, so the required drain is "
        "bounded from above by the stability paid and is otherwise unknown"
    ),
    "not_graduated_no_marker": (
        f"no {TURN_DURATION_MARKER_FIELD} was published, so no end-of-turn "
        "marker was seen and the full configured drain was owed "
        f"({GRADUATED_DRAIN_MS_CITATION})"
    ),
    "floor_not_binding": (
        "the marker was observed but the configured drain is already at or "
        "below the graduated floor, so graduated and non-graduated builds "
        "require the identical value and this attempt cannot tell them apart"
    ),
    "graduation_indeterminate": (
        "the marker was observed and the floor would bind, but the turn paid "
        "at least the full configured drain anyway, so its timings are "
        "consistent with BOTH a graduated and a non-graduated build. The "
        "required drain is bounded, not known"
    ),
    "unknown": (
        "no configured transcript_drain_ms was published for this attempt, so "
        "nothing can be said about what its gate required"
    ),
}


class RequiredDrain(NamedTuple):
    """What the commit gate actually required of this attempt, and how sure.

    `required_ms` is `None` exactly when the evidence does not establish it;
    `lower_bound_ms` is then the smallest value the gate could have required,
    and is what any margin claim must be taken against. Reporting the
    configured value in that case would be assuming the safe answer.
    """

    required_ms: int | None
    lower_bound_ms: int | None
    state: str


def derive_required_drain(
    marker_observed: bool | None,
    configured_drain_ms: Any,
    observed_drain_ms: Any,
    *,
    floor_ms: int = TURN_DURATION_DRAIN_FLOOR_MS,
) -> RequiredDrain:
    """The EFFECTIVE drain this attempt's gate required, from its own timings.

    Three published facts decide it: whether `turn_duration_observed_at_ms` is
    present (the marker was seen), what `compatibility.transcript_drain_ms`
    said, and what `drain_ms` -- the stability actually paid at commit -- was.

    The inference is deliberately one-directional. `drain_ms < configured`
    PROVES the gate required less than the configured value, because the gate
    is `stable_for_ms >= required` and a commit at lower stability is otherwise
    impossible. The converse proves nothing: a graduated turn whose transcript
    simply stayed quiet for 3 s also reports `drain_ms >= configured`. So a
    high `drain_ms` yields `graduation_indeterminate`, never
    "not graduated" -- claiming the latter would manufacture margin out of a
    quiet transcript.

    The same one-directional reading bounds graduation from BELOW. `drain_ms <
    floor` proves the gate required less than the floor, so it refutes
    "graduated" rather than confirming it; that lands in
    `below_graduated_floor`, not in a 250 ms claim.
    """

    if (
        not isinstance(configured_drain_ms, int)
        or isinstance(configured_drain_ms, bool)
        or marker_observed is None
    ):
        return RequiredDrain(None, None, "unknown")
    if not marker_observed:
        return RequiredDrain(
            configured_drain_ms, configured_drain_ms, "not_graduated_no_marker"
        )
    if floor_ms >= configured_drain_ms:
        return RequiredDrain(
            configured_drain_ms, configured_drain_ms, "floor_not_binding"
        )
    paid_ms = (
        observed_drain_ms
        if isinstance(observed_drain_ms, int)
        and not isinstance(observed_drain_ms, bool)
        else None
    )
    # Checked BEFORE the graduated verdict, because it refutes it. The gate is
    # `stable_for_ms >= required`, so a commit at less stability than the floor
    # proves the gate required less than the floor -- and "the marker was seen,
    # therefore the requirement was the floor" is then simply false. Under
    # `minified_drain_ms` (crates/service/src/v1/actor.rs) a minified-cell turn
    # that passes the ten fast-path checks can require
    # `MINIFIED_FAST_PATH_DRAIN_FLOOR_MS` instead, and `TurnResult` publishes no
    # cell field to distinguish the two. Reporting 250 for such a turn would
    # credit it with 5x the proof it has, which is the same class of flattery
    # `graduation_indeterminate` exists to refuse -- so this refuses too, and
    # bounds the requirement by what was paid.
    if paid_ms is not None and paid_ms < floor_ms:
        return RequiredDrain(None, 0, "below_graduated_floor")
    if paid_ms is not None and paid_ms < configured_drain_ms:
        return RequiredDrain(floor_ms, floor_ms, "graduated")
    return RequiredDrain(None, floor_ms, "graduation_indeterminate")


class RunRequiredDrain(NamedTuple):
    """The whole run's required drain, or the honest reason there isn't one."""

    required_ms: int | None
    lower_bound_ms: int | None
    note: str


def summarize_required_drain(
    per_attempt: Sequence[RequiredDrain],
) -> RunRequiredDrain:
    """Roll per-attempt required drains up into ONE value, or refuse to.

    Refuses in three distinguishable ways, and the refusal is the point: a
    single number here is what the headroom line is taken against, so it must
    never be produced by averaging away a disagreement or by falling back to
    the configured value when the evidence stopped short.
    """

    if not per_attempt:
        return RunRequiredDrain(None, None, "no successful attempt to derive it from")
    established = sorted(
        {item.required_ms for item in per_attempt if item.required_ms is not None}
    )
    unresolved = sum(1 for item in per_attempt if item.required_ms is None)
    bounds = [
        item.lower_bound_ms for item in per_attempt if item.lower_bound_ms is not None
    ]
    lower_bound = min(bounds) if len(bounds) == len(per_attempt) else None
    if unresolved:
        return RunRequiredDrain(
            None,
            lower_bound,
            f"{unresolved} of {len(per_attempt)} successful attempt(s) did not "
            "establish what their gate required; only a lower bound is known",
        )
    if len(established) == 1:
        return RunRequiredDrain(
            established[0],
            lower_bound,
            "constant across every successful attempt, derived from each "
            "attempt's own published timings",
        )
    return RunRequiredDrain(
        None,
        lower_bound,
        f"varies across attempts: {established}",
    )


# ---- Stop-hook ordering -------------------------------------------------------
#
# A DIFFERENT question from the late-arrival gap, kept in its own section and
# its own tally throughout. The gap above asks "how much longer did the
# transcript keep moving after the terminal-looking message", which calibrates
# how long the drain must be. This asks "when Claude fires its Stop hook, has
# it already written the last transcript row" -- which decides whether the
# drain can be SHORT-CIRCUITED at all. Averaging the two together would answer
# neither.

# Why each stop-hook sample is missing, in words, so "uncomputable" is never
# read as "zero" or as "nothing to see". Rendered in the DEFAULT text output
# beside the count.
STOP_HOOK_UNCOMPUTABLE_REASONS = {
    f"{STOP_HOOK_FIELD}_not_published": (
        "no Stop lifecycle hook was observed for this turn. Expected on every "
        "session running without the Hybrid lifecycle hook installed, and on "
        "any turn that ended before a hook arrived "
        f"({STOP_HOOK_ABSENCE_CITATION}). This is also exactly what a "
        f"rename of {STOP_HOOK_FIELD} upstream would look like"
    ),
    f"{STOP_HOOK_FIELD}_not_an_integer": (
        f"{STOP_HOOK_FIELD} was present but not an integer millisecond "
        "timestamp, so no signed difference can be taken from it"
    ),
    f"{LATE_ARRIVAL_FIELD}_not_published": (
        "the Stop hook was timed but the turn published no last-transcript-"
        "activity instant to compare it against (a turn that never reached the "
        "drain gate: cancelled, timed out, or failed)"
    ),
    f"{LATE_ARRIVAL_FIELD}_not_an_integer": (
        f"{LATE_ARRIVAL_FIELD} was present but not an integer millisecond "
        "timestamp, so no signed difference can be taken from it"
    ),
}


def compute_stop_hook_delta(
    timings: Mapping[str, Any],
) -> tuple[int | None, str | None]:
    """`stop_hook_at_ms - last_transcript_activity_at_ms`, signed and
    unclamped, exactly as documented on `TurnTimings::stop_hook_at_ms` in
    crates/protocol/src/v1.rs.

    The SIGN is the whole answer. Positive means the Stop hook arrived AFTER
    the final transcript write, so a `(stop_hook_observed || stable_for_ms >=
    drain)` fast path could only ever complete a turn EARLIER, never on
    unfinished output. A single negative means Stop can precede the last write,
    so that fast path would commit a TRUNCATED turn and must never be built.

    Therefore: no `max(0, ..)`, no `abs()`, no unsigned cast, and no clamping
    anywhere between here and the rendered report -- clamping would destroy the
    only observation capable of forbidding the optimization. Absence is
    reported as a named reason, never as zero, because "the hook was never
    timed" and "the hook arrived in the same millisecond as the last write" are
    different facts and only one of them is evidence.
    """

    if STOP_HOOK_FIELD not in timings:
        return None, f"{STOP_HOOK_FIELD}_not_published"
    stop_hook = timings[STOP_HOOK_FIELD]
    if not isinstance(stop_hook, int) or isinstance(stop_hook, bool):
        return None, f"{STOP_HOOK_FIELD}_not_an_integer"
    if LATE_ARRIVAL_FIELD not in timings:
        return None, f"{LATE_ARRIVAL_FIELD}_not_published"
    last_activity = timings[LATE_ARRIVAL_FIELD]
    if not isinstance(last_activity, int) or isinstance(last_activity, bool):
        return None, f"{LATE_ARRIVAL_FIELD}_not_an_integer"
    return stop_hook - last_activity, None


def nearest_rank(samples: Sequence[int], percentile: int) -> int:
    """Exact integer nearest-rank percentile -- no float interpolation.
    Independently reimplemented from the same rule
    `phase0_lib.summarize_drain_calibration`'s `_nearest_rank` uses, so both
    tools agree on what "p95" means without this checker importing the
    library it is auditing."""

    index = -(-percentile * len(samples) // 100) - 1
    return samples[min(max(index, 0), len(samples) - 1)]


# ---- Per-attempt analysis -----------------------------------------------------


def discover_attempt_dirs(evidence_root: Path) -> list[Path]:
    if not evidence_root.is_dir():
        raise VerifyError(f"evidence root does not exist: {evidence_root}")
    return sorted(
        path
        for path in evidence_root.iterdir()
        if path.is_dir() and path.name.startswith("attempt-")
    )


def _read_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def analyze_attempt(
    attempt_dir: Path, prompt_suite: Sequence[Mapping[str, Any]]
) -> dict[str, Any]:
    grade_by_index = {entry["index"]: entry for entry in prompt_suite}
    # `prompt_suite_index` is assigned by CLI argument order, so it identifies a
    # position, not a prompt. A campaign that passes a SUBSET -- resuming at
    # grade 03 after the earlier grades already succeeded, say -- restarts the
    # numbering at 1, and every attempt silently acquires the label of a prompt
    # it never ran. Content hash is the only stable identity, so prefer it and
    # keep the index purely as a fallback for a prompt not in this directory.
    grade_by_sha256 = {entry["sha256"]: entry for entry in prompt_suite}
    record: dict[str, Any] = {
        "attempt_dir": str(attempt_dir),
        "attempt_id": None,
        "global_attempt_ordinal": None,
        "prompt_suite_index": None,
        "grade_source": "none",
        "grade": "unreadable",
        "effort": None,
        "status": "unreadable",
        # The campaign's own error string for a non-successful attempt, kept as
        # a field rather than only inside a note so the report can tally what
        # actually went wrong across a run of failures.
        "error": None,
        "gap_ms": None,
        "gap_uncomputable_reason": None,
        # A separate question with a separate absence: `stop_hook_at_ms` is
        # optional in a way the late-arrival field is not, so a null here with a
        # reason is a normal, expected reading -- never a zero.
        "stop_hook_delta_ms": None,
        "stop_hook_delta_uncomputable_reason": None,
        # What was CONFIGURED versus what the gate actually REQUIRED. Kept as
        # four separate fields, never one, because the whole defect was reading
        # the first and calling it the second.
        "configured_transcript_drain_ms": None,
        "turn_duration_marker_observed": None,
        "observed_drain_ms": None,
        "required_drain_ms": None,
        "required_drain_ms_lower_bound": None,
        "graduation_state": "unknown",
        # Distinct from "not_applicable" (the grade's prompt did not ask for
        # a hash): "no_result" means this attempt never produced a public
        # result to check at all, so the two must not be tallied together.
        "hash_overall": "no_result",
        "hash_checks": [],
        "notes": [],
        "fatal_error": None,
    }
    try:
        reservation_path = attempt_dir / "reservation.json"
        if not reservation_path.is_file():
            record["notes"].append("reservation.json missing")
            return record
        reservation = _read_json(reservation_path)
        record["attempt_id"] = reservation.get("attempt_id")
        record["global_attempt_ordinal"] = reservation.get("global_attempt_ordinal")
        suite_index = reservation.get("prompt_suite_index")
        record["prompt_suite_index"] = suite_index
        record["effort"] = (reservation.get("cell") or {}).get("effort")
        reported_prompt_sha256 = (reservation.get("prompt") or {}).get("sha256")
        suite_entry = grade_by_sha256.get(reported_prompt_sha256)
        index_entry = grade_by_index.get(suite_index)
        if suite_entry is not None:
            record["grade"] = suite_entry["grade"]
            record["grade_source"] = "prompt_sha256"
            if (
                index_entry is not None
                and index_entry["sha256"] != reported_prompt_sha256
            ):
                # Not an error: a resumed or subset campaign renumbers from 1.
                # Recorded so the by-grade table cannot be read as if the
                # positions had lined up.
                record["notes"].append(
                    f"prompt_suite_index {suite_index} would have labelled this "
                    f"{index_entry['path'].name}, but its content is "
                    f"{suite_entry['path'].name}; graded by content"
                )
        elif index_entry is not None:
            record["grade"] = index_entry["grade"]
            record["grade_source"] = "prompt_suite_index"
            record["notes"].append(
                f"prompt sha256 {str(reported_prompt_sha256)[:12]} is not in the "
                f"prompts directory; graded by index {suite_index}, which names a "
                "position rather than a prompt"
            )
        else:
            record["grade"] = f"unknown-index-{suite_index}"
            record["grade_source"] = "none"

        outcome_path = attempt_dir / "outcome.json"
        if not outcome_path.is_file():
            record["status"] = "incomplete_no_outcome"
            record["notes"].append(
                "no outcome.json: a durable reservation with no final "
                "attempt artifact, i.e. an incomplete/crashed attempt "
                "(see tools/phase0/README.md, Crash/restart audit)"
            )
            return record
        outcome = _read_json(outcome_path)
        status = outcome.get("status")
        record["status"] = status
        if status != "pmux_exit_zero":
            record["hash_overall"] = "no_result"
            record["error"] = outcome.get("error")
            record["notes"].append(f"attempt did not succeed: {record['error']}")
            return record

        binding = outcome.get("public_result_binding") or {}
        timings = binding.get("timings") or {}
        gap_ms, gap_reason = compute_gap(timings)
        record["gap_ms"] = gap_ms
        record["gap_uncomputable_reason"] = gap_reason
        stop_delta, stop_reason = compute_stop_hook_delta(timings)
        record["stop_hook_delta_ms"] = stop_delta
        record["stop_hook_delta_uncomputable_reason"] = stop_reason
        compat = binding.get("compatibility") or {}
        configured_drain = compat.get("transcript_drain_ms")
        record["configured_transcript_drain_ms"] = configured_drain
        record["turn_duration_marker_observed"] = TURN_DURATION_MARKER_FIELD in timings
        observed_drain = timings.get(OBSERVED_DRAIN_FIELD)
        record["observed_drain_ms"] = (
            observed_drain
            if isinstance(observed_drain, int) and not isinstance(observed_drain, bool)
            else None
        )
        required = derive_required_drain(
            record["turn_duration_marker_observed"],
            configured_drain,
            record["observed_drain_ms"],
        )
        record["required_drain_ms"] = required.required_ms
        record["required_drain_ms_lower_bound"] = required.lower_bound_ms
        record["graduation_state"] = required.state
        if required.state == "graduation_indeterminate":
            record["notes"].append(
                "this attempt saw the turn_duration end-of-turn marker but paid "
                f"at least its full configured drain ({configured_drain} ms), so "
                "its timings cannot say whether the graduated floor was in "
                "effect; any margin claim must be taken against "
                f"{required.lower_bound_ms} ms"
            )
        if required.state == "below_graduated_floor":
            record["notes"].append(
                "this attempt committed at "
                f"{record['observed_drain_ms']} ms of stability, BELOW the "
                f"{TURN_DURATION_DRAIN_FLOOR_MS} ms graduated floor, so its gate "
                "was not the graduated one and no required drain can be derived "
                "from these timings; a minified-cell fast-path commit looks "
                "exactly like this, and nothing published here names the cell"
            )

        found = find_turn_result_artifact(attempt_dir)
        if found is None:
            record["hash_overall"] = "error"
            record["notes"].append(
                "no pmux-{run,turn,claude-p}.stdout.{json,ndjson} artifact found"
            )
            return record
        artifact_path, suffix = found
        turn_result = parse_turn_result(artifact_path.read_bytes(), suffix)
        final_text = final_text_from_turn_result(turn_result)
        body, hash_lines, extract_meta = extract_hash_lines_and_body(final_text)
        if hash_lines and not extract_meta["separator_present"]:
            record["notes"].append("no blank line before the hash block")
        if extract_meta["extra_trailing_blanks"]:
            record["notes"].append(
                f"{extract_meta['extra_trailing_blanks']} extra trailing blank "
                "line(s) trimmed from the poem/body text"
            )
        # Consult whichever entry actually produced the grade. Reading
        # `expects_hash` off `suite_entry` alone meant an index-graded attempt
        # always reported `expects_hash = False`, so a reply carrying NO hash at
        # all was filed as "this grade's prompt did not ask for a hash" -- a
        # positive claim about a prompt the tool had just admitted it could not
        # identify. One edit to the prompts directory turned every missing hash
        # in the tree into `not_applicable` at once.
        entry = suite_entry or index_entry
        if not hash_lines:
            if entry is None:
                record["hash_overall"] = "hash_expectation_unknown"
            else:
                record["hash_overall"] = (
                    "missing" if entry["expects_hash"] else "not_applicable"
                )
            return record
        checks = verify_hash_lines(body, hash_lines)
        record["hash_checks"] = checks
        if not all(check["match"] for check in checks):
            record["hash_overall"] = "mismatch"
            return record
        # Every hash that was PRESENT verified -- but a grade can ask for more
        # than one. `verify_hash_lines` iterates only over what the reply
        # carried, so a grade-06 answer with a single correct `SHA256(poem):`
        # scored "match" while both transform proofs were absent. Those proofs
        # are the entire reason grades 05 and 06 exist and are the ones a model
        # is most likely to botch, so they were not counted as missing -- they
        # were not counted at all.
        expected = set(entry["expected_labels"]) if entry else set()
        reported = {check["label"] for check in checks}
        missing = sorted(expected - reported)
        if missing:
            record["hash_overall"] = "partial"
            record["hash_missing_labels"] = missing
            record["notes"].append(
                "reply verified every hash it carried but omitted "
                f"{len(missing)} the prompt required: {', '.join(missing)}"
            )
        else:
            record["hash_overall"] = "match"
        return record
    except Exception as caught:
        # One unreadable attempt directory must not abort the whole report.
        record["fatal_error"] = f"{type(caught).__name__}: {caught}"
        return record


# ---- Report assembly -----------------------------------------------------------


def summarize_gaps(
    records: Sequence[Mapping[str, Any]], *, band_ms: int = ACTOR_POLL_INTERVAL_MS
) -> dict[str, Any] | None:
    gaps = sorted(
        record["gap_ms"] for record in records if isinstance(record.get("gap_ms"), int)
    )
    if not gaps:
        return None
    # `gap <= 0` was the wrong rule. `TurnTimings::last_transcript_activity_at_ms`
    # (crates/protocol/src/v1.rs) states it: the difference straddles zero by a
    # few milliseconds -- negative by the parse-and-analyze interval, positive
    # by the interval between the confirming poll's stability measurement (a
    # monotonic duration) and the completion timestamp read (a wall clock) --
    # and a difference within one actor
    # poll interval of zero reads as "no late rows". Classifying a +1 ms
    # clock artifact as a late row suppressed the absence-of-evidence banner and
    # republished the artifact as a measured 1 ms lower bound with 1,999 ms of
    # apparent headroom: an invitation to cut transcript_drain_ms on noise.
    # Gaps inside the band get their own bucket rather than being hidden, so
    # "we saw nothing" and "we saw only noise" stay distinguishable.
    no_late_row = sum(1 for gap in gaps if gap <= band_ms)
    within_band = sum(1 for gap in gaps if 0 < gap <= band_ms)
    summary = {
        "count": len(gaps),
        "min": gaps[0],
        "median": nearest_rank(gaps, 50),
        "max": gaps[-1],
        "noise_band_ms": band_ms,
        "no_late_row_attempts": no_late_row,
        "within_noise_band_attempts": within_band,
        "late_row_attempts": len(gaps) - no_late_row,
    }
    # nearest_rank(_, 95) is index -(-95*n//100)-1, which is the last element for
    # every n <= 19. Publishing it beside `max` at those sizes prints one number
    # twice and reads as two independent statistics.
    if len(gaps) >= P95_MEANINGFUL_MIN_SAMPLES:
        summary["p95"] = nearest_rank(gaps, 95)
    return summary


def summarize_stop_hook_deltas(
    records: Sequence[Mapping[str, Any]],
) -> dict[str, Any] | None:
    """The SIGN TALLY first, magnitudes second.

    Deliberately NOT folded into `summarize_gaps`: these are different
    questions over a different pair of timestamps, and one distribution
    covering both would let a positive ordering observation cancel a negative
    one against a number that has nothing to do with it.

    There is no noise band here, and that is not an oversight. The late-arrival
    gap has one because its two timestamps are stamped by the same read, so a
    few milliseconds either side of zero is known measurement noise
    (`TurnTimings::last_transcript_activity_at_ms`'s doc comment,
    crates/protocol/src/v1.rs). This difference is between two independent
    events, and the claim under test is an ORDERING claim: one
    negative sample forbids the fast path. Bucketing small negatives as noise
    would suppress exactly the observation being hunted, so every sample is
    counted at its own sign and `negative` is reported raw.
    """

    samples = sorted(
        record["stop_hook_delta_ms"]
        for record in records
        if isinstance(record.get("stop_hook_delta_ms"), int)
    )
    if not samples:
        return None
    return {
        "count": len(samples),
        # The answer.
        "positive": sum(1 for value in samples if value > 0),
        "negative": sum(1 for value in samples if value < 0),
        "zero": sum(1 for value in samples if value == 0),
        # Secondary, and far less robust than the tally above.
        "min": samples[0],
        "median": nearest_rank(samples, 50),
        "max": samples[-1],
    }


def uncomputable_stop_hook_reasons(
    records: Sequence[Mapping[str, Any]],
) -> Counter[str]:
    """Why each SUCCESSFUL attempt contributed no stop-hook ordering sample.

    Successful attempts only, for the same reason as
    `uncomputable_gap_reasons`: a failed or incomplete attempt had no timings
    for the field to be missing from.
    """

    return Counter(
        record["stop_hook_delta_uncomputable_reason"] or "reason_not_recorded"
        for record in records
        if record["status"] == "pmux_exit_zero" and record["stop_hook_delta_ms"] is None
    )


def classify_attempt(record: Mapping[str, Any]) -> str:
    """Which of `ATTEMPT_BUCKETS` this attempt belongs to -- exactly one.

    Priority matters: an attempt this tool crashed on is `fatal` whatever its
    status field said, because every other field is then suspect. `unreadable`
    is the reservation-less directory; anything else that is not
    `pmux_exit_zero` and not incomplete ran and failed.
    """

    if record["fatal_error"]:
        return "fatal"
    status = record["status"]
    if status == "pmux_exit_zero":
        return "successful"
    if status == "incomplete_no_outcome":
        return "incomplete"
    if status == "unreadable":
        return "unreadable"
    return "failed"


# Why an ANSWER that exists went unchecked, in words. Rendered in the DEFAULT
# text output beside the count, because a coverage hole the reader cannot name
# is a coverage hole the reader will assume is small.
#
# `no_result` is deliberately NOT in this table. An attempt that never produced
# a reply is "nothing to check", not "an unchecked answer", and folding the two
# together inflated the headline: on the Gate B tree it read "10 of 17
# discovered attempt(s) had NO TRUNCATION ORACLE" when 7 of those 10 were
# FAILED attempts that produced no answer at all. Its explanation lives in
# `NO_ANSWER_REASONS` below and is counted in its own denominator.
UNCHECKED_ANSWER_REASONS = {
    "not_applicable": (
        "this grade's prompt never asked for a hash, so its reply carries no "
        "self-check. Legitimate and by design (01-baseline-trivial and "
        "02-poem-only-no-tool exist to exercise short and tool-free turns), "
        "but it means an EMPTY or truncated reply from this attempt would "
        "grade exactly as a complete one does"
    ),
    "missing": (
        "the prompt asked for a hash and the reply carried none, so there was "
        "nothing to recompute. Already a failing condition on its own"
    ),
    "hash_expectation_unknown": (
        "the reply carried no hash and this tool could not identify the "
        "prompt, so it can say neither that one was required nor that one was "
        "not. Already a failing condition on its own"
    ),
    "error": (
        "the attempt succeeded but its turn-result artifact could not be read, "
        "so no reply text was available to check"
    ),
}


# Why an attempt contributed no answer to check in the first place. Counted and
# rendered SEPARATELY from the un-oracled answers above, so each number means
# exactly one thing.
NO_ANSWER_REASONS = {
    "no_result": (
        "the attempt never produced a public result (incomplete, crashed, or a "
        "failed pmux command), so there was no reply to check. Nothing to "
        "check -- NOT an unchecked answer"
    ),
}


def produced_answer(record: Mapping[str, Any]) -> bool:
    """Did this attempt produce a reply that COULD have been checked?

    `no_result` is the single `hash_overall` value that means the attempt never
    got as far as a public turn result: an incomplete run, a crash, or a pmux
    command that exited non-zero. There is no answer there to be truncated.

    `error` counts as an answer on purpose. pmux published a turn-result
    artifact and this tool could not read it, so an answer exists and went
    unchecked -- which is exactly the bucket the oracle count is about.
    """

    return record.get("hash_overall") != "no_result"


def has_truncation_oracle(record: Mapping[str, Any]) -> bool:
    """Did this attempt get CHECKED for truncation at all?

    The oracle is the hash: a reported `SHA256:` line recomputed from the poem
    text pmux's own result captured. Deleting one character from that poem
    flips the check. An attempt with no recomputed hash has NO such check over
    it -- a truncated reply and a complete one grade identically -- and that is
    true whatever its `hash_overall` says, including the reassuring-sounding
    `not_applicable`.

    Keyed off `hash_checks` rather than off `hash_overall` deliberately: the
    question is "was a hash independently recomputed", and only the list of
    recomputed checks answers it. `partial` counts as covered because the
    hashes the reply DID carry were verified against the body -- the omitted
    labels are a separate, already-failing condition.
    """

    return bool(record.get("hash_checks"))


def uncomputable_gap_reasons(records: Sequence[Mapping[str, Any]]) -> Counter[str]:
    """Why each SUCCESSFUL attempt contributed no gap sample.

    Only successful attempts are counted: a failed or incomplete attempt has no
    timings to be missing a field from, so folding it in here would inflate the
    tripwire this feeds with attempts that were never expected to publish one.
    `compute_gap` always returns a reason with a `None` gap, so the fallback
    label is unreachable defence, not an expected value.
    """

    return Counter(
        record["gap_uncomputable_reason"] or "reason_not_recorded"
        for record in records
        if record["status"] == "pmux_exit_zero" and record["gap_ms"] is None
    )


def build_report(
    records: Sequence[dict[str, Any]],
    prompt_suite: Sequence[Mapping[str, Any]],
    *,
    configured_drain_override: int | None,
) -> dict[str, Any]:
    grade_order = [entry["grade"] for entry in prompt_suite]
    extra_grades = sorted(
        {
            record["grade"]
            for record in records
            if record["grade"] not in grade_order and record["grade"] != "unreadable"
        }
    )
    grades: dict[str, Any] = {}
    for grade in grade_order + extra_grades:
        members = [record for record in records if record["grade"] == grade]
        hash_tally = Counter(record["hash_overall"] for record in members)
        member_gap_reasons = uncomputable_gap_reasons(members)
        grades[grade] = {
            "attempts": len(members),
            "gap_distribution": summarize_gaps(members),
            "attempts_without_computable_gap": sum(member_gap_reasons.values()),
            "gap_uncomputable_reasons": dict(member_gap_reasons),
            "hash_tally": dict(hash_tally),
            # How many of this grade's ANSWERS were actually CHECKED for
            # truncation. `hash_tally` alone could not answer it: a row reading
            # `{'not_applicable': 2}` looks cleared and is unexamined.
            #
            # The denominator is `answers`, not `attempts`: grade 01 on the
            # Gate B tree has 8 attempts of which 6 produced no reply at all,
            # and `oracle=0/8` blamed the oracle for six failures that had
            # nothing for an oracle to look at.
            "answers": sum(1 for record in members if produced_answer(record)),
            "attempts_with_no_answer": sum(
                1 for record in members if not produced_answer(record)
            ),
            "answers_with_truncation_oracle": sum(
                1 for record in members if has_truncation_oracle(record)
            ),
            "answers_without_truncation_oracle": sum(
                1
                for record in members
                if produced_answer(record) and not has_truncation_oracle(record)
            ),
            # A row whose members were not all identified by content is a row
            # whose label is partly a guess. Rendered, so the by-grade table
            # cannot be read as if every attempt's grade were established.
            "grade_source_tally": dict(
                Counter(record["grade_source"] for record in members)
            ),
        }

    successful = [record for record in records if record["status"] == "pmux_exit_zero"]
    hash_tally_overall = Counter(record["hash_overall"] for record in records)
    mismatches = [
        {
            "attempt_id": record["attempt_id"],
            "grade": record["grade"],
            "attempt_dir": record["attempt_dir"],
            "checks": [check for check in record["hash_checks"] if not check["match"]],
        }
        for record in records
        if record["hash_overall"] == "mismatch"
    ]
    missing = [
        {
            "attempt_id": record["attempt_id"],
            "grade": record["grade"],
            "attempt_dir": record["attempt_dir"],
        }
        for record in records
        if record["hash_overall"] == "missing"
    ]
    partial = [
        {
            "attempt_id": record["attempt_id"],
            "grade": record["grade"],
            "attempt_dir": record["attempt_dir"],
            "missing_labels": record.get("hash_missing_labels") or [],
        }
        for record in records
        if record["hash_overall"] == "partial"
    ]
    expectation_unknown = [
        {
            "attempt_id": record["attempt_id"],
            "grade": record["grade"],
            "attempt_dir": record["attempt_dir"],
        }
        for record in records
        if record["hash_overall"] == "hash_expectation_unknown"
    ]

    # Attempts whose `compatibility.transcript_drain_ms` was absent or not an
    # integer are EXCLUDED from the value below -- and the note has to say so.
    # Filtering them out and then claiming "constant across every successful
    # attempt" is a confident claim about attempts that were never consulted:
    # a run where half the attempts publish no configured drain earned exactly
    # the same sentence as a run where every one of them published 2000.
    configured_drain_published = [
        record["configured_transcript_drain_ms"]
        for record in successful
        if isinstance(record.get("configured_transcript_drain_ms"), int)
        and not isinstance(record.get("configured_transcript_drain_ms"), bool)
    ]
    configured_drains = sorted(set(configured_drain_published))
    configured_drain_excluded = len(successful) - len(configured_drain_published)
    excluded_clause = (
        ""
        if not configured_drain_excluded
        else (
            f"; {configured_drain_excluded} of {len(successful)} successful "
            "attempt(s) published no integer transcript_drain_ms and were "
            "EXCLUDED from this claim -- it says nothing about them"
        )
    )
    if configured_drain_override is not None:
        configured_drain = configured_drain_override
        configured_drain_note = (
            "overridden by --configured-drain-ms; what the attempts themselves "
            f"published was {configured_drains} over "
            f"{len(configured_drain_published)} of {len(successful)} successful "
            "attempt(s)"
        )
    elif not configured_drain_published:
        configured_drain = None
        configured_drain_note = (
            f"no configured value: not one of {len(successful)} successful "
            "attempt(s) published an integer "
            "compatibility.transcript_drain_ms"
        )
    elif len(configured_drains) == 1:
        configured_drain = configured_drains[0]
        configured_drain_note = (
            "constant across the "
            f"{len(configured_drain_published)} of {len(successful)} successful "
            "attempt(s) that published one" + excluded_clause
        )
    else:
        configured_drain = None
        configured_drain_note = (
            f"varies across the {len(configured_drain_published)} of "
            f"{len(successful)} successful attempt(s) that published one: "
            f"{configured_drains}" + excluded_clause
        )

    # The EFFECTIVE drain, kept strictly apart from the configured one. When
    # `--configured-drain-ms` overrides what the attempts published, the
    # required drain is re-derived against the override rather than left
    # stale -- otherwise the two lines would describe different builds.
    per_attempt_required = [
        (
            derive_required_drain(
                record["turn_duration_marker_observed"],
                configured_drain,
                record["observed_drain_ms"],
            )
            if configured_drain_override is not None
            else RequiredDrain(
                record["required_drain_ms"],
                record["required_drain_ms_lower_bound"],
                record["graduation_state"],
            )
        )
        for record in successful
    ]
    run_required = summarize_required_drain(per_attempt_required)
    graduation_state_tally = dict(Counter(item.state for item in per_attempt_required))
    marker_tally = dict(
        Counter(
            "observed"
            if record["turn_duration_marker_observed"] is True
            else (
                "absent"
                if record["turn_duration_marker_observed"] is False
                else "unknown"
            )
            for record in successful
        )
    )
    # The single field a regression check can watch. `True` only when EVERY
    # successful attempt proved graduation from its own timings; `False` only
    # when none of them saw the marker at all; `None` for every mixed or
    # indeterminate run, because "we could not tell" must not read as "off".
    states = set(graduation_state_tally)
    if states == {"graduated"}:
        run_is_graduated: bool | None = True
    elif states == {"not_graduated_no_marker"}:
        run_is_graduated = False
    else:
        run_is_graduated = None

    overall_gaps = summarize_gaps(records)
    # Headroom is only meaningful against a MEASURED worst case. When the
    # largest gap is inside the noise band there is no measured late row at all,
    # and publishing `configured - 1` as headroom turns a clock artifact into an
    # apparent 1,999 ms of proven margin. Leave it null and let the
    # absence-of-evidence banner carry the result instead.
    #
    # AND it must be taken against the drain the gate REQUIRED, not the one the
    # compatibility block configured. Against 2,000 ms the ordinal-70 near-miss
    # published 1,648 ms of headroom; against the 250 ms its gate actually
    # required the same sample is -102 ms. The second number is the one that
    # describes the mechanism under test, so it is the one `headroom_ms` now
    # carries -- and when only a LOWER BOUND on the required drain is known,
    # the bound is used, because that is the direction that cannot overstate
    # margin.
    headroom_basis_ms = (
        run_required.required_ms
        if run_required.required_ms is not None
        else run_required.lower_bound_ms
    )
    if headroom_basis_ms is None:
        headroom_basis = "no required drain could be derived"
    elif run_required.required_ms is not None:
        headroom_basis = "required drain (established)"
    else:
        headroom_basis = (
            "LOWER BOUND on the required drain (the exact value is unknown)"
        )
    measured_late_row = (
        overall_gaps is not None and overall_gaps["max"] > overall_gaps["noise_band_ms"]
    )
    # The worst case the margin is taken against, published rather than
    # recomputed at the render site, so the renderer cannot disagree with the
    # arithmetic. `None` means there was no measured late row to take it from.
    headroom_worst_case_gap_ms = (
        max(overall_gaps["max"], 0) if measured_late_row else None
    )
    headroom_ms = None
    if headroom_basis_ms is not None and headroom_worst_case_gap_ms is not None:
        headroom_ms = headroom_basis_ms - headroom_worst_case_gap_ms
    # Every reason headroom could not be stated, in words, so the DEFAULT text
    # output never just omits the line. Both reasons can hold at once and both
    # are listed: "we have no worst case" and "we have nothing to measure it
    # against" are different holes and collapsing them hides one.
    headroom_uncomputable_reasons: list[str] = []
    if headroom_basis_ms is None:
        headroom_uncomputable_reasons.append(
            "no required drain could be derived from the published timings, so "
            "there is no value to take a margin against"
        )
    if headroom_worst_case_gap_ms is None:
        headroom_uncomputable_reasons.append(
            "no late row was measured anywhere in this run (every gap sits at "
            "or below the noise band, or no gap was computable at all), so "
            "there is no measured worst case to take a margin from. See the "
            "absence-of-evidence block below"
        )
    # Kept, and kept LABELLED, so the old number is still readable beside the
    # new one and the gap between them is visible rather than silently swapped.
    headroom_vs_configured_ms = None
    if configured_drain is not None and headroom_worst_case_gap_ms is not None:
        headroom_vs_configured_ms = configured_drain - headroom_worst_case_gap_ms

    # `drain_ms` -- the stability each commit actually PAID -- is the third
    # input to every graduation verdict above, and until now it reached no
    # DEFAULT text render site at all. Summarized here so the verdict's
    # evidence is visible beside the verdict.
    observed_drains = sorted(
        record["observed_drain_ms"]
        for record in successful
        if isinstance(record.get("observed_drain_ms"), int)
    )
    observed_drain_summary = {
        "attempts_publishing_drain_ms": len(observed_drains),
        "attempts_not_publishing_drain_ms": len(successful) - len(observed_drains),
        "min": observed_drains[0] if observed_drains else None,
        "median": nearest_rank(observed_drains, 50) if observed_drains else None,
        "max": observed_drains[-1] if observed_drains else None,
    }

    effort_tally = dict(Counter(record["effort"] for record in successful))
    only_low = successful and set(effort_tally) <= {"low"}

    # How many ANSWERS were actually CHECKED for truncation, and how many were
    # not. `hash_tally_overall` could not answer this and read as if it did:
    # `{'match': 7, 'not_applicable': 2}` looks like nine attempts cleared, and
    # is seven checked plus two never examined. Emptying grade 02's entire poem
    # left that tally, the failing conditions and every note unchanged. This
    # block is the fix, and it is an OBSERVATION -- grades 01 and 02 are
    # un-oracled BY DESIGN, so making the hole fatal would gate a claim this
    # tool does not make. It is loud instead.
    #
    # TWO denominators, never one. "An answer nobody checked" is a coverage
    # hole; "an attempt that produced no answer" is a failure already counted
    # in the buckets above and has nothing for an oracle to look at. The first
    # version of this block added them together and headlined "10 of 17
    # discovered attempt(s) had NO TRUNCATION ORACLE" on a tree whose real
    # figure was 3 unchecked answers out of 10.
    answers = [record for record in records if produced_answer(record)]
    no_answer = [record for record in records if not produced_answer(record)]
    unchecked_answers = [
        record for record in answers if not has_truncation_oracle(record)
    ]
    truncation_oracle_coverage = {
        "attempts_discovered": len(records),
        # Nothing to check: no reply was ever produced.
        "attempts_with_no_answer": len(no_answer),
        "no_answer_by_reason": dict(
            Counter(record["hash_overall"] for record in no_answer)
        ),
        # The oracle's real denominator.
        "answers_discovered": len(answers),
        "answers_with_oracle": len(answers) - len(unchecked_answers),
        "answers_without_oracle": len(unchecked_answers),
        "unchecked_answers_by_reason": dict(
            Counter(record["hash_overall"] for record in unchecked_answers)
        ),
        # A grade is only oracle-blind if it produced answers that nobody
        # checked. A grade whose every attempt failed is not an oracle hole.
        "grades_without_oracle": sorted(
            {
                grade
                for grade in grade_order + extra_grades
                if grades[grade]["answers"]
                and grades[grade]["answers_with_truncation_oracle"] == 0
            }
        ),
        "unchecked_answers": [
            {
                "attempt_id": record["attempt_id"],
                "global_attempt_ordinal": record["global_attempt_ordinal"],
                "grade": record["grade"],
                "attempt_dir": record["attempt_dir"],
                "reason": record["hash_overall"],
            }
            for record in unchecked_answers
        ],
        "attempts_with_no_answer_detail": [
            {
                "attempt_id": record["attempt_id"],
                "global_attempt_ordinal": record["global_attempt_ordinal"],
                "grade": record["grade"],
                "attempt_dir": record["attempt_dir"],
                "reason": record["hash_overall"],
            }
            for record in no_answer
        ],
    }

    buckets = Counter(classify_attempt(record) for record in records)
    partition = {bucket: buckets.get(bucket, 0) for bucket in ATTEMPT_BUCKETS}
    fatal_errors = [
        {"attempt_dir": record["attempt_dir"], "error": record["fatal_error"]}
        for record in records
        if classify_attempt(record) == "fatal"
    ]
    incomplete = [
        record["attempt_dir"]
        for record in records
        if classify_attempt(record) == "incomplete"
    ]
    # An attempt that RAN and failed. It was in none of the three header
    # categories before, so a campaign that burnt six ordinals on failures
    # printed the same header as one that burnt none.
    failed = [
        {
            "attempt_id": record["attempt_id"],
            "global_attempt_ordinal": record["global_attempt_ordinal"],
            "grade": record["grade"],
            "attempt_dir": record["attempt_dir"],
            "error": record["error"],
        }
        for record in records
        if classify_attempt(record) == "failed"
    ]
    failed_error_tally = dict(Counter(str(item["error"]) for item in failed))
    # A directory with no readable reservation.json: not successful, not
    # failed, not incomplete. It appeared in NO row of the report at all,
    # because `extra_grades` filters "unreadable" out of the by-grade table.
    unreadable = [
        record["attempt_dir"]
        for record in records
        if classify_attempt(record) == "unreadable"
    ]
    # Everything the analysis recorded and the report used to drop on the
    # floor. Eight `notes.append` sites, zero render sites, was how the
    # grade-misattribution defect survived detection.
    attempts_with_notes = [
        {
            "attempt_id": record["attempt_id"],
            "global_attempt_ordinal": record["global_attempt_ordinal"],
            "grade": record["grade"],
            "grade_source": record["grade_source"],
            "attempt_dir": record["attempt_dir"],
            "notes": list(record["notes"]),
        }
        for record in records
        if record["notes"]
    ]
    grade_source_tally = dict(Counter(record["grade_source"] for record in records))
    overall_gap_reasons = uncomputable_gap_reasons(records)

    stop_hook_deltas = summarize_stop_hook_deltas(records)
    stop_hook_reasons = uncomputable_stop_hook_reasons(records)
    # Named individually, not just counted. A single negative closes the
    # optimization question permanently, so the reader must be able to go
    # straight to the attempt that produced it and check the evidence by hand.
    stop_hook_negative_attempts = [
        {
            "attempt_id": record["attempt_id"],
            "global_attempt_ordinal": record["global_attempt_ordinal"],
            "grade": record["grade"],
            "attempt_dir": record["attempt_dir"],
            "stop_hook_delta_ms": record["stop_hook_delta_ms"],
        }
        for record in records
        if isinstance(record.get("stop_hook_delta_ms"), int)
        and record["stop_hook_delta_ms"] < 0
    ]

    return {
        "attempts_discovered": len(records),
        "attempts_successful": len(successful),
        "attempts_incomplete": incomplete,
        "attempts_with_fatal_errors": fatal_errors,
        "attempts_by_bucket": partition,
        # Compares the buckets actually RENDERED against the discovered count,
        # so a classification `ATTEMPT_BUCKETS` does not list shows up as an
        # imbalance instead of silently dropping an attempt out of the header
        # again. `sum(Counter(...).values()) == len(records)` would have been
        # true by construction and checked nothing.
        "attempts_partition_balances": sum(partition.values()) == len(records),
        "attempts_failed": failed,
        "failed_error_tally": failed_error_tally,
        "attempts_unreadable": unreadable,
        "attempts_with_notes": attempts_with_notes,
        "grade_source_tally": grade_source_tally,
        "attempts_not_graded_by_content": sum(
            count
            for source, count in grade_source_tally.items()
            if source != "prompt_sha256"
        ),
        "gap_uncomputable_reasons": dict(overall_gap_reasons),
        "attempts_without_computable_gap": sum(overall_gap_reasons.values()),
        "hash_partial": partial,
        "hash_expectation_unknown": expectation_unknown,
        "effort_tally": effort_tally,
        "only_low_effort_exercised": bool(only_low),
        "hash_tally_overall": dict(hash_tally_overall),
        "hash_mismatches": mismatches,
        "hash_missing_when_expected": missing,
        "overall_gap_distribution": overall_gaps,
        # Reported SEPARATELY from `overall_gap_distribution` at every level:
        # the drain-length question and the hook-ordering question share a
        # timestamp but not a claim.
        "stop_hook_delta_distribution": stop_hook_deltas,
        "stop_hook_delta_uncomputable_reasons": dict(stop_hook_reasons),
        "attempts_without_computable_stop_hook_delta": sum(stop_hook_reasons.values()),
        "stop_hook_negative_attempts": stop_hook_negative_attempts,
        # The one-line answer to "may we short-circuit the drain on the hook?",
        # in a field a script can read. False when ANY sample was negative, and
        # None while no sample exists at all -- never False-by-default, because
        # "not yet observed" and "observed and forbidden" are different states
        # and only the second is a finding.
        # A zero delta does NOT establish which came first -- the text output
        # already says so -- so a zero-only sample set must not read as license in
        # the machine field either. It requires at least one POSITIVE observation
        # and no negative one; anything weaker is None, the same "not yet
        # observed" state as no samples at all. Reporting true here from
        # non-establishing samples would license the fast path from evidence the
        # prose refuses to license it from.
        "stop_hook_ordering_permits_fast_path": (
            None
            if stop_hook_deltas is None
            else (
                False
                if stop_hook_deltas["negative"] > 0
                else (True if stop_hook_deltas["positive"] > 0 else None)
            )
        ),
        "configured_transcript_drain_ms": configured_drain,
        "configured_transcript_drain_ms_note": configured_drain_note,
        # What the gate REQUIRED, reported as its own field beside -- never
        # instead of -- the configured value. On a graduated build these differ
        # by 1,750 ms and every margin claim depends on which one was used.
        "required_drain_ms": run_required.required_ms,
        "required_drain_ms_note": run_required.note,
        "required_drain_ms_lower_bound": run_required.lower_bound_ms,
        "graduation_state_tally": graduation_state_tally,
        "turn_duration_marker_tally": marker_tally,
        "run_is_graduated": run_is_graduated,
        # The stability each commit actually paid, which is what the graduation
        # verdict above is inferred from.
        "observed_drain_ms_summary": observed_drain_summary,
        "headroom_ms": headroom_ms,
        "headroom_basis_ms": headroom_basis_ms,
        "headroom_basis": headroom_basis,
        "headroom_worst_case_gap_ms": headroom_worst_case_gap_ms,
        # Why there is no headroom number, when there is none. Never an empty
        # line in the text output: a value this tool declined to compute has to
        # say so out loud, in the DEFAULT mode, with its reason.
        "headroom_uncomputable_reasons": headroom_uncomputable_reasons,
        # The OLD number, kept and named so the swap is visible rather than
        # silent. It is not a margin against anything the gate did.
        "headroom_vs_configured_ms": headroom_vs_configured_ms,
        "truncation_oracle_coverage": truncation_oracle_coverage,
        "answers_without_truncation_oracle": len(unchecked_answers),
        "attempts_with_no_answer": len(no_answer),
        "grades": grades,
        "grade_order": grade_order + extra_grades,
    }


def failing_conditions(report: Mapping[str, Any]) -> list[str]:
    """Every reason this report must not exit 0, in one place.

    `render_report` prints exactly this list and `main` returns nonzero for
    exactly this list, so the exit status can never disagree with the text
    beside it. Each entry names a claim the report cannot make, and nothing
    else: a note, a `not_applicable` hash, or an absence-of-evidence banner is
    an OBSERVATION and is rendered without touching the exit status.

    Nothing about the stop-hook ordering appears here, deliberately. A gate must
    gate exactly the claim it protects, and this tool's exit 0 claims that the
    published evidence checks out -- not that a proposed drain optimization is
    available. An absent `stop_hook_at_ms` is the documented normal case for any
    session without the Hybrid lifecycle hook, so gating on its presence would
    fail every campaign that predates the field; and a NEGATIVE sample is a true
    reading about Claude, not a corrupt artifact. Both are rendered loudly and
    neither is a failure.
    """

    conditions: list[str] = []
    tally = report["hash_tally_overall"]
    if report["hash_mismatches"]:
        conditions.append(
            f"{len(report['hash_mismatches'])} attempt(s) reported a hash this "
            "tool could not reproduce from the poem text pmux captured"
        )
    if report["attempts_with_fatal_errors"]:
        conditions.append(
            f"{len(report['attempts_with_fatal_errors'])} attempt(s) could not "
            "be parsed at all, so their evidence is unchecked"
        )
    if tally.get("partial"):
        conditions.append(
            f"{tally['partial']} attempt(s) verified every hash they carried but "
            "omitted at least one the prompt required"
        )
    if tally.get("missing"):
        conditions.append(
            f"{tally['missing']} attempt(s) were asked for a hash and the final "
            "reply carried none"
        )
    if tally.get("hash_expectation_unknown"):
        conditions.append(
            f"{tally['hash_expectation_unknown']} attempt(s) carried no hash and "
            "this tool could not identify their prompt, so it cannot say whether "
            "one was required"
        )
    if report["overall_gap_distribution"] is None:
        conditions.append(
            "no attempt produced a computable late-arrival gap, so this run "
            "calibrates nothing -- the drain tripwire fired"
        )
    if report["attempts_without_computable_gap"]:
        conditions.append(
            f"{report['attempts_without_computable_gap']} SUCCESSFUL attempt(s) "
            "published no computable gap; a successful turn that reports no "
            "timing is the field-rename tripwire, not a sample to drop"
        )
    if not report["attempts_partition_balances"]:
        conditions.append(
            "the attempt buckets do not sum to the discovered count, so at "
            "least one attempt is missing from this report"
        )
    return conditions


ABSENCE_OF_EVIDENCE_BANNER = (
    "ABSENCE OF EVIDENCE: {count} of {total} attempt(s) observed no late row "
    "-- a gap at or below the {band} ms noise band, of which {band_count} were "
    "inside the band rather than at or below zero. No transcript row arrived "
    "after the terminal candidate in any of them. This measures the ABSENCE of "
    "late rows, not a worst case, and is much weaker evidence than a measured "
    "gap. Do not read it as permission to cut transcript_drain_ms."
)


# Printed whenever the worst measured late arrival exceeded the drain the gate
# actually required. It is NOT a failing condition: no truncation follows from
# it, for the reason the banner spells out, and this tool's exit 0 claims the
# published evidence checks out -- not that the drain is correctly sized.
NEGATIVE_HEADROOM_BANNER = (
    "!!! THE WORST MEASURED LATE ARRIVAL EXCEEDED THE REQUIRED DRAIN: "
    "{basis} ms required, {max} ms measured, headroom {headroom} ms. "
    "The turn did not truncate, and the reason it did not is the property "
    "worth naming: `stable_for_ms` is quiet-since-the-last-transcript-BYTE, "
    "not time-since-the-end-of-turn-marker. Any post-marker write RE-ARMS the "
    f"full window: {STABLE_FOR_MS_CITATION} computes "
    f"stable_for_ms as state.last_change.elapsed(), and {REARM_CITATION} "
    "re-stamps state.last_change inside read_observed_range immediately after "
    "the bytes are pushed onto the cursor -- while a poll that read nothing "
    f"returns at {QUIET_POLL_RETURN_CITATION}, before that call, so a quiet "
    "poll leaves the window running. So a row {max} ms after the "
    "marker bought a fresh {basis} ms, and only rows arriving after the drain "
    "has already been satisfied AND the commit taken are at risk. Read this "
    "as: the margin came from re-arming, NOT from the size of the drain. "
    "Recorded as an observation, not a failure. !!!"
)


# The denominator is ANSWERS, not discovered attempts. An attempt that never
# produced a reply is counted on its own line below -- it is a failure the
# bucket header already reports, and there was never anything for an oracle to
# look at, so adding it here would inflate a coverage hole with somebody else's
# number.
NO_ORACLE_BANNER = (
    "!!! {without} of {answers} answer(s) had NO TRUNCATION ORACLE over them: "
    "not one hash was independently recomputed for those replies, so a "
    "truncated -- or entirely EMPTY -- reply would have graded exactly as a "
    "complete one. The hash tally below is a statement about the {checked} "
    "answer(s) that WERE checked, and about nothing else. Read "
    '"{answers} answers, no mismatches" as "{checked} answers checked". '
    "This is an OBSERVATION, not a failure: prompts that ask for no hash are "
    "un-oracled by design, and a gate must gate exactly the claim it "
    "protects. !!!"
)


FULL_ORACLE_LINE = (
    "every one of {answers} answer(s) had a truncation oracle over it: at "
    "least one reported hash was independently recomputed from the reply body, "
    "so an emptied or truncated reply would have been caught."
)


NO_ANSWER_LINE = (
    "separately, and NOT part of the oracle figure above: {no_answer} of "
    "{total} discovered attempt(s) produced no answer at all, so there was "
    "nothing for any oracle to check. That is a failure already counted in the "
    "bucket header at the top of this report, not a coverage hole here."
)


NO_ANSWERS_AT_ALL_LINE = (
    "!!! NOT ONE of {total} discovered attempt(s) produced an answer, so the "
    "truncation oracle covered nothing and the hash tally below is empty of "
    "any checked reply. This says nothing about truncation either way. !!!"
)


STOP_HOOK_NEGATIVE_BANNER = (
    "!!! NEGATIVE STOP-HOOK ORDERING OBSERVED: {negative} of {count} sample(s) "
    "had Claude's Stop hook arrive BEFORE the final transcript write "
    "(most negative: {min} ms). A SINGLE negative observation is decisive: "
    "completing a turn on the Stop hook -- even as "
    "(stop_hook_observed || stable_for_ms >= drain), where the hook can only "
    "make completion faster -- would commit a TRUNCATED turn, because the hook "
    "can fire while the last row is still unwritten. DO NOT BUILD THE FAST "
    "PATH. The drain stays as it is. This closes the question in the safe "
    "direction; it is not a pmux defect and does not fail this run. !!!"
)

STOP_HOOK_POSITIVE_BANNER = (
    "consistent so far, and only so far: {positive} of {count} sample(s) "
    "positive, {zero} exactly zero, ZERO negative. Every observed Stop hook "
    "arrived at or after the final transcript write, which is the necessary "
    "condition for the (stop_hook_observed || stable_for_ms >= drain) fast "
    "path. This is an ABSENCE of negative observations across {count} "
    "sample(s), not a proof of ordering: the flush and the hook are separate "
    "events, and none of these turns exercised resume, replay, cancellation or "
    "a loaded machine. A zero sample is NOT a positive one -- it means both "
    "landed inside the same millisecond, which does not establish which came "
    "first. Do not cut the drain on this alone."
)

STOP_HOOK_NO_SAMPLE_BANNER = (
    "NO STOP-HOOK ORDERING SAMPLE IN THIS RUN: not one successful attempt "
    f"published {STOP_HOOK_FIELD}, so this run says NOTHING about whether "
    "Claude flushes the transcript before firing Stop. The drain question "
    "stays OPEN, and an open question forbids the fast path exactly as a "
    "negative observation would. This is an observation, not a failure: the "
    "field is optional by design and absent on every session without the "
    "Hybrid lifecycle hook installed. The reasons below say which case each "
    "attempt was."
)


def grade_source_suffix(grade_source_tally: Mapping[str, int]) -> str:
    """The by-grade row's mark for members whose label is not established.

    Empty when every member was identified by prompt content hash, so a clean
    table stays readable and any mark on it means something.
    """

    marks = [
        f"{count} {GRADE_SOURCE_LABELS.get(source, source)}"
        for source, count in sorted(grade_source_tally.items())
        if source != "prompt_sha256"
    ]
    return f" [{'; '.join(marks)}]" if marks else ""


def render_drain_block(report: Mapping[str, Any], out: Any) -> None:
    """Every drain quantity this report computes, in the DEFAULT text output.

    Called UNCONDITIONALLY, and every line inside it prints in every branch --
    including the branches where a value is `None`, which say so and say why.
    That rule is the whole point of this function. The previous shape put the
    entire block behind `overall_gap_distribution is not None` and put
    `headroom_basis` behind `headroom_ms is not None`, so `headroom_basis =
    "no required drain could be derived"` -- a detection, computed and
    assigned -- had ZERO reachable render sites in text mode and appeared only
    under `--json`. Noticing something and not saying it is the defect this
    tool exists to catch; it must not be the defect this tool contains.
    """

    out(
        f"configured_transcript_drain_ms: {report['configured_transcript_drain_ms']} "
        f"({report['configured_transcript_drain_ms_note']})"
    )
    # The configured value alone was the whole of Defect 1: on a graduated
    # build it overstates what the commit gate required by 1,750 ms, and a
    # regression that silently disabled graduation left every other field in
    # this report identical.
    out(
        f"required_drain_ms (EFFECTIVE, what the commit gate asked for): "
        f"{report['required_drain_ms']} ({report['required_drain_ms_note']})"
    )
    if report["required_drain_ms_lower_bound"] is None:
        out(
            "  lower bound on the required drain: none could be derived "
            "either, so this run supports NO margin claim at all"
        )
    elif report["required_drain_ms"] is None:
        out(
            "  lower bound on the required drain: "
            f"{report['required_drain_ms_lower_bound']} ms -- every margin "
            "claim below is taken against this bound, not against the "
            "configured value"
        )
    else:
        out(
            "  lower bound on the required drain: "
            f"{report['required_drain_ms_lower_bound']} ms (the exact required "
            "value above is established, so the bound is redundant here)"
        )
    drains = report["observed_drain_ms_summary"]
    if drains["attempts_publishing_drain_ms"]:
        out(
            f"{OBSERVED_DRAIN_FIELD} actually PAID at commit, over the "
            f"{drains['attempts_publishing_drain_ms']} successful attempt(s) "
            f"that published it: min={drains['min']} median={drains['median']} "
            f"max={drains['max']} ms "
            f"({drains['attempts_not_publishing_drain_ms']} published none). "
            "A commit at less stability than the configured drain is what "
            "PROVES graduation below"
        )
    else:
        out(
            f"{OBSERVED_DRAIN_FIELD} actually PAID at commit: not published by "
            f"any of the {drains['attempts_not_publishing_drain_ms']} "
            "successful attempt(s), so no attempt here can prove graduation "
            "from its own timings"
        )
    out(
        f"graduated end-of-turn drain in effect: {report['run_is_graduated']} "
        f"(states: {report['graduation_state_tally']}; "
        f"{TURN_DURATION_MARKER_FIELD}: {report['turn_duration_marker_tally']})"
    )
    if not report["graduation_state_tally"]:
        out(
            "    (no successful attempt, so there is no graduation state to "
            "report and `run_is_graduated: None` means UNKNOWN, not off)"
        )
    for state, count in sorted(report["graduation_state_tally"].items()):
        out(
            f"    {state}: {count} -- "
            + GRADUATION_STATE_LABELS.get(
                state, "no explanation is recorded for this state"
            )
        )
    # The basis is printed BEFORE the headroom number and whether or not there
    # is a headroom number, because it is the sentence that decides what the
    # number would have meant.
    out(
        f"headroom basis: {report['headroom_basis_ms']} ms -- "
        f"{report['headroom_basis']}"
    )
    worst = report["headroom_worst_case_gap_ms"]
    out(
        "worst measured late arrival the margin is taken from: "
        + (
            f"{worst} ms"
            if worst is not None
            else "NONE -- no gap in this run exceeded the noise band, so there "
            "is no measured worst case"
        )
    )
    if report["headroom_ms"] is not None:
        out(
            f"headroom vs measured worst case: {report['headroom_ms']} ms "
            f"(against {report['headroom_basis_ms']} ms = "
            f"{report['headroom_basis']})"
        )
    else:
        out("headroom vs measured worst case: NOT COMPUTED, because:")
        for reason in report["headroom_uncomputable_reasons"]:
            out(f"    - {reason}")
    if report["headroom_vs_configured_ms"] is not None:
        out(
            "  for comparison only, NOT a margin the gate ever had: "
            f"{report['headroom_vs_configured_ms']} ms against the "
            f"configured {report['configured_transcript_drain_ms']} ms"
        )
    else:
        out(
            "  for comparison only, NOT a margin the gate ever had: not "
            "computed either (no configured drain, or no measured worst case)"
        )
    if report["headroom_ms"] is not None and report["headroom_ms"] < 0:
        out(
            NEGATIVE_HEADROOM_BANNER.format(
                basis=report["headroom_basis_ms"],
                max=report["headroom_worst_case_gap_ms"],
                headroom=report["headroom_ms"],
            )
        )


def render_report(report: Mapping[str, Any]) -> str:
    lines: list[str] = []
    out = lines.append
    out("=== Gate B drain-calibration verification ===")
    partition = report["attempts_by_bucket"]
    out(
        f"attempts discovered: {report['attempts_discovered']} "
        f"(successful: {partition['successful']}, "
        f"failed: {partition['failed']}, "
        f"incomplete: {partition['incomplete']}, "
        f"unreadable: {partition['unreadable']}, "
        f"fatal errors: {partition['fatal']})"
    )
    # Printed as arithmetic because the previous header was not a partition:
    # `failed` attempts were in none of its three categories and a directory
    # with no reservation.json was in no row of the report at all, so ordinals
    # that had been spent disappeared.
    out(
        "  every discovered attempt is in exactly one bucket: "
        + " + ".join(str(partition[bucket]) for bucket in ATTEMPT_BUCKETS)
        + f" = {sum(partition.values())} (discovered "
        f"{report['attempts_discovered']})"
    )
    if not report["attempts_partition_balances"]:
        out(
            "  !!! THE BUCKETS DO NOT SUM TO THE DISCOVERED COUNT: at least one "
            "attempt is missing from this report !!!"
        )
    if report["attempts_with_fatal_errors"]:
        out("  attempts this tool could not parse at all:")
        for item in report["attempts_with_fatal_errors"]:
            out(f"    {item['attempt_dir']}: {item['error']}")
    if report["attempts_failed"]:
        out("  attempts that ran and failed, by error string:")
        for error, count in sorted(
            report["failed_error_tally"].items(), key=lambda item: (-item[1], item[0])
        ):
            out(f"    {count} x {error}")
        for item in report["attempts_failed"]:
            out(
                f"    ordinal={item['global_attempt_ordinal']} "
                f"attempt {item['attempt_id']} ({item['grade']})"
            )
    if report["attempts_unreadable"]:
        out(
            "  attempt directories with no readable reservation.json (a spent "
            "ordinal this tool cannot attribute to any grade):"
        )
        for attempt_dir in report["attempts_unreadable"]:
            out(f"    {attempt_dir}")
    out(f"grade labels by source: {report['grade_source_tally']}")
    if report["attempts_not_graded_by_content"]:
        out(
            f"  NOTE: {report['attempts_not_graded_by_content']} attempt(s) were "
            "not identified by prompt content hash. prompt_sha256 is the only "
            "source that names a PROMPT; prompt_suite_index names a position in "
            "an argv list that a resumed or subset campaign renumbers from 1."
        )
    out(f"effort exercised: {report['effort_tally']}")
    if report["only_low_effort_exercised"]:
        out(
            '  NOTE: only "low" effort was exercised. "medium" is the only '
            f"other value APPROVED_EFFORTS permits ({APPROVED_EFFORTS_CITATION}"
            '); "high" remains outside the current authorization. A '
            'calibration run at "low" only says nothing about "medium" turns.'
        )
    out("")
    # MANDATORY: printed whether or not there is anything to print, because
    # "the tool recorded nothing" and "the tool recorded something and the
    # report withheld it" must not look the same. Every `notes.append` site in
    # `analyze_attempt` surfaces here.
    out("--- ATTEMPTS THIS REPORT COULD NOT FULLY GRADE ---")
    out(
        "every note the per-attempt analysis recorded, including notes that do "
        "not change a verdict. A note is an observation, not a failure."
    )
    if not report["attempts_with_notes"]:
        out("  (none: every discovered attempt graded cleanly)")
    for item in report["attempts_with_notes"]:
        source = GRADE_SOURCE_LABELS.get(item["grade_source"], item["grade_source"])
        out(
            f"  ordinal={item['global_attempt_ordinal']} "
            f"attempt {item['attempt_id']} grade={item['grade']} ({source}):"
        )
        for note in item["notes"]:
            out(f"    - {note}")
    out("")
    out("--- Hash verification (independent proof-of-work check) ---")
    # PRINTED FIRST, BEFORE the tally, and in every branch. The tally is the
    # sentence a reader takes away, and it is a statement about the checked
    # attempts only; the denominator has to arrive before the numerator, not
    # after it.
    coverage = report["truncation_oracle_coverage"]
    if not coverage["answers_discovered"]:
        out(NO_ANSWERS_AT_ALL_LINE.format(total=coverage["attempts_discovered"]))
    elif coverage["answers_without_oracle"]:
        out(
            NO_ORACLE_BANNER.format(
                without=coverage["answers_without_oracle"],
                answers=coverage["answers_discovered"],
                checked=coverage["answers_with_oracle"],
            )
        )
        out("  why each of those answers went unchecked:")
        for reason, count in sorted(coverage["unchecked_answers_by_reason"].items()):
            out(f"    {reason}: {count}")
            out(
                "      "
                + UNCHECKED_ANSWER_REASONS.get(
                    reason, "no explanation is recorded for this reason"
                )
            )
        if coverage["grades_without_oracle"]:
            out(
                "  grade(s) with NO oracle over ANY of the answers they "
                "produced: " + ", ".join(coverage["grades_without_oracle"])
            )
        else:
            out(
                "  no grade is wholly un-oracled: every grade that produced an "
                "answer had at least one of them checked"
            )
        out("  the unchecked answers, so they can be checked by hand:")
        for item in coverage["unchecked_answers"]:
            out(
                f"    ordinal={item['global_attempt_ordinal']} "
                f"attempt {item['attempt_id']} ({item['grade']}): {item['reason']}"
            )
    else:
        out(FULL_ORACLE_LINE.format(answers=coverage["answers_discovered"]))
    # The SECOND denominator, always printed when it is nonzero and always kept
    # out of the first. Its absence is stated too, so "no attempt failed to
    # answer" and "the report forgot to say" cannot look the same.
    if coverage["attempts_with_no_answer"]:
        out(
            NO_ANSWER_LINE.format(
                no_answer=coverage["attempts_with_no_answer"],
                total=coverage["attempts_discovered"],
            )
        )
        for reason, count in sorted(coverage["no_answer_by_reason"].items()):
            out(f"    {reason}: {count}")
            out(
                "      "
                + NO_ANSWER_REASONS.get(
                    reason, "no explanation is recorded for this reason"
                )
            )
        out("  the attempts that produced no answer:")
        for item in coverage["attempts_with_no_answer_detail"]:
            out(
                f"    ordinal={item['global_attempt_ordinal']} "
                f"attempt {item['attempt_id']} ({item['grade']}): {item['reason']}"
            )
    else:
        out(
            f"every one of {coverage['attempts_discovered']} discovered "
            "attempt(s) produced an answer, so no attempt is excluded from the "
            "oracle denominator above."
        )
    out(
        f"tally (over the {coverage['answers_with_oracle']} of "
        f"{coverage['answers_discovered']} answer(s) an oracle covered, plus "
        f"the {coverage['answers_without_oracle']} it did not, plus the "
        f"{coverage['attempts_with_no_answer']} attempt(s) with no answer): "
        f"{report['hash_tally_overall']}"
    )
    out(
        "  match = independently recomputed from the poem text pmux's own "
        "result captured, and it equals the reported hash"
    )
    out("  mismatch = recomputed hash disagrees with every byte variant tried")
    out(
        "  partial = every hash the reply carried verified, but the prompt "
        "required at least one more that the reply omitted"
    )
    out("  missing = the prompt asked for a hash but the final reply had none")
    out("  not_applicable = this grade's prompt did not ask for a hash")
    out(
        "  hash_expectation_unknown = the reply carried no hash and this tool "
        "could not identify the prompt, so it cannot say whether one was asked "
        "for -- NOT the same claim as not_applicable"
    )
    out(
        "  error = the attempt succeeded but its turn-result artifact could not "
        "be read, so no hash was checked"
    )
    out(
        "  no_result = this attempt never produced a public result to check "
        "(incomplete/crashed or a failed pmux command)"
    )
    if report["hash_mismatches"]:
        out("")
        out("  !!! HASH MISMATCHES (investigate before trusting this run) !!!")
        for item in report["hash_mismatches"]:
            out(f"    attempt {item['attempt_id']} ({item['grade']}):")
            for check in item["checks"]:
                out(
                    f"      label={check['label']} reported={check['reported']} "
                    f"recomputed(exact)={check['recomputed_example']} "
                    f"reason={check['reason']}"
                )
    if report["hash_partial"]:
        out("")
        out("  !!! REQUIRED HASHES THE REPLY NEVER PRODUCED !!!")
        for item in report["hash_partial"]:
            out(
                f"    attempt {item['attempt_id']} ({item['grade']}) omitted: "
                f"{', '.join(item['missing_labels'])}"
            )
    if report["hash_missing_when_expected"]:
        out("")
        out("  hash expected but absent from the final reply:")
        for item in report["hash_missing_when_expected"]:
            out(f"    attempt {item['attempt_id']} ({item['grade']})")
    if report["hash_expectation_unknown"]:
        out("")
        out(
            "  no hash in the reply and no identifiable prompt, so whether one "
            "was required is UNKNOWN (do not read these as not_applicable):"
        )
        for item in report["hash_expectation_unknown"]:
            out(f"    attempt {item['attempt_id']} ({item['grade']})")
    out("")
    out(
        "--- Late-arrival gap distribution "
        f"({LATE_ARRIVAL_FIELD} - terminal_candidate_at_ms) ---"
    )
    overall = report["overall_gap_distribution"]
    if overall is None:
        # The module docstring promises this is "a loud, safe failure rather
        # than a silently wrong number". It used to be one bland line and exit
        # 0, which is neither loud nor a failure.
        out("!!! NO COMPUTABLE LATE-ARRIVAL GAP ANYWHERE IN THIS RUN !!!")
        out("no attempt produced a computable gap; nothing to calibrate from.")
        out(
            f"this check depends on {LATE_ARRIVAL_FIELD}; if that field were "
            "renamed or dropped upstream, this is exactly what it would look "
            "like. The reasons below say which."
        )
    else:
        p95 = (
            f"p95={overall['p95']} "
            if "p95" in overall
            else f"(p95 omitted: identical to max below {P95_MEANINGFUL_MIN_SAMPLES} samples) "
        )
        out(
            f"OVERALL: count={overall['count']} min={overall['min']} "
            f"median={overall['median']} {p95}max={overall['max']} "
            f"| no-late-row={overall['no_late_row_attempts']} "
            f"within-noise-band={overall['within_noise_band_attempts']} "
            f"late-row={overall['late_row_attempts']}"
        )
        out(
            f"a gap within {overall['noise_band_ms']} ms of zero (one actor poll "
            f"interval, {ACTOR_POLL_INTERVAL_CITATION}) is measurement noise, "
            "not a late row"
        )
    # MANDATORY, in the DEFAULT text output, OUTSIDE the branch above. Every
    # line of it used to sit inside the `overall is not None` arm, so a run
    # with no computable gap published a required drain, a graduation verdict
    # and a headroom basis into --json and printed none of them -- the exact
    # eight-write-sites-zero-render-sites shape that hid a real bug in this
    # project for weeks. A value this tool computes gets a render site here or
    # it does not get computed.
    render_drain_block(report, out)
    # Printed in BOTH branches, and before the banner, because the banner's
    # denominator only means anything once the reader knows how many successful
    # attempts contributed no sample at all. A distribution over 1 of 6
    # successful attempts used to render as `count=1` with no hint that five
    # were dropped, and the reader had to guess where they went.
    if report["attempts_without_computable_gap"]:
        out(
            f"gaps NOT computable: {report['attempts_without_computable_gap']} "
            "successful attempt(s) published no usable timing pair, by reason:"
        )
        for reason, count in sorted(report["gap_uncomputable_reasons"].items()):
            out(f"    {reason}: {count}")
    else:
        out(
            "every successful attempt published a computable gap "
            "(no attempt was silently dropped from the distribution)"
        )
    if overall is not None and overall["late_row_attempts"] == 0:
        out("")
        out(
            ABSENCE_OF_EVIDENCE_BANNER.format(
                count=overall["no_late_row_attempts"],
                total=overall["count"],
                band=overall["noise_band_ms"],
                band_count=overall["within_noise_band_attempts"],
            )
        )
    out("")
    # MANDATORY, in the DEFAULT text output, printed in every branch -- the same
    # rule the notes block follows. A quantity that only appears under --json is
    # this project's signature defect, and this one decides whether ~2,300 ms per
    # turn is recoverable.
    out(f"--- Stop-hook ordering ({STOP_HOOK_FIELD} - {LATE_ARRIVAL_FIELD}) ---")
    out(
        "a SEPARATE question from the gap above: not how long the transcript "
        "kept moving, but whether Claude had finished writing it when it fired "
        "Stop. The SIGN TALLY is the answer; the magnitudes are secondary."
    )
    stop_hook = report["stop_hook_delta_distribution"]
    if stop_hook is None:
        out(STOP_HOOK_NO_SAMPLE_BANNER)
    else:
        out(
            f"SIGN TALLY: count={stop_hook['count']} "
            f"positive={stop_hook['positive']} "
            f"negative={stop_hook['negative']} zero={stop_hook['zero']}"
        )
        out(
            f"magnitudes (secondary): min={stop_hook['min']} "
            f"median={stop_hook['median']} max={stop_hook['max']} ms "
            "-- signed and unclamped; a negative value is a real reading, not "
            "an error"
        )
        if stop_hook["negative"]:
            out(STOP_HOOK_NEGATIVE_BANNER.format(**stop_hook))
            out("  the sample(s) that forbid it, so they can be checked by hand:")
            for item in report["stop_hook_negative_attempts"]:
                out(
                    f"    ordinal={item['global_attempt_ordinal']} "
                    f"attempt {item['attempt_id']} ({item['grade']}): "
                    f"{item['stop_hook_delta_ms']} ms -- Stop preceded the last "
                    "transcript write"
                )
                out(f"      {item['attempt_dir']}")
        else:
            out(STOP_HOOK_POSITIVE_BANNER.format(**stop_hook))
    # Printed in BOTH branches, and never as a zero sample: "the hook was never
    # timed" is a different fact from "the hook and the last write shared a
    # millisecond", and only the second is evidence about ordering.
    if report["attempts_without_computable_stop_hook_delta"]:
        out(
            "UNCOMPUTABLE: "
            f"{report['attempts_without_computable_stop_hook_delta']} successful "
            "attempt(s) produced no stop-hook ordering sample (counted as "
            "uncomputable, NOT as zero), by reason:"
        )
        for reason, count in sorted(
            report["stop_hook_delta_uncomputable_reasons"].items()
        ):
            out(f"    {reason}: {count}")
            out(
                "      "
                + STOP_HOOK_UNCOMPUTABLE_REASONS.get(
                    reason, "no explanation is recorded for this reason"
                )
            )
    else:
        out(
            "every successful attempt published a computable stop-hook ordering "
            "sample (none was silently dropped from the sign tally)"
        )
    out("")
    banner_count = sum(
        count
        for grade in report["grade_order"]
        for source, count in report["grades"][grade]["grade_source_tally"].items()
        if source != "prompt_sha256"
    )
    out("By grade (least -> most transcript structure):")
    if banner_count:
        out(
            f"  !!! {banner_count} attempt(s) below carry a grade label this tool "
            "did NOT establish from prompt content; the rows are marked. A "
            "prompt_suite_index names a position in an argv list, not a prompt !!!"
        )
    for grade in report["grade_order"]:
        info = report["grades"][grade]
        dist = info["gap_distribution"]
        source_suffix = grade_source_suffix(info["grade_source_tally"])
        no_gap = info["attempts_without_computable_gap"]
        no_gap_note = f" no-gap={no_gap}" if no_gap else ""
        # Beside `hash=`, never folded into it: `{'not_applicable': 2}` reads
        # as two attempts cleared and means two attempts unexamined.
        # Silent for a grade with no attempts: `oracle=0/0` on an empty row is
        # noise, and only a row with attempts can have a coverage hole.
        #
        # The denominator is ANSWERS. Grade 01 on the Gate B tree has 8
        # attempts of which 6 produced no reply at all, and the old
        # `oracle=0/8` charged the oracle for six failures it was never given
        # anything to look at. `no-answer=` carries those separately.
        no_answer = info["attempts_with_no_answer"]
        no_answer_note = f" no-answer={no_answer}" if no_answer else ""
        if not info["attempts"]:
            oracle_note = ""
        elif not info["answers"]:
            oracle_note = f" oracle=n/a (no answers){no_answer_note}"
        else:
            oracle_note = (
                f" oracle={info['answers_with_truncation_oracle']}/"
                f"{info['answers']} answers"
                + (" NO-ORACLE" if not info["answers_with_truncation_oracle"] else "")
                + no_answer_note
            )
        if dist is None:
            out(
                f"  {grade}: attempts={info['attempts']} no computable gap"
                f"{no_gap_note} hash={info['hash_tally']}{oracle_note}{source_suffix}"
            )
            for reason, count in sorted(info["gap_uncomputable_reasons"].items()):
                out(f"      {reason}: {count}")
            continue
        single_sample_note = (
            " (single sample: not a real distribution)" if dist["count"] == 1 else ""
        )
        grade_p95 = f"p95={dist['p95']} " if "p95" in dist else ""
        band_note = (
            f" within-noise-band={dist['within_noise_band_attempts']}"
            if dist["within_noise_band_attempts"]
            else ""
        )
        out(
            f"  {grade}: attempts={info['attempts']} count={dist['count']} "
            f"min={dist['min']} median={dist['median']} {grade_p95}"
            f"max={dist['max']} no-late-row={dist['no_late_row_attempts']}"
            f"{band_note} "
            f"late-row={dist['late_row_attempts']}{no_gap_note} "
            f"hash={info['hash_tally']}{oracle_note}{single_sample_note}"
            f"{source_suffix}"
        )
        for reason, count in sorted(info["gap_uncomputable_reasons"].items()):
            out(f"      {reason}: {count}")
        if dist["late_row_attempts"] == 0:
            out(
                "      ABSENCE OF EVIDENCE for this grade: "
                f"{dist['no_late_row_attempts']}/{dist['count']} attempt(s) "
                "at or below the noise band; no measured late row here."
            )
    out("")
    # The exit status, spelled out beside the report that produced it, from the
    # same list `main` returns on. A reader who only has the text must be able
    # to see why the tool failed -- and, when it did not, that the clean exit
    # was a checked claim rather than an omission.
    conditions = failing_conditions(report)
    out("--- VERDICT ---")
    if conditions:
        out(f"exit code 1: {len(conditions)} condition(s) this report cannot clear:")
        for condition in conditions:
            out(f"  - {condition}")
    else:
        out(
            "exit code 0: no hash mismatch, no unparseable attempt, no required "
            "hash absent, and every successful attempt produced a gap sample. "
            "This is a statement about what was CHECKED; read the notes and "
            "absence-of-evidence blocks above for what was merely observed."
        )
    return "\n".join(lines)


# ---- CLI -----------------------------------------------------------------------


def build_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--evidence-root",
        type=Path,
        required=True,
        help="The --evidence-root a live phase0.py probe published attempts into.",
    )
    parser.add_argument(
        "--prompts-dir",
        type=Path,
        default=Path(__file__).resolve().parent / "prompts",
        help="Defaults to tools/phase0/prompts. Sorted filenames define the "
        "grade for each attempt's prompt_suite_index.",
    )
    parser.add_argument(
        "--configured-drain-ms",
        type=int,
        default=None,
        help="Override the CONFIGURED transcript_drain_ms, instead of reading "
        "it from each attempt's own compatibility.transcript_drain_ms. The "
        "EFFECTIVE (required) drain is re-derived against the override rather "
        "than left stale, so the two lines always describe the same build; "
        "headroom is taken against the required drain, never this one.",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Emit the full machine-readable report as JSON instead of text.",
    )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_arg_parser().parse_args(argv)
    try:
        prompt_suite = load_prompt_suite(args.prompts_dir)
        attempt_dirs = discover_attempt_dirs(args.evidence_root)
    except VerifyError as caught:
        print(f"verify_calibration: {caught}", file=sys.stderr)
        return 2
    records = [analyze_attempt(path, prompt_suite) for path in attempt_dirs]
    records.sort(
        key=lambda record: (
            record["global_attempt_ordinal"] is None,
            record["global_attempt_ordinal"] or 0,
            record["attempt_dir"],
        )
    )
    report = build_report(
        records, prompt_suite, configured_drain_override=args.configured_drain_ms
    )
    conditions = failing_conditions(report)
    if args.json:
        # Bootstrapped here rather than at the top of the file, for the reason
        # `tools/promotion/measure_transcript_drain.py` gives at its own import:
        # this file is cited by line from `docs/current-state.md` and
        # `docs/instrument-fix-plan.md` as high as line 857, and an import above
        # those lines would move every sentence off the line it names. The
        # `--json` receipt is committed as `evidence/gate-b-drain-calibration.
        # json` and names the campaign tree it verified, so it is rendered
        # whole at the one point it becomes bytes.
        sys.path.insert(0, str(REPO_ROOT / "tools" / "evidence_common"))
        import portable_paths  # noqa: E402 -- tools/evidence_common, above

        print(
            json.dumps(
                portable_paths.render_document(
                    {**report, "attempts": records, "failing_conditions": conditions}
                ),
                indent=2,
                sort_keys=True,
            )
        )
    else:
        print(render_report(report))
    # Exactly the list rendered under VERDICT, so the exit status and the text
    # beside it can never disagree.
    return 1 if conditions else 0


if __name__ == "__main__":
    raise SystemExit(main())
