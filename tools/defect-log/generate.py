#!/usr/bin/env python3
"""Generate `docs/defect-log.md` from the pre-squash commit range.

Deterministic and re-runnable: the same range and the same class table produce
the same bytes. Three mechanical substitutions are applied to every message and
to nothing else, and each is declared in the document the run writes:

1. machine-specific identifiers -> structure-preserving placeholders, derived
   by `tools/defect-log/machine.py` and never written down here;
2. commit hashes that this repository's history holds -> this document's own
   entry ordinals, so the squash does not turn them into dead tokens;
3. line numbers dropped from citations of a linted Path B document, whose set
   is read out of the same table `path_b_doc_citations.rs` reads.

Hand-editing evidence is forbidden in this repository, and it has already
caught a hand-written receipt naming whatever HEAD happened to be when it was
saved. A declared, deterministic, uniformly-applied substitution is different
in kind precisely because the transformation is committed and can be re-run,
which is what this file is for.

    python3 tools/defect-log/generate.py [range]

AFTER THE SQUASH, THE DEFAULT RANGE IS GONE AND THE ARGUMENT IS HOW THIS IS
RE-RUN. The history this archive is about stops existing in the published
repository the moment it is squashed -- that is the event the archive exists
because of -- so `origin/main..HEAD` then resolves to the one commit that
replaced it and this refuses rather than writing a one-entry log. Re-running it
means naming a range that still holds those commits, which is what the
pre-squash tip is preserved for: `python3 tools/defect-log/generate.py
origin/main..pre-squash-<sha>` reproduces this document byte for byte. A reader
who does not have that tip cannot re-run it and is not being asked to take the
result on trust either: every rule below is stated in the document itself, and
`tools/gate-a/tests/test_redaction.py` checks the artefact rather than the run.

`tools/gate-a/tests/test_redaction.py` is the check that a run of it left
nothing of this machine behind.
"""

import collections
import re
import subprocess
import sys
import textwrap
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import machine  # noqa: E402

REPO = machine.WORKSPACE
DEFAULT_RANGE = "origin/main..HEAD"
ARCHIVE = "docs/defect-log.md"


def git(*args):
    return subprocess.run(
        ["git", *args], cwd=REPO, capture_output=True, text=True, check=True
    ).stdout


# --- the redaction map -------------------------------------------------
# Asked of the running machine, longest needle first so that a shorter one
# cannot half-substitute a longer one. Nothing is spelled out here.
REDACTIONS = machine.substitutions()

HEX = re.compile(r"\b[0-9a-f]{7,40}\b")

# --- rule 3: line citations of a linted Path B document ----------------
# Derived from the same table `crates/service/tests/path_b_doc_citations.rs`
# reads its own set out of, and from the same status vocabulary, so a
# document promoted or demoted there moves this too.
READING_ORDER_DOCUMENT = "docs/path-b.md"
READING_ORDER_HEADING = "## 0.0 THE PATH B READING ORDER"
LINTED_STATUSES = {"CURRENT", "DATED RECEIPT"}


def linted_documents():
    source = (REPO / READING_ORDER_DOCUMENT).read_text()
    table = source.split(READING_ORDER_HEADING, 1)[1].split("\n---", 1)[0]
    documents = set()
    for line in table.splitlines():
        cells = [c.strip() for c in line.split("|")]
        if len(cells) < 6 or not cells[1].isdigit():
            continue
        name = re.findall(r"`([^`]+)`", cells[2])[0]
        if cells[3] in LINTED_STATUSES:
            documents.add(name)
    assert len(documents) >= 4, documents
    return documents


CITATION = re.compile(r"([\w./-]+\.\w+):\d+(?:[-–]\d+)?")


def names_a_linted_document(cited, documents):
    """The checker's own resolution rule: cited path is a component suffix.

    A LONGER path is a different file -- `tests/conformance/v1/README.md` is
    not the reading order's `README.md` -- which is what keeps this rule off
    the three other `README.md` files in the tree.
    """
    return any(("/" + document).endswith("/" + cited) for document in documents)


def build_hash_map(shas):
    """Map every in-range commit hash to its ordinal in this log.

    A token is rewritten only if git resolves it AND the commit it
    resolves to is one of the commits catalogued here. sha256 digests,
    upstream hashes and dead references therefore never match.
    """
    order = {sha: i + 1 for i, sha in enumerate(shas)}
    cache = {}

    def resolve(token):
        if token in cache:
            return cache[token]
        r = subprocess.run(
            ["git", "rev-parse", "--verify", "--quiet", token + "^{commit}"],
            cwd=REPO,
            capture_output=True,
            text=True,
        )
        full = r.stdout.strip() if r.returncode == 0 else None
        cache[token] = order.get(full)
        return cache[token]

    return resolve


LINTED = linted_documents()


def drop_linted_line(m):
    return m.group(1) if names_a_linted_document(m.group(1), LINTED) else m.group(0)


def scrub(text, resolve):
    for needle, placeholder in REDACTIONS:
        text = text.replace(needle, placeholder)

    def sub_hash(m):
        n = resolve(m.group(0))
        return f"<c{n}>" if n else m.group(0)

    text = HEX.sub(sub_hash, text)
    return CITATION.sub(drop_linted_line, text)


CLASSES = [
    (
        "A",
        "The house bug class",
        "house-bug-class",
        "A guard, comment, document, test name or receipt whose message promises "
        "more than its predicate tests; or a check whose set-of-things-to-check is "
        "hand-written where it could be derived.",
        "This is the class the repository names and counts itself. It is the "
        "largest single group and it recurs in the instruments built to find it: "
        "a citation grader that skipped 70 of the 132 claims its heading called "
        '"every", a mutation gate whose refusal named two compiler settings its '
        "predicate never read, a survivor register keyed on a line number that had "
        "moved for 100 of its rows. The fix is almost always the same edit: "
        "replace the list with the derivation, and assert the derivation is not "
        "empty.",
    ),
    (
        "B",
        "Composer, screen and prompt delivery",
        "composer-screen",
        "Everything that follows from typing into a real Claude Code TUI: mode "
        "prefixes, normalisation, geometry gates, render proofs, modal screens.",
        "pmux drives a terminal it does not own, so every input and completion "
        "gate is a claim about geometry and text handling that Claude can change "
        "without notice. This group holds the only defects in the log that let a "
        "caller's bytes do something other than become a prompt -- a leading `!` "
        "that ran a shell command on the host, a BOM-prefixed `/clear` -- and the "
        "long tail of ordinary caller inputs that each destroyed a pooled "
        "instance because the composer rewrote them.",
    ),
    (
        "E",
        "Completion authority and the transcript",
        "completion-authority",
        "When a turn is over: the drain, the end-of-turn marker, schema drift, "
        "session rotation, and the rows that mean a turn is still running.",
        "The founding decision of the product is that Claude's own JSONL "
        "transcript is the sole authority for whether a turn finished. Almost "
        "every defect here is the same shape from a different side: something "
        "that looks terminal is not, and committing on it returns a truncated "
        "answer -- the one failure mode the architecture exists to make "
        "unrepresentable. The recurring discipline is that a mistake must cost "
        "unavailability, never wrongness.",
    ),
    (
        "C",
        "Pool, daemon and store lifecycle",
        "pool-lifecycle",
        "Instance states, teardown order, transports, signals, concurrency, "
        "health, and durable stores.",
        "Path B pools fifteen stateless engines and recycles them with `/clear`, "
        "so almost every defect in this group is about what a second task sees "
        "while the first is between locks, or about a teardown arm that could not "
        "tell two situations apart. The other half is the daemon around it: a "
        "poisoned transport that was daemon-wide, a SIGTERM window whose whole "
        "warm mint ran at the kernel's disposition, a health surface that "
        "reported `healthy` through four real failures.",
    ),
    (
        "D",
        "Isolation and containment",
        "isolation",
        "What a launched cell may reach: environment inheritance, config roots, "
        "MCP, containment predicates, credentials, the agent resource.",
        "A stateless cell is only stateless if nothing crosses into it and "
        "nothing escapes. The most expensive defects here were inherited "
        "environment variables, and the most instructive is a retraction: a claim "
        "that no MCP server is spawned was measured correctly by a "
        "descendant-process inventory, and the sentence built on it -- that a flag "
        "was no longer load-bearing -- was about an HTTP endpoint that inventory "
        "structurally cannot see.",
    ),
    (
        "F",
        "Receipts, evidence and budgets",
        "receipts-evidence",
        "The attempt ledger, gate receipts, promotion evidence, the mutation "
        "survivor register, and retracted measurements.",
        "This project spends an irreplaceable budget of real model attempts, so "
        "what it can prove is bounded by what it wrote down. The defects are "
        "correspondingly about provenance: a receipt that named whatever HEAD "
        "happened to be when it was saved, a budget the file's own recount "
        "command contradicted by 38 ordinals, two numbers for one quantity and "
        "neither with a receipt. Retracted claims are kept struck rather than "
        "deleted, because the reason a false measurement was believed is the "
        "durable finding.",
    ),
    (
        "G",
        "Instruments, gates and tooling",
        "tooling",
        "The Gate A/B drivers, verifiers, harnesses, citation graders, mutation "
        "machinery, handoffs and upstream reports.",
        "An instrument defect is worse than a product defect: it publishes a "
        "wrong number someone believes, or blames the product for something it "
        "did not do. Two verifier defects here would have printed a loud and "
        'entirely false "pmux dropped a byte" on a clean run; a per-binary '
        "harness reported that every one of its targets passed while enumerating "
        "zero of them. Read this group as the reason the other six can be "
        "trusted at all.",
    ),
]

# --- what the preamble says about itself ------------------------------
# The SELECTION of terms below is editorial: these are the words the subjects
# recur on, and choosing them is a reading. Every NUMBER attached to them is
# measured at generation time from the messages this run actually catalogued,
# under the rule the document states, because a count typed into prose beside a
# growing catalogue is this log's own section A -- and it had already gone
# stale, by six messages, before anyone re-ran the generator.
VOCABULARY = (
    "gate",
    "pool",
    "prompt",
    "receipt",
    "agent",
    "drain",
    "evidence",
    "refusal",
    "instance",
    "daemon",
    "transcript",
    "citation",
    "screen",
    "composer",
    "mutation",
    "ordinal",
    "ledger",
    "MCP",
    "/clear",
    "isolation",
)

BUG_CLASS = "bug class"
BUG_CLASS_HEADING = r"the bug class, instance"


def occurrences(text, term):
    """Counted case-insensitively wherever a word starts.

    So `gate` counts `gates`, `gated` and `Gate A`, and `MCP` and `/clear`
    count as themselves. Stated because a reader checking these numbers against
    the document needs the rule that produced them, and `grep -c` is not it.
    """
    return len(re.findall(r"(?<!\w)" + re.escape(term), text, re.I))


def measured(entries, raw):
    """Every number the preamble states, taken from what this run catalogued."""
    messages = [f"{entry['subject']}\n{entry['body']}" for entry in entries]
    quoted = "\n".join(messages)
    dropped, kept = 0, set()
    for text in raw:
        for citation in CITATION.finditer(text):
            if names_a_linted_document(citation.group(1), LINTED):
                dropped += 1
            else:
                kept.add(citation.group(0))
    # The example the map's own paragraph offers has to be an ordinal some
    # message really carries. The one that used to be printed there was not:
    # `<c103>` appeared exactly once in the whole document, on the line
    # offering it as an example of what a message body looks like. So it is
    # taken from the messages -- the ordinal they cite most often, smallest
    # first where they tie, which makes it deterministic as well as real.
    cited = collections.Counter(re.findall(r"<c(\d+)>", quoted))
    ordinals = sorted(cited, key=lambda n: (-cited[n], int(n)))
    return {
        "n": len(entries),
        "vocabulary": sorted(
            ((term, occurrences(quoted, term)) for term in VOCABULARY),
            key=lambda pair: (-pair[1], pair[0]),
        ),
        "bug_class_messages": sum(1 for m in messages if BUG_CLASS in m.lower()),
        "bug_class_lines": sum(
            1 for line in quoted.splitlines() if BUG_CLASS in line.lower()
        ),
        "numbered_instances": sum(
            1 for m in messages if re.search(BUG_CLASS_HEADING, m, re.I)
        ),
        "dropped_citations": dropped,
        "kept_citations": len(kept),
        "example_ordinal": ordinals[0],
    }


WIDTH = 78


def wrapped(paragraph):
    """One paragraph, re-flowed to the width the rest of this document uses.

    The numbers are interpolated, so their widths are not known when the prose
    around them is written; without this a three-digit count and a two-digit
    one produce differently ragged files from the same source.
    """
    return textwrap.fill(
        " ".join(paragraph.split()),
        width=WIDTH,
        break_long_words=False,
        break_on_hyphens=False,
    )


def preamble(entries, raw):
    f = measured(entries, raw)
    n = f["n"]
    example = f["example_ordinal"]
    ordinals_rule = wrapped(f"""
**2. Commit hashes were replaced with this document's own ordinals.** The squash
destroys every hash these messages cite. A token was rewritten only if git
resolves it *and* the commit it resolves to is one of the {n} catalogued here;
`sha256` digests, upstream rmux hashes and references that were already dead
resolve to nothing and were left exactly as written. So `<c{example}>` in a
message body means entry {example} of this log, and no replacement hash has been
invented.
""")
    citation_rule = wrapped(f"""
**3. Line numbers were dropped from citations of a linted Path B document.**
{f["dropped_citations"]} sites, all in messages that are themselves about such a
citation having rotted. `crates/service/tests/path_b_doc_citations.rs` fails the
build if anything in this repository cites one of those documents by line, for
the reason it gives: *a section survives insertion above it; a line number does
not*, and this repository has already had a stale line citation become a live
isolation leak. An archive that reproduced those citations would arm that guard
against a file nobody can edit. The document set and the suffix-resolution rule
are read out of `docs/path-b.md` §0.0, the same table the guard reads, so a
document promoted or demoted there moves this too. The path is kept and only the
`:NNN` is dropped -- the same rule as for hashes, drop the reference rather than
invent a replacement. The other {f["kept_citations"]} distinct `path:line`
citations in these messages, all of them into source files, are untouched.
""")
    bug_class_reading = wrapped(f"""
**First, the repository names one class explicitly and machine-checks the
count.** {f["bug_class_messages"]} of the {n} messages use the phrase "bug
class", over {f["bug_class_lines"]} lines, and {f["numbered_instances"]} of them
number the instance in words under a heading reading `THE BUG CLASS, instance
...`. The counter is not prose: the test
`test_every_statement_of_the_bug_class_counter_spells_the_same_ordinal` exists
in the tree and holds four Rust sites and the last such heading to one number.
That class is section A below, quoted from the tree:
""")
    vocabulary = ", ".join(f"`{term}` {count}" for term, count in f["vocabulary"])
    vocabulary_reading = wrapped(f"""
**Second, the remaining subjects partition on their own recurring vocabulary.**
Which terms to read is editorial; the counts are not. Over the {n} messages
quoted below, each term counted case-insensitively wherever a word starts, so
`gate` counts `gates` and `Gate A`: {vocabulary}. Those terms cluster into six
subjects that do not overlap in what a reader would go looking for -- what a
caller types, when a turn is over, what the pool holds, what a cell can reach,
what was written down, and what does the measuring. They are sections B
through G.
""")
    filing_rule = wrapped(f"""
Every entry appears exactly once, filed under the class of the defect its
**subject** names first. Most subjects here name two defects, joined by "and",
and they are frequently in two different classes; only the first one decides
where the entry is filed, and no cross-listing is offered, because assigning a
second class to {n} subjects would be a hand-written set of exactly the kind
section A is about. Use the index and search the text.
""")
    chronology = wrapped(f"""
Within each section, entries are in commit order, oldest first. The index below
carries all {n} in commit order regardless of class, so the log can still be
read as a chronology.
""")
    return f"""# The defect log

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

{ordinals_rule}

{citation_rule}

## How the grouping was derived

The messages classify themselves twice over, and both readings were used.

{bug_class_reading}

> A guard, comment, document, test name or receipt whose message promises more
> than its predicate tests; or a check whose set-of-things-to-check is
> hand-written where it could be derived.

{vocabulary_reading}

{filing_rule}

{chronology}
"""


def archive_was_born_at(commits):
    """The commit that first added the file this generator writes.

    The tail rule below needs a boundary and the catalogue's own last entry is
    the wrong one, which took a second defect to see. It moves every time the
    catalogue grows, and the first time it moves past the archive's own birth
    the archive's files stop being "born after" it, so a commit that does
    nothing but regenerate the log is refused and the log can never be brought
    forward again. The archive's birth does not move. It is asked of git, and
    of the one path this file writes, so renaming the archive follows it here
    rather than needing an edit.
    """
    span = f"{commits[0]}^..{commits[-1]}"
    born = git("log", "--format=%H", "--diff-filter=A", span, "--", ARCHIVE).split()
    assert born, f"nothing in {span} added {ARCHIVE}"
    return born[-1]


def carries_only_files_the_archive_brought(sha, birth):
    """Does this commit touch nothing that predates the archive itself?

    The archive cannot catalogue the commit that lands it, and the moment it
    lands, the range is one longer than the class table. Pinning the endpoint
    to a hash would not survive the squash the archive exists because of, and
    growing the table by a row saying "here is the archive" would file a commit
    that names no defect. So the tail is DERIVED: a commit after the catalogue
    is admitted as the archive's own maintenance only if every path it touches
    was born with the archive or later. Nothing here lists the archive's files,
    so a file added to it later needs no edit, and one product file touched in
    the same commit refuses.
    """
    files = git("show", "--name-only", "--format=", sha).split("\n")
    for path in (path for path in files if path.strip()):
        born = git(
            "log", "--format=%H", "--diff-filter=A", f"{birth}^..{sha}", "--", path
        ).split()
        if not born:
            return False
    return True


def catalogued(shas, wanted):
    """The commits this log is about: the range, less the archive's own tail."""
    birth = archive_was_born_at(shas)
    tail = list(shas)
    while len(tail) > wanted and carries_only_files_the_archive_brought(
        tail[-1], birth
    ):
        tail.pop()
    return tail


def main(argv=None):
    span = (argv or sys.argv[1:] or [DEFAULT_RANGE])[0]
    shas = git("log", "--reverse", "--format=%H", span).split()
    classes = {}
    table = (Path(__file__).resolve().parent / "classes.txt").read_text()
    for line in table.splitlines():
        if line.strip():
            number, letter = line.split()
            classes[int(number)] = letter
    # A range SHORTER than the catalogue is the squash, not a miscount, and it
    # is the one failure this tool is guaranteed to meet: the history it is
    # about is deleted by the event it exists because of. Saying so is the
    # difference between a tool that looks broken and one that tells the reader
    # what to run instead.
    assert len(shas) >= len(classes), (
        f"{span} holds {len(shas)} commits against {len(classes)} catalogued. If "
        "the branch has been squashed, that history is only in the preserved "
        "pre-squash tip: name a range that still reaches it."
    )
    # The class table is the one hand-made thing in this generator, so it is
    # held to the range rather than trusted: a commit with no class, or a
    # class no section defines, refuses rather than being filed somewhere.
    shas = catalogued(shas, len(classes))
    assert len(shas) == len(classes), (
        f"{len(shas)} commits in {span} against {len(classes)} classified"
    )
    assert sorted(classes) == list(range(1, len(shas) + 1)), "classes.txt is not 1..N"
    defined = {letter for letter, *_ in CLASSES}
    unknown = sorted(set(classes.values()) - defined)
    assert not unknown, f"classes.txt uses {unknown}, which no section defines"
    empty = sorted(defined - set(classes.values()))
    assert not empty, f"{empty} are sections nothing is filed under"
    resolve = build_hash_map(shas)

    entries, raw = [], []
    for i, sha in enumerate(shas, 1):
        date = git("log", "-1", "--format=%ad", "--date=short", sha).strip()
        subject = git("log", "-1", "--format=%s", sha).strip()
        body = git("log", "-1", "--format=%b", sha).rstrip("\n")
        raw.append(f"{subject}\n{body}")
        entries.append(
            {
                "n": i,
                "date": date,
                "subject": scrub(subject, resolve),
                "body": scrub(body, resolve),
                "class": classes[i],
            }
        )

    out = [preamble(entries, raw), ""]

    out.append("## Index, in commit order\n")
    out.append("| # | date | class | subject |")
    out.append("| --- | --- | --- | --- |")
    slugs = {c[0]: c[2] for c in CLASSES}
    for e in entries:
        subj = e["subject"].replace("|", "\\|")
        out.append(
            f"| {e['n']} | {e['date']} | [{e['class']}](#{e['class'].lower()}-"
            f"{slugs[e['class']]}) | [{subj}](#c{e['n']}) |"
        )
    out.append("")

    for letter, name, slug, definition, why in CLASSES:
        members = [e for e in entries if e["class"] == letter]
        out.append("---\n")
        out.append(f"## {letter}. {name}")
        out.append("")
        out.append(f"**{definition}**")
        out.append("")
        out.append(why)
        out.append("")
        out.append(f"{len(members)} entries.")
        out.append("")
        for e in members:
            out.append(f'<a id="c{e["n"]}"></a>')
            out.append("")
            out.append(f"### {e['n']}. {e['subject']}")
            out.append("")
            out.append(f"*{e['date']}*")
            out.append("")
            if e["body"].strip():
                out.append("`````text")
                out.append(e["body"])
                out.append("`````")
            else:
                out.append("*(no body)*")
            out.append("")

    text = "\n".join(out).rstrip() + "\n"
    # The line below says "bytes", so the write is the one that makes it true.
    # `len(text)` counts CHARACTERS and `write_text` encodes with whatever codec
    # the locale names: at this head the two differ by 247 -- 598,004 characters
    # against a 598,251-byte file -- because the messages this document quotes
    # verbatim are not all ASCII. Encoding once and writing those bytes makes
    # the count the file's own size on any host rather than on a UTF-8 one.
    data = text.encode("utf-8")
    (REPO / "docs" / "defect-log.md").write_bytes(data)
    print(f"wrote {len(data)} bytes, {text.count(chr(10))} lines")


if __name__ == "__main__":
    main()
