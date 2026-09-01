use proptest::{
    prelude::*,
    test_runner::{Config as ProptestConfig, RngAlgorithm, RngSeed},
};
use pseudomux_claude::{
    CompleteLine, CursorChange, FileIdentity, FileMetadata, JsonlParser, LogicalMessageKey,
    ParseMode, SourceLocation, StopReason, TerminalOutcome, TokenUsage, TranscriptAnalysis,
    TranscriptCursor, TranscriptEngine, TranscriptError, TurnStatus,
};
use serde_json::{Value, json};

const IDENTITY: FileIdentity = FileIdentity {
    device: 7,
    inode: 11,
};

fn deterministic_config() -> ProptestConfig {
    ProptestConfig {
        failure_persistence: None,
        rng_algorithm: RngAlgorithm::ChaCha,
        rng_seed: RngSeed::Fixed(0x504d_5558_434c_4155),
        ..ProptestConfig::default()
    }
}

fn record_strategy() -> impl Strategy<Value = (Vec<u8>, bool)> {
    (
        prop::collection::vec(
            any::<u8>().prop_filter("record bytes exclude line terminators", |byte| {
                !matches!(*byte, b'\r' | b'\n')
            }),
            0..128,
        ),
        any::<bool>(),
    )
}

#[derive(Clone, Debug)]
struct Mutation {
    action: u8,
    payload: Vec<u8>,
    cutoff: usize,
    chunks: Vec<usize>,
}

fn mutation_strategy() -> impl Strategy<Value = Mutation> {
    (
        0_u8..3,
        prop::collection::vec(any::<u8>(), 0..160),
        any::<usize>(),
        prop::collection::vec(1_usize..48, 1..12),
    )
        .prop_map(|(action, payload, cutoff, chunks)| Mutation {
            action,
            payload,
            cutoff,
            chunks,
        })
}

#[derive(Default)]
struct ReferenceFramer {
    pending: Vec<u8>,
    pending_offset: u64,
    next_line: u64,
}

impl ReferenceFramer {
    fn reset(&mut self) {
        self.pending.clear();
        self.pending_offset = 0;
        self.next_line = 1;
    }

    fn push(&mut self, read_offset: u64, bytes: &[u8]) -> Vec<CompleteLine> {
        if self.pending.is_empty() {
            self.pending_offset = read_offset;
        }
        self.pending.extend_from_slice(bytes);

        let mut lines = Vec::new();
        while let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line_end = if newline > 0 && self.pending[newline - 1] == b'\r' {
                newline - 1
            } else {
                newline
            };
            lines.push(CompleteLine {
                location: SourceLocation {
                    line: self.next_line,
                    byte_offset: self.pending_offset,
                },
                bytes: self.pending[..line_end].to_vec(),
            });
            self.next_line += 1;
            self.pending.drain(..=newline);
            self.pending_offset += newline as u64 + 1;
        }
        lines
    }
}

fn line(value: &Value, line_number: u64) -> CompleteLine {
    CompleteLine {
        location: SourceLocation {
            line: line_number,
            byte_offset: line_number.saturating_mul(100),
        },
        bytes: serde_json::to_vec(value).unwrap(),
    }
}

fn string_strategy(maximum: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..maximum)
        .prop_map(|characters| characters.into_iter().collect())
}

#[derive(Clone, Debug)]
struct UsageCase {
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_input_tokens: u64,
    cache_read_input_tokens: u64,
}

impl UsageCase {
    fn json(&self) -> Value {
        json!({
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
            "cache_creation_input_tokens": self.cache_creation_input_tokens,
            "cache_read_input_tokens": self.cache_read_input_tokens,
        })
    }

    fn tokens(&self) -> TokenUsage {
        TokenUsage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_creation_input_tokens: self.cache_creation_input_tokens,
            cache_read_input_tokens: self.cache_read_input_tokens,
        }
    }
}

fn usage_strategy() -> impl Strategy<Value = UsageCase> {
    (
        0_u64..1_000_000,
        0_u64..1_000_000,
        0_u64..1_000_000,
        0_u64..1_000_000,
    )
        .prop_map(
            |(
                input_tokens,
                output_tokens,
                cache_creation_input_tokens,
                cache_read_input_tokens,
            )| UsageCase {
                input_tokens,
                output_tokens,
                cache_creation_input_tokens,
                cache_read_input_tokens,
            },
        )
}

fn add_tokens(left: &TokenUsage, right: &TokenUsage) -> TokenUsage {
    TokenUsage {
        input_tokens: left.input_tokens + right.input_tokens,
        output_tokens: left.output_tokens + right.output_tokens,
        cache_creation_input_tokens: left.cache_creation_input_tokens
            + right.cache_creation_input_tokens,
        cache_read_input_tokens: left.cache_read_input_tokens + right.cache_read_input_tokens,
    }
}

fn prompt_row(prompt: &str) -> Value {
    json!({
        "parentUuid": null,
        "sessionId": "session",
        "type": "user",
        "uuid": "prompt",
        "promptSource": "typed",
        "promptId": "prompt-id",
        "message": {"role": "user", "content": prompt},
    })
}

#[allow(clippy::too_many_arguments)]
fn assistant_row(
    parent_uuid: &str,
    uuid: &str,
    message_id: Option<&str>,
    request_id: Option<&str>,
    model: &str,
    content: Vec<Value>,
    stop_reason: Option<&str>,
    usage: Option<&UsageCase>,
    is_sidechain: bool,
    is_api_error: bool,
) -> Value {
    let mut row = json!({
        "parentUuid": parent_uuid,
        "sessionId": "session",
        "type": "assistant",
        "uuid": uuid,
        "message": {
            "model": model,
            "content": content,
            "stop_reason": stop_reason,
        },
    });
    if let Some(message_id) = message_id {
        row["message"]["id"] = json!(message_id);
    }
    if let Some(request_id) = request_id {
        row["requestId"] = json!(request_id);
    }
    if let Some(usage) = usage {
        row["message"]["usage"] = usage.json();
    }
    if is_sidechain {
        row["isSidechain"] = Value::Bool(true);
    }
    if is_api_error {
        row["isApiErrorMessage"] = Value::Bool(true);
    }
    row
}

fn attachment_row(parent_uuid: &str, uuid: &str, attachment_type: &str) -> Value {
    json!({
        "parentUuid": parent_uuid,
        "sessionId": "session",
        "type": "attachment",
        "uuid": uuid,
        "attachment": {"type": attachment_type},
    })
}

fn tool_results_row(parent_uuid: &str, uuid: &str, results: Vec<Value>) -> Value {
    json!({
        "parentUuid": parent_uuid,
        "sessionId": "session",
        "type": "user",
        "uuid": uuid,
        "message": {"role": "user", "content": results},
    })
}

fn analyze_values(prompt: &str, values: &[Value]) -> Result<TranscriptAnalysis, TranscriptError> {
    let parser = JsonlParser::new(ParseMode::Strict);
    let mut engine = TranscriptEngine::new(ParseMode::Strict);
    engine.arm_turn(prompt)?;
    for (index, value) in values.iter().enumerate() {
        let row = parser.parse(&line(value, index as u64 + 1))?;
        engine.ingest(row)?;
    }
    engine.analyze()
}

fn semantic_mutation_rows(
    prompt: &str,
    mutation: u8,
    generated_text: &str,
    generated_usage: &UsageCase,
) -> Vec<Value> {
    let mut rows = vec![prompt_row(prompt)];
    let text_block = || vec![json!({"type": "text", "text": generated_text})];
    let thinking_block = || vec![json!({"type": "thinking", "thinking": generated_text})];

    match mutation {
        0 => {
            let first = generated_usage.clone();
            let mut conflicting = generated_usage.clone();
            conflicting.output_tokens += 1;
            rows.push(assistant_row(
                "prompt",
                "fragment-a",
                Some("one-message"),
                None,
                "model",
                thinking_block(),
                None,
                Some(&first),
                false,
                false,
            ));
            rows.push(assistant_row(
                "fragment-a",
                "fragment-b",
                Some("one-message"),
                None,
                "model",
                text_block(),
                Some("end_turn"),
                Some(&conflicting),
                false,
                false,
            ));
        }
        1 => {
            rows.push(assistant_row(
                "prompt",
                "fragment-a",
                Some("one-message"),
                None,
                "model-a",
                thinking_block(),
                None,
                Some(generated_usage),
                false,
                false,
            ));
            rows.push(assistant_row(
                "fragment-a",
                "fragment-b",
                Some("one-message"),
                None,
                "model-b",
                text_block(),
                Some("end_turn"),
                Some(generated_usage),
                false,
                false,
            ));
        }
        2 => {
            rows.push(assistant_row(
                "prompt",
                "fragment-a",
                Some("one-message"),
                None,
                "model",
                thinking_block(),
                Some("tool_use"),
                Some(generated_usage),
                false,
                false,
            ));
            rows.push(assistant_row(
                "fragment-a",
                "fragment-b",
                Some("one-message"),
                None,
                "model",
                text_block(),
                Some("end_turn"),
                Some(generated_usage),
                false,
                false,
            ));
        }
        3 => rows.push(assistant_row(
            "prompt",
            "duplicate-calls",
            Some("call-message"),
            None,
            "model",
            vec![
                json!({"type": "tool_use", "id": "same", "name": generated_text, "input": {"n": 1}}),
                json!({"type": "tool_use", "id": "same", "name": generated_text, "input": {"n": 2}}),
            ],
            Some("tool_use"),
            Some(generated_usage),
            false,
            false,
        )),
        4 => {
            rows.push(assistant_row(
                "prompt",
                "call",
                Some("call-message"),
                None,
                "model",
                vec![json!({"type": "tool_use", "id": "tool", "name": generated_text, "input": {}})],
                Some("tool_use"),
                Some(generated_usage),
                false,
                false,
            ));
            rows.push(tool_results_row(
                "call",
                "duplicate-results",
                vec![
                    json!({"type": "tool_result", "tool_use_id": "tool", "content": generated_text}),
                    json!({"type": "tool_result", "tool_use_id": "tool", "content": generated_text}),
                ],
            ));
        }
        5 => rows.push(tool_results_row(
            "prompt",
            "orphan-result",
            vec![json!({
                "type": "tool_result",
                "tool_use_id": generated_text,
                "content": {"generated": generated_text},
            })],
        )),
        6 => rows.push(assistant_row(
            "prompt",
            "unknown-stop",
            Some("unknown-stop-message"),
            None,
            "model",
            text_block(),
            Some("future_stop_reason"),
            Some(generated_usage),
            false,
            false,
        )),
        7 => {
            let overflowing = UsageCase {
                input_tokens: u64::MAX,
                output_tokens: generated_usage.output_tokens,
                cache_creation_input_tokens: generated_usage.cache_creation_input_tokens,
                cache_read_input_tokens: generated_usage.cache_read_input_tokens,
            };
            let positive = UsageCase {
                input_tokens: generated_usage.input_tokens + 1,
                output_tokens: 0,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            };
            rows.push(assistant_row(
                "prompt",
                "overflow-a",
                Some("overflow-message-a"),
                None,
                "model",
                thinking_block(),
                Some("tool_use"),
                Some(&overflowing),
                false,
                false,
            ));
            rows.push(assistant_row(
                "overflow-a",
                "overflow-b",
                Some("overflow-message-b"),
                None,
                "model",
                text_block(),
                Some("end_turn"),
                Some(&positive),
                false,
                false,
            ));
        }
        8 => {
            rows.push(assistant_row(
                "prompt",
                "a-one",
                Some("message-a"),
                None,
                "model",
                thinking_block(),
                None,
                Some(generated_usage),
                false,
                false,
            ));
            rows.push(assistant_row(
                "a-one",
                "b",
                Some("message-b"),
                None,
                "model",
                thinking_block(),
                Some("tool_use"),
                Some(generated_usage),
                false,
                false,
            ));
            rows.push(assistant_row(
                "b",
                "a-two",
                Some("message-a"),
                None,
                "model",
                text_block(),
                Some("end_turn"),
                Some(generated_usage),
                false,
                false,
            ));
        }
        9 => {
            let mut conflict = prompt_row(prompt);
            conflict["promptId"] = json!(generated_text);
            conflict["mutationMarker"] = Value::Bool(true);
            rows.insert(1, conflict);
        }
        10 => rows.push(assistant_row(
            "prompt",
            "unknown-content",
            Some("unknown-content-message"),
            None,
            "model",
            vec![json!({"type": "future_content", "generated": generated_text})],
            Some("end_turn"),
            Some(generated_usage),
            false,
            false,
        )),
        _ => unreachable!("semantic mutation strategy is bounded"),
    }
    rows
}

fn graph_mutation_rows(prompt: &str, depth: usize, mutation: u8, text: &str) -> Vec<Value> {
    // Every name here is admitted by `is_supported_attachment_type`, so the
    // chain this builds is one Claude could really write.
    // `remote_session_change` joined the set at 2.1.257 (MEASURED linux/x86_64,
    // `SessionCell::Minified`) and rides an arbitrary-depth chain here for the
    // same reason the others do.
    const ATTACHMENTS: [&str; 6] = [
        "agent_listing_delta",
        "deferred_tools_delta",
        "file",
        "remote_session_change",
        "skill_listing",
        "task_reminder",
    ];
    let mut rows = vec![prompt_row(prompt)];
    let mut parent = "prompt".to_owned();
    for index in 0..depth {
        let uuid = format!("attachment-{index}");
        rows.push(attachment_row(
            &parent,
            &uuid,
            ATTACHMENTS[index % ATTACHMENTS.len()],
        ));
        parent = uuid;
    }
    rows.push(assistant_row(
        &parent,
        "answer",
        Some("answer-message"),
        None,
        "model",
        vec![json!({"type": "text", "text": text})],
        Some("end_turn"),
        None,
        false,
        false,
    ));

    let answer_index = rows.len() - 1;
    match mutation {
        0 => rows[answer_index]["parentUuid"] = json!("missing-parent"),
        1 => rows[1]["parentUuid"] = json!("answer"),
        2 => rows.push(assistant_row(
            &parent,
            "sibling-answer",
            Some("sibling-message"),
            None,
            "model",
            vec![json!({"type": "text", "text": text})],
            Some("end_turn"),
            None,
            false,
            false,
        )),
        3 => {
            let mut conflict = prompt_row(prompt);
            conflict["promptId"] = json!(text);
            conflict["mutationMarker"] = Value::Bool(true);
            rows.insert(1, conflict);
        }
        4 => {
            let mut second_prompt = prompt_row(prompt);
            second_prompt["uuid"] = json!("second-prompt");
            second_prompt["parentUuid"] = json!("prompt");
            rows.insert(1, second_prompt);
        }
        5 => {
            rows[answer_index]["message"]["content"] =
                json!([{"type": "future_content", "generated": text}]);
        }
        6 => rows[answer_index]["message"]["stop_reason"] = json!("future_stop_reason"),
        7 => rows.swap(answer_index, answer_index - 1),
        _ => unreachable!("graph mutation strategy is bounded"),
    }
    rows
}

fn mutated_turn(mutation: u8) -> (Value, Value) {
    let mut prompt = json!({
        "parentUuid": null,
        "sessionId": "session",
        "type": "user",
        "uuid": "prompt",
        "promptSource": "typed",
        "promptId": "prompt-id",
        "message": {"role": "user", "content": "go"},
    });
    let mut answer = json!({
        "parentUuid": "prompt",
        "sessionId": "session",
        "type": "assistant",
        "uuid": "answer",
        "message": {
            "id": "message",
            "model": "model",
            "content": [{"type": "text", "text": "done"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1},
        },
    });

    match mutation {
        0 => prompt["uuid"] = Value::Bool(true),
        1 => prompt["parentUuid"] = Value::Bool(true),
        2 => prompt["sessionId"] = Value::Array(Vec::new()),
        3 => prompt["message"] = Value::Null,
        4 => prompt["message"]["role"] = Value::String("assistant".to_owned()),
        5 => prompt["message"]["content"] = Value::Array(Vec::new()),
        6 => prompt["promptSource"] = Value::Bool(true),
        7 => prompt["promptId"] = json!({"wrong": true}),
        8 => prompt["isSidechain"] = Value::String("true".to_owned()),
        9 => answer["uuid"] = Value::Null,
        10 => answer["parentUuid"] = json!(17),
        11 => answer["message"] = Value::Null,
        12 => answer["message"]["content"] = Value::String("done".to_owned()),
        13 => answer["message"]["content"][0]["text"] = Value::Bool(true),
        14 => answer["message"]["stop_reason"] = Value::Bool(true),
        15 => answer["message"]["stop_reason"] = Value::String("future_stop".to_owned()),
        16 => answer["message"]["usage"] = Value::String("one".to_owned()),
        17 => answer["message"]["usage"]["input_tokens"] = json!(-1),
        18 => answer["message"]["usage"] = json!({}),
        19 => answer["message"]["id"] = Value::Bool(true),
        20 => prompt["type"] = Value::String("future-user".to_owned()),
        21 => answer["type"] = Value::String("future-assistant".to_owned()),
        22 => answer["isApiErrorMessage"] = Value::String("true".to_owned()),
        23 => answer["message"]["stop_reason"] = Value::Null,
        24 => answer["message"]["stop_reason"] = Value::String("tool_use".to_owned()),
        25 => answer["isSidechain"] = Value::Bool(true),
        26 => answer["message"]["content"] = Value::Array(Vec::new()),
        _ => unreachable!("mutation strategy is bounded"),
    }
    (prompt, answer)
}

proptest! {
    #![proptest_config(deterministic_config())]

    #[test]
    fn cursor_frames_arbitrary_records_for_arbitrary_chunks(
        records in prop::collection::vec(record_strategy(), 0..48),
        chunks in prop::collection::vec(1_usize..96, 1..24),
    ) {
        let mut bytes = Vec::new();
        let mut expected = Vec::new();
        let mut offset = 0_u64;
        for (index, (record, carriage_return)) in records.into_iter().enumerate() {
            expected.push(CompleteLine {
                location: SourceLocation {
                    line: index as u64 + 1,
                    byte_offset: offset,
                },
                bytes: record.clone(),
            });
            offset += record.len() as u64 + u64::from(carriage_return) + 1;
            bytes.extend_from_slice(&record);
            if carriage_return {
                bytes.push(b'\r');
            }
            bytes.push(b'\n');
        }

        let mut cursor = TranscriptCursor::new();
        let observation = cursor.observe(FileMetadata {
            identity: IDENTITY,
            len: bytes.len() as u64,
        });
        prop_assert_eq!(observation.change, CursorChange::Initialized);

        let mut actual = Vec::new();
        let mut read = 0_usize;
        let mut chunk_index = 0_usize;
        while read < bytes.len() {
            let end = (read + chunks[chunk_index % chunks.len()]).min(bytes.len());
            let update = cursor.push(IDENTITY, read as u64, &bytes[read..end])?;
            actual.extend(update.lines);
            read = end;
            chunk_index += 1;
        }

        prop_assert_eq!(actual, expected);
        prop_assert!(!cursor.has_partial_line());
        prop_assert_eq!(cursor.next_offset(), bytes.len() as u64);
    }

    #[test]
    fn cursor_matches_reference_across_append_truncate_and_replace_sequences(
        mutations in prop::collection::vec(mutation_strategy(), 1..56),
    ) {
        let mut cursor = TranscriptCursor::new();
        let mut reference = ReferenceFramer::default();
        let mut file = Vec::new();
        let mut identity = IDENTITY;
        let mut observed_identity = None;
        let mut expected_generation = 0_u64;

        for mutation in mutations {
            match mutation.action {
                0 => file.extend_from_slice(&mutation.payload),
                1 => {
                    let cutoff = mutation.cutoff % (file.len() + 1);
                    file.truncate(cutoff);
                }
                2 => {
                    file = mutation.payload;
                    identity.inode = identity.inode.saturating_add(1);
                }
                _ => unreachable!("mutation action is bounded"),
            }

            let previous_offset = cursor.next_offset();
            let expected_change = match observed_identity {
                None => CursorChange::Initialized,
                Some(previous) if previous != identity => CursorChange::Replaced {
                    previous,
                    current: identity,
                },
                Some(_) if (file.len() as u64) < previous_offset => CursorChange::Truncated {
                    previous_offset,
                    current_len: file.len() as u64,
                },
                Some(_) => CursorChange::Unchanged,
            };
            let reset = !matches!(expected_change, CursorChange::Unchanged);
            if reset {
                expected_generation += 1;
                reference.reset();
            }

            let observation = cursor.observe(FileMetadata {
                identity,
                len: file.len() as u64,
            });
            prop_assert_eq!(observation.change, expected_change);
            prop_assert_eq!(observation.generation, expected_generation);
            prop_assert_eq!(observation.read_from, if reset { 0 } else { previous_offset });
            prop_assert_eq!(observation.read_to, file.len() as u64);
            observed_identity = Some(identity);

            let mut read = observation.read_from as usize;
            let mut chunk_index = 0_usize;
            while read < observation.read_to as usize {
                let chunk_size = mutation.chunks[chunk_index % mutation.chunks.len()];
                let end = (read + chunk_size).min(observation.read_to as usize);
                let expected_lines = reference.push(read as u64, &file[read..end]);
                let update = cursor.push(identity, read as u64, &file[read..end])?;
                prop_assert_eq!(update.lines, expected_lines);
                prop_assert_eq!(update.generation, expected_generation);
                prop_assert_eq!(update.next_offset, end as u64);
                prop_assert_eq!(update.pending_bytes, reference.pending.len());
                read = end;
                chunk_index += 1;
            }
            prop_assert_eq!(cursor.next_offset(), file.len() as u64);
            prop_assert_eq!(cursor.has_partial_line(), !reference.pending.is_empty());
        }
    }

    #[test]
    fn strict_parser_is_total_over_arbitrary_bounded_bytes(
        bytes in prop::collection::vec(any::<u8>(), 0..2048),
        line_number in any::<u64>(),
        byte_offset in any::<u64>(),
    ) {
        let location = SourceLocation {
            line: line_number,
            byte_offset,
        };
        let parsed = JsonlParser::new(ParseMode::Strict).parse(&CompleteLine {
            location,
            bytes,
        });
        if let Ok(row) = parsed {
            prop_assert_eq!(row.source, location);
            prop_assert!(row.raw.is_object());
        }
    }

    #[test]
    fn generated_fragmented_tool_turns_preserve_graph_message_tool_usage_and_stop_semantics(
        prompt in string_strategy(96),
        final_fragments in prop::collection::vec(string_strategy(64), 1..6),
        tools in prop::collection::vec((string_strategy(48), any::<i32>()), 1..6),
        call_usage in usage_strategy(),
        final_usage in usage_strategy(),
        sidechain_usage in usage_strategy(),
        stop_case in 0_u8..7,
        use_request_id in any::<bool>(),
        include_sidechain in any::<bool>(),
        include_exact_duplicates in any::<bool>(),
        is_api_error in any::<bool>(),
    ) {
        let mut rows = vec![prompt_row(&prompt)];
        if include_sidechain {
            rows.push(assistant_row(
                "prompt",
                "sidechain",
                Some("sidechain-message"),
                None,
                "sidechain-model",
                vec![json!({"type": "text", "text": "sidechain"})],
                Some("end_turn"),
                Some(&sidechain_usage),
                true,
                false,
            ));
        }

        let tool_blocks: Vec<Value> = tools
            .iter()
            .enumerate()
            .map(|(index, (label, number))| {
                json!({
                    "type": "tool_use",
                    "id": format!("tool-{index}"),
                    "name": label,
                    "input": {"label": label, "number": number},
                })
            })
            .collect();
        rows.push(assistant_row(
            "prompt",
            "tool-calls",
            Some("tool-call-message"),
            None,
            "call-model",
            tool_blocks,
            Some("tool_use"),
            Some(&call_usage),
            false,
            false,
        ));

        let tool_results: Vec<Value> = tools
            .iter()
            .enumerate()
            .rev()
            .map(|(index, (label, number))| {
                json!({
                    "type": "tool_result",
                    "tool_use_id": format!("tool-{index}"),
                    "content": {"label": label, "number": number},
                    "is_error": index % 2 == 1,
                })
            })
            .collect();
        rows.push(tool_results_row("tool-calls", "tool-results", tool_results));

        let stop_reason = match stop_case {
            0 => Some("end_turn"),
            1 => Some("max_tokens"),
            2 => Some("stop_sequence"),
            3 => Some("refusal"),
            4 => Some("tool_use"),
            5 => Some("pause_turn"),
            6 => None,
            _ => unreachable!("stop strategy is bounded"),
        };
        let mut parent = "tool-results".to_owned();
        for (index, fragment) in final_fragments.iter().enumerate() {
            let uuid = format!("final-fragment-{index}");
            let is_last = index + 1 == final_fragments.len();
            rows.push(assistant_row(
                &parent,
                &uuid,
                (!use_request_id).then_some("final-message"),
                use_request_id.then_some("final-request"),
                "final-model",
                vec![json!({"type": "text", "text": fragment})],
                is_last.then_some(stop_reason).flatten(),
                Some(&final_usage),
                false,
                is_last && is_api_error,
            ));
            parent = uuid;
        }

        let mut delivered = Vec::new();
        for (index, row) in rows.into_iter().enumerate() {
            delivered.push(row.clone());
            if include_exact_duplicates && index % 2 == 0 {
                delivered.push(row);
            }
        }
        let analysis = analyze_values(&prompt, &delivered)?;

        prop_assert!(analysis.acknowledgement.is_some());
        prop_assert_eq!(analysis.messages.len(), 2);
        prop_assert_eq!(
            analysis.messages[1].key.clone(),
            if use_request_id {
                LogicalMessageKey::RequestId("final-request".to_owned())
            } else {
                LogicalMessageKey::MessageId("final-message".to_owned())
            }
        );
        prop_assert_eq!(analysis.messages[1].row_uuids.len(), final_fragments.len());

        prop_assert_eq!(analysis.tools.len(), tools.len());
        for (index, ((label, number), tool)) in tools.iter().zip(&analysis.tools).enumerate() {
            prop_assert_eq!(&tool.tool_use_id, &format!("tool-{index}"));
            prop_assert_eq!(&tool.name, label);
            prop_assert_eq!(&tool.input, &json!({"label": label, "number": number}));
            let result = tool.result.as_ref().expect("every generated call has a result");
            prop_assert_eq!(
                &result.content,
                &json!({"label": label, "number": number})
            );
            prop_assert_eq!(result.is_error, Some(index % 2 == 1));
            prop_assert_eq!(tool.order, index as u64);
        }

        let expected_main = add_tokens(&call_usage.tokens(), &final_usage.tokens());
        prop_assert_eq!(analysis.usage.tokens, expected_main.clone());
        prop_assert_eq!(analysis.usage.model_calls_with_usage, 2);
        let expected_sidechain = if include_sidechain {
            sidechain_usage.tokens()
        } else {
            TokenUsage::default()
        };
        prop_assert_eq!(analysis.sidechain_usage.tokens, expected_sidechain.clone());
        prop_assert_eq!(
            analysis.sidechain_usage.model_calls_with_usage,
            u64::from(include_sidechain)
        );
        prop_assert_eq!(
            analysis.combined_usage.tokens,
            add_tokens(&expected_main, &expected_sidechain)
        );
        prop_assert_eq!(
            analysis.combined_usage.model_calls_with_usage,
            2 + u64::from(include_sidechain)
        );
        prop_assert!(analysis.warnings.is_empty());

        let parsed_stop = stop_reason.map(StopReason::parse);
        if is_api_error {
            let TurnStatus::Terminal(final_turn) = analysis.status else {
                prop_assert!(false, "an API-error fragment must be terminal");
                unreachable!();
            };
            prop_assert_eq!(final_turn.outcome, TerminalOutcome::ApiError);
            prop_assert_eq!(final_turn.stop_reason, parsed_stop);
            prop_assert_eq!(final_turn.final_text_blocks, final_fragments.clone());
            prop_assert_eq!(final_turn.final_text, final_fragments.concat());
        } else if let Some(expected_outcome) = match stop_case {
            0 | 2 => Some(TerminalOutcome::Completed),
            1 => Some(TerminalOutcome::MaxTokens),
            3 => Some(TerminalOutcome::Refused),
            _ => None,
        } {
            let TurnStatus::Terminal(final_turn) = analysis.status else {
                prop_assert!(false, "a terminal generated stop was not terminal");
                unreachable!();
            };
            prop_assert_eq!(final_turn.outcome, expected_outcome);
            prop_assert_eq!(final_turn.stop_reason, parsed_stop);
            prop_assert_eq!(final_turn.final_text_blocks, final_fragments.clone());
            prop_assert_eq!(final_turn.final_text, final_fragments.concat());
        } else {
            let TurnStatus::Running { latest_stop_reason } = analysis.status else {
                prop_assert!(false, "a nonterminal generated stop completed the turn");
                unreachable!();
            };
            prop_assert_eq!(latest_stop_reason, parsed_stop);
        }
    }

    #[test]
    fn generated_parallel_sidechain_interleaving_preserves_deduplicated_usage(
        sidechain_usages in prop::collection::vec(usage_strategy(), 1..7),
        fragments_per_message in 2_usize..6,
    ) {
        let mut rows = vec![prompt_row("parallel")];
        let mut parents = vec!["prompt".to_owned(); sidechain_usages.len()];
        for fragment_index in 0..fragments_per_message {
            for (message_index, usage) in sidechain_usages.iter().enumerate() {
                let uuid = format!("side-{message_index}-{fragment_index}");
                rows.push(assistant_row(
                    &parents[message_index],
                    &uuid,
                    Some(&format!("side-message-{message_index}")),
                    None,
                    "side-model",
                    vec![json!({"type": "text", "text": format!("{message_index}:{fragment_index}")})],
                    (fragment_index + 1 == fragments_per_message).then_some("end_turn"),
                    Some(usage),
                    true,
                    false,
                ));
                parents[message_index] = uuid;
            }
        }
        rows.push(assistant_row(
            "prompt",
            "main-answer",
            Some("main-message"),
            None,
            "main-model",
            vec![json!({"type": "text", "text": "main"})],
            Some("end_turn"),
            None,
            false,
            false,
        ));

        let analysis = analyze_values("parallel", &rows)?;
        let TurnStatus::Terminal(final_turn) = analysis.status else {
            prop_assert!(false, "parallel sidechain traffic prevented a main terminal");
            unreachable!();
        };
        prop_assert_eq!(final_turn.final_text, "main");
        prop_assert_eq!(analysis.messages.len(), 1);
        prop_assert_eq!(
            analysis.sidechain_usage.model_calls_with_usage,
            sidechain_usages.len() as u64
        );
        let expected = sidechain_usages
            .iter()
            .fold(TokenUsage::default(), |total, usage| add_tokens(&total, &usage.tokens()));
        prop_assert_eq!(analysis.sidechain_usage.tokens, expected.clone());
        prop_assert_eq!(analysis.combined_usage.tokens, expected);
    }

    #[test]
    fn generated_semantic_conflicts_never_produce_an_authoritative_terminal(
        prompt in string_strategy(96),
        generated_text in string_strategy(64),
        generated_usage in usage_strategy(),
        mutation in 0_u8..11,
    ) {
        let rows = semantic_mutation_rows(&prompt, mutation, &generated_text, &generated_usage);
        if let Ok(analysis) = analyze_values(&prompt, &rows) {
            prop_assert!(
                !matches!(analysis.status, TurnStatus::Terminal(_)),
                "semantic mutation {mutation} produced an authoritative terminal result"
            );
        }
    }

    #[test]
    fn generated_graph_mutations_at_arbitrary_depth_fail_closed(
        prompt in string_strategy(96),
        generated_text in string_strategy(64),
        depth in 1_usize..16,
        mutation in 0_u8..8,
    ) {
        let rows = graph_mutation_rows(&prompt, depth, mutation, &generated_text);
        if let Ok(analysis) = analyze_values(&prompt, &rows) {
            prop_assert!(
                !matches!(analysis.status, TurnStatus::Terminal(_)),
                "graph mutation {mutation} at depth {depth} produced an authoritative terminal"
            );
        }
    }

    #[test]
    fn malformed_semantic_mutations_never_complete_a_strict_turn(mutation in 0_u8..27) {
        let parser = JsonlParser::new(ParseMode::Strict);
        let (prompt, answer) = mutated_turn(mutation);
        if let Ok(prompt) = parser.parse(&line(&prompt, 1)) {
            let mut engine = TranscriptEngine::new(ParseMode::Strict);
            engine.arm_turn("go")?;
            if engine.ingest(prompt).is_ok()
                && let Ok(answer) = parser.parse(&line(&answer, 2))
                && engine.ingest(answer).is_ok()
                && let Ok(analysis) = engine.analyze()
            {
                prop_assert!(
                    !matches!(analysis.status, TurnStatus::Terminal(_)),
                    "mutation {mutation} produced an authoritative terminal result"
                );
            }
        }
    }

    #[test]
    fn valid_unicode_turns_round_trip_through_parser_and_engine(
        prompt in string_strategy(128),
        answer in string_strategy(256),
        input_tokens in 0_u64..1_000_000,
        output_tokens in 0_u64..1_000_000,
    ) {
        let prompt_row = json!({
            "parentUuid": null,
            "sessionId": "session",
            "type": "user",
            "uuid": "prompt",
            "promptSource": "typed",
            "message": {"role": "user", "content": prompt},
        });
        let answer_row = json!({
            "parentUuid": "prompt",
            "sessionId": "session",
            "type": "assistant",
            "uuid": "answer",
            "message": {
                "id": "message",
                "model": "model",
                "content": [{"type": "text", "text": answer}],
                "stop_reason": "end_turn",
                "usage": {
                    "input_tokens": input_tokens,
                    "output_tokens": output_tokens,
                },
            },
        });
        let parser = JsonlParser::new(ParseMode::Strict);
        let mut engine = TranscriptEngine::new(ParseMode::Strict);
        engine.arm_turn(prompt.clone())?;
        engine.ingest(parser.parse(&line(&prompt_row, 1))?)?;
        engine.ingest(parser.parse(&line(&answer_row, 2))?)?;
        let analysis = engine.analyze()?;

        let TurnStatus::Terminal(final_turn) = analysis.status else {
            prop_assert!(false, "valid end_turn did not complete");
            unreachable!();
        };
        prop_assert_eq!(final_turn.outcome, TerminalOutcome::Completed);
        prop_assert_eq!(final_turn.final_text, answer);
        prop_assert_eq!(analysis.usage.tokens.input_tokens, input_tokens);
        prop_assert_eq!(analysis.usage.tokens.output_tokens, output_tokens);
    }
}
