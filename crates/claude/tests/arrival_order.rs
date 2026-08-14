//! Row-level predicates behind the `turn_duration` arrival-order measurement.
//!
//! The measurement itself lives in the service actor: it stamps when pmux
//! *observed* the `turn_duration` marker and whether anything the analysis reads
//! arrived on a strictly later read. Both questions are asked of individual rows
//! by [`ParsedRow::is_turn_duration_marker`] and
//! [`ParsedRow::is_analysis_changing`], so those two predicates carry the whole
//! risk of the measurement lying, and they are pinned here against the engine's
//! own behaviour rather than against a restatement of themselves.

use pseudomux_claude::{
    CompleteLine, JsonlParser, ParseMode, ParsedRow, SourceLocation, TranscriptAnalysis,
    TranscriptEngine,
};

const PROMPT: &str = "Reply with the word ready.";

/// The typed prompt, the terminal assistant message, and the two proven-inert
/// markers, in the order Claude 2.1.220 writes them: the `turn_duration` row
/// parents onto the `stop_hook_summary` row, and both trail the answer.
const TYPED_PROMPT: &str = r#"{"parentUuid":null,"sessionId":"s","type":"user","message":{"role":"user","content":"Reply with the word ready."},"uuid":"prompt","promptSource":"typed","promptId":"prompt-1"}"#;
const ASSISTANT_ANSWER: &str = r#"{"parentUuid":"prompt","sessionId":"s","type":"assistant","requestId":"req","uuid":"answer","message":{"id":"msg","model":"claude-opus-4-6","content":[{"type":"text","text":"ready"}],"stop_reason":"end_turn","usage":{"input_tokens":11,"output_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#;
const STOP_HOOK_SUMMARY: &str = r#"{"parentUuid":"answer","isSidechain":false,"type":"system","subtype":"stop_hook_summary","hookCount":1,"hookInfos":[],"hookErrors":[],"hookAdditionalContext":[],"preventedContinuation":false,"stopReason":"","hasOutput":false,"level":"suggestion","timestamp":"2026-07-30T01:28:04.414Z","uuid":"stop-hook","sessionId":"s","version":"2.1.220"}"#;
const TURN_DURATION: &str = r#"{"parentUuid":"stop-hook","isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":4327,"messageCount":7,"timestamp":"2026-07-30T01:28:04.415Z","uuid":"turn-duration","isMeta":false,"sessionId":"s","version":"2.1.220"}"#;

/// A `turn_duration` row that is not on the main chain. Same subtype, same
/// payload, `isSidechain` flipped.
const SIDECHAIN_TURN_DURATION: &str = r#"{"parentUuid":"answer","isSidechain":true,"type":"system","subtype":"turn_duration","durationMs":11,"messageCount":2,"timestamp":"2026-07-30T01:28:04.415Z","uuid":"sidechain-turn-duration","sessionId":"s","version":"2.1.220"}"#;

/// An enumerated metadata record: tier one of the row policy, excluded from the
/// graph before analysis begins.
const METADATA_RECORD: &str = r#"{"type":"file-history-snapshot","sessionId":"s","messageId":"msg","snapshot":{"trackedFileBackups":{}},"isSnapshotUpdate":false}"#;

/// A sidechain assistant fragment descended from the acknowledged prompt. It is
/// never on the main chain, yet the engine reads it: it moves
/// `sidechain_usage` and `combined_usage`.
const SIDECHAIN_ASSISTANT: &str = r#"{"parentUuid":"prompt","isSidechain":true,"sessionId":"s","type":"assistant","requestId":"req-side","uuid":"sidechain-answer","message":{"id":"msg-side","model":"claude-opus-4-6","content":[{"type":"text","text":"sub-agent"}],"stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":7,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#;

/// An unrecognized top-level type. Off the chain it is a rendered warning, on
/// the chain it is fatal; either way the analysis is not indifferent to it.
const UNKNOWN_ROW: &str =
    r#"{"parentUuid":"answer","sessionId":"s","type":"future-semantic-row","uuid":"unknown"}"#;

fn parse(line: &str) -> ParsedRow {
    JsonlParser::new(ParseMode::Strict)
        .parse(&CompleteLine {
            location: SourceLocation {
                line: 1,
                byte_offset: 0,
            },
            bytes: line.as_bytes().to_vec(),
        })
        .expect("fixture rows are valid strict-mode JSONL")
}

fn armed_engine(rows: &[&str]) -> TranscriptEngine {
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn(PROMPT).expect("a fresh engine can arm");
    for line in rows {
        engine.ingest(parse(line)).expect("fixture rows ingest");
    }
    engine
}

fn analysis(rows: &[&str]) -> TranscriptAnalysis {
    armed_engine(rows).analyze().expect("fixture turns analyze")
}

#[test]
fn the_turn_duration_marker_predicate_agrees_with_the_engine_on_the_active_chain() {
    let completed = [
        TYPED_PROMPT,
        ASSISTANT_ANSWER,
        STOP_HOOK_SUMMARY,
        TURN_DURATION,
    ];
    assert!(
        analysis(&completed).turn_duration_seen,
        "the scripted turn is the shape Claude actually writes"
    );
    let marker_rows = completed
        .iter()
        .filter(|line| parse(line).is_turn_duration_marker())
        .count();
    assert_eq!(
        marker_rows, 1,
        "exactly the row the engine counted must satisfy the predicate"
    );

    // The same turn without the marker: the engine says it was not seen, and no
    // row claims to be it. `stop_hook_summary` shares the trailing zone and must
    // not be mistaken for the marker -- it proves a Stop hook ran, not that the
    // stream ended.
    let unmarked = [TYPED_PROMPT, ASSISTANT_ANSWER, STOP_HOOK_SUMMARY];
    assert!(!analysis(&unmarked).turn_duration_seen);
    assert!(
        unmarked
            .iter()
            .all(|line| !parse(line).is_turn_duration_marker())
    );
}

#[test]
fn a_turn_duration_row_off_the_main_chain_is_not_the_marker() {
    // Scope, not subtype alone. A sidechain `turn_duration` describes a
    // sub-agent's turn and can never end the caller's, so stamping the marker
    // from it would understate when pmux saw the real one.
    assert!(!parse(SIDECHAIN_TURN_DURATION).is_turn_duration_marker());
    assert!(
        !analysis(&[
            TYPED_PROMPT,
            ASSISTANT_ANSWER,
            SIDECHAIN_TURN_DURATION,
            STOP_HOOK_SUMMARY,
        ])
        .turn_duration_seen,
        "the engine ignores it too, so the predicate must as well"
    );
}

#[test]
fn only_off_graph_metadata_records_are_excluded_from_the_analysis_changing_set() {
    assert!(!parse(METADATA_RECORD).is_analysis_changing());
    for line in [
        TYPED_PROMPT,
        ASSISTANT_ANSWER,
        STOP_HOOK_SUMMARY,
        TURN_DURATION,
        SIDECHAIN_TURN_DURATION,
        SIDECHAIN_ASSISTANT,
        UNKNOWN_ROW,
    ] {
        assert!(
            parse(line).is_analysis_changing(),
            "every row the engine reads anywhere must count: {line}"
        );
    }
}

#[test]
fn an_excluded_metadata_record_provably_cannot_change_the_analysis() {
    // The predicate's one `false` is only safe if the engine truly ignores such
    // a row. Asserted against the whole analysis rather than a chosen field, so
    // a future metadata record that starts carrying semantics fails here.
    let completed = [
        TYPED_PROMPT,
        ASSISTANT_ANSWER,
        STOP_HOOK_SUMMARY,
        TURN_DURATION,
    ];
    let before = analysis(&completed);
    let after = analysis(&[
        TYPED_PROMPT,
        ASSISTANT_ANSWER,
        STOP_HOOK_SUMMARY,
        TURN_DURATION,
        METADATA_RECORD,
    ]);
    assert_eq!(
        before, after,
        "an off-graph metadata record must leave the analysis byte-identical"
    );
}

#[test]
fn a_counted_late_row_that_is_legal_after_the_marker_does_change_the_analysis() {
    // The interesting case for the measurement. A semantic main-chain row after
    // the marker is schema drift and fails the turn, but a sidechain assistant
    // fragment is legal there -- and it moves usage. If it arrives on a strictly
    // later read than the marker, completing at the marker would have published
    // a different result, which is exactly what the published pair reports.
    let completed = [
        TYPED_PROMPT,
        ASSISTANT_ANSWER,
        STOP_HOOK_SUMMARY,
        TURN_DURATION,
    ];
    let before = analysis(&completed);
    let after = analysis(&[
        TYPED_PROMPT,
        ASSISTANT_ANSWER,
        STOP_HOOK_SUMMARY,
        TURN_DURATION,
        SIDECHAIN_ASSISTANT,
    ]);

    assert!(parse(SIDECHAIN_ASSISTANT).is_analysis_changing());
    assert_ne!(before, after);
    assert_eq!(before.combined_usage.tokens.output_tokens, 2);
    assert_eq!(
        after.combined_usage.tokens.output_tokens, 9,
        "the late sidechain fragment is counted in combined usage"
    );
    assert_eq!(
        after.status, before.status,
        "and it changes usage without disturbing the terminal outcome"
    );
}
