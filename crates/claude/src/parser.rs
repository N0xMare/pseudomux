use std::str;

use pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER;
use serde_json::{Map, Value};

use crate::{
    AssistantFragment, CommonFields, CompleteLine, ContentBlock, ParseMode, ParsedRow, RowKind,
    RowScope, SystemRow, TokenUsage, ToolResultBlock, TranscriptError, UsageSnapshot,
};

/// Strict decoder for complete Claude Code JSONL records.
#[derive(Clone, Copy, Debug)]
pub struct JsonlParser {
    mode: ParseMode,
}

impl JsonlParser {
    #[must_use]
    pub fn new(mode: ParseMode) -> Self {
        Self { mode }
    }

    pub fn parse(&self, line: &CompleteLine) -> Result<ParsedRow, TranscriptError> {
        str::from_utf8(&line.bytes).map_err(|error| TranscriptError::InvalidUtf8 {
            location: line.location,
            message: error.to_string(),
        })?;

        let raw: Value = serde_json::from_slice(&line.bytes).map_err(|error| {
            TranscriptError::MalformedJson {
                location: line.location,
                message: error.to_string(),
            }
        })?;
        let object = raw
            .as_object()
            .ok_or_else(|| TranscriptError::SchemaDrift {
                row_uuid: None,
                path: "$".to_owned(),
                message: "top-level JSONL value must be an object".to_owned(),
            })?;

        let row_uuid = optional_string(object, "uuid", "$", None)?;
        let parent_uuid = optional_nullable_string(object, "parentUuid", "$", row_uuid.as_deref())?;
        let session_id = optional_string(object, "sessionId", "$", row_uuid.as_deref())?;
        let scope = parse_scope(object, row_uuid.as_deref())?;
        let declared_type = optional_string(object, "type", "$", row_uuid.as_deref())?;

        let common = CommonFields {
            uuid: row_uuid.clone(),
            parent_uuid,
            session_id,
            scope,
        };

        let kind = match declared_type.as_deref() {
            Some("user") => self.parse_user(object, row_uuid.as_deref())?,
            Some("assistant") => self.parse_assistant(object, row_uuid.as_deref())?,
            Some("attachment") => self.parse_attachment(object, row_uuid.as_deref())?,
            Some("system") => self.parse_system(object, row_uuid.as_deref())?,
            Some(record_type) if is_metadata_record(record_type) => RowKind::Metadata {
                record_type: record_type.to_owned(),
            },
            _ => RowKind::Unknown { declared_type },
        };

        if matches!(
            kind,
            RowKind::Assistant(_)
                | RowKind::TypedUser { .. }
                | RowKind::UserToolResults { .. }
                | RowKind::UserOther
                | RowKind::Attachment { .. }
        ) && row_uuid.is_none()
            && self.mode == ParseMode::Strict
        {
            return Err(schema(
                None,
                "$.uuid",
                "semantic row requires a string UUID",
            ));
        }

        Ok(ParsedRow {
            source: line.location,
            common,
            kind,
            raw,
        })
    }

    fn parse_attachment(
        &self,
        object: &Map<String, Value>,
        row_uuid: Option<&str>,
    ) -> Result<RowKind, TranscriptError> {
        let Some(attachment) = object.get("attachment").and_then(Value::as_object) else {
            return if self.mode == ParseMode::Strict {
                Err(schema(
                    row_uuid,
                    "$.attachment",
                    "attachment row payload must be an object",
                ))
            } else {
                Ok(RowKind::Unknown {
                    declared_type: Some("attachment".to_owned()),
                })
            };
        };
        let Some(attachment_type) = optional_string(attachment, "type", "$.attachment", row_uuid)?
        else {
            return if self.mode == ParseMode::Strict {
                Err(schema(
                    row_uuid,
                    "$.attachment.type",
                    "attachment type must be a string",
                ))
            } else {
                Ok(RowKind::Unknown {
                    declared_type: Some("attachment".to_owned()),
                })
            };
        };
        if !is_supported_attachment_type(&attachment_type) && self.mode == ParseMode::Strict {
            return Err(schema(
                row_uuid,
                "$.attachment.type",
                "unsupported attachment type",
            ));
        }
        Ok(RowKind::Attachment { attachment_type })
    }

    fn parse_user(
        &self,
        object: &Map<String, Value>,
        row_uuid: Option<&str>,
    ) -> Result<RowKind, TranscriptError> {
        let message = match object.get("message").and_then(Value::as_object) {
            Some(message) => message,
            None if self.mode == ParseMode::Strict => {
                return Err(schema(
                    row_uuid,
                    "$.message",
                    "user message must be an object",
                ));
            }
            None => return Ok(RowKind::UserOther),
        };
        let role = optional_string(message, "role", "$.message", row_uuid)?;
        if self.mode == ParseMode::Strict
            && role
                .as_deref()
                .is_some_and(|message_role| message_role != "user")
        {
            return Err(schema(
                row_uuid,
                "$.message.role",
                "user message role must be \"user\"",
            ));
        }
        let prompt_source = optional_string(object, "promptSource", "$", row_uuid)?;

        if prompt_source.as_deref() == Some("typed") {
            let prompt = message
                .get("content")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            return match prompt {
                Some(prompt) => Ok(RowKind::TypedUser {
                    prompt,
                    prompt_id: optional_string(object, "promptId", "$", row_uuid)?,
                }),
                None if self.mode == ParseMode::Strict => Err(schema(
                    row_uuid,
                    "$.message.content",
                    "typed prompt content must be a string",
                )),
                None => Ok(RowKind::UserOther),
            };
        }

        let Some(content) = message.get("content").and_then(Value::as_array) else {
            return Ok(RowKind::UserOther);
        };
        let mut results = Vec::new();
        let mut first_unsupported_block = None;
        for (index, block) in content.iter().enumerate() {
            let Some(block_object) = block.as_object() else {
                first_unsupported_block.get_or_insert(index);
                continue;
            };
            if block_object.get("type").and_then(Value::as_str) != Some("tool_result") {
                first_unsupported_block.get_or_insert(index);
                continue;
            }
            let Some(tool_use_id) = block_object.get("tool_use_id").and_then(Value::as_str) else {
                if self.mode == ParseMode::Strict {
                    return Err(schema(
                        row_uuid,
                        &format!("$.message.content[{index}].tool_use_id"),
                        "tool result requires a string tool_use_id",
                    ));
                }
                continue;
            };
            let is_error = optional_bool(
                block_object,
                "is_error",
                &format!("$.message.content[{index}]"),
                row_uuid,
            )?;
            results.push(ToolResultBlock {
                tool_use_id: tool_use_id.to_owned(),
                content: block_object.get("content").cloned().unwrap_or(Value::Null),
                is_error,
            });
        }

        // A pure non-tool-result array remains UserOther so strict analysis can
        // reject it only when it is causal. This preserves historical/meta rows.
        // Once a row carries a real tool result, however, every sibling block is
        // semantically part of that active result row and must be understood.
        if self.mode == ParseMode::Strict
            && !results.is_empty()
            && let Some(index) = first_unsupported_block
        {
            return Err(schema(
                row_uuid,
                &format!("$.message.content[{index}]"),
                "tool result row contains an unknown or malformed sibling block",
            ));
        }

        if results.is_empty() {
            Ok(RowKind::UserOther)
        } else {
            Ok(RowKind::UserToolResults { results })
        }
    }

    fn parse_system(
        &self,
        object: &Map<String, Value>,
        row_uuid: Option<&str>,
    ) -> Result<RowKind, TranscriptError> {
        let subtype = optional_string(object, "subtype", "$", row_uuid)?;
        if self.mode == ParseMode::Strict {
            // Admission to the active parent chain is earned here and nowhere
            // else: a subtype is admitted only after its own payload proves
            // what the engine will be entitled to assume about it. The inert
            // markers must prove they carry no semantics and that the turn is
            // over; `api_error` must prove it is a retry record, which is the
            // proof that the turn is *not* over.
            match subtype.as_deref() {
                Some("turn_duration") => Self::prove_turn_duration_inert(object, row_uuid)?,
                Some("stop_hook_summary") => {
                    Self::prove_stop_hook_summary_inert(object, row_uuid)?;
                }
                Some("api_error") => Self::prove_api_error_is_a_retry_record(object, row_uuid)?,
                _ => {}
            }
        }
        Ok(RowKind::System(SystemRow { subtype }))
    }

    /// A `turn_duration` row is admitted only when its own payload proves the
    /// turn is over.
    ///
    /// That proof now has to be load-bearing in a way it was not before: the
    /// drain is graduated on this marker, so a row admitted here shortens the
    /// window in which a late row could still be caught. A marker that announces
    /// pending work while claiming the turn ended is exactly the row that must
    /// not buy that shortening.
    fn prove_turn_duration_inert(
        object: &Map<String, Value>,
        row_uuid: Option<&str>,
    ) -> Result<(), TranscriptError> {
        // `pendingWorkflowCount` is a continuation signal, not a statistic: it
        // was written by 2.1.177 and is absent on 2.1.207+. A non-zero count is
        // the marker itself saying more work is queued, which contradicts the
        // only thing the subtype is admitted for. Absent or zero is proof;
        // present-and-nonzero -- and anything that cannot be read as a count,
        // because an unprovable guarantee is drift rather than a default -- is
        // not.
        match object.get("pendingWorkflowCount") {
            None => {}
            Some(value) if value.as_u64() == Some(0) => {}
            Some(_) => {
                return Err(schema(
                    row_uuid,
                    "$.pendingWorkflowCount",
                    "turn_duration announces pending workflow work, so the turn is not proven over",
                ));
            }
        }

        if object
            .get("durationMs")
            .is_some_and(|value| value.as_u64().is_none())
        {
            return Err(schema(
                row_uuid,
                "$.durationMs",
                "turn_duration durationMs must be a non-negative integer",
            ));
        }

        if object
            .get("messageCount")
            .is_some_and(|value| value.as_u64().is_none())
        {
            return Err(schema(
                row_uuid,
                "$.messageCount",
                "turn_duration messageCount must be a non-negative integer",
            ));
        }

        // Observed turn_duration rows are structural timing markers. Do not
        // let a future semantic payload hide behind the allowlisted subtype.
        reject_semantic_payload(object, row_uuid, "turn_duration")
    }

    /// A `stop_hook_summary` row is admitted only when its own payload proves
    /// that no further model output can follow it. A Stop hook may *block* the
    /// stop (`decision: "block"`), after which Claude continues the turn; the
    /// window before the continuation's first row lands includes a fresh model
    /// call's first-token latency and so can outlast the drain. During that
    /// window the chain leaf is this system row, the latest logical message
    /// still says `end_turn`, and the screen shows a ready prompt -- which is
    /// exactly the shape that would let pmux commit a truncated turn. So the
    /// row must carry its own proof, and an unproven row stays SchemaDrift.
    fn prove_stop_hook_summary_inert(
        object: &Map<String, Value>,
        row_uuid: Option<&str>,
    ) -> Result<(), TranscriptError> {
        // Absence is not falsity. A Claude that renames or drops this field has
        // withdrawn the guarantee, and a guarantee that cannot be proven is
        // drift rather than a default.
        match object.get("preventedContinuation") {
            Some(Value::Bool(false)) => {}
            Some(Value::Bool(true)) => {
                return Err(schema(
                    row_uuid,
                    "$.preventedContinuation",
                    "stop_hook_summary blocked the stop, so the turn may still continue",
                ));
            }
            _ => {
                return Err(schema(
                    row_uuid,
                    "$.preventedContinuation",
                    "stop_hook_summary requires preventedContinuation:false to prove inertness",
                ));
            }
        }

        // Both arrays are hook feedback that Claude can act on, so a non-empty
        // one is evidence that the turn is not over. Only an absent key or an
        // empty array proves nothing was fed back.
        for key in ["hookErrors", "hookAdditionalContext"] {
            match object.get(key) {
                None => {}
                Some(Value::Array(entries)) if entries.is_empty() => {}
                _ => {
                    return Err(schema(
                        row_uuid,
                        &format!("$.{key}"),
                        "stop_hook_summary hook feedback must be absent or an empty array",
                    ));
                }
            }
        }

        reject_semantic_payload(object, row_uuid, "stop_hook_summary")
    }

    /// An `api_error` row is admitted for the opposite reason the inert markers
    /// are: it proves the turn is still running. Claude writes one row per
    /// transport retry -- across every transcript on this machine the 115
    /// observed rows are ordinary connection resets, request timeouts,
    /// connection refusals, and two laptop sleeps -- and then usually succeeds,
    /// so failing the turn on the row is a self-inflicted failure on a dropped
    /// wifi connection.
    ///
    /// What must be proven is that the row really is a retry record. Both
    /// counters are required as integers: a row that has lost them has lost the
    /// only evidence distinguishing "a retry is in flight" from some future
    /// terminal transport failure wearing the same subtype, and a guarantee that
    /// cannot be proven is drift rather than a permissive default.
    ///
    /// Exhaustion (`retryAttempt >= maxRetries`) is deliberately not treated
    /// specially. It was NEVER observed -- every observed ladder stops well
    /// short of the maximum, and `maxRetries` was 10 in all 115 rows -- so an
    /// exhausted row is admitted exactly like any other and stays non-terminal,
    /// leaving the drain and the remaining completion factors to decide. That is
    /// the safe direction: pmux would refuse to return rather than return early.
    fn prove_api_error_is_a_retry_record(
        object: &Map<String, Value>,
        row_uuid: Option<&str>,
    ) -> Result<(), TranscriptError> {
        for key in ["retryAttempt", "maxRetries"] {
            match object.get(key) {
                Some(value) if value.as_u64().is_some() => {}
                _ => {
                    return Err(schema(
                        row_uuid,
                        &format!("$.{key}"),
                        "api_error requires retryAttempt and maxRetries as non-negative integers to prove it is a retry record",
                    ));
                }
            }
        }

        reject_semantic_payload(object, row_uuid, "api_error")
    }

    fn parse_assistant(
        &self,
        object: &Map<String, Value>,
        row_uuid: Option<&str>,
    ) -> Result<RowKind, TranscriptError> {
        let message = match object.get("message").and_then(Value::as_object) {
            Some(message) => message,
            None if self.mode == ParseMode::Strict => {
                return Err(schema(
                    row_uuid,
                    "$.message",
                    "assistant message must be an object",
                ));
            }
            None => {
                return Ok(RowKind::Unknown {
                    declared_type: Some("assistant".to_owned()),
                });
            }
        };
        let blocks = match message.get("content").and_then(Value::as_array) {
            Some(content) => content
                .iter()
                .enumerate()
                .map(|(index, block)| parse_content_block(block, index))
                .collect(),
            None if self.mode == ParseMode::Strict => {
                return Err(schema(
                    row_uuid,
                    "$.message.content",
                    "assistant content must be an array",
                ));
            }
            None => Vec::new(),
        };

        let top_level_api_error =
            optional_bool(object, "isApiErrorMessage", "$", row_uuid)?.unwrap_or(false);
        let message_api_error =
            optional_bool(message, "isApiErrorMessage", "$.message", row_uuid)?.unwrap_or(false);

        let usage = match message.get("usage") {
            None | Some(Value::Null) => None,
            Some(value) => Some(parse_usage(value, self.mode, row_uuid)?),
        };

        Ok(RowKind::Assistant(AssistantFragment {
            message_id: optional_string(message, "id", "$.message", row_uuid)?,
            request_id: optional_string(object, "requestId", "$", row_uuid)?,
            model: optional_string(message, "model", "$.message", row_uuid)?,
            blocks,
            stop_reason: optional_nullable_string(message, "stop_reason", "$.message", row_uuid)?,
            usage,
            is_api_error: top_level_api_error || message_api_error,
        }))
    }
}

fn parse_scope(
    object: &Map<String, Value>,
    row_uuid: Option<&str>,
) -> Result<RowScope, TranscriptError> {
    if optional_bool(object, "isMeta", "$", row_uuid)?.unwrap_or(false) {
        return Ok(RowScope::Meta);
    }
    const TEAM_FIELDS: [&str; 6] = [
        "teamName",
        "teamId",
        "teammateName",
        "teammateId",
        "agentName",
        "agentId",
    ];
    if TEAM_FIELDS
        .iter()
        .any(|field| object.get(*field).is_some_and(|value| !value.is_null()))
    {
        return Ok(RowScope::Team);
    }
    if optional_bool(object, "isSidechain", "$", row_uuid)?.unwrap_or(false) {
        return Ok(RowScope::Sidechain);
    }
    Ok(RowScope::Main)
}

fn parse_content_block(value: &Value, _index: usize) -> ContentBlock {
    let Some(object) = value.as_object() else {
        return ContentBlock::Unknown {
            declared_type: None,
            raw: value.clone(),
        };
    };
    let declared_type = object.get("type").and_then(Value::as_str);
    match declared_type {
        Some("text") => object
            .get("text")
            .and_then(Value::as_str)
            .map(|text| ContentBlock::Text {
                text: text.to_owned(),
            })
            .unwrap_or_else(|| ContentBlock::Unknown {
                declared_type: Some("text".to_owned()),
                raw: value.clone(),
            }),
        Some("thinking") => object
            .get("thinking")
            .and_then(Value::as_str)
            .map(|thinking| ContentBlock::Thinking {
                thinking: thinking.to_owned(),
            })
            .unwrap_or_else(|| ContentBlock::Unknown {
                declared_type: Some("thinking".to_owned()),
                raw: value.clone(),
            }),
        Some("tool_use") => match (
            object.get("id").and_then(Value::as_str),
            object.get("name").and_then(Value::as_str),
        ) {
            (Some(id), Some(name)) => ContentBlock::ToolUse {
                id: id.to_owned(),
                name: name.to_owned(),
                input: object.get("input").cloned().unwrap_or(Value::Null),
            },
            _ => ContentBlock::Unknown {
                declared_type: Some("tool_use".to_owned()),
                raw: value.clone(),
            },
        },
        other => ContentBlock::Unknown {
            declared_type: other.map(ToOwned::to_owned),
            raw: value.clone(),
        },
    }
}

fn parse_usage(
    value: &Value,
    mode: ParseMode,
    row_uuid: Option<&str>,
) -> Result<UsageSnapshot, TranscriptError> {
    let Some(object) = value.as_object() else {
        return Err(schema(
            row_uuid,
            "$.message.usage",
            "usage must be an object",
        ));
    };
    let token = |name: &'static str| -> Result<u64, TranscriptError> {
        let value = match object.get(name) {
            None | Some(Value::Null) => Ok(0),
            Some(value) => value.as_u64().ok_or_else(|| {
                schema(
                    row_uuid,
                    &format!("$.message.usage.{name}"),
                    "token count must be a non-negative integer",
                )
            }),
        }?;
        if value > MAX_SAFE_JSON_INTEGER {
            return Err(schema(
                row_uuid,
                &format!("$.message.usage.{name}"),
                "token count exceeds protocol-v1's safe-integer maximum",
            ));
        }
        Ok(value)
    };

    let tokens = TokenUsage {
        input_tokens: token("input_tokens")?,
        output_tokens: token("output_tokens")?,
        cache_creation_input_tokens: token("cache_creation_input_tokens")?,
        cache_read_input_tokens: token("cache_read_input_tokens")?,
    };
    if mode == ParseMode::Strict
        && !object.keys().any(|key| {
            matches!(
                key.as_str(),
                "input_tokens"
                    | "output_tokens"
                    | "cache_creation_input_tokens"
                    | "cache_read_input_tokens"
            )
        })
    {
        return Err(schema(
            row_uuid,
            "$.message.usage",
            "usage has no recognized token fields",
        ));
    }
    Ok(UsageSnapshot {
        tokens,
        raw: value.clone(),
    })
}

fn optional_string(
    object: &Map<String, Value>,
    key: &str,
    parent: &str,
    row_uuid: Option<&str>,
) -> Result<Option<String>, TranscriptError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(schema(
            row_uuid,
            &format!("{parent}.{key}"),
            "field must be a string",
        )),
    }
}

fn optional_nullable_string(
    object: &Map<String, Value>,
    key: &str,
    parent: &str,
    row_uuid: Option<&str>,
) -> Result<Option<String>, TranscriptError> {
    optional_string(object, key, parent, row_uuid)
}

fn optional_bool(
    object: &Map<String, Value>,
    key: &str,
    parent: &str,
    row_uuid: Option<&str>,
) -> Result<Option<bool>, TranscriptError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(schema(
            row_uuid,
            &format!("{parent}.{key}"),
            "field must be a boolean",
        )),
    }
}

fn is_metadata_record(record_type: &str) -> bool {
    matches!(
        record_type,
        "ai-title"
            | "mode"
            | "permission-mode"
            | "progress"
            | "file-history-snapshot"
            | "queue-operation"
            | "summary"
            | "last-prompt"
            | "pr-link"
            | "bridge-session"
    )
}

fn is_supported_attachment_type(attachment_type: &str) -> bool {
    matches!(
        attachment_type,
        "agent_listing_delta"
            | "command_permissions"
            | "compact_file_reference"
            | "date_change"
            | "deferred_tools_delta"
            | "edited_text_file"
            | "file"
            | "invoked_skills"
            | "plan_mode"
            | "queued_command"
            | "skill_listing"
            | "task_reminder"
            // MEASURED on Claude Code 2.1.232 linux/x86_64, SessionCell::Minified:
            // attachment.type was this string (mint and again after /clear).
            // This match admits the type name only; it does not read attachment.text.
            | "total_tokens_reminder"
            | "ultra_effort_enter"
            | "workflow_keyword_request"
    )
}

/// Shared by every allowlisted system subtype: no semantic payload may hide
/// behind a subtype that strict analysis has agreed to treat as inert.
fn reject_semantic_payload(
    object: &Map<String, Value>,
    row_uuid: Option<&str>,
    subtype: &str,
) -> Result<(), TranscriptError> {
    for key in ["message", "content", "attachment"] {
        if object.contains_key(key) {
            return Err(schema(
                row_uuid,
                &format!("$.{key}"),
                &format!("{subtype} must not contain a semantic payload"),
            ));
        }
    }
    Ok(())
}

fn schema(row_uuid: Option<&str>, path: &str, message: &str) -> TranscriptError {
    TranscriptError::SchemaDrift {
        row_uuid: row_uuid.map(ToOwned::to_owned),
        path: path.to_owned(),
        message: message.to_owned(),
    }
}
