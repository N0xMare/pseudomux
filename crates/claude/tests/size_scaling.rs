use std::hint::black_box;
use std::time::{Duration, Instant};

use pseudomux_claude::{
    CompleteLine, JsonlParser, ParseMode, ParsedRow, SourceLocation, TranscriptAnalysis,
    TranscriptEngine, TranscriptError, TurnStatus,
};

const SMALL_ROWS: usize = 512;
const LARGE_ROWS: usize = SMALL_ROWS * 8;
const SAMPLES: usize = 7;

#[test]
fn strict_engine_production_work_is_affine_for_valid_and_adversarial_graphs() {
    assert_affine_work("main_chain", |assistant_rows| {
        terminal_analysis_work(&transcript_rows(assistant_rows))
    });
    assert_affine_work("sidechain", |sidechain_rows| {
        let (analysis, work) = analyze_with_work(&sidechain_transcript_rows(sidechain_rows));
        let analysis = analysis.unwrap();
        assert!(matches!(analysis.status, TurnStatus::Terminal(_)));
        assert_eq!(
            analysis.sidechain_usage.model_calls_with_usage,
            sidechain_rows as u64
        );
        work
    });
    assert_affine_work("reversed_parent_chain", |assistant_rows| {
        let (analysis, work) = analyze_with_work(&reversed_parent_rows(assistant_rows));
        assert!(matches!(
            analysis,
            Err(TranscriptError::ParentAppendOrder { .. })
        ));
        work
    });
}

#[test]
fn strict_engine_elapsed_time_is_diagnostic_only() {
    let small = transcript_rows(SMALL_ROWS);
    let large = transcript_rows(LARGE_ROWS);

    // Warm instruction/data caches before collecting the diagnostic median.
    assert_analysis(&small, SMALL_ROWS);
    assert_analysis(&large, LARGE_ROWS);
    let small_elapsed = median_elapsed(&small, SMALL_ROWS);
    let large_elapsed = median_elapsed(&large, LARGE_ROWS);

    eprintln!(
        "pmux_transcript_scaling small_rows={SMALL_ROWS} small_ns={} large_rows={LARGE_ROWS} large_ns={} ratio_x1000={}",
        small_elapsed.as_nanos(),
        large_elapsed.as_nanos(),
        large_elapsed
            .as_nanos()
            .saturating_mul(1_000)
            .checked_div(small_elapsed.as_nanos().max(1))
            .unwrap_or(u128::MAX),
    );
}

fn assert_affine_work(label: &str, work_at_size: impl Fn(usize) -> u64) {
    let two = work_at_size(2);
    let three = work_at_size(3);
    let slope = three.checked_sub(two).expect("work must not decrease");
    assert!(slope > 0, "{label} must record per-row production work");

    let small = work_at_size(SMALL_ROWS);
    let large = work_at_size(LARGE_ROWS);
    let expected_small = two + slope * (SMALL_ROWS as u64 - 2);
    let expected_large = two + slope * (LARGE_ROWS as u64 - 2);
    assert_eq!(
        small, expected_small,
        "{label} small work stopped being affine"
    );
    assert_eq!(
        large, expected_large,
        "{label} large work stopped being affine"
    );
    eprintln!(
        "pmux_transcript_work label={label} base_rows=2 base_work={two} per_row={slope} small_rows={SMALL_ROWS} small_work={small} large_rows={LARGE_ROWS} large_work={large}"
    );
}

fn median_elapsed(rows: &[ParsedRow], expected_messages: usize) -> Duration {
    let mut samples = (0..SAMPLES)
        .map(|_| {
            let started = Instant::now();
            assert_analysis(rows, expected_messages);
            started.elapsed()
        })
        .collect::<Vec<_>>();
    samples.sort_unstable();
    samples[SAMPLES / 2]
}

fn assert_analysis(rows: &[ParsedRow], expected_messages: usize) {
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn("scale").unwrap();
    for row in rows.iter().cloned() {
        engine.ingest(row).unwrap();
    }
    assert_eq!(engine.row_count(), expected_messages + 1);
    let analysis = engine.analyze().unwrap();
    assert_eq!(analysis.messages.len(), expected_messages);
    assert_eq!(
        analysis.usage.model_calls_with_usage as usize,
        expected_messages
    );
    assert!(matches!(analysis.status, TurnStatus::Terminal(_)));
    black_box(analysis);
}

fn terminal_analysis_work(rows: &[ParsedRow]) -> u64 {
    let (analysis, work) = analyze_with_work(rows);
    let analysis = analysis.unwrap();
    assert!(matches!(analysis.status, TurnStatus::Terminal(_)));
    work
}

fn analyze_with_work(rows: &[ParsedRow]) -> (Result<TranscriptAnalysis, TranscriptError>, u64) {
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn("scale").unwrap();
    for row in rows.iter().cloned() {
        engine.ingest(row).unwrap();
    }
    let (analysis, work) = engine.analyze_with_work();
    (analysis, work.element_visits())
}

fn transcript_rows(assistant_rows: usize) -> Vec<ParsedRow> {
    assert!(assistant_rows > 0);
    let mut encoded = Vec::with_capacity(assistant_rows + 1);
    encoded.push(
        r#"{"parentUuid":null,"sessionId":"scale-session","type":"user","message":{"role":"user","content":"scale"},"uuid":"user-00000000","promptSource":"typed","promptId":"prompt-0"}"#
            .to_owned(),
    );
    let mut parent = "user-00000000".to_owned();
    for index in 0..assistant_rows {
        let uuid = format!("assistant-{index:08}");
        let stop_reason = if index + 1 == assistant_rows {
            r#""end_turn""#
        } else {
            "null"
        };
        encoded.push(format!(
            r#"{{"parentUuid":"{parent}","sessionId":"scale-session","type":"assistant","requestId":"request-{index:08}","uuid":"{uuid}","message":{{"id":"message-{index:08}","model":"scale-model","content":[{{"type":"text","text":"x"}}],"stop_reason":{stop_reason},"usage":{{"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#,
        ));
        parent = uuid;
    }
    parse_rows(encoded)
}

fn sidechain_transcript_rows(sidechain_rows: usize) -> Vec<ParsedRow> {
    assert!(sidechain_rows > 0);
    let mut encoded = vec![
        r#"{"parentUuid":null,"sessionId":"scale-session","type":"user","message":{"role":"user","content":"scale"},"uuid":"user-00000000","promptSource":"typed","promptId":"prompt-0"}"#.to_owned(),
        r#"{"parentUuid":"user-00000000","sessionId":"scale-session","type":"assistant","requestId":"main-request","uuid":"main-final","message":{"id":"main-message","model":"scale-model","content":[{"type":"text","text":"done"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#.to_owned(),
    ];
    let mut parent = "user-00000000".to_owned();
    for index in 0..sidechain_rows {
        let uuid = format!("side-{index:08}");
        encoded.push(format!(
            r#"{{"parentUuid":"{parent}","isSidechain":true,"sessionId":"scale-session","type":"assistant","requestId":"side-request-{index:08}","uuid":"{uuid}","message":{{"id":"side-message-{index:08}","model":"scale-model","content":[{{"type":"text","text":"side"}}],"stop_reason":"end_turn","usage":{{"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#,
        ));
        parent = uuid;
    }
    parse_rows(encoded)
}

fn reversed_parent_rows(assistant_rows: usize) -> Vec<ParsedRow> {
    assert!(assistant_rows >= 2);
    let mut encoded = vec![
        r#"{"parentUuid":null,"sessionId":"scale-session","type":"user","message":{"role":"user","content":"scale"},"uuid":"user-00000000","promptSource":"typed","promptId":"prompt-0"}"#.to_owned(),
    ];
    for index in 0..assistant_rows {
        let parent = if index + 1 == assistant_rows {
            "user-00000000".to_owned()
        } else {
            format!("reversed-{:08}", index + 1)
        };
        encoded.push(format!(
            r#"{{"parentUuid":"{parent}","sessionId":"scale-session","type":"assistant","requestId":"reversed-request-{index:08}","uuid":"reversed-{index:08}","message":{{"id":"reversed-message-{index:08}","model":"scale-model","content":[{{"type":"text","text":"x"}}],"stop_reason":"end_turn","usage":{{"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#,
        ));
    }
    parse_rows(encoded)
}

fn parse_rows(encoded: Vec<String>) -> Vec<ParsedRow> {
    let parser = JsonlParser::new(ParseMode::Strict);
    encoded
        .into_iter()
        .enumerate()
        .map(|(index, bytes)| {
            parser
                .parse(&CompleteLine {
                    location: SourceLocation {
                        line: u64::try_from(index + 1).unwrap(),
                        byte_offset: 0,
                    },
                    bytes: bytes.into_bytes(),
                })
                .unwrap()
        })
        .collect()
}
