#![no_main]

use std::collections::HashSet;

use libfuzzer_sys::fuzz_target;
use pseudomux_claude::{
    ContentBlock, FileIdentity, FileMetadata, JsonlParser, ParseMode, StopReason, TerminalOutcome,
    TokenUsage, TranscriptAnalysis, TranscriptCursor, TranscriptEngine, TurnStatus, UsageTotals,
};

const MAX_INPUT_BYTES: usize = 1024 * 1024;
const IDENTITY: FileIdentity = FileIdentity {
    device: 17,
    inode: 29,
};

fuzz_target!(|input: &[u8]| {
    let input = &input[..input.len().min(MAX_INPUT_BYTES)];
    let mut cursor = TranscriptCursor::new();
    let observation = cursor.observe(FileMetadata {
        identity: IDENTITY,
        len: input.len() as u64,
    });
    let update = cursor
        .push(IDENTITY, observation.read_from, input)
        .expect("a bounded exact production-cursor range must frame or buffer");
    assert_eq!(update.next_offset, input.len() as u64);
    assert_eq!(
        cursor.has_partial_line(),
        !input.is_empty() && !input.ends_with(b"\n")
    );

    let parser = JsonlParser::new(ParseMode::Strict);
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    // A fixed prompt makes arbitrary typed-user rows exercise exact
    // acknowledgement, graph, tool, usage, and terminal rejection paths.
    engine
        .arm_turn("fuzz")
        .expect("a new production engine must accept exactly one arm");

    for line in update.lines {
        // A real transcript tailer cannot skip an invalid record and regain
        // authority later in the same generation. The fuzz harness follows
        // that same stop-on-first-error rule.
        let Ok(row) = parser.parse(&line) else {
            return;
        };
        if engine.ingest(row).is_err() {
            return;
        }
        let Ok(analysis) = engine.analyze() else {
            return;
        };
        assert_semantic_consistency(&analysis);
    }
});

fn assert_semantic_consistency(analysis: &TranscriptAnalysis) {
    let mut message_keys = HashSet::new();
    let mut row_uuids = HashSet::new();
    for message in &analysis.messages {
        assert!(message_keys.insert(&message.key));
        assert!(message.first_ordinal <= message.last_ordinal);
        for uuid in &message.row_uuids {
            assert!(row_uuids.insert(uuid));
        }
    }

    let expected_usage = usage_for_messages(&analysis.messages);
    assert_eq!(analysis.usage, expected_usage);
    assert_eq!(
        analysis.combined_usage,
        add_usage(&analysis.usage, &analysis.sidechain_usage)
    );

    let mut active_uuids = HashSet::new();
    assert!(
        analysis
            .active_chain
            .iter()
            .all(|uuid| active_uuids.insert(uuid))
    );
    if let Some(acknowledgement) = &analysis.acknowledgement {
        assert_eq!(
            analysis.active_chain.first(),
            Some(&acknowledgement.row_uuid)
        );
    } else {
        assert!(matches!(
            analysis.status,
            TurnStatus::AwaitingPromptAcknowledgement
        ));
        assert!(analysis.active_chain.is_empty());
        assert!(analysis.messages.is_empty());
        assert!(analysis.tools.is_empty());
        assert_eq!(analysis.usage, UsageTotals::default());
    }

    let mut tool_ids = HashSet::new();
    for (order, tool) in analysis.tools.iter().enumerate() {
        assert!(tool_ids.insert(&tool.tool_use_id));
        assert_eq!(tool.order, order as u64);
        if let Some(result) = &tool.result {
            assert_eq!(result.tool_use_id, tool.tool_use_id);
        }
    }

    let TurnStatus::Terminal(final_turn) = &analysis.status else {
        return;
    };
    let message = analysis
        .messages
        .iter()
        .find(|message| message.key == final_turn.message_key)
        .expect("a terminal result must identify one exposed logical message");
    assert_eq!(
        message.last_ordinal,
        analysis
            .messages
            .iter()
            .map(|candidate| candidate.last_ordinal)
            .max()
            .expect("terminal analysis has a message")
    );
    assert_eq!(final_turn.stop_reason, message.stop_reason);
    assert_eq!(final_turn.model, message.model);
    let expected_blocks: Vec<String> = message
        .blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(final_turn.final_text_blocks, expected_blocks);
    assert_eq!(final_turn.final_text, final_turn.final_text_blocks.concat());

    match final_turn.outcome {
        TerminalOutcome::Completed => assert!(matches!(
            final_turn.stop_reason,
            Some(StopReason::EndTurn | StopReason::StopSequence)
        )),
        TerminalOutcome::MaxTokens => {
            assert_eq!(final_turn.stop_reason, Some(StopReason::MaxTokens));
        }
        TerminalOutcome::Refused => {
            assert_eq!(final_turn.stop_reason, Some(StopReason::Refusal));
        }
        TerminalOutcome::ApiError => assert!(message.is_api_error),
    }
    if matches!(
        final_turn.outcome,
        TerminalOutcome::Completed | TerminalOutcome::MaxTokens
    ) {
        assert!(!final_turn.final_text_blocks.is_empty());
    }
}

fn usage_for_messages(messages: &[pseudomux_claude::LogicalAssistantMessage]) -> UsageTotals {
    let mut totals = UsageTotals::default();
    for usage in messages.iter().filter_map(|message| message.usage.as_ref()) {
        totals.model_calls_with_usage = totals
            .model_calls_with_usage
            .checked_add(1)
            .expect("successful analysis already rejected usage overflow");
        totals.tokens = add_tokens(&totals.tokens, &usage.tokens);
    }
    totals
}

fn add_usage(left: &UsageTotals, right: &UsageTotals) -> UsageTotals {
    UsageTotals {
        tokens: add_tokens(&left.tokens, &right.tokens),
        model_calls_with_usage: left
            .model_calls_with_usage
            .checked_add(right.model_calls_with_usage)
            .expect("successful analysis already rejected usage overflow"),
    }
}

fn add_tokens(left: &TokenUsage, right: &TokenUsage) -> TokenUsage {
    TokenUsage {
        input_tokens: left
            .input_tokens
            .checked_add(right.input_tokens)
            .expect("successful analysis already rejected usage overflow"),
        output_tokens: left
            .output_tokens
            .checked_add(right.output_tokens)
            .expect("successful analysis already rejected usage overflow"),
        cache_creation_input_tokens: left
            .cache_creation_input_tokens
            .checked_add(right.cache_creation_input_tokens)
            .expect("successful analysis already rejected usage overflow"),
        cache_read_input_tokens: left
            .cache_read_input_tokens
            .checked_add(right.cache_read_input_tokens)
            .expect("successful analysis already rejected usage overflow"),
    }
}
