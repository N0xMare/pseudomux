//! Every citation OF a Path B document refused, and every citation INSIDE one
//! checked against the thing it names.
//!
//! Both halves of that sentence are now total over their own set, and saying
//! how they got there is the point. Rule 1 sees every citation of a linted
//! document, in any spelling, anywhere in the repository. Rule 2 grades every
//! citation a linted document contains -- not the subset it happens to be able
//! to grade, which is what it used to do and what the word "every" over it was
//! not true of.
//!
//! The difference is one decision. A citation whose sentence names nothing the
//! cited file holds used to be SKIPPED, and 70 of 132 were: a table row that
//! gives a path and a number and an English paraphrase has nothing a predicate
//! can hold to, so the grader passed over it in silence and the document said
//! "every" anyway. Skipping was the wrong answer, for the reason rule 3 already
//! gives about the abbreviated form: **a citation that escapes the checker is
//! worth less than no citation at all**, because a reader takes a `path:line`
//! in this tree to be one the build verifies. So an ungradable citation is now
//! REFUSED, with a message naming what to add, and the fix is to write the
//! sentence so it names what the line holds. That is not a burden the rule
//! invents -- it is the rule §0.4 of `docs/path-b.md` already states, applied
//! without the exemption that made it cheap.
//!
//! What counts as naming is derived from the citation, not from a shape this
//! file believes code has:
//!
//! * an identifier the sentence marks as a quotation and the cited file holds,
//!   distinctive enough that landing on it says something (see
//!   [`MAX_ANCHOR_OCCURRENCES`]), or
//! * any marked span of the sentence that occurs in the cited file verbatim,
//!   once both sides are read past the ways the same words are spelled
//!   differently in a document and in source -- comment markers, line wrapping,
//!   markdown emphasis (see [`reduced`]).
//!
//! and "marks as a quotation" is itself four things, not one: backticks,
//! straight quotes, typographic quotes and emphasis (see [`QUOTING_MARKS`]).
//!
//! The second was added because the first could not grade a citation of a
//! *comment*, and a MEASURED comment is what half of these documents cite. A
//! sentence that quotes the line it cites is the most direct anchor there is,
//! and it was the one the grader could not see.
//!
//! # The defect this exists for
//!
//! Line 187 of `docs/path-b.md` was the MCP-isolation row of §2.2 -- the flag
//! is not spelled here, because `stateless.rs` gives four files the right to
//! name it and this is not one. Four files in the code tree cited that line by
//! number for the retraction it carried. `20bf20f` -- the commit that fixed the defect the retraction had
//! caused -- inserted rows above it, and line 187 became a paragraph about the
//! replace-mode system prompt. Every one of those four citations then pointed
//! at an unrelated claim, and one of the four was written by that same commit.
//! The sentence still promised a measurement about MCP; the line no longer held
//! one.
//!
//! Nothing below spells the `<document>:<line>` shape it refuses -- not even to
//! quote the defect. An exemption for "the one place that has to name it" is
//! how a scan acquires its first hole, and this one does not need it.
//!
//! That is the house bug class aimed at a document: a citation whose text
//! promises more than the line it resolves to. It matters more here than
//! elsewhere because this repository has already had a false sentence in
//! `docs/path-b.md` become a live isolation leak -- a retraction was believed,
//! the flag stopped being passed, and a minified cell reached the operator's
//! account connector list over HTTP.
//!
//! # The rules, and why none of them is a number anybody maintains
//!
//! 1. **Nothing may cite a Path B document by line -- not a code tree, and not
//!    another document.** A section survives insertion above it; a line number
//!    does not. A `§N.M` a file does name must be a heading the document really
//!    has. The scanned set is the whole repository, minus build output and
//!    minus `vendor`, because the half of `docs/` that is not itself a Path B
//!    document was where these citations mostly lived: 37 line citations of
//!    `docs/path-b.md` sat in documents the old scan never opened, and the
//!    §0.4 repair before this one was written to be line-count neutral to
//!    avoid disturbing them. A rule that makes the document it protects
//!    un-editable is not protecting it.
//! 2. **Inside a Path B document, every `path:line` citation must land on a
//!    line holding something the sentence names.** Cite the thing you name.
//!    Total: a citation this cannot grade is an offence, not a skip.
//! 3. **A citation must carry its own path**, so the grader can see it, and
//!    enough of the path to resolve to one file, so the grader can see which.
//!
//! Rule 2's predicate is not run over the code tree, and that is a scope
//! decision this file states rather than hides. It WAS run, once, at the commit
//! that wrote this: 55 of the `path:line` citations in `.rs`, `.py` and `.sh`
//! sources are gradable and **38 of them do not land on what they name**. The
//! 23 whose anchor sat in exactly one place were repaired in that commit; the
//! rest need either judgment about which of several candidate lines a comment
//! meant, or a grader that matches anchors to citations PAIRWISE within a line
//! -- ``InvariantViolation` (`instance.rs:186`), `InstanceClass`
//! (`class.rs:258`)` is two claims, and pooling their anchors reports the first
//! against the second's identifier. That change was written, measured, and
//! reverted: it cost seven correct citations in the linted documents and bought
//! three here. Turning this on is a defect list of 38, mostly Path A, and it
//! belongs to whoever owns that list -- not to a rule that would ship red.
//!
//! Nothing here is a list of citations. The document set is read out of
//! `docs/path-b.md` §0.0, the anchor is read out of the prose, and the line is
//! read out of the file. A since-deleted Phase 0 verifier stated the same
//! rule for the numbers a tool prints -- *"a citation nobody re-measures has
//! already rotted"* -- and resolved them from anchors at import. A markdown
//! file cannot do that to itself, so the check is external and the anchor is
//! what the prose was already naming.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The statuses `docs/path-b.md` §0.0 publishes, and whether a row carrying one
/// is linted here.
///
/// `PARTIAL` marks a normative document that contains Path B material and a
/// great deal that is not; its citations are out of this test's scope for
/// reasons of scope and not of confidence, which §0.4 says in the document
/// itself.
const READING_ORDER_STATUSES: [(&str, bool); 3] = [
    ("CURRENT", true),
    ("DATED RECEIPT", true),
    ("PARTIAL", false),
];

/// Directories the scan does not descend into.
///
/// Build output, tool caches, and `vendor` -- vendored third-party source whose
/// line numbers this repository does not maintain and whose comments cite the
/// upstream project's own tree. Everything else in the workspace is scanned,
/// including `docs/`, `evidence/`, `scripts/` and the root files: the set is
/// what is left after this list rather than a list of trees to visit, because
/// a list of trees to visit is how `docs/` came to be exempt from a rule about
/// documents.
const SCAN_SKIPPED_DIRECTORIES: [&str; 9] = [
    ".git",
    ".context",
    ".pseudomux",
    ".ruff_cache",
    "__pycache__",
    "dist",
    "node_modules",
    "target",
    "vendor",
];

/// The extensions a `path:line` citation can name, in one place.
///
/// Two copies of this list used to disagree: the citation scanner knew seven
/// extensions and the path-shaped-span filter knew eight, so a `.tsx` citation
/// was invisible to the grader and visible to the filter that exists to keep
/// paths out of the anchor set.
const CITED_EXTENSIONS: [&str; 8] = [".rs", ".py", ".md", ".sh", ".toml", ".json", ".ts", ".tsx"];

/// The document whose §0.0 table names the Path B documents.
///
/// The one path this file spells. Everything else -- which documents are Path B
/// documents, what status each carries, which are linted -- is read out of it.
const READING_ORDER_DOCUMENT: &str = "docs/path-b.md";

/// The heading §0.0's table lives under, named so an edit that deletes the
/// table fails loudly instead of silently linting nothing.
const READING_ORDER_HEADING: &str = "## 0.0 THE PATH B READING ORDER";

/// The shortest reduced span that grades a citation on its own.
///
/// A quoted phrase this long is a claim about the line; anything shorter is a
/// word or two that a file of this size holds by accident. Identifiers are not
/// held to it -- they carry their own distinctness through
/// [`gradable_identifier`].
const MINIMUM_PHRASE_ANCHOR: usize = 16;

/// How many times the cited file may hold an anchor before naming it stops
/// being a claim about a LINE.
///
/// `UnexpectedTypedPrompt` occurs twice in `engine.rs` and a citation that
/// misses both is rotted. `Claude` occurs hundreds of times in `native.rs`: a
/// citation graded against that has been graded against a word the file is
/// written in, not against a line, and reporting the miss as rot is how a
/// checker teaches its readers to ignore it.
///
/// Thirty-two, and the first attempt at eight is why the number is written
/// down rather than guessed at: a declaration and its callers put
/// `validate_prompt` in `driver_io.rs` eleven times and
/// `TestedCompatibilityProfile` in `compatibility.rs` twelve, and a bound of
/// eight called four correct citations ungradable and demanded they be
/// rewritten. A landmark is used by the code around it; a word is used by every
/// line.
const MAX_ANCHOR_OCCURRENCES: usize = 32;

/// How many lines a quoted phrase may be spelled across when this test reports
/// where it really is. A hard-wrapped markdown paragraph and a `///` block wrap
/// a sentence over two or three lines; four is one more than that.
const PHRASE_WRAP_LINES: usize = 4;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root must resolve")
}

fn read(root: &Path, relative: &str) -> String {
    std::fs::read_to_string(root.join(relative))
        .unwrap_or_else(|error| panic!("could not read {relative}: {error}"))
}

fn read_lossy(path: &Path) -> String {
    String::from_utf8_lossy(
        &std::fs::read(path)
            .unwrap_or_else(|error| panic!("could not read {}: {error}", path.display())),
    )
    .into_owned()
}

/// The backtick-quoted spans of `line`, in order.
fn backticked(line: &str) -> Vec<String> {
    let mut spans = Vec::new();
    let mut rest = line;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        match after.find('`') {
            Some(close) => {
                spans.push(after[..close].to_owned());
                rest = &after[close + 1..];
            }
            None => break,
        }
    }
    spans
}

/// Every way these documents mark a span as somebody else's words.
///
/// Backticks, straight quotes, typographic quotes, and markdown emphasis. All
/// four, because the grader used to know one of them and the documents use all
/// four for the same job: `crates/service/src/pool/config.rs` records a wave as
/// `**703, 723, 727, 730, 748, 749, 756 ms**, median 730` and
/// `docs/2.1.226-acceptance.md` cites that line by repeating the numbers in
/// bold. That is a quotation of the cited line -- the most direct anchor a
/// citation can carry -- and a grader that reads only backticks calls it
/// "names nothing".
///
/// Emphasis can only ADD anchors, never remove one, and an anchor still has to
/// occur in the cited file: a document's own bolded prose is dropped by the
/// same filter that drops `retractedMessageUuids`.
const QUOTING_MARKS: [(&str, &str); 5] = [
    ("`", "`"),
    ("\"", "\""),
    ("\u{201c}", "\u{201d}"),
    ("**", "**"),
    ("*", "*"),
];

/// The spans of `line` marked as a quotation, in order, however they were
/// marked.
fn quoted_spans(line: &str) -> Vec<String> {
    let mut spans = Vec::new();
    for (open, close) in QUOTING_MARKS {
        let mut rest = line;
        while let Some(start) = rest.find(open) {
            let after = &rest[start + open.len()..];
            match after.find(close) {
                Some(end) => {
                    spans.push(after[..end].to_owned());
                    rest = &after[end + close.len()..];
                }
                None => break,
            }
        }
    }
    spans
}

/// The form both sides of a prose comparison are read in.
///
/// A document and the source it cites spell the same claim differently:
/// `///` and `#` and `|` and `>` open the line, markdown bolds a word, a
/// sentence wraps in the middle, an identifier joins its words with `_` where
/// the prose uses a space, and one of the two writes `--` where the other
/// writes an em dash. None of that is part of the claim, and a comparison that
/// respects all of it can only grade citations whose author copied bytes.
///
/// So: keep the alphanumerics, lowercase them, and make every run of anything
/// else exactly one space. `MAX_ASSERT_EMPTY_USER_ROWS = 2` and *"max assert
/// empty user rows = 2"* reduce to the same string, which is the point.
fn reduced(text: &str) -> String {
    let mut out = String::new();
    for character in text.chars() {
        if character.is_ascii_alphanumeric() {
            out.push(character.to_ascii_lowercase());
        } else if !out.ends_with(' ') {
            out.push(' ');
        }
    }
    out.trim().to_owned()
}

/// The Path B documents and their statuses, DERIVED from §0.0's table.
///
/// A row is `| n | `document` | STATUS | prose |`. The document is the row's
/// first backticked span; the status is the third cell, matched against
/// [`READING_ORDER_STATUSES`] so a new spelling cannot enter the table without
/// this test being taught what it means.
fn reading_order(root: &Path) -> BTreeMap<String, String> {
    let source = read(root, READING_ORDER_DOCUMENT);
    let (_, after) = source.split_once(READING_ORDER_HEADING).unwrap_or_else(|| {
        panic!("{READING_ORDER_DOCUMENT} no longer publishes {READING_ORDER_HEADING:?}")
    });
    let table = after
        .split_once("\n---")
        .map(|(table, _)| table)
        .unwrap_or(after);
    let mut rows = BTreeMap::new();
    for line in table.lines() {
        let cells = line.split('|').map(str::trim).collect::<Vec<_>>();
        // `| n | doc | status | prose |` splits to a leading and a trailing
        // empty cell, so a data row has at least six.
        if cells.len() < 6 || cells[1].parse::<u32>().is_err() {
            continue;
        }
        let document = backticked(cells[2])
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("§0.0 row {:?} names no document", cells[1]));
        let status = cells[3].to_owned();
        assert!(
            READING_ORDER_STATUSES
                .iter()
                .any(|(known, _)| *known == status),
            "§0.0 row {} gives {document} the status {status:?}, which is not one of {:?}",
            cells[1],
            READING_ORDER_STATUSES.map(|(known, _)| known),
        );
        assert!(
            root.join(&document).is_file(),
            "§0.0 row {} names {document}, which does not exist",
            cells[1],
        );
        assert!(
            rows.insert(document.clone(), status).is_none(),
            "§0.0 names {document} twice",
        );
    }
    assert!(
        rows.len() >= 4,
        "§0.0's table parsed to {} rows, which is not a reading order",
        rows.len(),
    );
    assert!(
        rows.contains_key(READING_ORDER_DOCUMENT),
        "§0.0 does not list the document it is in",
    );
    rows
}

/// The documents §0.0 marks as linted here.
fn linted_documents(order: &BTreeMap<String, String>) -> BTreeSet<String> {
    let linted = order
        .iter()
        .filter(|(_, status)| {
            READING_ORDER_STATUSES
                .iter()
                .any(|(known, linted)| known == status && *linted)
        })
        .map(|(document, _)| document.clone())
        .collect::<BTreeSet<_>>();
    assert!(
        linted.len() >= 4,
        "§0.0 marks only {} documents linted, so this test is nearly vacuous",
        linted.len(),
    );
    linted
}

fn walk(directory: &Path, files: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => panic!("could not read {}: {error}", directory.display()),
    };
    for entry in entries {
        let path = entry
            .expect("workspace directory entry must be readable")
            .path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if SCAN_SKIPPED_DIRECTORIES.contains(&name) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&path)
            .unwrap_or_else(|error| panic!("could not stat {}: {error}", path.display()));
        if metadata.is_dir() {
            walk(&path, files);
        } else if metadata.is_file() {
            files.push(path);
        }
    }
}

/// Every file in the workspace the scan reads, DERIVED by subtraction.
fn scanned_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    walk(root, &mut files);
    files.sort();
    assert!(
        files.len() > 250,
        "the workspace scan found only {} files",
        files.len(),
    );
    files
}

/// Whether the path token `cited` names `document`, however it was abbreviated.
///
/// A reader resolves a bare `path-b.md` and a `./docs/path-b.md` to exactly the
/// file `docs/path-b.md` names, so the ban has to as well. The relation is
/// *path-component suffix*, and its direction is the whole content of the rule:
/// `path-b.md` is a suffix of `docs/path-b.md` and is refused, while
/// `evidence/README.md` is LONGER than the reading order's `README.md` and is
/// therefore a different file -- which is why matching the basename alone would
/// refuse a citation of a file this rule has no opinion about.
///
/// Written this way because the ban was, for two commits, the fully-qualified
/// spelling alone: it searched for the reading order's own path followed by a
/// colon, as a literal. The one live instance in the tree was written bare, and
/// it was inside §0.4 of `docs/path-b.md` -- the paragraph that states the rule.
/// A scan whose set of spellings is hand-written is the house bug class aimed
/// at a scan.
fn names_the_document(cited: &str, document: &str) -> bool {
    let cited = cited.strip_prefix("./").unwrap_or(cited);
    let cited = cited.split('/').collect::<Vec<_>>();
    let document = document.split('/').collect::<Vec<_>>();
    cited.len() <= document.len() && document[document.len() - cited.len()..] == cited[..]
}

/// Rule 1, first half: nothing may cite a Path B document by line.
#[test]
fn nothing_cites_a_path_b_document_by_line_number() {
    let root = workspace_root();
    let order = reading_order(&root);
    let linted = linted_documents(&order);
    let mut offences = Vec::new();
    // The whole workspace, which includes the documents themselves, on the same
    // terms. A dated receipt is free to record what a line USED to say; it is
    // not free to hold a number that a later insertion silently repoints.
    for path in scanned_files(&root) {
        let text = read_lossy(&path);
        for document in &linted {
            let basename = document.rsplit('/').next().unwrap_or(document);
            let needle = format!("{basename}:");
            for (offset, _) in text.match_indices(&needle) {
                let tail = &text[offset + needle.len()..];
                if !tail.starts_with(|character: char| character.is_ascii_digit()) {
                    continue;
                }
                // The whole path token the citation was written with, walked
                // back over the characters a path is spelled from. A citation
                // of line 72 of `evidence/README.md` is not a citation of the
                // top-level one, and it is this token -- not a left-boundary
                // character class -- that says so.
                let start = text[..offset]
                    .char_indices()
                    .rev()
                    .take_while(|(_, character)| {
                        character.is_ascii_alphanumeric()
                            || matches!(character, '/' | '-' | '_' | '.' | '+')
                    })
                    .last()
                    .map_or(offset, |(index, _)| index);
                if !names_the_document(&text[start..offset + basename.len()], document) {
                    continue;
                }
                let line = text[..offset].matches('\n').count() + 1;
                let cited = tail
                    .chars()
                    .take_while(|character| character.is_ascii_digit())
                    .collect::<String>();
                offences.push(format!(
                    "{}:{line} cites {}:{cited}",
                    path.strip_prefix(&root).unwrap_or(&path).display(),
                    &text[start..offset + basename.len()],
                ));
            }
        }
    }
    assert!(
        offences.is_empty(),
        "a Path B document may only be cited by SECTION -- a line number is correct \
         exactly once, and line 187 of `docs/path-b.md` stopped being the \
         MCP-isolation row the moment `20bf20f` inserted rows above it \
         (`docs/path-b.md` §0.4):\n  {}",
        offences.join("\n  "),
    );
}

/// Rule 1, second half: a `§N.M` any file names must be a real heading.
#[test]
fn every_path_b_section_the_workspace_names_is_a_heading_that_document_has() {
    let root = workspace_root();
    let order = reading_order(&root);
    let linted = linted_documents(&order);
    let headings = linted
        .iter()
        .map(|document| {
            let numbers = read(&root, document)
                .lines()
                .filter_map(|line| {
                    let rest = line.trim_start_matches('#');
                    if rest.len() == line.len() {
                        return None;
                    }
                    rest.split_whitespace().next().map(|number| {
                        number
                            .trim_end_matches('.')
                            .trim_end_matches(':')
                            .to_owned()
                    })
                })
                .collect::<BTreeSet<_>>();
            (document.clone(), numbers)
        })
        .collect::<BTreeMap<_, _>>();
    let mut offences = Vec::new();
    let mut checked = 0usize;
    for path in scanned_files(&root) {
        let text = read_lossy(&path);
        for document in &linted {
            for (offset, _) in text.match_indices(document.as_str()) {
                // `docs/path-b.md` §2.2 -- optionally through a closing
                // backtick, which is how every site in the tree spells it.
                let tail = text[offset + document.len()..]
                    .trim_start_matches('`')
                    .trim_start();
                let Some(rest) = tail.strip_prefix('§') else {
                    continue;
                };
                let section = rest
                    .trim_start()
                    .chars()
                    .take_while(|character| character.is_ascii_digit() || *character == '.')
                    .collect::<String>();
                let section = section.trim_end_matches('.').to_owned();
                if section.is_empty() {
                    continue;
                }
                checked += 1;
                if !headings[document].contains(&section) {
                    let line = text[..offset].matches('\n').count() + 1;
                    offences.push(format!(
                        "{}:{line} cites {document} §{section}, which is not a heading it has",
                        path.strip_prefix(&root).unwrap_or(&path).display(),
                    ));
                }
            }
        }
    }
    assert!(
        checked >= 5,
        "only {checked} section citations were graded, so this test is nearly vacuous",
    );
    assert!(
        offences.is_empty(),
        "a section citation must resolve to a heading:\n  {}",
        offences.join("\n  "),
    );
}

/// An identifier a citation can be graded on: long enough and shaped like code
/// rather than like prose.
///
/// `clear`, `mode` and `effort` are words a sentence uses about the product;
/// `FORBIDDEN_DRIVER_FLAGS` and `classify_terminal_snapshot` are things a line
/// either holds or does not. Grading on the first kind flags correct citations,
/// which is why the shape is required rather than the length alone.
fn gradable_identifier(token: &str) -> bool {
    if token.len() < 5 || !token.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        return false;
    }
    if !token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return false;
    }
    let screaming = token
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    let camel = token
        .chars()
        .zip(token.chars().skip(1))
        .any(|(a, b)| a.is_ascii_lowercase() && b.is_ascii_uppercase());
    // A single capitalised word is a type name: `Ambiguous`, `Ready`,
    // `Replace`. Without this the shape rule silently skips every citation
    // whose anchor is a one-word variant, which is most enum arms.
    let type_name = token.starts_with(|c: char| c.is_ascii_uppercase())
        && token.chars().any(|c| c.is_ascii_lowercase());
    token.contains('_') || screaming || camel || type_name
}

/// True when `span` is a path or a citation rather than something to grade on.
///
/// A path is one token. Requiring that is not decoration: without it, any
/// quoted sentence containing a slash was read as a path and thrown away, and
/// `docs/version-drift.md` quotes the line it cites as *"MEASURED … over 61
/// post-`/clear` transcripts"* -- prose, one slash, discarded as a path, and
/// then reported as a citation that names nothing.
fn is_path_like(span: &str) -> bool {
    let head = span.split(':').next().unwrap_or_default();
    !head.contains(char::is_whitespace)
        && (head.contains('/')
            || CITED_EXTENSIONS
                .iter()
                .any(|extension| head.ends_with(extension)))
}

/// A `path:line` or `path:line-line` citation found in prose.
struct Citation {
    line: usize,
    raw: String,
    path: String,
    from: usize,
    to: usize,
}

fn citations_in(text: &str) -> Vec<Citation> {
    let mut found = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let bytes = line.as_bytes();
        let mut cursor = 0usize;
        while let Some(offset) = CITED_EXTENSIONS
            .iter()
            .filter_map(|extension| {
                line[cursor..]
                    .find(&format!("{extension}:"))
                    .map(|found| found + cursor)
            })
            .min()
        {
            // Walk back over the path.
            let mut start = offset;
            while start > 0 {
                let previous = bytes[start - 1];
                if previous.is_ascii_alphanumeric()
                    || previous == b'_'
                    || previous == b'.'
                    || previous == b'/'
                    || previous == b'-'
                    || previous == b'+'
                {
                    start -= 1;
                } else {
                    break;
                }
            }
            let colon = line[offset..]
                .find(':')
                .map(|o| o + offset)
                .unwrap_or(offset);
            let path = line[start..colon].to_owned();
            let mut end = colon + 1;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            if end == colon + 1 {
                cursor = colon + 1;
                continue;
            }
            let from = line[colon + 1..end].parse::<usize>().unwrap_or(0);
            let mut to = from;
            let mut scan = end;
            if scan < bytes.len() && (bytes[scan] == b'-' || line[scan..].starts_with('\u{2013}')) {
                let dash = if bytes[scan] == b'-' { 1 } else { 3 };
                let mut tail = scan + dash;
                let digits = tail;
                while tail < bytes.len() && bytes[tail].is_ascii_digit() {
                    tail += 1;
                }
                if tail > digits {
                    to = line[digits..tail].parse::<usize>().unwrap_or(from);
                    scan = tail;
                }
            }
            found.push(Citation {
                line: index + 1,
                raw: line[start..scan].to_owned(),
                path,
                from,
                to: to.max(from),
            });
            cursor = scan.max(colon + 1);
        }
    }
    found
}

/// What a citation's path resolved to.
enum Resolution {
    /// Exactly one file in the scanned workspace.
    One(PathBuf),
    /// A basename several scanned files share, abbreviated past the point where
    /// a reader -- or this test -- can tell which one is meant.
    Ambiguous(usize),
    /// No file in the scanned workspace. `library/std/src/sys/fs/unix.rs:1212`
    /// is a citation of the Rust standard library and this test has no opinion
    /// about it.
    Outside,
}

fn resolve(root: &Path, by_basename: &BTreeMap<String, Vec<PathBuf>>, cited: &str) -> Resolution {
    // A path the scan does not descend into is outside, however it is spelled:
    // `.context/` is gitignored workspace coordination and `vendor/` is
    // somebody else's tree, and grading a citation of either against a line
    // number would be this test claiming an authority it does not have. The
    // direct-path branch used to resolve them anyway, because it asked the
    // filesystem instead of asking the scan.
    if cited
        .split('/')
        .any(|component| SCAN_SKIPPED_DIRECTORIES.contains(&component))
    {
        return Resolution::Outside;
    }
    let direct = root.join(cited);
    if direct.is_file() {
        return Resolution::One(direct);
    }
    let basename = cited.rsplit('/').next().unwrap_or(cited);
    match by_basename.get(basename) {
        None => Resolution::Outside,
        Some(candidates) if candidates.len() == 1 => Resolution::One(candidates[0].clone()),
        Some(candidates) => {
            let suffixed = candidates
                .iter()
                .filter(|candidate| candidate.ends_with(cited))
                .collect::<Vec<_>>();
            match suffixed.len() {
                1 => Resolution::One(suffixed[0].clone()),
                _ => Resolution::Ambiguous(candidates.len()),
            }
        }
    }
}

/// The result of grading one file's citations.
#[derive(Default)]
struct Grade {
    seen: usize,
    graded: usize,
    offences: Vec<String>,
}

/// Rule 2's predicate over one file's text.
///
/// `total` is the difference between a document that promises its citations are
/// checked and a source tree that does not: under it, a citation this cannot
/// grade is an offence naming what to add, rather than a silent pass.
fn grade_citations(
    root: &Path,
    by_basename: &BTreeMap<String, Vec<PathBuf>>,
    cache: &mut BTreeMap<PathBuf, Vec<String>>,
    where_: &str,
    text: &str,
    total: bool,
) -> Grade {
    let mut grade = Grade::default();
    let document_lines = text.lines().collect::<Vec<_>>();
    for citation in citations_in(text) {
        let resolved = match resolve(root, by_basename, &citation.path) {
            Resolution::One(path) => path,
            // Stdlib and deleted freeze paths are not this test's set. Counting
            // them as `seen` without `graded` made `graded == seen` fail as if
            // a living citation had escaped, which is the wrong message.
            Resolution::Outside => continue,
            Resolution::Ambiguous(count) => {
                if total {
                    grade.offences.push(format!(
                        "{where_}:{} cites {} -- {count} scanned files carry that name, so \
                         nothing can tell which line was meant; write enough of the path to \
                         resolve to one",
                        citation.line, citation.raw,
                    ));
                }
                continue;
            }
        };
        grade.seen += 1;
        let lines = cache.entry(resolved.clone()).or_insert_with(|| {
            std::fs::read_to_string(&resolved)
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        });
        if lines.is_empty() {
            if total {
                grade.offences.push(format!(
                    "{where_}:{} cites {} -- the file is empty, so nothing can land",
                    citation.line, citation.raw,
                ));
            }
            continue;
        }
        // The wrapped sentence, not the line and not the paragraph.
        // Markdown hard-wraps, so `TURN_DURATION_DRAIN_FLOOR_MS` sits one
        // line above the number that cites it and grading on the citation's
        // own line called that correct citation rotted.
        //
        // BOTH neighbours, because a hard wrap puts the anchor below the
        // citation exactly as readily as above it and this rule used to
        // take only the line above. `docs/version-drift.md` said *"flipping
        // the predicate at ... from `since_candidate > 0` to `< 0`"* with
        // the identifier on the FOLLOWING line, so the citation named no
        // gradable anchor, was skipped, and pointed at a `continue` inside
        // a JSON-decode loop 163 lines from the predicate it quoted. The
        // one-sided join was not a rule about markdown; it was the only
        // case anybody had hit.
        //
        // `structural` and the own-citation guard below do the work the
        // one-sidedness was standing in for: in a table and in a list the
        // adjacent line is a different claim about a different file, and
        // joining them let a row borrow its neighbour's constant and pass.
        // Those guards were always symmetric -- only the join was not.
        let own = document_lines[citation.line - 1];
        let structural = |line: &str| {
            let trimmed = line.trim_start();
            trimmed.is_empty()
                || trimmed.starts_with('|')
                || trimmed.starts_with('#')
                || trimmed.starts_with("- ")
                || trimmed.starts_with("* ")
                || trimmed
                    .split_once(". ")
                    .is_some_and(|(head, _)| head.parse::<u32>().is_ok())
        };
        // A blockquote marker is NOT structural: `>` opens every line of one
        // quoted paragraph the way `///` opens every line of a doc comment, so
        // refusing to join two `>` lines split a single sentence -- and four
        // citations in `docs/2.1.226-compatibility.md` name their constant on
        // the `>` line after the one that cites it.
        //
        // A neighbour carrying its own citation is not joined: its
        // identifiers belong to THAT citation, and letting them travel is
        // how a proxy of this shape reports `pool/mod.rs:1068` -- which
        // holds exactly the predicate its sentence quotes -- as rotted,
        // because the line above named `sidechain_on_toolless_cell` for a
        // different file.
        // Only the NEIGHBOUR's shape decides, not the citation's own. Requiring
        // both to be prose meant a bullet or a table row could never reach its
        // own continuation line, and `docs/version-drift.md` quotes the line it
        // cites across a wrap in exactly that shape -- the quotation opens on
        // the bullet and closes on the line below, so the grader saw an
        // unterminated quote, found no anchor, and called a citation that
        // quotes its own line "names nothing". A row's neighbour in a table is
        // another row, which `structural` already refuses; what the extra
        // clause refused on top of that was only ever the wrap.
        let joinable = |neighbour: Option<&&str>| {
            neighbour
                .filter(|line| !structural(line) && citations_in(line).is_empty())
                .map(|line| (*line).to_owned())
        };
        let above = joinable(
            citation
                .line
                .checked_sub(2)
                .and_then(|i| document_lines.get(i)),
        );
        let below = joinable(document_lines.get(citation.line));
        // Every join, not one of them, because a quotation mark is a PAIR and a
        // join decides which two are the pair. `docs/version-drift.md` lists
        // three cited lines as three bullets, each quoting its own line across
        // a wrap; joining all three lines at once pairs the closing mark of one
        // bullet with the opening mark of the next and yields spans that are in
        // no document. Reading each join and taking the union costs three more
        // passes over three lines and cannot lose a span any single join finds.
        let joins = [
            Some(own.to_owned()),
            above.as_ref().map(|line| format!("{line} {own}")).or(None),
            below.as_ref().map(|line| format!("{own} {line}")).or(None),
            match (&above, &below) {
                (Some(a), Some(b)) => Some(format!("{a} {own} {b}")),
                _ => None,
            },
        ];
        // A path-shaped span is not an anchor -- grading `driver_io.rs:135` on
        // the word `driver_io` would pass every citation of that file at every
        // line. But `test_runner.py:296::test_linux_manifest_is_the_exact_…`
        // names a test INSIDE a path-shaped span, and dropping the span whole
        // dropped the one thing in it that says which line was meant. What is
        // kept is the part after the last `::`, which is the only part of that
        // shape that is not the path.
        let spans = joins
            .iter()
            .flatten()
            .flat_map(|join| quoted_spans(join))
            .filter_map(|span| match is_path_like(&span) {
                false => Some(span),
                true => span
                    .rsplit("::")
                    .next()
                    .filter(|tail| *tail != span)
                    .map(str::to_owned),
            })
            .collect::<BTreeSet<_>>();
        // Grade only on anchors the cited file actually has. A sentence
        // names plenty of things that are not in the file it is citing --
        // `retractedMessageUuids` is a Claude transcript key, `FAILED` is a
        // word -- and treating those as a claim about the file turns every
        // such sentence into a false report. What is left is exactly the rot
        // worth refusing: the right file, the wrong line.
        let whole = lines.join("\n");
        let whole_reduced = reduced(&whole);
        let identifiers = spans
            .iter()
            .flat_map(|span| {
                span.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .filter(|token| gradable_identifier(token))
            .map(|token| {
                let occurrences = whole.matches(token.as_str()).count();
                (token, occurrences)
            })
            .filter(|(_, occurrences)| (1..=MAX_ANCHOR_OCCURRENCES).contains(occurrences))
            .collect::<BTreeMap<_, _>>();
        let phrases = spans
            .iter()
            .map(|span| reduced(span))
            .filter(|span| span.len() >= MINIMUM_PHRASE_ANCHOR)
            .map(|span| {
                let occurrences = whole_reduced.matches(span.as_str()).count();
                (span, occurrences)
            })
            .filter(|(_, occurrences)| (1..=MAX_ANCHOR_OCCURRENCES).contains(occurrences))
            .collect::<BTreeMap<_, _>>();
        if identifiers.is_empty() && phrases.is_empty() {
            if total {
                grade.offences.push(format!(
                    "{where_}:{} cites {} and names nothing that line can be checked against; \
                     name an identifier {} holds, or quote the line",
                    citation.line, citation.raw, citation.path,
                ));
            }
            continue;
        }
        grade.graded += 1;
        if citation.to > lines.len() {
            grade.offences.push(format!(
                "{where_}:{} cites {} -- the file has {} lines",
                citation.line,
                citation.raw,
                lines.len(),
            ));
            continue;
        }
        let cited = lines[citation.from.saturating_sub(1)..citation.to].join("\n");
        let cited_reduced = reduced(&cited);
        if identifiers
            .keys()
            .any(|identifier| cited.contains(identifier.as_str()))
            || phrases.keys().any(|phrase| cited_reduced.contains(phrase))
        {
            continue;
        }
        // The rarest thing the sentence names, because that is the one that
        // points hardest at a line, and the one a reader repairing this wants
        // named back at them.
        let (named, occurrences) = identifiers
            .iter()
            .chain(phrases.iter())
            .min_by_key(|(_, occurrences)| **occurrences)
            .expect("the anchor set was filtered to what the file holds");
        // Where the thing really is. A quoted phrase wraps, so the window is
        // as wide as the wrap: reporting per line found nothing at all for
        // every multi-line quote and printed "which is at " with no number,
        // which is a repair instruction that does not name the repair.
        let real = (0..lines.len())
            .filter(|index| {
                let window = lines[*index..(index + PHRASE_WRAP_LINES).min(lines.len())].join("\n");
                lines[*index].contains(named.as_str()) || reduced(&window).contains(named.as_str())
            })
            .map(|index| (index + 1).to_string())
            .collect::<Vec<_>>()
            .join(", ");
        grade.offences.push(format!(
            "{where_}:{} cites {} for `{named}` ({occurrences}x), which is at {real}",
            citation.line, citation.raw,
        ));
    }
    grade
}

/// A basename index over the scanned workspace, so `driver_io.rs:142` resolves
/// the way a reader resolves it.
fn basename_index(files: &[PathBuf]) -> BTreeMap<String, Vec<PathBuf>> {
    let mut index: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for path in files {
        if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
            index.entry(name.to_owned()).or_default().push(path.clone());
        }
    }
    index
}

/// An outside path (stdlib, or a deleted freeze file) is not this test's set.
/// Counting it as `seen` without `graded` used to fail `graded == seen`.
#[test]
fn an_outside_citation_is_not_an_ungraded_seen() {
    let root = workspace_root();
    let files = scanned_files(&root);
    let by_basename = basename_index(&files);
    let mut cache = BTreeMap::new();
    let text = "see `library/std/src/sys/fs/unix.rs:1212` for the stdlib.\n";
    let grade = grade_citations(&root, &by_basename, &mut cache, "synthetic", text, true);
    assert_eq!(grade.seen, 0, "outside citations must not count as seen");
    assert_eq!(grade.graded, 0);
    assert!(grade.offences.is_empty());
}

/// Rule 2: EVERY citation in a Path B document lands on a line that holds
/// something its sentence names.
#[test]
fn every_citation_in_a_path_b_document_lands_on_what_it_names() {
    let root = workspace_root();
    let order = reading_order(&root);
    let linted = linted_documents(&order);
    let files = scanned_files(&root);
    let by_basename = basename_index(&files);
    let mut cache: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    let mut total = Grade::default();

    for document in &linted {
        let text = read(&root, document);
        let grade = grade_citations(&root, &by_basename, &mut cache, document, &text, true);
        total.seen += grade.seen;
        total.graded += grade.graded;
        total.offences.extend(grade.offences);
    }
    assert!(
        total.seen >= 100,
        "only {} citations were found in the linted documents, so this test is nearly vacuous",
        total.seen,
    );
    assert!(
        total.offences.is_empty(),
        "cite the line that holds the thing you name (`docs/path-b.md` §0.4); \
         {} of the {} citations in the linted documents do not:\n  {}",
        total.offences.len(),
        total.seen,
        total.offences.join("\n  "),
    );
    // Every citation that resolves into this repository is graded, and the two
    // numbers are printed rather than asserted apart, so a run says what its
    // own coverage was instead of leaving it to this comment.
    assert_eq!(
        total.graded,
        total.seen,
        "{} of the {} citations in the linted documents went ungraded, which the offence \
         list above should have refused one by one",
        total.seen - total.graded,
        total.seen,
    );
}

/// Rule 3: a citation may not abbreviate its path away entirely.
///
/// `citations_in` anchors on a file extension before the colon, so it grades
/// `crates/claude/src/composer.rs:220` and is blind to `` (`:220`) `` — the
/// abbreviated form, where the path is left to the surrounding sentence. Three
/// of those sat in `docs/path-b-adversarial.md` §4.5 naming
/// `COMPOSER_MODE_PREFIXES`, `COMPOSER_REWRITTEN_CHARACTERS` and
/// `composer_refusal`; a later commit moved all three definitions and nothing
/// failed, because the grader had never seen them.
///
/// **A citation that escapes the checker is worth less than no citation at
/// all**, since a reader takes a `path:line` in this tree to be one the build
/// verifies. So the shape is refused rather than taught to the grader: teaching
/// it would mean inventing a rule for which preceding path an abbreviation
/// belongs to, and the abbreviation saves a reader nothing that is worth a rule.
///
/// The set of documents is the reading order's own linted set, not a list here,
/// so a document promoted to linted arrives under this rule with the others.
#[test]
fn no_path_b_citation_abbreviates_its_path() {
    let root = workspace_root();
    let order = reading_order(&root);
    let linted = linted_documents(&order);
    let mut offences = Vec::new();
    let mut inspected = 0usize;
    for document in &linted {
        let path = root.join(document);
        let text = read_lossy(&path);
        for (number, line) in text.lines().enumerate() {
            inspected += 1;
            for (offset, _) in line.match_indices("`:") {
                let tail = &line[offset + 2..];
                if !tail.starts_with(|character: char| character.is_ascii_digit()) {
                    continue;
                }
                let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
                // `` `:220` `` closes right after the number (or after a range).
                let rest = &tail[digits.len()..];
                let closes = rest.starts_with('`') || rest.starts_with('-');
                if closes {
                    offences.push(format!(
                        "{document}:{} writes `:{digits}` with no path; \
                         write the path the line belongs to",
                        number + 1,
                    ));
                }
            }
        }
    }
    assert!(
        inspected > 500,
        "only {inspected} lines were inspected, so this test is nearly vacuous",
    );
    assert!(
        offences.is_empty(),
        "a line citation must carry its own path, so the build can check it \
         (`docs/path-b.md` §0.4); {} do not:\n  {}",
        offences.len(),
        offences.join("\n  "),
    );
}
