use std::fs;

use pretty_assertions::assert_eq;
use pseudomux_claude::{
    CompleteLine, EngineWarning, IngestOutcome, JsonlParser, LogicalMessageKey, ParseMode,
    ParsedRow, RowKind, SourceLocation, StopReason, TerminalOutcome, TranscriptEngine,
    TranscriptError, TurnStatus,
};

/// MEASURED on Claude Code 2.1.257 linux/x86_64, `SessionCell::Minified`: the
/// post-turn `cost-state` row, verbatim but for the session id. It carries no
/// `uuid`, no `parentUuid` and no `timestamp`.
const COST_STATE_ROW: &str = r#"{"type":"cost-state","sessionId":"s","totalCostUSD":0,"totalAPIDuration":0,"totalDuration":2712,"startTime":1788294529829,"modelUsage":{}}"#;

#[test]
fn fragmented_tool_turn_is_grouped_correlated_and_deduplicated() {
    let rows = fixture("fragmented_tool_turn.jsonl", ParseMode::Strict);
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    for row in rows.iter().take(2).cloned() {
        engine.ingest(row).unwrap();
    }
    engine.arm_turn("Inspect README").unwrap();
    for row in rows.into_iter().skip(2) {
        engine.ingest(row).unwrap();
    }

    let analysis = engine.analyze().unwrap();
    let TurnStatus::Terminal(final_turn) = &analysis.status else {
        panic!("expected terminal analysis: {analysis:#?}");
    };
    assert_eq!(final_turn.outcome, TerminalOutcome::Completed);
    assert_eq!(final_turn.final_text, "Done.");
    assert_eq!(final_turn.final_text_blocks, ["Done."]);
    assert!(!final_turn.final_text.contains("The read succeeded"));
    assert!(!final_turn.final_text.contains("stale"));
    assert!(!final_turn.final_text.contains("SIDECHAIN"));
    assert!(!final_turn.final_text.contains("TEAM"));
    assert!(!final_turn.final_text.contains("META"));

    assert_eq!(analysis.messages.len(), 2);
    assert_eq!(
        analysis.messages[0].key,
        LogicalMessageKey::MessageId("msg-tool".to_owned())
    );
    assert_eq!(analysis.messages[0].row_uuids.len(), 2);
    assert_eq!(analysis.messages[1].row_uuids.len(), 2);
    assert_eq!(analysis.usage.model_calls_with_usage, 2);
    assert_eq!(analysis.usage.tokens.input_tokens, 220);
    assert_eq!(analysis.usage.tokens.output_tokens, 8);
    assert_eq!(analysis.usage.tokens.cache_creation_input_tokens, 2);
    assert_eq!(analysis.usage.tokens.cache_read_input_tokens, 7);
    assert_eq!(analysis.sidechain_usage.model_calls_with_usage, 1);
    assert_eq!(analysis.sidechain_usage.tokens.input_tokens, 900);
    assert_eq!(analysis.sidechain_usage.tokens.output_tokens, 900);
    assert_eq!(analysis.combined_usage.tokens.input_tokens, 1_120);
    assert_eq!(analysis.combined_usage.tokens.output_tokens, 908);

    assert_eq!(analysis.tools.len(), 1);
    assert_eq!(analysis.tools[0].tool_use_id, "tool-1");
    assert_eq!(analysis.tools[0].name, "Read");
    assert_eq!(analysis.tools[0].input["file_path"], "README.md");
    assert_eq!(
        analysis.tools[0].result.as_ref().unwrap().content,
        "# Project"
    );
    assert!(analysis.turn_duration_seen);
    assert_eq!(analysis.warnings, []);
}

#[test]
fn historical_terminal_and_off_branch_terminal_cannot_complete_turn() {
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.ingest(parse(user(None, "old-user", "old"))).unwrap();
    engine
        .ingest(parse(assistant(
            "old-user",
            "old-answer",
            Some("old-message"),
            None,
            text("stale"),
            Some("end_turn"),
            100,
            100,
        )))
        .unwrap();
    engine.arm_turn("new").unwrap();
    engine
        .ingest(parse(user(Some("old-answer"), "new-user", "new")))
        .unwrap();
    engine
        .ingest(parse(assistant_with_scope(
            "new-user",
            "side-answer",
            Some("side-message"),
            None,
            text("side"),
            Some("end_turn"),
            50,
            50,
            r#", "isSidechain":true"#,
        )))
        .unwrap();

    let analysis = engine.analyze().unwrap();
    assert!(matches!(analysis.status, TurnStatus::Running { .. }));
    assert!(analysis.messages.is_empty());
    assert_eq!(analysis.usage.tokens.input_tokens, 0);
}

/// A sidechain row that carries NO usage is still counted.
///
/// This is the exact residue the pool's sidechain guard used to leave. That
/// guard has two halves -- a row count and `usage.sidechain` -- and until now
/// the row count did not exist in production, so the token half was the whole
/// guard. A `Task` subagent whose rows report no usage leaves
/// `sidechain_usage` at its default; the turn then commits, tokens correct, and
/// the isolation claim it should have refuted goes unmade.
///
/// The two assertions are a pair on purpose: `sidechain_usage` at its default
/// AND `sidechain_rows` positive, on the same analysis. Either alone is
/// satisfiable by a counter that copies the other.
#[test]
fn a_sidechain_row_with_no_usage_is_counted_even_though_it_moves_no_tokens() {
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn("count me").unwrap();
    engine.ingest(parse(user(None, "u", "count me"))).unwrap();
    // The row a `Task` subagent opens with: a user row on the sidechain. It has
    // no `usage` field anywhere, because a user row never carries one.
    engine
        .ingest(parse(
            r#"{"parentUuid":"u","isSidechain":true,"sessionId":"s","type":"user","message":{"role":"user","content":"sub-agent prompt"},"uuid":"side-user"}"#,
        ))
        .unwrap();
    engine
        .ingest(parse(assistant(
            "u",
            "answer",
            Some("msg-main"),
            None,
            text("done"),
            Some("end_turn"),
            11,
            2,
        )))
        .unwrap();

    let analysis = engine.analyze().unwrap();
    assert_eq!(
        analysis.sidechain_usage,
        Default::default(),
        "the token half of the guard sees nothing, which is the whole problem"
    );
    assert_eq!(
        analysis.sidechain_rows, 1,
        "a sidechain row that moved no tokens is still a sidechain row"
    );
    // And the main chain is untouched by it.
    assert_eq!(analysis.usage.tokens.input_tokens, 11);
}

/// A turn with no sidechain at all counts zero.
///
/// Control for the test above: without it, a counter that returned the number
/// of rows in the file, or a constant 1, would pass.
#[test]
fn a_turn_with_no_sidechain_counts_no_sidechain_rows() {
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn("clean").unwrap();
    engine.ingest(parse(user(None, "u", "clean"))).unwrap();
    engine
        .ingest(parse(assistant(
            "u",
            "answer",
            Some("msg-main"),
            None,
            text("done"),
            Some("end_turn"),
            11,
            2,
        )))
        .unwrap();

    assert_eq!(engine.analyze().unwrap().sidechain_rows, 0);
}

#[test]
fn interleaved_parallel_sidechain_messages_are_grouped_without_main_authority() {
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn("parallel").unwrap();
    engine.ingest(parse(user(None, "u", "parallel"))).unwrap();
    engine
        .ingest(parse(assistant_with_scope(
            "u",
            "side-a-1",
            Some("side-message-a"),
            None,
            thinking("a starts"),
            None,
            3,
            2,
            r#", "isSidechain":true"#,
        )))
        .unwrap();
    engine
        .ingest(parse(assistant_with_scope(
            "u",
            "side-b",
            Some("side-message-b"),
            None,
            text("b finishes"),
            Some("end_turn"),
            5,
            4,
            r#", "isSidechain":true"#,
        )))
        .unwrap();
    engine
        .ingest(parse(assistant_with_scope(
            "side-a-1",
            "side-a-2",
            Some("side-message-a"),
            None,
            text("a finishes"),
            Some("end_turn"),
            3,
            2,
            r#", "isSidechain":true"#,
        )))
        .unwrap();
    engine
        .ingest(parse(assistant(
            "u",
            "main-final",
            Some("main-message"),
            None,
            text("main only"),
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();

    let analysis = engine.analyze().unwrap();
    let TurnStatus::Terminal(final_turn) = analysis.status else {
        panic!("the main branch should remain terminal");
    };
    assert_eq!(final_turn.final_text, "main only");
    assert_eq!(analysis.messages.len(), 1);
    assert_eq!(analysis.usage.model_calls_with_usage, 1);
    assert_eq!(analysis.sidechain_usage.model_calls_with_usage, 2);
    assert_eq!(analysis.sidechain_usage.tokens.input_tokens, 8);
    assert_eq!(analysis.sidechain_usage.tokens.output_tokens, 6);
    assert_eq!(analysis.combined_usage.model_calls_with_usage, 3);
    assert_eq!(analysis.combined_usage.tokens.input_tokens, 9);
    assert_eq!(analysis.combined_usage.tokens.output_tokens, 7);
}

#[test]
fn supported_attachments_are_structural_active_chain_nodes_only() {
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn("go").unwrap();
    engine.ingest(parse(user(None, "u", "go"))).unwrap();
    engine
        .ingest(parse(attachment("u", "context-1", "deferred_tools_delta")))
        .unwrap();
    engine
        .ingest(parse(attachment("context-1", "context-2", "skill_listing")))
        .unwrap();
    engine
        .ingest(parse(assistant(
            "context-2",
            "answer",
            Some("message"),
            None,
            text("done"),
            Some("end_turn"),
            3,
            1,
        )))
        .unwrap();

    let analysis = engine.analyze().unwrap();
    let TurnStatus::Terminal(final_turn) = analysis.status else {
        panic!("expected terminal result");
    };
    assert_eq!(final_turn.final_text, "done");
    assert_eq!(analysis.messages.len(), 1);
    assert!(analysis.tools.is_empty());
}

#[test]
fn strict_admits_total_tokens_reminder_type_string() {
    let accepted = parse(
        r#"{"parentUuid":"u","sessionId":"s","type":"attachment","uuid":"a","attachment":{"type":"total_tokens_reminder","text":"<total_tokens>15000000 tokens left</total_tokens>"}}"#,
    );
    assert!(matches!(
        accepted.kind,
        RowKind::Attachment {
            ref attachment_type
        } if attachment_type == "total_tokens_reminder"
    ));
}

#[test]
fn measured_total_tokens_reminder_text_is_not_final_text() {
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn("go").unwrap();
    engine.ingest(parse(user(None, "u", "go"))).unwrap();
    engine
        .ingest(parse(
            r#"{"parentUuid":"u","sessionId":"s","type":"attachment","uuid":"reminder","attachment":{"type":"total_tokens_reminder","text":"<total_tokens>15000000 tokens left</total_tokens>"}}"#,
        ))
        .unwrap();
    engine
        .ingest(parse(assistant(
            "reminder",
            "answer",
            Some("message"),
            None,
            text("done"),
            Some("end_turn"),
            3,
            1,
        )))
        .unwrap();

    let analysis = engine.analyze().unwrap();
    let TurnStatus::Terminal(final_turn) = analysis.status else {
        panic!("expected terminal result");
    };
    assert_eq!(final_turn.final_text, "done");
    assert!(!final_turn.final_text.contains("15000000"));
    assert!(!final_turn.final_text.contains("tokens left"));
    assert!(analysis.tools.is_empty());
}

/// The 2.1.257 turn shape, MEASURED on linux/x86_64 in a `SessionCell::Minified`
/// pool cell: the typed prompt is followed by `total_tokens_reminder` and then
/// by a `remote_session_change` attachment before the assistant answers. Both
/// attachments sit ON the active parent chain, so the proof this test carries is
/// that the new one neither closes the logical message nor interleaves it: the
/// answer text and its usage still resolve. The session URL is a placeholder.
#[test]
fn measured_2_1_257_remote_session_change_rides_the_chain_without_closing_the_turn() {
    const SESSION_URL: &str = "https://claude.ai/code/session_PLACEHOLDER";

    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn("go").unwrap();
    engine.ingest(parse(user(None, "u", "go"))).unwrap();
    engine
        .ingest(parse(
            r#"{"parentUuid":"u","sessionId":"s","type":"attachment","uuid":"reminder","attachment":{"type":"total_tokens_reminder","text":"<total_tokens>15000000 tokens left</total_tokens>"}}"#,
        ))
        .unwrap();
    engine
        .ingest(parse(format!(
            r#"{{"parentUuid":"reminder","sessionId":"s","type":"attachment","uuid":"remote","attachment":{{"type":"remote_session_change","url":"{SESSION_URL}","commit":"Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>\nClaude-Session: {SESSION_URL}","pr":"Generated with Claude Code\n\n{SESSION_URL}","sendUserFileHint":false}}}}"#
        )))
        .unwrap();
    engine
        .ingest(parse(assistant(
            "remote",
            "answer",
            Some("message"),
            None,
            text("done"),
            Some("end_turn"),
            3,
            1,
        )))
        .unwrap();
    engine
        .ingest(parse(
            r#"{"parentUuid":"answer","sessionId":"s","type":"system","subtype":"turn_duration","uuid":"duration","durationMs":2712}"#,
        ))
        .unwrap();

    let analysis = engine.analyze().unwrap();
    let TurnStatus::Terminal(final_turn) = &analysis.status else {
        panic!("expected terminal result: {analysis:#?}");
    };
    assert_eq!(final_turn.outcome, TerminalOutcome::Completed);
    assert_eq!(final_turn.final_text, "done");
    // The attachment's own strings are transport, not answer.
    assert!(!final_turn.final_text.contains("claude.ai/code/session"));
    assert!(!final_turn.final_text.contains("Co-Authored-By"));
    // One logical message, not two: the attachment did not split the answer.
    assert_eq!(analysis.messages.len(), 1);
    assert_eq!(analysis.usage.model_calls_with_usage, 1);
    assert_eq!(analysis.usage.tokens.input_tokens, 3);
    assert_eq!(analysis.usage.tokens.output_tokens, 1);
    assert!(analysis.turn_duration_seen);
    assert_eq!(analysis.warnings, []);
}

/// The whole 2.1.257 file shape MEASURED on linux/x86_64 in a
/// `SessionCell::Minified` pool cell: a five-row launch preamble (`atis-latch`
/// is the row 2.1.236 did not write), one turn, and a trailing
/// `bridge-session`/`cost-state`/`last-prompt`/`cost-state` tail. None of the
/// metadata rows carries `parentUuid`, `uuid` or `timestamp`. Strict analysis
/// must complete the turn and warn about nothing: an `UnknownRow` warning here
/// would mean the new record types are still drifting past the parser.
#[test]
fn measured_2_1_257_launch_preamble_and_cost_state_tail_analyse_cleanly() {
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    for preamble in [
        r#"{"type":"mode","sessionId":"s","mode":"default"}"#,
        r#"{"type":"permission-mode","sessionId":"s","permissionMode":"bypassPermissions"}"#,
        r#"{"type":"atis-latch","atis":"","sessionId":"s"}"#,
        r#"{"type":"bridge-session","sessionId":"s"}"#,
        r#"{"type":"file-history-snapshot","messageId":"snapshot","snapshot":{}}"#,
    ] {
        engine.ingest(parse(preamble)).unwrap();
    }

    engine.arm_turn("go").unwrap();
    engine.ingest(parse(user(None, "u", "go"))).unwrap();
    engine
        .ingest(parse(attachment("u", "reminder", "total_tokens_reminder")))
        .unwrap();
    engine
        .ingest(parse(attachment(
            "reminder",
            "remote",
            "remote_session_change",
        )))
        .unwrap();
    engine
        .ingest(parse(r#"{"type":"ai-title","sessionId":"s"}"#))
        .unwrap();
    engine
        .ingest(parse(assistant(
            "remote",
            "answer",
            Some("message"),
            None,
            text("done"),
            Some("end_turn"),
            3,
            1,
        )))
        .unwrap();
    engine
        .ingest(parse(
            r#"{"parentUuid":"answer","sessionId":"s","type":"system","subtype":"turn_duration","uuid":"duration","durationMs":2712}"#,
        ))
        .unwrap();

    for tail in [
        r#"{"type":"bridge-session","sessionId":"s"}"#,
        COST_STATE_ROW,
        r#"{"type":"last-prompt","sessionId":"s","lastPrompt":"go"}"#,
        COST_STATE_ROW,
    ] {
        engine.ingest(parse(tail)).unwrap();
    }

    let analysis = engine.analyze().unwrap();
    let TurnStatus::Terminal(final_turn) = &analysis.status else {
        panic!("expected terminal result: {analysis:#?}");
    };
    assert_eq!(final_turn.outcome, TerminalOutcome::Completed);
    assert_eq!(final_turn.final_text, "done");
    assert!(analysis.turn_duration_seen);
    assert_eq!(analysis.warnings, []);
}

#[test]
fn unknown_or_malformed_attachment_types_fail_closed() {
    let parser = JsonlParser::new(ParseMode::Strict);
    for row in [
        r#"{"parentUuid":"u","sessionId":"s","type":"attachment","uuid":"a","attachment":{"type":"future_semantics"}}"#,
        r#"{"parentUuid":"u","sessionId":"s","type":"attachment","uuid":"a","attachment":{}}"#,
        r#"{"parentUuid":"u","sessionId":"s","type":"attachment","uuid":"a","attachment":"wrong"}"#,
    ] {
        let error = parser
            .parse(&CompleteLine {
                location: SourceLocation {
                    line: 1,
                    byte_offset: 0,
                },
                bytes: row.as_bytes().to_vec(),
            })
            .unwrap_err();
        assert!(matches!(error, TranscriptError::SchemaDrift { .. }));
    }
}

#[test]
fn strict_rejects_ambiguous_main_branches() {
    let rows = vec![
        parse(user(None, "u", "branch")),
        parse(assistant(
            "u",
            "old-branch",
            Some("old-branch-message"),
            None,
            text("must not win"),
            Some("end_turn"),
            10,
            10,
        )),
        parse(assistant(
            "u",
            "new-branch",
            Some("new-branch-message"),
            None,
            thinking("still working"),
            Some("pause_turn"),
            2,
            1,
        )),
    ];

    let mut strict = TranscriptEngine::new(ParseMode::Strict);
    strict.arm_turn("branch").unwrap();
    for row in rows {
        strict.ingest(row).unwrap();
    }
    assert!(matches!(
        strict.analyze(),
        Err(TranscriptError::AmbiguousActiveBranches { leaf_count: 2 })
    ));
}

#[test]
fn sibling_fragments_are_a_real_graph_fork_even_when_message_identity_matches() {
    let rows = vec![
        parse(user(None, "u", "fragmented")),
        parse(assistant(
            "u",
            "thinking-fragment",
            Some("one-message"),
            None,
            thinking("work"),
            None,
            3,
            2,
        )),
        parse(assistant(
            "u",
            "text-fragment",
            Some("one-message"),
            None,
            text("done"),
            Some("end_turn"),
            3,
            2,
        )),
    ];

    let mut strict = TranscriptEngine::new(ParseMode::Strict);
    strict.arm_turn("fragmented").unwrap();
    for row in rows {
        strict.ingest(row).unwrap();
    }
    assert!(matches!(
        strict.analyze(),
        Err(TranscriptError::AmbiguousActiveBranches { leaf_count: 2 })
    ));
}

#[test]
fn request_id_groups_fragments_and_row_uuid_is_final_fallback() {
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn("go").unwrap();
    engine.ingest(parse(user(None, "u", "go"))).unwrap();
    engine
        .ingest(parse(assistant(
            "u",
            "a1",
            None,
            Some("request-1"),
            thinking("work"),
            None,
            4,
            2,
        )))
        .unwrap();
    engine
        .ingest(parse(assistant(
            "a1",
            "a2",
            None,
            Some("request-1"),
            text("answer"),
            Some("end_turn"),
            4,
            2,
        )))
        .unwrap();
    let analysis = engine.analyze().unwrap();
    assert_eq!(analysis.messages.len(), 1);
    assert_eq!(
        analysis.messages[0].key,
        LogicalMessageKey::RequestId("request-1".to_owned())
    );

    engine.disarm_turn().unwrap();
    engine.arm_turn("again").unwrap();
    engine
        .ingest(parse(user(Some("a2"), "u2", "again")))
        .unwrap();
    engine
        .ingest(parse(assistant(
            "u2",
            "row-fallback",
            None,
            None,
            text("row answer"),
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();
    let analysis = engine.analyze().unwrap();
    assert_eq!(
        analysis.messages[0].key,
        LogicalMessageKey::RowUuid("row-fallback".to_owned())
    );
}

#[test]
fn parallel_tool_calls_and_results_keep_call_order() {
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn("tools").unwrap();
    engine.ingest(parse(user(None, "u", "tools"))).unwrap();
    engine
        .ingest(parse(assistant(
            "u",
            "calls",
            Some("call-message"),
            None,
            r#"[{"type":"tool_use","id":"one","name":"Read","input":{"n":1}},{"type":"tool_use","id":"two","name":"Read","input":{"n":2}}]"#,
            Some("tool_use"),
            10,
            2,
        )))
        .unwrap();
    engine
        .ingest(parse(tool_results(
            "calls",
            "results",
            r#"[{"type":"tool_result","tool_use_id":"two","content":"second"},{"type":"tool_result","tool_use_id":"one","content":"first"}]"#,
        )))
        .unwrap();
    engine
        .ingest(parse(assistant(
            "results",
            "final",
            Some("final-message"),
            None,
            text("ok"),
            Some("end_turn"),
            12,
            1,
        )))
        .unwrap();

    let analysis = engine.analyze().unwrap();
    assert_eq!(
        analysis
            .tools
            .iter()
            .map(|tool| tool.tool_use_id.as_str())
            .collect::<Vec<_>>(),
        ["one", "two"]
    );
    assert_eq!(analysis.tools[0].result.as_ref().unwrap().content, "first");
    assert_eq!(analysis.tools[1].result.as_ref().unwrap().content, "second");
}

#[test]
fn multiple_main_typed_prompt_acknowledgements_fail_closed() {
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn("one prompt").unwrap();
    assert!(matches!(
        engine.ingest(parse(user(None, "first", "one prompt"))),
        Ok(IngestOutcome::PromptAcknowledged(_))
    ));

    let error = engine
        .ingest(parse(user(Some("first"), "second", "one prompt")))
        .unwrap_err();
    assert!(matches!(
        error,
        TranscriptError::MultiplePromptAcknowledgements
    ));
    assert_eq!(
        engine.row_count(),
        1,
        "the rejected acknowledgement must not mutate graph history"
    );
}

#[test]
fn non_main_typed_rows_cannot_acknowledge_or_conflict_with_the_active_prompt() {
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn("main prompt").unwrap();
    for row in [
        r#"{"parentUuid":null,"sessionId":"s","type":"user","message":{"role":"user","content":"side prompt"},"uuid":"side-user","promptSource":"typed","isSidechain":true}"#,
        r#"{"parentUuid":null,"sessionId":"s","type":"user","message":{"role":"user","content":"team prompt"},"uuid":"team-user","promptSource":"typed","agentId":"agent"}"#,
        r#"{"parentUuid":null,"sessionId":"s","type":"user","message":{"role":"user","content":"meta prompt"},"uuid":"meta-user","promptSource":"typed","isMeta":true}"#,
    ] {
        assert!(matches!(
            engine.ingest(parse(row)),
            Ok(IngestOutcome::Added { .. })
        ));
    }
    assert!(matches!(
        engine.analyze().unwrap().status,
        TurnStatus::AwaitingPromptAcknowledgement
    ));

    assert!(matches!(
        engine.ingest(parse(user(None, "main-user", "main prompt"))),
        Ok(IngestOutcome::PromptAcknowledged(_))
    ));
    engine
        .ingest(parse(assistant(
            "main-user",
            "answer",
            Some("message"),
            None,
            text("main answer"),
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();
    let analysis = engine.analyze().unwrap();
    let TurnStatus::Terminal(final_turn) = analysis.status else {
        panic!("the exact main prompt did not establish authority");
    };
    assert_eq!(final_turn.final_text, "main answer");
    assert_eq!(analysis.messages.len(), 1);
}

#[test]
fn exact_duplicate_tool_blocks_are_deduplicated_by_tool_use_id() {
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn("tools").unwrap();
    engine.ingest(parse(user(None, "u", "tools"))).unwrap();
    engine
        .ingest(parse(assistant(
            "u",
            "calls",
            Some("call-message"),
            None,
            r#"[{"type":"tool_use","id":"same","name":"Read","input":{"path":"README.md"}},{"type":"tool_use","id":"same","name":"Read","input":{"path":"README.md"}}]"#,
            Some("tool_use"),
            1,
            1,
        )))
        .unwrap();
    engine
        .ingest(parse(tool_results(
            "calls",
            "results",
            r#"[{"type":"tool_result","tool_use_id":"same","content":"ok"},{"type":"tool_result","tool_use_id":"same","content":"ok"}]"#,
        )))
        .unwrap();
    engine
        .ingest(parse(assistant(
            "results",
            "answer",
            Some("answer-message"),
            None,
            text("done"),
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();

    let analysis = engine.analyze().unwrap();
    assert!(matches!(analysis.status, TurnStatus::Terminal(_)));
    assert_eq!(analysis.tools.len(), 1);
    assert_eq!(analysis.tools[0].tool_use_id, "same");
    assert_eq!(analysis.tools[0].order, 0);
    assert_eq!(analysis.tools[0].result.as_ref().unwrap().content, "ok");
}

#[test]
fn duplicate_and_orphan_tool_ids_fail_closed() {
    let mut duplicate_call = TranscriptEngine::new(ParseMode::Strict);
    duplicate_call.arm_turn("tools").unwrap();
    duplicate_call
        .ingest(parse(user(None, "u", "tools")))
        .unwrap();
    duplicate_call
        .ingest(parse(assistant(
            "u",
            "calls",
            Some("message"),
            None,
            r#"[{"type":"tool_use","id":"same","name":"Read","input":{}},{"type":"tool_use","id":"same","name":"Write","input":{}}]"#,
            Some("tool_use"),
            1,
            1,
        )))
        .unwrap();
    assert!(matches!(
        duplicate_call.analyze(),
        Err(TranscriptError::DuplicateToolCall { tool_use_id }) if tool_use_id == "same"
    ));

    let mut duplicate_result = TranscriptEngine::new(ParseMode::Strict);
    duplicate_result.arm_turn("tools").unwrap();
    duplicate_result
        .ingest(parse(user(None, "u", "tools")))
        .unwrap();
    duplicate_result
        .ingest(parse(assistant(
            "u",
            "call",
            Some("message"),
            None,
            r#"[{"type":"tool_use","id":"same","name":"Read","input":{}}]"#,
            Some("tool_use"),
            1,
            1,
        )))
        .unwrap();
    duplicate_result
        .ingest(parse(tool_results(
            "call",
            "results",
            r#"[{"type":"tool_result","tool_use_id":"same","content":"one"},{"type":"tool_result","tool_use_id":"same","content":"two"}]"#,
        )))
        .unwrap();
    assert!(matches!(
        duplicate_result.analyze(),
        Err(TranscriptError::DuplicateToolResult { tool_use_id }) if tool_use_id == "same"
    ));

    let mut orphan_result = TranscriptEngine::new(ParseMode::Strict);
    orphan_result.arm_turn("tools").unwrap();
    orphan_result
        .ingest(parse(user(None, "u", "tools")))
        .unwrap();
    orphan_result
        .ingest(parse(tool_results(
            "u",
            "result",
            r#"[{"type":"tool_result","tool_use_id":"missing","content":"orphan"}]"#,
        )))
        .unwrap();
    assert!(matches!(
        orphan_result.analyze(),
        Err(TranscriptError::OrphanToolResult { tool_use_id }) if tool_use_id == "missing"
    ));
}

#[test]
fn usage_aggregation_overflow_fails_closed() {
    let safe_max = pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER;
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn("go").unwrap();
    engine.ingest(parse(user(None, "u", "go"))).unwrap();
    engine
        .ingest(parse(assistant(
            "u",
            "call",
            Some("call-message"),
            None,
            r#"[{"type":"tool_use","id":"tool","name":"Read","input":{}}]"#,
            Some("tool_use"),
            safe_max,
            0,
        )))
        .unwrap();
    engine
        .ingest(parse(tool_results(
            "call",
            "result",
            r#"[{"type":"tool_result","tool_use_id":"tool","content":"ok"}]"#,
        )))
        .unwrap();
    engine
        .ingest(parse(assistant(
            "result",
            "final",
            Some("final-message"),
            None,
            text("done"),
            Some("end_turn"),
            1,
            0,
        )))
        .unwrap();

    assert!(matches!(
        engine.analyze(),
        Err(TranscriptError::UsageOverflow {
            field: "input_tokens"
        })
    ));
}

#[test]
fn one_logical_response_may_continue_across_interleaved_tool_results() {
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn("tools").unwrap();
    engine.ingest(parse(user(None, "u", "tools"))).unwrap();
    engine
        .ingest(parse(assistant(
            "u",
            "call-one",
            Some("streamed-call"),
            Some("request-one"),
            r#"[{"type":"thinking","thinking":"work"},{"type":"tool_use","id":"one","name":"Read","input":{"n":1}}]"#,
            Some("tool_use"),
            10,
            2,
        )))
        .unwrap();
    engine
        .ingest(parse(tool_results(
            "call-one",
            "result-one",
            r#"[{"type":"tool_result","tool_use_id":"one","content":"first"}]"#,
        )))
        .unwrap();
    engine
        .ingest(parse(assistant(
            "result-one",
            "call-two",
            Some("streamed-call"),
            Some("request-one"),
            r#"[{"type":"tool_use","id":"two","name":"Read","input":{"n":2}}]"#,
            Some("tool_use"),
            10,
            2,
        )))
        .unwrap();
    engine
        .ingest(parse(tool_results(
            "call-two",
            "result-two",
            r#"[{"type":"tool_result","tool_use_id":"two","content":"second"}]"#,
        )))
        .unwrap();
    engine
        .ingest(parse(assistant(
            "result-two",
            "final",
            Some("final-message"),
            Some("request-two"),
            text("ok"),
            Some("end_turn"),
            12,
            1,
        )))
        .unwrap();

    let analysis = engine.analyze().unwrap();
    assert!(matches!(analysis.status, TurnStatus::Terminal(_)));
    assert_eq!(analysis.messages.len(), 2);
    assert_eq!(analysis.messages[0].row_uuids, ["call-one", "call-two"]);
    assert_eq!(analysis.usage.model_calls_with_usage, 2);
    assert_eq!(
        analysis
            .tools
            .iter()
            .map(|tool| tool.tool_use_id.as_str())
            .collect::<Vec<_>>(),
        ["one", "two"]
    );
    assert_eq!(analysis.tools[0].result.as_ref().unwrap().content, "first");
    assert_eq!(analysis.tools[1].result.as_ref().unwrap().content, "second");
}

#[test]
fn strict_mode_rejects_unknown_row_on_selected_parent_chain() {
    let rows = fixture("unknown_active.jsonl", ParseMode::Strict);
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn("hello").unwrap();
    for row in rows {
        engine.ingest(row).unwrap();
    }
    assert!(matches!(
        engine.analyze(),
        Err(TranscriptError::SchemaDrift { path, .. }) if path == "$.type"
    ));
    assert_eq!(
        engine.rows().count(),
        3,
        "unknown row must remain preserved"
    );
}

#[test]
fn strict_mode_preserves_but_does_not_trust_unknown_off_branch_rows() {
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn("go").unwrap();
    engine.ingest(parse(user(None, "u", "go"))).unwrap();
    engine
        .ingest(parse(
            r#"{"parentUuid":"unrelated","sessionId":"s","type":"future-event","uuid":"unknown-branch","payload":true}"#,
        ))
        .unwrap();
    engine
        .ingest(parse(assistant(
            "u",
            "final",
            Some("final-message"),
            None,
            text("safe branch"),
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();

    let analysis = engine.analyze().unwrap();
    assert!(matches!(analysis.status, TurnStatus::Terminal(_)));
    assert!(analysis.warnings.iter().any(|warning| matches!(
        warning,
        EngineWarning::UnknownRow {
            declared_type: Some(value),
            ..
        } if value == "future-event"
    )));
    assert_eq!(engine.rows().count(), 3);
}

#[test]
fn strict_graph_rejects_disconnected_semantics_and_parent_append_reordering() {
    let mut disconnected = TranscriptEngine::new(ParseMode::Strict);
    disconnected.arm_turn("go").unwrap();
    disconnected.ingest(parse(user(None, "u", "go"))).unwrap();
    disconnected
        .ingest(parse(assistant(
            "u",
            "final",
            Some("final-message"),
            None,
            text("done"),
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();
    disconnected
        .ingest(parse(assistant(
            "missing-parent",
            "orphan",
            Some("orphan-message"),
            None,
            text("must not be ignored"),
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();
    assert!(matches!(
        disconnected.analyze(),
        Err(TranscriptError::DisconnectedActiveRow { row_uuid }) if row_uuid == "orphan"
    ));

    let mut reordered = TranscriptEngine::new(ParseMode::Strict);
    reordered.arm_turn("go").unwrap();
    reordered.ingest(parse(user(None, "u", "go"))).unwrap();
    reordered
        .ingest(parse(assistant(
            "parent-appended-later",
            "child-appended-first",
            Some("child-message"),
            None,
            text("child"),
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();
    reordered
        .ingest(parse(assistant(
            "u",
            "parent-appended-later",
            Some("parent-message"),
            None,
            thinking("parent"),
            None,
            1,
            1,
        )))
        .unwrap();
    assert!(matches!(
        reordered.analyze(),
        Err(TranscriptError::ParentAppendOrder { row_uuid, parent_uuid })
            if row_uuid == "child-appended-first" && parent_uuid == "parent-appended-later"
    ));
}

#[test]
fn strict_graph_rejects_interleaved_logical_message_identities() {
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn("go").unwrap();
    engine.ingest(parse(user(None, "u", "go"))).unwrap();
    engine
        .ingest(parse(assistant(
            "u",
            "a1",
            Some("message-a"),
            None,
            thinking("a starts"),
            None,
            1,
            1,
        )))
        .unwrap();
    engine
        .ingest(parse(assistant(
            "a1",
            "b",
            Some("message-b"),
            None,
            thinking("b"),
            None,
            1,
            1,
        )))
        .unwrap();
    engine
        .ingest(parse(assistant(
            "b",
            "a2",
            Some("message-a"),
            None,
            text("a returns"),
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();

    assert!(matches!(
        engine.analyze(),
        Err(TranscriptError::InterleavedLogicalMessage {
            key: LogicalMessageKey::MessageId(key)
        }) if key == "message-a"
    ));
}

#[test]
fn strict_graph_requires_uuid_for_future_descendants_but_excludes_metadata() {
    let mut metadata = TranscriptEngine::new(ParseMode::Strict);
    metadata.arm_turn("go").unwrap();
    metadata.ingest(parse(user(None, "u", "go"))).unwrap();
    metadata
        .ingest(parse(assistant(
            "u",
            "a",
            Some("m"),
            None,
            text("done"),
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();
    metadata
        .ingest(parse(
            r#"{"type":"progress","parentUuid":"a","sessionId":"s","payload":true}"#,
        ))
        .unwrap();
    assert!(matches!(
        metadata.analyze().unwrap().status,
        TurnStatus::Terminal(_)
    ));

    let mut future = TranscriptEngine::new(ParseMode::Strict);
    future.arm_turn("go").unwrap();
    future.ingest(parse(user(None, "u", "go"))).unwrap();
    future
        .ingest(parse(assistant(
            "u",
            "a",
            Some("m"),
            None,
            text("not final"),
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();
    future
        .ingest(parse(
            r#"{"type":"future-semantic","parentUuid":"a","sessionId":"s","payload":true}"#,
        ))
        .unwrap();
    assert!(matches!(
        future.analyze(),
        Err(TranscriptError::ActiveRowMissingUuid { ordinal: 2 })
    ));
}

#[test]
fn strict_mode_rejects_unknown_correlated_content_and_usage_conflicts() {
    let mut unknown = TranscriptEngine::new(ParseMode::Strict);
    unknown.arm_turn("go").unwrap();
    unknown.ingest(parse(user(None, "u", "go"))).unwrap();
    unknown
        .ingest(parse(assistant(
            "u",
            "a",
            Some("m"),
            None,
            r#"[{"type":"future","value":1}]"#,
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();
    assert!(matches!(
        unknown.analyze(),
        Err(TranscriptError::SchemaDrift { path, .. }) if path == "$.message.content"
    ));

    let mut usage = TranscriptEngine::new(ParseMode::Strict);
    usage.arm_turn("go").unwrap();
    usage.ingest(parse(user(None, "u", "go"))).unwrap();
    usage
        .ingest(parse(assistant(
            "u",
            "a1",
            Some("m"),
            None,
            thinking("x"),
            None,
            1,
            1,
        )))
        .unwrap();
    usage
        .ingest(parse(assistant(
            "a1",
            "a2",
            Some("m"),
            None,
            text("x"),
            Some("end_turn"),
            1,
            2,
        )))
        .unwrap();
    assert!(matches!(
        usage.analyze(),
        Err(TranscriptError::LogicalMessageConflict { field: "usage", .. })
    ));
}

#[test]
fn stop_sequence_with_text_is_terminal_and_nonterminal_stops_remain_running() {
    for reason in ["tool_use", "pause_turn"] {
        let mut engine = TranscriptEngine::new(ParseMode::Strict);
        engine.arm_turn("go").unwrap();
        engine.ingest(parse(user(None, "u", "go"))).unwrap();
        engine
            .ingest(parse(assistant(
                "u",
                "a",
                Some("m"),
                None,
                text("not authoritative"),
                Some(reason),
                1,
                1,
            )))
            .unwrap();
        let analysis = engine.analyze().unwrap();
        assert!(
            matches!(analysis.status, TurnStatus::Running { .. }),
            "reason {reason}"
        );
    }

    let rows = vec![
        parse(user(None, "u", "go")),
        parse(assistant(
            "u",
            "a",
            Some("m"),
            None,
            text("ambiguous"),
            Some("stop_sequence"),
            1,
            1,
        )),
    ];
    let mut strict = TranscriptEngine::new(ParseMode::Strict);
    strict.arm_turn("go").unwrap();
    for row in rows {
        strict.ingest(row).unwrap();
    }
    assert!(matches!(
        strict.analyze().unwrap().status,
        TurnStatus::Terminal(ref turn) if turn.outcome == TerminalOutcome::Completed
    ));
}

#[test]
fn trailing_structural_or_tool_result_leaf_prevents_earlier_terminal_commit() {
    let mut attachment_leaf = TranscriptEngine::new(ParseMode::Strict);
    attachment_leaf.arm_turn("go").unwrap();
    attachment_leaf
        .ingest(parse(user(None, "u", "go")))
        .unwrap();
    attachment_leaf
        .ingest(parse(assistant(
            "u",
            "answer",
            Some("message"),
            None,
            text("not final while context is trailing"),
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();
    attachment_leaf
        .ingest(parse(attachment("answer", "context", "task_reminder")))
        .unwrap();
    assert!(matches!(
        attachment_leaf.analyze().unwrap().status,
        TurnStatus::Running { .. }
    ));

    let mut result_leaf = TranscriptEngine::new(ParseMode::Strict);
    result_leaf.arm_turn("tool").unwrap();
    result_leaf
        .ingest(parse(user(None, "tool-user", "tool")))
        .unwrap();
    result_leaf
        .ingest(parse(assistant(
            "tool-user",
            "tool-call",
            Some("tool-message"),
            None,
            r#"[{"type":"text","text":"premature"},{"type":"tool_use","id":"tool","name":"Read","input":{}}]"#,
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();
    result_leaf
        .ingest(parse(tool_results(
            "tool-call",
            "tool-result",
            r#"[{"type":"tool_result","tool_use_id":"tool","content":"late"}]"#,
        )))
        .unwrap();
    let analysis = result_leaf.analyze().unwrap();
    assert!(matches!(analysis.status, TurnStatus::Running { .. }));
    assert_eq!(analysis.tools[0].result.as_ref().unwrap().content, "late");
}

#[test]
fn exact_stop_reason_matrix_is_fail_closed() {
    enum Expected {
        Running(Option<StopReason>),
        Terminal(TerminalOutcome),
        SchemaDrift,
    }

    let cases = [
        (
            Some("end_turn"),
            Expected::Terminal(TerminalOutcome::Completed),
        ),
        (
            Some("max_tokens"),
            Expected::Terminal(TerminalOutcome::MaxTokens),
        ),
        (
            Some("stop_sequence"),
            Expected::Terminal(TerminalOutcome::Completed),
        ),
        (
            Some("refusal"),
            Expected::Terminal(TerminalOutcome::Refused),
        ),
        (
            Some("tool_use"),
            Expected::Running(Some(StopReason::ToolUse)),
        ),
        (
            Some("pause_turn"),
            Expected::Running(Some(StopReason::PauseTurn)),
        ),
        (None, Expected::Running(None)),
        (Some("future_stop"), Expected::SchemaDrift),
    ];

    for (reason, expected) in cases {
        let mut engine = TranscriptEngine::new(ParseMode::Strict);
        engine.arm_turn("go").unwrap();
        engine.ingest(parse(user(None, "u", "go"))).unwrap();
        engine
            .ingest(parse(assistant(
                "u",
                "a",
                Some("m"),
                None,
                text("answer"),
                reason,
                1,
                1,
            )))
            .unwrap();

        match expected {
            Expected::Running(latest_stop_reason) => assert_eq!(
                engine.analyze().unwrap().status,
                TurnStatus::Running { latest_stop_reason },
                "unexpected running status for {reason:?}"
            ),
            Expected::Terminal(expected_outcome) => {
                let TurnStatus::Terminal(turn) = engine.analyze().unwrap().status else {
                    panic!("expected terminal status for {reason:?}");
                };
                assert_eq!(turn.outcome, expected_outcome, "reason {reason:?}");
                assert_eq!(
                    turn.stop_reason,
                    reason.map(StopReason::parse),
                    "reason {reason:?}"
                );
            }
            Expected::SchemaDrift => assert!(matches!(
                engine.analyze(),
                Err(TranscriptError::SchemaDrift { path, .. })
                    if path == "$.message.stop_reason"
            )),
        }
    }
}

#[test]
fn strict_rejects_newer_unknown_user_content_after_a_terminal_assistant() {
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn("go").unwrap();
    engine.ingest(parse(user(None, "u", "go"))).unwrap();
    engine
        .ingest(parse(assistant(
            "u",
            "a",
            Some("m"),
            None,
            text("not final anymore"),
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();
    engine
        .ingest(parse(
            r#"{"type":"user","uuid":"future-user","parentUuid":"a","sessionId":"s","message":{"role":"user","content":[{"type":"future_user_control"}]}}"#,
        ))
        .unwrap();
    assert!(matches!(
        engine.analyze(),
        Err(TranscriptError::SchemaDrift { path, .. }) if path == "$.message"
    ));
}

/// A `strings` scan of the 2.1.257 binary turned up two system subtypes that
/// were never OBSERVED on a minified pool cell: `tool_host_result` (converted
/// from an equally unobserved `tool_host_result_lines` attachment) and
/// `cloud_session_status`. Neither is admitted, so both are drift the moment
/// they land on the active chain. This is the negative control for the three
/// names 2.1.257 measurement did admit.
#[test]
fn unobserved_2_1_257_system_subtypes_are_refused_on_the_active_chain() {
    for subtype in ["tool_host_result", "cloud_session_status"] {
        let mut engine = TranscriptEngine::new(ParseMode::Strict);
        engine.arm_turn("go").unwrap();
        engine.ingest(parse(user(None, "u", "go"))).unwrap();
        engine
            .ingest(parse(assistant(
                "u",
                "a",
                Some("m"),
                None,
                text("done"),
                Some("end_turn"),
                1,
                1,
            )))
            .unwrap();
        engine
            .ingest(parse(format!(
                r#"{{"type":"system","subtype":"{subtype}","uuid":"system","parentUuid":"a","sessionId":"s"}}"#
            )))
            .unwrap();
        assert!(
            matches!(
                engine.analyze(),
                Err(TranscriptError::SchemaDrift { ref path, .. }) if path == "$.subtype"
            ),
            "{subtype} must stay refused on the active chain"
        );
    }
}

#[test]
fn strict_system_rows_are_allowlisted_and_turn_duration_must_be_trailing() {
    let mut unknown = TranscriptEngine::new(ParseMode::Strict);
    unknown.arm_turn("go").unwrap();
    unknown.ingest(parse(user(None, "u", "go"))).unwrap();
    unknown
        .ingest(parse(assistant(
            "u",
            "a",
            Some("m"),
            None,
            text("done"),
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();
    unknown
        .ingest(parse(
            r#"{"type":"system","subtype":"future-system","uuid":"system","parentUuid":"a","sessionId":"s"}"#,
        ))
        .unwrap();
    assert!(matches!(
        unknown.analyze(),
        Err(TranscriptError::SchemaDrift { path, .. }) if path == "$.subtype"
    ));

    let mut nontrailing = TranscriptEngine::new(ParseMode::Strict);
    nontrailing.arm_turn("go").unwrap();
    nontrailing.ingest(parse(user(None, "u", "go"))).unwrap();
    nontrailing
        .ingest(parse(assistant(
            "u",
            "a",
            Some("m1"),
            None,
            text("premature"),
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();
    nontrailing
        .ingest(parse(
            r#"{"type":"system","subtype":"turn_duration","uuid":"duration","parentUuid":"a","sessionId":"s","durationMs":1}"#,
        ))
        .unwrap();
    nontrailing
        .ingest(parse(assistant(
            "duration",
            "later",
            Some("m2"),
            None,
            text("later"),
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();
    assert!(matches!(
        nontrailing.analyze(),
        Err(TranscriptError::SchemaDrift { message, .. })
            if message.contains("turn_duration must be trailing")
    ));
}

/// The exact turn tail that failed live ordinal 49, with the
/// `stop_hook_summary` and `turn_duration` rows copied out of
/// `~/.claude/projects/-Users-<USER>-dev-pmux-phase12-cwd/1aa963e5-ad99-47ee-9c32-cf67854cdea2.jsonl`
/// lines 16 and 17 -- byte for byte except that the recording machine's home
/// directory reads `<HOME>` and its login name reads `<USER>`, the substitutions
/// `tools/evidence_common/portable_paths.py` declares. Claude's own project
/// directory spells a path with `-` for `/`, which is why the login name shows
/// up there without the home directory around it. `turn_duration` parents onto
/// the summary, so the summary is a first-class graph node: demoting it
/// off-graph would break turn_duration resolution.
#[test]
fn the_observed_stop_hook_summary_completes_a_turn_and_turn_duration_chains_through_it() {
    let rows = fixture("stop_hook_summary_turn.jsonl", ParseMode::Strict);
    assert_eq!(rows.len(), 4);

    // The summary alone is terminal: this is the window the drain observes when
    // the transcript stops at the summary row.
    let mut at_summary = TranscriptEngine::new(ParseMode::Strict);
    at_summary.arm_turn("Reply with the word ready.").unwrap();
    for row in rows.iter().take(3).cloned() {
        at_summary.ingest(row).unwrap();
    }
    let analysis = at_summary.analyze().unwrap();
    let TurnStatus::Terminal(final_turn) = &analysis.status else {
        panic!("expected the proven-inert summary leaf to stay terminal: {analysis:#?}");
    };
    assert_eq!(final_turn.outcome, TerminalOutcome::Completed);
    assert_eq!(final_turn.final_text, "ready");
    assert!(analysis.stop_hook_summary_seen);
    assert!(!analysis.turn_duration_seen);
    assert_eq!(analysis.warnings, []);

    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn("Reply with the word ready.").unwrap();
    for row in rows.into_iter() {
        engine.ingest(row).unwrap();
    }
    let analysis = engine.analyze().unwrap();
    let TurnStatus::Terminal(final_turn) = &analysis.status else {
        panic!("expected a terminal analysis: {analysis:#?}");
    };
    assert_eq!(final_turn.outcome, TerminalOutcome::Completed);
    assert_eq!(final_turn.final_text, "ready");
    assert!(analysis.stop_hook_summary_seen);
    assert!(analysis.turn_duration_seen);
    assert_eq!(analysis.warnings, []);
    assert_eq!(
        analysis.active_chain,
        [
            "synthetic-typed-prompt",
            "255be144-39d6-4cbf-8065-4bf67375dfad",
            "f517343d-d00e-419c-b005-9cc8c5a464be",
            "7b14ce22-8235-44b2-9385-190db20c1a5d",
        ],
        "turn_duration must still resolve its parent through the summary row"
    );
}

/// The truncation race: a Stop hook may answer `decision: "block"`, after which
/// Claude *continues* the turn. In the window before the continuation's first
/// row lands the chain leaf is this system row, the latest logical message still
/// says `end_turn`, and the screen shows a ready prompt -- so an unproven
/// summary is exactly the shape that would let pmux commit a truncated turn.
/// Only `preventedContinuation:false` rules that out, and only its presence
/// proves anything.
#[test]
fn a_stop_hook_summary_is_only_admitted_onto_the_active_chain_when_it_proves_no_continuation() {
    for (payload, expected_path) in [
        (
            r#","preventedContinuation":true"#,
            "$.preventedContinuation",
        ),
        ("", "$.preventedContinuation"),
        (
            r#","preventedContinuation":false,"hookErrors":["pmux-hook exited 1"]"#,
            "$.hookErrors",
        ),
        (
            r#","preventedContinuation":false,"hookAdditionalContext":["keep going"]"#,
            "$.hookAdditionalContext",
        ),
        (
            r#","preventedContinuation":false,"message":{"role":"assistant","content":[{"type":"text","text":"more"}]}"#,
            "$.message",
        ),
    ] {
        let error = JsonlParser::new(ParseMode::Strict)
            .parse(&CompleteLine {
                location: SourceLocation {
                    line: 1,
                    byte_offset: 0,
                },
                bytes: stop_hook_summary("a", "summary", payload).into_bytes(),
            })
            .unwrap_err();
        assert!(
            matches!(
                error,
                TranscriptError::SchemaDrift { ref path, .. } if path == expected_path
            ),
            "payload {payload:?} was not rejected at {expected_path}: {error:?}"
        );
    }

    // Proven inert, and therefore admitted.
    let mut accepted = TranscriptEngine::new(ParseMode::Strict);
    accepted.arm_turn("go").unwrap();
    accepted.ingest(parse(user(None, "u", "go"))).unwrap();
    accepted
        .ingest(parse(assistant(
            "u",
            "a",
            Some("m"),
            None,
            text("done"),
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();
    accepted
        .ingest(parse(stop_hook_summary(
            "a",
            "summary",
            r#","preventedContinuation":false,"hookErrors":[],"hookAdditionalContext":[]"#,
        )))
        .unwrap();
    let analysis = accepted.analyze().unwrap();
    assert!(matches!(analysis.status, TurnStatus::Terminal(_)));
    assert!(analysis.stop_hook_summary_seen);
}

/// An accepted summary opens the same trailing zone `turn_duration` opens: if
/// any semantic row still arrives on the chain afterwards, the transcript has
/// contradicted the proof and pmux refuses rather than commits.
#[test]
fn a_semantic_row_after_an_accepted_stop_hook_summary_is_drift() {
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn("go").unwrap();
    engine.ingest(parse(user(None, "u", "go"))).unwrap();
    engine
        .ingest(parse(assistant(
            "u",
            "a",
            Some("m1"),
            None,
            text("premature"),
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();
    engine
        .ingest(parse(stop_hook_summary(
            "a",
            "summary",
            r#","preventedContinuation":false"#,
        )))
        .unwrap();
    engine
        .ingest(parse(assistant(
            "summary",
            "later",
            Some("m2"),
            None,
            text("continued after all"),
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();
    assert!(matches!(
        engine.analyze(),
        Err(TranscriptError::SchemaDrift { ref message, .. })
            if message == "stop_hook_summary must be trailing on the active parent chain"
    ));

    // The zone is shared, not parallel: a summary followed by turn_duration is
    // still a legal tail, and the zone is named by whichever marker opened it.
    let mut through = TranscriptEngine::new(ParseMode::Strict);
    through.arm_turn("go").unwrap();
    through.ingest(parse(user(None, "u", "go"))).unwrap();
    through
        .ingest(parse(assistant(
            "u",
            "a",
            Some("m1"),
            None,
            text("done"),
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();
    through
        .ingest(parse(stop_hook_summary(
            "a",
            "summary",
            r#","preventedContinuation":false"#,
        )))
        .unwrap();
    through
        .ingest(parse(
            r#"{"type":"system","subtype":"turn_duration","uuid":"duration","parentUuid":"summary","sessionId":"s","durationMs":1}"#,
        ))
        .unwrap();
    through
        .ingest(parse(tool_results(
            "duration",
            "late-result",
            r#"[{"type":"tool_result","tool_use_id":"tool","content":"late"}]"#,
        )))
        .unwrap();
    assert!(matches!(
        through.analyze(),
        Err(TranscriptError::SchemaDrift { ref message, .. })
            if message == "stop_hook_summary must be trailing on the active parent chain"
    ));
}

/// The live defect: a dropped wifi connection makes Claude write a main-chain
/// `api_error` row, Claude retries, Claude succeeds -- and pmux used to fail the
/// whole turn with SchemaDrift at `$.subtype` even though the answer arrived.
/// All 115 `api_error` rows observed on this machine are main-chain and all have
/// a child, so this mid-chain position is the only position they occupy.
#[test]
fn an_api_error_mid_chain_followed_by_a_successful_reply_completes_the_turn() {
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn("go").unwrap();
    engine.ingest(parse(user(None, "u", "go"))).unwrap();
    engine
        .ingest(parse(api_error("u", "retry-1", 1, 10)))
        .unwrap();
    engine
        .ingest(parse(assistant(
            "retry-1",
            "a",
            Some("m-after-retry"),
            None,
            text("done"),
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();

    let analysis = engine.analyze().unwrap();
    let TurnStatus::Terminal(final_turn) = &analysis.status else {
        panic!("a recovered retry must not stop the turn completing: {analysis:#?}");
    };
    assert_eq!(final_turn.outcome, TerminalOutcome::Completed);
    assert_eq!(final_turn.final_text, "done");
    assert_eq!(analysis.api_error_retries_seen, 1);
    assert!(!analysis.turn_duration_seen);
    assert!(!analysis.stop_hook_summary_seen);
    assert_eq!(analysis.warnings, []);
    assert_eq!(analysis.active_chain, ["u", "retry-1", "a"]);
}

/// THE TRUNCATION TEST. An `api_error` leaf means a retry is in flight, so the
/// turn is not over -- but the latest logical message on the chain still carries
/// the pre-retry `end_turn`. If the leaf inherited the terminal compatibility
/// that `engine.rs` grants system rows, pmux would publish that pre-retry text
/// as the answer and complete a turn mid-retry during a network blip. The
/// asymmetry says refusing to return is merely bad; returning early is not.
#[test]
fn an_api_error_leaf_is_never_a_terminal_leaf() {
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn("go").unwrap();
    engine.ingest(parse(user(None, "u", "go"))).unwrap();
    engine
        .ingest(parse(assistant(
            "u",
            "a",
            Some("m-before-retry"),
            None,
            text("partial answer"),
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();
    engine
        .ingest(parse(api_error("a", "retry-1", 1, 10)))
        .unwrap();

    let analysis = engine.analyze().unwrap();
    assert!(
        matches!(analysis.status, TurnStatus::Running { .. }),
        "an api_error leaf must leave the turn running: {analysis:#?}"
    );
    assert_eq!(analysis.api_error_retries_seen, 1);

    // Exhaustion was never observed in the wild. It takes the same non-terminal
    // path rather than a special one, which is the safe direction.
    let mut exhausted = TranscriptEngine::new(ParseMode::Strict);
    exhausted.arm_turn("go").unwrap();
    exhausted.ingest(parse(user(None, "u", "go"))).unwrap();
    exhausted
        .ingest(parse(assistant(
            "u",
            "a",
            Some("m-before-retry"),
            None,
            text("partial answer"),
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();
    exhausted
        .ingest(parse(api_error("a", "retry-10", 10, 10)))
        .unwrap();
    assert!(matches!(
        exhausted.analyze().unwrap().status,
        TurnStatus::Running { .. }
    ));
}

/// One network incident emits a ladder of rows -- (1,10) through (8,10) were all
/// observed -- and the turn it interrupted completes exactly once afterwards.
/// The ladder is counted rather than flagged so an operator can tell "slow
/// because Claude retried eight times" from "pmux was slow".
#[test]
fn a_ladder_of_api_errors_then_success_completes_once_and_reports_its_length() {
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn("go").unwrap();
    engine.ingest(parse(user(None, "u", "go"))).unwrap();
    let mut parent = "u".to_owned();
    for attempt in 1..=8 {
        let uuid = format!("retry-{attempt}");
        engine
            .ingest(parse(api_error(&parent, &uuid, attempt, 10)))
            .unwrap();
        parent = uuid;
    }
    engine
        .ingest(parse(assistant(
            &parent,
            "a",
            Some("m-after-ladder"),
            None,
            text("done"),
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();

    let analysis = engine.analyze().unwrap();
    let TurnStatus::Terminal(final_turn) = &analysis.status else {
        panic!("the recovered ladder must complete: {analysis:#?}");
    };
    assert_eq!(final_turn.outcome, TerminalOutcome::Completed);
    assert_eq!(final_turn.final_text, "done");
    assert_eq!(final_turn.final_text_blocks, ["done"]);
    assert_eq!(analysis.messages.len(), 1);
    assert_eq!(analysis.api_error_retries_seen, 8);
    assert_eq!(analysis.warnings, []);
}

/// Admitting `api_error` must not widen the allowlist by one inch, and must not
/// borrow the trailing machinery: it opens no zone, and a `turn_duration` tail
/// still behaves exactly as before.
#[test]
fn api_error_opens_no_trailing_zone_and_every_other_subtype_stays_rejected() {
    for subtype in [
        "compact_boundary",
        "model_refusal_fallback",
        "away_summary",
        "local_command",
        "future-system",
    ] {
        let mut engine = TranscriptEngine::new(ParseMode::Strict);
        engine.arm_turn("go").unwrap();
        engine.ingest(parse(user(None, "u", "go"))).unwrap();
        engine
            .ingest(parse(assistant(
                "u",
                "a",
                Some("m"),
                None,
                text("done"),
                Some("end_turn"),
                1,
                1,
            )))
            .unwrap();
        engine
            .ingest(parse(format!(
                r#"{{"type":"system","subtype":"{subtype}","uuid":"system","parentUuid":"a","sessionId":"s","retryAttempt":1,"maxRetries":10}}"#
            )))
            .unwrap();
        assert!(
            matches!(
                engine.analyze(),
                Err(TranscriptError::SchemaDrift { ref path, .. }) if path == "$.subtype"
            ),
            "{subtype} must stay rejected on the active chain even carrying api_error's counters"
        );
    }

    // A retry is not a trailing marker, so the reply, its tool traffic, and a
    // closing turn_duration all remain legal after one.
    let mut recovered = TranscriptEngine::new(ParseMode::Strict);
    recovered.arm_turn("go").unwrap();
    recovered.ingest(parse(user(None, "u", "go"))).unwrap();
    recovered
        .ingest(parse(api_error("u", "retry-1", 1, 10)))
        .unwrap();
    recovered
        .ingest(parse(assistant(
            "retry-1",
            "call",
            Some("m-tool"),
            None,
            r#"[{"type":"tool_use","id":"tool-1","name":"Read","input":{"file_path":"README.md"}}]"#,
            Some("tool_use"),
            1,
            1,
        )))
        .unwrap();
    recovered
        .ingest(parse(tool_results(
            "call",
            "result",
            r#"[{"type":"tool_result","tool_use_id":"tool-1","content":"project readme"}]"#,
        )))
        .unwrap();
    recovered
        .ingest(parse(assistant(
            "result",
            "answer",
            Some("m-answer"),
            None,
            text("done"),
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();
    recovered
        .ingest(parse(
            r#"{"type":"system","subtype":"turn_duration","uuid":"duration","parentUuid":"answer","sessionId":"s","durationMs":1}"#,
        ))
        .unwrap();
    let analysis = recovered.analyze().unwrap();
    let TurnStatus::Terminal(final_turn) = &analysis.status else {
        panic!("the recovered turn must complete: {analysis:#?}");
    };
    assert_eq!(final_turn.final_text, "done");
    assert_eq!(analysis.api_error_retries_seen, 1);
    assert!(analysis.turn_duration_seen);
    assert_eq!(analysis.tools.len(), 1);
    assert_eq!(analysis.warnings, []);

    // The zone turn_duration opens is unchanged by any of this: a retry landing
    // after the closing marker still contradicts it. Refusal, not completion.
    let mut after_marker = TranscriptEngine::new(ParseMode::Strict);
    after_marker.arm_turn("go").unwrap();
    after_marker.ingest(parse(user(None, "u", "go"))).unwrap();
    after_marker
        .ingest(parse(assistant(
            "u",
            "a",
            Some("m"),
            None,
            text("done"),
            Some("end_turn"),
            1,
            1,
        )))
        .unwrap();
    after_marker
        .ingest(parse(
            r#"{"type":"system","subtype":"turn_duration","uuid":"duration","parentUuid":"a","sessionId":"s","durationMs":1}"#,
        ))
        .unwrap();
    after_marker
        .ingest(parse(api_error("duration", "retry-1", 1, 10)))
        .unwrap();
    assert!(
        matches!(
            after_marker.analyze().unwrap().status,
            TurnStatus::Running { .. }
        ),
        "an api_error after a closing marker is an unobserved shape, so pmux must not complete"
    );
}

#[test]
fn strict_terminal_success_requires_text_but_refusal_may_be_textless() {
    for reason in ["end_turn", "max_tokens", "stop_sequence"] {
        let mut textless = TranscriptEngine::new(ParseMode::Strict);
        textless.arm_turn("go").unwrap();
        textless.ingest(parse(user(None, "u", "go"))).unwrap();
        textless
            .ingest(parse(assistant(
                "u",
                "a",
                Some("m"),
                None,
                thinking("no text"),
                Some(reason),
                1,
                1,
            )))
            .unwrap();
        assert!(
            matches!(
                textless.analyze(),
                Err(TranscriptError::TerminalMessageMissingText { .. })
            ),
            "{reason} accepted a textless success"
        );
    }

    let mut refusal = TranscriptEngine::new(ParseMode::Strict);
    refusal.arm_turn("go").unwrap();
    refusal.ingest(parse(user(None, "u", "go"))).unwrap();
    refusal
        .ingest(parse(assistant(
            "u",
            "a",
            Some("m"),
            None,
            thinking("policy"),
            Some("refusal"),
            1,
            1,
        )))
        .unwrap();
    assert!(matches!(
        refusal.analyze().unwrap().status,
        TurnStatus::Terminal(ref turn) if turn.outcome == TerminalOutcome::Refused
    ));
}

#[test]
fn api_error_overrides_end_turn_and_prompt_matching_is_exact_after_normalization() {
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn("line one\r\nline two").unwrap();
    assert!(matches!(
        engine.ingest(parse(user(None, "u", "line one\nline two"))),
        Ok(IngestOutcome::PromptAcknowledged(_))
    ));
    engine
        .ingest(parse(format!(
            r#"{{"parentUuid":"u","sessionId":"s","type":"assistant","isApiErrorMessage":true,"uuid":"a","message":{{"id":"m","model":"test","content":{},"stop_reason":"future_error_stop","usage":{{"input_tokens":1,"output_tokens":1}}}}}}"#,
            text("error")
        )))
        .unwrap();
    let analysis = engine.analyze().unwrap();
    assert!(matches!(
        analysis.status,
        TurnStatus::Terminal(ref final_turn) if final_turn.outcome == TerminalOutcome::ApiError
    ));

    let mut mismatch = TranscriptEngine::new(ParseMode::Strict);
    let expected_secret = "expected prompt secret ";
    let actual_secret = "unexpected prompt secret";
    mismatch.arm_turn(expected_secret).unwrap();
    let error = mismatch
        .ingest(parse(user(None, "u2", actual_secret)))
        .unwrap_err();
    assert!(matches!(
        error,
        TranscriptError::UnexpectedTypedPrompt { .. }
    ));
    let rendered = error.to_string();
    assert!(!rendered.contains(expected_secret));
    assert!(!rendered.contains(actual_secret));
    assert_eq!(
        mismatch.row_count(),
        0,
        "a mismatched first typed prompt must fail before graph mutation"
    );
}

/// The prompt pmux typed and the prompt Claude recorded, MEASURED, byte for
/// byte, through one real `pmux ask` at Claude Code 2.1.226.
///
/// The turn armed with `e` + U+0301 COMBINING ACUTE ACCENT and the child's own
/// transcript row came back carrying U+00E9 -- the composer records NFC. Before
/// `normalize_prompt` composed, this pair raised
/// [`TranscriptError::UnexpectedTypedPrompt`], the daemon answered
/// `PromptNotAcknowledged`, and the Path B pool destroyed the instance proving
/// it: a prompt containing one accented character, which is what macOS's own
/// NFD filesystem hands a caller, emptied a pooled slot.
///
/// The second pair is the heavier sequence from the same session, where the
/// composition is partial: U+0065 U+0327 U+0331 U+0301 U+0361 was recorded as
/// U+0229 U+0331 U+0301 U+0361, composing the cedilla only. It is here because
/// a fix that only handled the fully-composable case would pass on the first
/// pair and still lose the instance on the second.
#[test]
fn a_decomposed_prompt_is_acknowledged_by_the_composed_row_claude_records() {
    for (index, (typed, recorded)) in [
        (
            "Nonce N2. e\u{301} What is 3 plus 4?",
            "Nonce N2. \u{e9} What is 3 plus 4?",
        ),
        (
            "Nonce N3. e\u{327}\u{331}\u{301}\u{361}a",
            "Nonce N3. \u{229}\u{331}\u{301}\u{361}a",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        assert_ne!(typed, recorded, "the pair must differ before normalization");
        let mut engine = TranscriptEngine::new(ParseMode::Strict);
        engine.arm_turn(typed).unwrap();
        // JSON-encoded rather than built through `user`, whose `{:?}` escaping
        // is Rust's and not JSON's: `\u{327}` is a valid Rust escape and an
        // invalid JSON one, and the row under test is made of exactly those.
        let uuid = format!("u{index}");
        let row = format!(
            r#"{{"parentUuid":null,"sessionId":"s","type":"user","message":{{"role":"user","content":{}}},"uuid":"{uuid}","promptSource":"typed","promptId":"prompt-{uuid}"}}"#,
            serde_json::to_string(recorded).unwrap()
        );
        assert!(
            matches!(
                engine.ingest(parse(row)),
                Ok(IngestOutcome::PromptAcknowledged(_))
            ),
            "pmux typed {typed:?} and Claude recorded {recorded:?}; the turn must be acknowledged"
        );
    }
}

#[test]
fn duplicate_rows_are_idempotent_but_conflicting_uuid_is_rejected() {
    let row = parse(user(None, "u", "go"));
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn("go").unwrap();
    engine.ingest(row.clone()).unwrap();
    assert_eq!(
        engine.ingest(row).unwrap(),
        IngestOutcome::DuplicateIgnored {
            uuid: "u".to_owned()
        }
    );
    assert!(matches!(
        engine.ingest(parse(user(None, "u", "different"))),
        Err(TranscriptError::ConflictingDuplicateRow { .. })
    ));
}

fn fixture(name: &str, mode: ParseMode) -> Vec<ParsedRow> {
    let content = fs::read(format!("tests/fixtures/{name}")).unwrap();
    let parser = JsonlParser::new(mode);
    content
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .enumerate()
        .map(|(index, bytes)| {
            parser
                .parse(&CompleteLine {
                    location: SourceLocation {
                        line: index as u64 + 1,
                        byte_offset: 0,
                    },
                    bytes: bytes.to_vec(),
                })
                .unwrap()
        })
        .collect()
}

fn parse(json: impl AsRef<str>) -> ParsedRow {
    JsonlParser::new(ParseMode::Strict)
        .parse(&CompleteLine {
            location: SourceLocation {
                line: 1,
                byte_offset: 0,
            },
            bytes: json.as_ref().as_bytes().to_vec(),
        })
        .unwrap()
}

fn user(parent: Option<&str>, uuid: &str, prompt: &str) -> String {
    let parent = parent.map_or("null".to_owned(), |value| format!(r#""{value}""#));
    format!(
        r#"{{"parentUuid":{parent},"sessionId":"s","type":"user","message":{{"role":"user","content":{prompt:?}}},"uuid":"{uuid}","promptSource":"typed","promptId":"prompt-{uuid}"}}"#
    )
}

fn attachment(parent: &str, uuid: &str, attachment_type: &str) -> String {
    format!(
        r#"{{"parentUuid":"{parent}","sessionId":"s","type":"attachment","uuid":"{uuid}","attachment":{{"type":"{attachment_type}"}}}}"#
    )
}

#[allow(clippy::too_many_arguments)]
fn assistant(
    parent: &str,
    uuid: &str,
    message_id: Option<&str>,
    request_id: Option<&str>,
    content: impl AsRef<str>,
    stop_reason: Option<&str>,
    input_tokens: u64,
    output_tokens: u64,
) -> String {
    assistant_with_scope(
        parent,
        uuid,
        message_id,
        request_id,
        content,
        stop_reason,
        input_tokens,
        output_tokens,
        "",
    )
}

#[allow(clippy::too_many_arguments)]
fn assistant_with_scope(
    parent: &str,
    uuid: &str,
    message_id: Option<&str>,
    request_id: Option<&str>,
    content: impl AsRef<str>,
    stop_reason: Option<&str>,
    input_tokens: u64,
    output_tokens: u64,
    extra_top_level: &str,
) -> String {
    let id = message_id.map_or(String::new(), |value| format!(r#", "id":"{value}""#));
    let request = request_id.map_or(String::new(), |value| format!(r#", "requestId":"{value}""#));
    let stop = stop_reason.map_or("null".to_owned(), |value| format!(r#""{value}""#));
    format!(
        r#"{{"parentUuid":"{parent}","sessionId":"s","type":"assistant"{extra_top_level}{request},"uuid":"{uuid}","message":{{"model":"test"{id},"content":{},"stop_reason":{stop},"usage":{{"input_tokens":{input_tokens},"output_tokens":{output_tokens}}}}}}}"#,
        content.as_ref()
    )
}

/// `extra_top_level` carries the payload under proof, so every case states its
/// own `preventedContinuation` (or omits it) explicitly rather than inheriting a
/// default from this helper.
fn stop_hook_summary(parent: &str, uuid: &str, extra_top_level: &str) -> String {
    format!(
        r#"{{"parentUuid":"{parent}","isSidechain":false,"sessionId":"s","type":"system","subtype":"stop_hook_summary","uuid":"{uuid}","hookCount":1,"hookInfos":[{{"command":"pmux-hook --event Stop","durationMs":14}}],"hasOutput":false,"level":"suggestion"{extra_top_level}}}"#
    )
}

/// The real `api_error` field shape: every key is present in the 115 rows
/// observed across every transcript on this machine, and `maxRetries` was 10 in
/// all of them.
fn api_error(parent: &str, uuid: &str, retry_attempt: u64, max_retries: u64) -> String {
    format!(
        r#"{{"parentUuid":"{parent}","isSidechain":false,"type":"system","subtype":"api_error","error":"Connection error (ECONNRESET)","level":"error","retryAttempt":{retry_attempt},"maxRetries":{max_retries},"retryInMs":1000,"uuid":"{uuid}","timestamp":"2026-07-30T01:28:03.001Z","sessionId":"s","cwd":"/tmp","gitBranch":"HEAD","version":"2.1.220","entrypoint":"cli","userType":"external","slug":"go"}}"#
    )
}

fn tool_results(parent: &str, uuid: &str, content: &str) -> String {
    format!(
        r#"{{"parentUuid":"{parent}","sessionId":"s","type":"user","uuid":"{uuid}","message":{{"role":"user","content":{content}}}}}"#
    )
}

fn text(value: &str) -> String {
    format!(r#"[{{"type":"text","text":{value:?}}}]"#)
}

fn thinking(value: &str) -> String {
    format!(r#"[{{"type":"thinking","thinking":{value:?}}}]"#)
}
