//! Replays the recorded screen corpus through the production classifier and
//! asserts the geometry invariants over every frame in it.
//!
//! # What this buys
//!
//! pmux snapshots the pane every ~25 ms, so a five-turn session produces
//! hundreds of frames that today are classified once and discarded. Recording
//! them (`pseudomux_service::screen_corpus`, opt-in via `PMUX_SCREEN_CORPUS_DIR`)
//! turns each of those into a permanent test case, and this file is the standing
//! test that runs the classifier over all of them offline and for free.
//!
//! The 85/85 empty-composer calibration on Claude Code 2.1.220 was derived by
//! hand, once, and then written into a comment. A comment does not fail. Here it
//! is [`pseudomux_service::screen_corpus::MEASURED_RENDERED_ROWS_BELOW_READY_COMPOSER`],
//! checked against every recorded frame forever.
//!
//! # Extending the corpus when a new Claude Code ships
//!
//! See the module docs on `screen_corpus`, and
//! [`the_corpus_directory_is_not_empty`] for why a vanished corpus is a failure
//! rather than a quiet pass.

use std::path::{Path, PathBuf};

use pseudomux_rmux::{CellColor, StyledCell, StyledScreen, TerminalCursor, TerminalSnapshot};
use pseudomux_service::driver_io::{TerminalScreenState, classify_terminal_snapshot};
use pseudomux_service::screen_corpus::{
    CORPUS_SCHEMA, Corpus, CorpusFrame, CorpusStamp, CorpusWriter, check_corpus,
};

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("corpus")
}

fn load_all() -> Vec<Corpus> {
    Corpus::load_dir(corpus_dir()).expect("the checked-in corpus must load")
}

/// A corpus that silently disappeared would make every invariant below pass
/// vacuously, which is the one failure mode a standing check over recorded data
/// cannot tolerate. A check the suite cannot see is not a check.
#[test]
fn the_corpus_directory_is_not_empty() {
    let corpora = load_all();
    assert!(
        !corpora.is_empty(),
        "no corpus files under {}; rebuild them with \
         `python3 tools/screen-corpus/seed_corpus.py`",
        corpus_dir().display()
    );
    let frames: usize = corpora.iter().map(|corpus| corpus.frames.len()).sum();
    assert!(
        frames >= 6,
        "the corpus holds only {frames} frames; the checked-in seed alone is 6"
    );
}

/// **The standing check.** Every invariant, over every frame, in every corpus.
#[test]
fn every_recorded_frame_satisfies_the_geometry_invariants() {
    let corpora = load_all();
    let mut violations = Vec::new();
    for corpus in &corpora {
        for violation in check_corpus(corpus) {
            violations.push(format!("{}: {violation}", corpus.source.display()));
        }
    }
    assert!(
        violations.is_empty(),
        "{} corpus frames violated a geometry invariant:\n{}",
        violations.len(),
        violations.join("\n")
    );
}

/// The invariants must not be vacuous: a corpus in which nothing is ever `Ready`
/// satisfies every `Ready`-conditional check above by having no cases.
///
/// This is the same trap the one-directional readiness property in
/// `screen_properties.rs` avoids, and it is worth closing twice: a Claude
/// release that stops matching the composer shape would otherwise turn this
/// whole file green while pmux refused every turn in production.
#[test]
fn the_corpus_contains_frames_the_classifier_calls_ready() {
    let corpora = load_all();
    let ready = corpora
        .iter()
        .flat_map(|corpus| corpus.frames.iter())
        .filter(|frame| {
            classify_terminal_snapshot(&frame.to_terminal_snapshot()) == TerminalScreenState::Ready
        })
        .count();
    assert!(
        ready > 0,
        "no recorded frame classifies as Ready, so every Ready invariant is vacuous"
    );
}

/// The checked-in corpus must carry unconditional expectations, not only
/// conditional invariants.
///
/// MEASURED, and the reason this test exists: replaying this corpus through the
/// pre-fix composer gate PASSED. The broken classifier simply stopped returning
/// `Ready` for the post-`/clear` frame, and every `Ready`-conditional invariant
/// went vacuous. `expect_ready` is the half that cannot go vacuous, so a corpus
/// carrying none of them is a corpus that cannot catch that class of bug.
#[test]
fn the_checked_in_corpus_carries_unconditional_expectations() {
    let corpora = load_all();
    let expectations = corpora
        .iter()
        .flat_map(|corpus| corpus.frames.iter())
        .filter(|frame| frame.expect_ready().is_some())
        .count();
    assert!(
        expectations >= 6,
        "only {expectations} frames carry an independently established verdict; \
         without them every invariant is conditional on the classifier's own \
         answer and a classifier that answers nothing passes"
    );
}

/// Provenance is not optional. An invariant is a claim about a version of Claude
/// Code, and a frame whose version is unknown cannot confirm or refute one.
#[test]
fn every_corpus_carries_provenance() {
    for corpus in load_all() {
        let source = corpus.source.display().to_string();
        assert_eq!(corpus.stamp.schema, CORPUS_SCHEMA, "{source}: wrong schema");
        assert!(
            !corpus.stamp.claude_version.is_empty() && corpus.stamp.claude_version != "unknown",
            "{source}: a checked-in corpus must name the Claude Code version it \
             was captured under"
        );
        assert!(
            !corpus.stamp.label.is_empty(),
            "{source}: a checked-in corpus must say what it is"
        );
    }
}

/// The 2.1.70 captures pin the footer geometry, and the post-`/clear` frame pins
/// the case that broke the gate. Naming both keeps a corpus edit that quietly
/// drops one of them from passing.
#[test]
fn the_seed_corpus_still_contains_the_screens_it_was_built_from() {
    let corpora = load_all();
    let sites: Vec<String> = corpora
        .iter()
        .flat_map(|corpus| corpus.frames.iter())
        .map(|frame| frame.site().to_owned())
        .collect();
    for expected in [
        "fixture.claude_2_1_70_ready.txt",
        "fixture.claude_2_1_70_response.txt",
        "fixture.claude_2_1_70_thinking.txt",
        "fixture.claude_2_1_70_tool_use.txt",
        "fixture.claude_2_1_70_error.txt",
        "input_gate.pre_paste",
    ] {
        assert!(
            sites.iter().any(|site| site == expected),
            "the corpus no longer contains a frame recorded at {expected}"
        );
    }
}

// ---------------------------------------------------------------------------
// Format round trip
// ---------------------------------------------------------------------------

/// A corpus outlives the code that recorded it, so the format has to survive a
/// write/read cycle exactly. Anything lost here is evidence quietly destroyed at
/// recording time and never noticed.
#[test]
fn a_styled_screen_round_trips_through_the_corpus_byte_exactly() {
    // A row that exercises everything `row_text` is sensitive to: an explicitly
    // coloured run, a double-width glyph with its padding cell, and trailing
    // blanks that must be trimmed the same way on both sides of the trip.
    let row = vec![
        StyledCell::new("/", CellColor::indexed(4)),
        StyledCell::new("c", CellColor::indexed(4)),
        StyledCell::new("l", CellColor::indexed(4)),
        StyledCell::new("e", CellColor::Unstyled),
        StyledCell::new("\u{4f60}", CellColor::indexed(9)),
        // The trailing half of the wide glyph. Its text is deliberately NOT
        // empty: `render_cells_lossy` skips padding cells, so a padding cell
        // carrying no glyph renders identically whether or not the flag
        // survived, and a round-trip test built on one is blind to losing it.
        // MEASURED: with an empty payload here, deleting the flag inside
        // `StyledCell::padding` left this whole test green.
        StyledCell::padding("PAD", CellColor::indexed(9)),
        StyledCell::new(" ", CellColor::Unstyled),
        StyledCell::new(" ", CellColor::Unstyled),
    ];
    let screen = StyledScreen::new(
        7,
        2,
        8,
        Some(TerminalCursor {
            row: 1,
            col: 2,
            visible: true,
            style: 0,
        }),
        vec![row, vec![StyledCell::new("\u{276f}", CellColor::Unstyled)]],
    );

    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("round-trip.ndjson");
    let stamp = CorpusStamp::for_host(2, 8, "round trip");
    let mut writer = CorpusWriter::create(&path, &stamp).expect("the corpus is writable");
    writer
        .append(&CorpusFrame::from_styled("test", &screen))
        .expect("the frame is appendable");
    drop(writer);

    let loaded = Corpus::load(&path).expect("the corpus reloads");
    assert_eq!(loaded.frames.len(), 1);
    let recovered = loaded.frames[0]
        .to_styled_screen()
        .expect("a styled frame recovers a styled screen");
    assert_eq!(
        recovered, screen,
        "a styled screen did not survive the corpus round trip"
    );

    // Equality above compares the recovered screen against a fixture built with
    // the same constructor, so a constructor that drops the padding flag would
    // drop it on both sides and compare equal. These two assert the flag's
    // OBSERVABLE consequence against a literal, which no constructor bug can
    // satisfy from both directions at once.
    assert!(
        recovered.row(0)[5].is_padding(),
        "the padding flag did not survive the corpus round trip"
    );
    assert_eq!(
        recovered.row_text(0),
        "/cle\u{4f60}",
        "a padding cell must contribute no glyph to the rendered row, and \
         trailing blanks must be trimmed"
    );
    // And the lossy view both production and the invariants read must agree too.
    assert_eq!(
        recovered.visible_text(),
        screen.visible_text(),
        "the rendered text changed across the round trip"
    );
    assert_eq!(
        loaded.frames[0].to_terminal_snapshot(),
        screen.to_terminal_snapshot(),
        "the plain-text projection changed across the round trip"
    );
}

#[test]
fn a_snapshot_round_trips_through_the_corpus_byte_exactly() {
    let snapshot = TerminalSnapshot {
        revision: 42,
        rows: 4,
        cols: 20,
        cursor: Some(TerminalCursor {
            row: 1,
            col: 2,
            visible: true,
            style: 3,
        }),
        // Deliberately awkward: a blank tail, a wide glyph, and a trailing
        // empty row that must not be dropped by the newline join.
        visible_text: "\u{276f} \n\u{4f60}\u{597d}\n\n".to_owned(),
    };
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("round-trip.ndjson");
    let mut writer = CorpusWriter::create(&path, &CorpusStamp::for_host(4, 20, "round trip"))
        .expect("the corpus is writable");
    writer
        .append(&CorpusFrame::from_snapshot("test", &snapshot))
        .expect("the frame is appendable");
    drop(writer);

    let loaded = Corpus::load(&path).expect("the corpus reloads");
    assert_eq!(
        loaded.frames[0].to_terminal_snapshot(),
        snapshot,
        "a snapshot did not survive the corpus round trip"
    );
}

/// A reader must refuse a corpus it does not understand rather than
/// reinterpreting it. Silently skipping unparseable frames would turn a schema
/// change into a corpus that quietly shrinks to nothing and keeps passing.
#[test]
fn a_corpus_from_a_future_schema_is_refused() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("future.ndjson");
    let stamp = CorpusStamp {
        schema: CORPUS_SCHEMA + 1,
        ..CorpusStamp::for_host(24, 80, "from the future")
    };
    CorpusWriter::create(&path, &stamp).expect("the corpus is writable");
    assert!(
        Corpus::load(&path).is_err(),
        "a corpus from an unknown schema must be refused, not reinterpreted"
    );
}

#[test]
fn a_malformed_frame_is_an_error_and_not_a_skip() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("malformed.ndjson");
    let mut writer = CorpusWriter::create(&path, &CorpusStamp::for_host(24, 80, "malformed"))
        .expect("the corpus is writable");
    writer
        .append(&CorpusFrame::from_snapshot(
            "test",
            &TerminalSnapshot {
                revision: 1,
                rows: 1,
                cols: 1,
                cursor: None,
                visible_text: "x".to_owned(),
            },
        ))
        .expect("the frame is appendable");
    drop(writer);
    let mut text = std::fs::read_to_string(&path).expect("the corpus is readable");
    text.push_str("{\"kind\":\"snapshot\",\"site\":\n");
    std::fs::write(&path, text).expect("the corpus is writable");

    assert!(
        Corpus::load(&path).is_err(),
        "a malformed frame must fail the load rather than shrink the corpus"
    );
}

/// Recording is off unless the environment turns it on, and a disabled recorder
/// must accept frames without doing anything with them.
#[test]
fn recording_is_off_by_default() {
    // No `PMUX_SCREEN_CORPUS_DIR` is set for the test process, so these are the
    // production no-ops. What is being checked is that the disabled path is
    // reachable, total, and silent -- not that it records.
    pseudomux_service::screen_corpus::record_snapshot(
        "test",
        &TerminalSnapshot {
            revision: 1,
            rows: 1,
            cols: 1,
            cursor: None,
            visible_text: String::new(),
        },
    );
    pseudomux_service::screen_corpus::record_styled(
        "test",
        &StyledScreen::new(1, 0, 0, None, Vec::new()),
    );
    assert_eq!(
        pseudomux_service::screen_corpus::dropped_frames(),
        0,
        "a disabled recorder must not count drops"
    );
}
