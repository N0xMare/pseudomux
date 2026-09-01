use std::fs;

use pseudomux_claude::{
    CompleteLine, CursorChange, FileIdentity, FileMetadata, JsonlParser, ParseMode, RowKind,
    SourceLocation, TranscriptCursor, TranscriptError,
};

const IDENTITY: FileIdentity = FileIdentity {
    device: 10,
    inode: 20,
};

#[test]
fn cursor_frames_fixture_for_every_chunk_size() {
    let bytes = fs::read("tests/fixtures/fragmented_tool_turn.jsonl").unwrap();
    let expected: Vec<Vec<u8>> = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(<[u8]>::to_vec)
        .collect();

    for chunk_size in 1..=bytes.len() {
        let mut cursor = TranscriptCursor::new();
        let observation = cursor.observe(FileMetadata {
            identity: IDENTITY,
            len: bytes.len() as u64,
        });
        assert_eq!(observation.read_from, 0, "chunk size {chunk_size}");
        let mut actual = Vec::new();
        let mut offset = 0;
        for chunk in bytes.chunks(chunk_size) {
            let update = cursor.push(IDENTITY, offset, chunk).unwrap();
            actual.extend(update.lines.into_iter().map(|line| line.bytes));
            offset += chunk.len() as u64;
        }
        assert_eq!(actual, expected, "chunk size {chunk_size}");
        assert!(!cursor.has_partial_line(), "chunk size {chunk_size}");
        assert_eq!(cursor.next_offset(), bytes.len() as u64);
    }
}

#[test]
fn cursor_holds_partial_line_until_newline_arrives() {
    let first = br#"{"type":"system""#;
    let second = b"}\n";
    let mut cursor = TranscriptCursor::new();
    cursor.observe(FileMetadata {
        identity: IDENTITY,
        len: first.len() as u64,
    });
    let update = cursor.push(IDENTITY, 0, first).unwrap();
    assert!(update.lines.is_empty());
    assert!(cursor.has_partial_line());

    cursor.observe(FileMetadata {
        identity: IDENTITY,
        len: (first.len() + second.len()) as u64,
    });
    let update = cursor.push(IDENTITY, first.len() as u64, second).unwrap();
    assert_eq!(update.lines.len(), 1);
    assert_eq!(update.lines[0].bytes, br#"{"type":"system"}"#);
    assert!(!cursor.has_partial_line());
}

/// Regression, minimized from libFuzzer
/// `crash-3f4d09ccb50dbe940c95532eb00a64f65f24f3da` (`transcript_cursor`,
/// Gate A 2026-07-27). The cursor strips **exactly one** trailing carriage
/// return, which is CRLF normalization and nothing more. A source line ending
/// in several CRs therefore still ends in `\r` after framing, and callers must
/// not assume otherwise. The fuzz target had asserted the stronger
/// "no line ever ends with CR", which this behavior legitimately violates.
///
/// Seed corpus: `fuzz/corpus/transcript_cursor/regression-multi-cr-line`.
#[test]
fn cursor_strips_exactly_one_trailing_carriage_return() {
    for (source, expected) in [
        (b"a\n".as_slice(), b"a".as_slice()),
        (b"a\r\n".as_slice(), b"a".as_slice()),
        (b"a\r\r\n".as_slice(), b"a\r".as_slice()),
        (b"a\r\r\r\n".as_slice(), b"a\r\r".as_slice()),
        (b"\r\n".as_slice(), b"".as_slice()),
        (b"\r\r\n".as_slice(), b"\r".as_slice()),
    ] {
        let mut cursor = TranscriptCursor::new();
        cursor.observe(FileMetadata {
            identity: IDENTITY,
            len: source.len() as u64,
        });
        let update = cursor.push(IDENTITY, 0, source).unwrap();
        assert_eq!(
            update.lines.len(),
            1,
            "source {source:?} must frame one line"
        );
        assert_eq!(
            update.lines[0].bytes, expected,
            "source {source:?} must strip exactly one trailing CR"
        );
        assert!(
            !update.lines[0].bytes.contains(&b'\n'),
            "framed lines never contain LF"
        );
        assert!(!cursor.has_partial_line());
    }
}

#[test]
fn cursor_resets_partial_state_on_truncation_and_replacement() {
    let mut cursor = TranscriptCursor::new();
    cursor.observe(FileMetadata {
        identity: IDENTITY,
        len: 8,
    });
    cursor.push(IDENTITY, 0, b"partial!").unwrap();
    let truncated = cursor.observe(FileMetadata {
        identity: IDENTITY,
        len: 2,
    });
    assert_eq!(
        truncated.change,
        CursorChange::Truncated {
            previous_offset: 8,
            current_len: 2,
        }
    );
    assert_eq!(truncated.read_from, 0);
    assert!(!cursor.has_partial_line());

    cursor.push(IDENTITY, 0, b"x\n").unwrap();
    let replacement = FileIdentity {
        device: 10,
        inode: 21,
    };
    let replaced = cursor.observe(FileMetadata {
        identity: replacement,
        len: 2,
    });
    assert_eq!(
        replaced.change,
        CursorChange::Replaced {
            previous: IDENTITY,
            current: replacement,
        }
    );
    assert_eq!(replaced.read_from, 0);
    assert_eq!(cursor.generation(), 3);
}

#[test]
fn cursor_rejects_gaps_wrong_identity_and_reads_beyond_stat() {
    let mut cursor = TranscriptCursor::new();
    cursor.observe(FileMetadata {
        identity: IDENTITY,
        len: 3,
    });
    assert!(matches!(
        cursor.push(IDENTITY, 1, b"x"),
        Err(TranscriptError::CursorOffsetMismatch { .. })
    ));
    assert!(matches!(
        cursor.push(
            FileIdentity {
                device: 10,
                inode: 99,
            },
            0,
            b"x"
        ),
        Err(TranscriptError::FileIdentityMismatch { .. })
    ));
    assert!(matches!(
        cursor.push(IDENTITY, 0, b"four"),
        Err(TranscriptError::ReadBeyondObservedFile { .. })
    ));
}

#[test]
fn cursor_can_arm_at_eof_without_reading_history() {
    let mut cursor = TranscriptCursor::new();
    cursor.seek_to_eof(FileMetadata {
        identity: IDENTITY,
        len: 10_000_000_000,
    });
    assert_eq!(cursor.next_offset(), 10_000_000_000);
    let observation = cursor.observe(FileMetadata {
        identity: IDENTITY,
        len: 10_000_000_004,
    });
    assert_eq!(observation.read_from, 10_000_000_000);
    assert_eq!(observation.read_to, 10_000_000_004);
}

#[test]
fn cursor_rejects_an_unbounded_unterminated_record() {
    let limit = pseudomux_claude::MAX_TRANSCRIPT_LINE_BYTES;
    let first = vec![b'x'; limit];
    let mut cursor = TranscriptCursor::new();
    cursor.observe(FileMetadata {
        identity: IDENTITY,
        len: limit as u64,
    });
    cursor.push(IDENTITY, 0, &first).unwrap();
    cursor.observe(FileMetadata {
        identity: IDENTITY,
        len: limit as u64 + 1,
    });
    assert!(matches!(
        cursor.push(IDENTITY, limit as u64, b"x"),
        Err(TranscriptError::LineTooLong { .. })
    ));
}

#[test]
fn parser_rejects_malformed_json_and_invalid_utf8() {
    let parser = JsonlParser::new(ParseMode::Strict);
    let malformed = CompleteLine {
        location: SourceLocation {
            line: 7,
            byte_offset: 80,
        },
        bytes: br#"{"type":"assistant""#.to_vec(),
    };
    assert!(matches!(
        parser.parse(&malformed),
        Err(TranscriptError::MalformedJson { .. })
    ));

    let invalid_utf8 = CompleteLine {
        location: SourceLocation {
            line: 8,
            byte_offset: 100,
        },
        bytes: vec![b'{', 0xff, b'}'],
    };
    assert!(matches!(
        parser.parse(&invalid_utf8),
        Err(TranscriptError::InvalidUtf8 { .. })
    ));
}

#[test]
fn parser_rejects_usage_above_the_protocol_safe_integer_maximum() {
    let parser = JsonlParser::new(ParseMode::Strict);
    let maximum = pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER;
    let boundary = line(
        format!(
            r#"{{"type":"assistant","uuid":"usage-boundary","message":{{"id":"message","content":[],"usage":{{"input_tokens":{maximum}}}}}}}"#
        )
        .as_bytes(),
    );
    assert!(parser.parse(&boundary).is_ok());

    let above = line(
        format!(
            r#"{{"type":"assistant","uuid":"usage-above","message":{{"id":"message","content":[],"usage":{{"input_tokens":{}}}}}}}"#,
            maximum + 1
        )
        .as_bytes(),
    );
    assert!(matches!(
        parser.parse(&above),
        Err(TranscriptError::SchemaDrift { ref path, .. })
            if path == "$.message.usage.input_tokens"
    ));
}

#[test]
fn parser_preserves_unknown_rows_and_content_blocks() {
    let parser = JsonlParser::new(ParseMode::Strict);
    let unknown_row =
        line(br#"{"type":"future-event","uuid":"future-1","newField":{"nested":true}}"#);
    let parsed = parser.parse(&unknown_row).unwrap();
    assert!(matches!(
        parsed.kind,
        RowKind::Unknown {
            declared_type: Some(ref value)
        } if value == "future-event"
    ));
    assert_eq!(parsed.raw["newField"]["nested"], true);

    let assistant = line(
        br#"{"type":"assistant","uuid":"assistant-1","message":{"id":"message-1","content":[{"type":"future-block","payload":42}],"stop_reason":null}}"#,
    );
    let parsed = parser.parse(&assistant).unwrap();
    let RowKind::Assistant(fragment) = parsed.kind else {
        panic!("expected assistant");
    };
    assert!(matches!(
        &fragment.blocks[0],
        pseudomux_claude::ContentBlock::Unknown {
            declared_type: Some(value),
            raw,
        } if value == "future-block" && raw["payload"] == 42
    ));
}

#[test]
fn strict_parser_rejects_unknown_sibling_in_tool_result_row_at_block_path() {
    let mixed = line(
        br#"{"type":"user","uuid":"result","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-1","content":"ok"},{"type":"future_user_control","payload":true}]}}"#,
    );
    let error = JsonlParser::new(ParseMode::Strict)
        .parse(&mixed)
        .unwrap_err();
    assert!(matches!(
        error,
        TranscriptError::SchemaDrift {
            row_uuid: Some(ref row_uuid),
            ref path,
            ..
        } if row_uuid == "result" && path == "$.message.content[1]"
    ));

    let noncausal = line(
        br#"{"type":"user","uuid":"context","message":{"role":"user","content":[{"type":"future_user_control"}]}}"#,
    );
    assert!(matches!(
        JsonlParser::new(ParseMode::Strict)
            .parse(&noncausal)
            .unwrap()
            .kind,
        RowKind::UserOther
    ));
}

#[test]
fn strict_parser_rejects_wrong_user_message_role() {
    let parser = JsonlParser::new(ParseMode::Strict);
    let wrong_role = line(
        br#"{"type":"user","uuid":"wrong-role","promptSource":"typed","message":{"role":"assistant","content":"hello"}}"#,
    );
    assert!(matches!(
        parser.parse(&wrong_role),
        Err(TranscriptError::SchemaDrift { ref path, .. })
            if path == "$.message.role"
    ));

    let legacy = line(
        br#"{"type":"user","uuid":"legacy","promptSource":"typed","message":{"content":"hello"}}"#,
    );
    assert!(matches!(
        parser.parse(&legacy).unwrap().kind,
        RowKind::TypedUser { ref prompt, .. } if prompt == "hello"
    ));
}

#[test]
fn strict_parser_validates_turn_duration_shape_and_semantic_payload() {
    let parser = JsonlParser::new(ParseMode::Strict);
    for row in [
        line(br#"{"type":"system","subtype":"turn_duration","uuid":"duration"}"#),
        line(
            br#"{"type":"system","subtype":"turn_duration","uuid":"duration","durationMs":42}"#,
        ),
        // DELIBERATE CHANGE: this row previously carried `pendingWorkflowCount:1`
        // and was asserted admitted, so the test encoded the permissive
        // behaviour rather than the guarantee. `pendingWorkflowCount` is a
        // continuation signal (written by 2.1.177, absent on 2.1.207+), and the
        // drain is now graduated on this marker, so a marker that claims the
        // turn is over while announcing pending work must not be admitted. Zero
        // is still proof of inertness and is what this row now asserts; the
        // non-zero case moved to the rejection table below.
        line(
            br#"{"type":"system","subtype":"turn_duration","uuid":"duration","durationMs":42,"messageCount":3,"pendingWorkflowCount":0}"#,
        ),
    ] {
        assert!(matches!(parser.parse(&row).unwrap().kind, RowKind::System(_)));
    }

    for (row, expected_path) in [
        (
            line(
                br#"{"type":"system","subtype":"turn_duration","uuid":"duration","durationMs":42,"messageCount":3,"pendingWorkflowCount":1}"#,
            ),
            "$.pendingWorkflowCount",
        ),
        (
            line(
                br#"{"type":"system","subtype":"turn_duration","uuid":"duration","durationMs":42,"pendingWorkflowCount":"one"}"#,
            ),
            "$.pendingWorkflowCount",
        ),
        (
            line(
                br#"{"type":"system","subtype":"turn_duration","uuid":"duration","durationMs":-1}"#,
            ),
            "$.durationMs",
        ),
        (
            line(
                br#"{"type":"system","subtype":"turn_duration","uuid":"duration","durationMs":42,"messageCount":"three"}"#,
            ),
            "$.messageCount",
        ),
        (
            line(
                br#"{"type":"system","subtype":"turn_duration","uuid":"duration","durationMs":42,"message":{"role":"system","content":[{"type":"future_block"}]}}"#,
            ),
            "$.message",
        ),
        (
            line(
                br#"{"type":"system","subtype":"turn_duration","uuid":"duration","durationMs":42,"content":[{"type":"future_block"}]}"#,
            ),
            "$.content",
        ),
        (
            line(
                br#"{"type":"system","subtype":"turn_duration","uuid":"duration","durationMs":42,"attachment":{"type":"future_block"}}"#,
            ),
            "$.attachment",
        ),
    ] {
        assert!(matches!(
            parser.parse(&row),
            Err(TranscriptError::SchemaDrift { ref path, .. }) if path == expected_path
        ));
    }
}

#[test]
fn strict_parser_admits_a_stop_hook_summary_only_when_its_payload_proves_inertness() {
    let parser = JsonlParser::new(ParseMode::Strict);

    // Accepted: the observed shape, and the same shape with the two optional
    // feedback arrays absent rather than empty.
    for row in [
        line(
            br#"{"type":"system","subtype":"stop_hook_summary","uuid":"summary","hookCount":1,"hookErrors":[],"hookAdditionalContext":[],"preventedContinuation":false,"stopReason":"","hasOutput":false,"level":"suggestion"}"#,
        ),
        line(
            br#"{"type":"system","subtype":"stop_hook_summary","uuid":"summary","hookCount":1,"preventedContinuation":false}"#,
        ),
    ] {
        let parsed = parser.parse(&row).unwrap();
        let RowKind::System(ref system) = parsed.kind else {
            panic!("expected a system row: {parsed:#?}");
        };
        assert_eq!(system.subtype.as_deref(), Some("stop_hook_summary"));
        assert!(system.is_proven_inert_marker());
    }

    for (row, expected_path) in [
        // A blocking Stop hook makes Claude continue the turn, so this row is
        // not a completion marker at all.
        (
            line(
                br#"{"type":"system","subtype":"stop_hook_summary","uuid":"summary","hookCount":1,"hookErrors":[],"hookAdditionalContext":[],"preventedContinuation":true}"#,
            ),
            "$.preventedContinuation",
        ),
        // Absence must never read as false: an unprovable guarantee is drift.
        (
            line(
                br#"{"type":"system","subtype":"stop_hook_summary","uuid":"summary","hookCount":1,"hookErrors":[],"hookAdditionalContext":[]}"#,
            ),
            "$.preventedContinuation",
        ),
        // A renamed or retyped field is the same loss of proof.
        (
            line(
                br#"{"type":"system","subtype":"stop_hook_summary","uuid":"summary","hookCount":1,"blockedContinuation":false}"#,
            ),
            "$.preventedContinuation",
        ),
        (
            line(
                br#"{"type":"system","subtype":"stop_hook_summary","uuid":"summary","hookCount":1,"preventedContinuation":null}"#,
            ),
            "$.preventedContinuation",
        ),
        (
            line(
                br#"{"type":"system","subtype":"stop_hook_summary","uuid":"summary","hookCount":1,"preventedContinuation":"false"}"#,
            ),
            "$.preventedContinuation",
        ),
        (
            line(
                br#"{"type":"system","subtype":"stop_hook_summary","uuid":"summary","hookCount":1,"preventedContinuation":false,"hookErrors":["pmux-hook exited 1"]}"#,
            ),
            "$.hookErrors",
        ),
        (
            line(
                br#"{"type":"system","subtype":"stop_hook_summary","uuid":"summary","hookCount":1,"preventedContinuation":false,"hookErrors":{}}"#,
            ),
            "$.hookErrors",
        ),
        (
            line(
                br#"{"type":"system","subtype":"stop_hook_summary","uuid":"summary","hookCount":1,"preventedContinuation":false,"hookAdditionalContext":["keep going"]}"#,
            ),
            "$.hookAdditionalContext",
        ),
        // The same payload guard turn_duration carries: a future semantic
        // payload must not hide behind the allowlisted subtype.
        (
            line(
                br#"{"type":"system","subtype":"stop_hook_summary","uuid":"summary","hookCount":1,"preventedContinuation":false,"message":{"role":"assistant","content":[{"type":"text","text":"more"}]}}"#,
            ),
            "$.message",
        ),
        (
            line(
                br#"{"type":"system","subtype":"stop_hook_summary","uuid":"summary","hookCount":1,"preventedContinuation":false,"content":[{"type":"text","text":"more"}]}"#,
            ),
            "$.content",
        ),
        (
            line(
                br#"{"type":"system","subtype":"stop_hook_summary","uuid":"summary","hookCount":1,"preventedContinuation":false,"attachment":{"type":"future_block"}}"#,
            ),
            "$.attachment",
        ),
    ] {
        assert!(
            matches!(
                parser.parse(&row),
                Err(TranscriptError::SchemaDrift { ref path, .. }) if path == expected_path
            ),
            "expected drift at {expected_path} for {}",
            String::from_utf8_lossy(&row.bytes)
        );
    }
}

/// `api_error` is admitted for the opposite reason the inert markers are: it
/// proves a retry is in flight and the turn is *not* over. The proof required is
/// that the row really is a retry record, so both counters must be present as
/// integers and no semantic payload may hide behind the subtype.
#[test]
fn strict_parser_admits_an_api_error_only_when_its_payload_proves_it_is_a_retry_record() {
    let parser = JsonlParser::new(ParseMode::Strict);

    // Accepted: the observed field shape, the same row at exhaustion (never
    // observed in the wild, admitted anyway and left non-terminal), and a
    // zero-valued attempt, which is a non-negative integer like any other.
    for row in [
        line(
            br#"{"parentUuid":"a","isSidechain":false,"type":"system","subtype":"api_error","error":"Connection error (ECONNRESET)","level":"error","retryAttempt":1,"maxRetries":10,"retryInMs":1000,"uuid":"retry","timestamp":"2026-07-30T01:28:03.001Z","sessionId":"s","cwd":"/tmp","gitBranch":"HEAD","version":"2.1.220","entrypoint":"cli","userType":"external","slug":"go"}"#,
        ),
        line(
            br#"{"type":"system","subtype":"api_error","uuid":"retry","error":"Request timed out.","level":"error","retryAttempt":10,"maxRetries":10}"#,
        ),
        line(
            br#"{"type":"system","subtype":"api_error","uuid":"retry","error":"Connection interrupted by system sleep","level":"error","retryAttempt":0,"maxRetries":10}"#,
        ),
    ] {
        let parsed = parser.parse(&row).unwrap();
        let RowKind::System(ref system) = parsed.kind else {
            panic!("expected a system row: {parsed:#?}");
        };
        assert_eq!(system.subtype.as_deref(), Some("api_error"));
        assert!(system.is_admitted_on_active_chain());
        assert!(system.is_retry_in_flight_marker());
        assert!(
            !system.is_proven_inert_marker(),
            "an api_error means the turn is NOT over, so it must never be an inert marker"
        );
    }

    for (row, expected_path) in [
        // Absence is not zero and not permission: without the counters the row
        // has lost the only evidence that a retry is what is being reported.
        (
            line(
                br#"{"type":"system","subtype":"api_error","uuid":"retry","error":"Connection error","level":"error","maxRetries":10}"#,
            ),
            "$.retryAttempt",
        ),
        (
            line(
                br#"{"type":"system","subtype":"api_error","uuid":"retry","error":"Connection error","level":"error","retryAttempt":1}"#,
            ),
            "$.maxRetries",
        ),
        (
            line(br#"{"type":"system","subtype":"api_error","uuid":"retry"}"#),
            "$.retryAttempt",
        ),
        // A renamed or retyped counter is the same loss of proof.
        (
            line(
                br#"{"type":"system","subtype":"api_error","uuid":"retry","retryAttempt":null,"maxRetries":10}"#,
            ),
            "$.retryAttempt",
        ),
        (
            line(
                br#"{"type":"system","subtype":"api_error","uuid":"retry","retryAttempt":"1","maxRetries":10}"#,
            ),
            "$.retryAttempt",
        ),
        (
            line(
                br#"{"type":"system","subtype":"api_error","uuid":"retry","retryAttempt":1.5,"maxRetries":10}"#,
            ),
            "$.retryAttempt",
        ),
        (
            line(
                br#"{"type":"system","subtype":"api_error","uuid":"retry","retryAttempt":1,"maxRetries":-1}"#,
            ),
            "$.maxRetries",
        ),
        (
            line(
                br#"{"type":"system","subtype":"api_error","uuid":"retry","retryAttempt":1,"maxRetries":{"limit":10}}"#,
            ),
            "$.maxRetries",
        ),
        // The same payload guard the inert markers carry.
        (
            line(
                br#"{"type":"system","subtype":"api_error","uuid":"retry","retryAttempt":1,"maxRetries":10,"message":{"role":"assistant","content":[{"type":"text","text":"partial answer"}]}}"#,
            ),
            "$.message",
        ),
        (
            line(
                br#"{"type":"system","subtype":"api_error","uuid":"retry","retryAttempt":1,"maxRetries":10,"content":[{"type":"text","text":"partial answer"}]}"#,
            ),
            "$.content",
        ),
        (
            line(
                br#"{"type":"system","subtype":"api_error","uuid":"retry","retryAttempt":1,"maxRetries":10,"attachment":{"type":"future_block"}}"#,
            ),
            "$.attachment",
        ),
    ] {
        assert!(
            matches!(
                parser.parse(&row),
                Err(TranscriptError::SchemaDrift { ref path, .. }) if path == expected_path
            ),
            "expected drift at {expected_path} for {}",
            String::from_utf8_lossy(&row.bytes)
        );
    }
}

#[test]
fn current_session_control_rows_are_typed_metadata() {
    let parser = JsonlParser::new(ParseMode::Strict);
    // `atis-latch` and `cost-state` are the two record types Claude Code 2.1.257
    // added on linux/x86_64; the rest predate it. This is the enumerating table
    // for `is_metadata_record`, so a record type admitted there belongs here.
    for record_type in [
        "mode",
        "permission-mode",
        "ai-title",
        "atis-latch",
        "cost-state",
    ] {
        let row = line(format!(r#"{{"type":"{record_type}","sessionId":"s"}}"#).as_bytes());
        let parsed = parser.parse(&row).unwrap();
        assert!(matches!(
            parsed.kind,
            RowKind::Metadata {
                record_type: ref parsed_type
            } if parsed_type == record_type
        ));
    }
}

/// MEASURED on Claude Code 2.1.257 linux/x86_64, `SessionCell::Minified`: the
/// real field shapes of the three rows 2.1.236 did not write, with the session
/// URL replaced by a placeholder. `atis-latch` sits third in the launch
/// preamble and `cost-state` after the turn; `remote_session_change` follows
/// `total_tokens_reminder` on the typed prompt. Strict mode must type all
/// three, and must type them from the name alone -- no field is read.
#[test]
fn measured_2_1_257_records_are_typed_metadata_and_attachment() {
    let parser = JsonlParser::new(ParseMode::Strict);

    for (row, record_type) in [
        (
            line(br#"{"type":"atis-latch","atis":"","sessionId":"s"}"#),
            "atis-latch",
        ),
        (
            line(
                br#"{"type":"cost-state","sessionId":"s","totalCostUSD":0,"totalAPIDuration":0,"totalDuration":2712,"startTime":1788294529829,"modelUsage":{}}"#,
            ),
            "cost-state",
        ),
    ] {
        let parsed = parser.parse(&row).unwrap();
        assert!(
            matches!(
                parsed.kind,
                RowKind::Metadata {
                    record_type: ref parsed_type
                } if parsed_type == record_type
            ),
            "{record_type} must parse as metadata: {:?}",
            parsed.kind
        );
    }

    let remote = line(
        br#"{"parentUuid":"reminder","sessionId":"s","type":"attachment","uuid":"remote","attachment":{"type":"remote_session_change","url":"https://claude.ai/code/session_PLACEHOLDER","commit":"Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>\nClaude-Session: https://claude.ai/code/session_PLACEHOLDER","pr":"Generated with Claude Code\n\nhttps://claude.ai/code/session_PLACEHOLDER","sendUserFileHint":false}}"#,
    );
    let parsed = parser.parse(&remote).unwrap();
    assert!(matches!(
        parsed.kind,
        RowKind::Attachment {
            ref attachment_type
        } if attachment_type == "remote_session_change"
    ));
    // Admitted by name only: the payload is still verbatim on the raw row, and
    // nothing in the parser has promised to interpret it.
    assert_eq!(parsed.raw["attachment"]["sendUserFileHint"], false);
}

/// The other names a `strings` scan of the 2.1.257 binary turned up. None of
/// them was OBSERVED on a minified pool cell, so strict mode still refuses
/// them: an unobserved kind must fail closed until it is measured, which is the
/// only reason the three above could be admitted with confidence.
#[test]
fn unobserved_2_1_257_kinds_are_still_refused() {
    let parser = JsonlParser::new(ParseMode::Strict);

    // A new attachment type is drift, not a new inert node: the row could carry
    // tool output, and admitting it by name would put that on the active chain.
    let tool_host_result_lines = line(
        br#"{"parentUuid":"u","sessionId":"s","type":"attachment","uuid":"a","attachment":{"type":"tool_host_result_lines"}}"#,
    );
    assert!(matches!(
        parser.parse(&tool_host_result_lines),
        Err(TranscriptError::SchemaDrift { ref path, .. }) if path == "$.attachment.type"
    ));

    // A new top-level record type is `Unknown`, which the engine refuses on the
    // active parent chain and the pool's assert-empty proof refuses outright.
    // (`tool_host_result` and `cloud_session_status` are system SUBTYPES rather
    // than top-level types; their refusal is proved by
    // `unobserved_2_1_257_system_subtypes_are_refused_on_the_active_chain` in
    // transcript_engine.rs.)
    let fork_briefing = line(br#"{"type":"fork_briefing","sessionId":"s"}"#);
    let parsed = parser.parse(&fork_briefing).unwrap();
    assert!(matches!(
        parsed.kind,
        RowKind::Unknown {
            declared_type: Some(ref declared)
        } if declared == "fork_briefing"
    ));
}

fn line(bytes: &[u8]) -> CompleteLine {
    CompleteLine {
        location: SourceLocation {
            line: 1,
            byte_offset: 0,
        },
        bytes: bytes.to_vec(),
    }
}
