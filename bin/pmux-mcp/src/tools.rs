//! Thin MCP-to-native-protocol request mapping.
//!
//! This module deliberately contains no prompt loop, completion logic, daemon
//! discovery, process launch, or terminal interpretation. The only published
//! tool is `run_stateless`; every other `tools/call` name is `unknown_tool`.

use std::fmt;

use pseudomux_client::{ClientError, PmuxClient};
use pseudomux_protocol::v1::{ErrorCode, RECOMMENDATION_KEY, Request, ResponseResult};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PublicFailure {
    InvalidArguments,
    /// Arguments pmux ITSELF refused, carrying the sentence pmux composed.
    ///
    /// **THE ONE ARGUMENT FAILURE THAT SAYS ANYTHING**, and it is bounded by
    /// construction rather than by care: the string comes from
    /// `caller_actionable_decode_refusal`, which returns only the span between
    /// `DECODE_REFUSAL_MARKER` and serde's own position suffix -- text the
    /// protocol crate composed out of field paths and its own argument, never a
    /// value the caller sent. Everything else stays the content-free
    /// [`Self::InvalidArguments`], because a decode failure's rendered text is
    /// not safe to return in general: MEASURED,
    /// `{"effort":42}` renders as ``invalid type: integer `42`, expected a
    /// string``, and a `run_stateless` frame carries the caller's prompt.
    InvalidConfig {
        reason: String,
    },
    DaemonRejected {
        code: ErrorCode,
        retryable: bool,
        /// The daemon's own `details.recommendation`, verbatim, or `None` when
        /// this refusal carries no advice.
        ///
        /// **THE ONE KEY A REFUSAL WRITES FOR A PERSON TO READ**, and the same
        /// one `bin/pmux` renders — read through
        /// [`pseudomux_protocol::v1::RECOMMENDATION_KEY`] so neither surface
        /// can go looking for a field the daemon stopped writing. Everything
        /// else in `details` stays here: it is a general diagnostic channel
        /// that also carries attach capability tokens and backend matcher text,
        /// and a surface that renders all of it renders those.
        ///
        /// `message` stays redacted, and that is a decision rather than an
        /// omission. A daemon message is not always pmux's own composition:
        /// MEASURED, `{"effort":42}` comes back as ``invalid type: integer
        /// `42`, expected a string``, so forwarding every message would
        /// forward caller values from the prompt. `recommendation` is written
        /// by `ErrorBody::advising` and by nothing else, always out of pmux's
        /// own vocabulary.
        recommendation: Option<String>,
    },
    TransportUnavailable,
    TimedOut,
    InvalidDaemonResponse,
}

/// A deliberately redacted error suitable for returning to an MCP caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ToolCallError {
    UnknownTool,
    Failure(PublicFailure),
}

impl ToolCallError {
    pub(crate) fn is_unknown_tool(&self) -> bool {
        matches!(self, Self::UnknownTool)
    }

    pub(crate) fn result(&self) -> Value {
        let (message, structured) = match self {
            Self::UnknownTool => (
                "unknown pmux tool".to_owned(),
                json!({"error": {"kind": "unknown_tool"}}),
            ),
            Self::Failure(PublicFailure::InvalidArguments) => (
                "arguments do not match the native pmux protocol schema".to_owned(),
                json!({"error": {"kind": "invalid_arguments"}}),
            ),
            Self::Failure(PublicFailure::InvalidConfig { reason }) => {
                (reason.clone(), json!({"error": {"kind": "invalid_config"}}))
            }
            Self::Failure(PublicFailure::DaemonRejected {
                code,
                retryable,
                recommendation,
            }) => (
                // The advice is the WHOLE of what a model can act on here, so
                // it goes in the text channel and not only in the structured
                // one: a tool result whose `content` reads "pmuxd rejected the
                // native request" for every refusal there is teaches a caller
                // to stop reading it.
                match recommendation {
                    Some(recommendation) => {
                        format!("pmuxd rejected the native request: {recommendation}")
                    }
                    None => "pmuxd rejected the native request".to_owned(),
                },
                {
                    let mut error = json!({
                        "kind": "daemon_rejected",
                        "code": code,
                        "retryable": retryable,
                    });
                    // Present when there is advice and ABSENT when there is
                    // none, rather than always present and sometimes `null`: a
                    // caller branching on this field should not have to tell
                    // the two apart.
                    if let Some(recommendation) = recommendation {
                        error[RECOMMENDATION_KEY] = json!(recommendation);
                    }
                    json!({ "error": error })
                },
            ),
            Self::Failure(PublicFailure::TransportUnavailable) => (
                "the explicit pmuxd socket is unavailable".to_owned(),
                json!({"error": {"kind": "transport_unavailable"}}),
            ),
            Self::Failure(PublicFailure::TimedOut) => (
                "the native pmux request timed out".to_owned(),
                json!({"error": {"kind": "timeout", "retryable": true}}),
            ),
            Self::Failure(PublicFailure::InvalidDaemonResponse) => (
                "pmuxd returned an invalid protocol response".to_owned(),
                json!({"error": {"kind": "invalid_daemon_response"}}),
            ),
        };
        json!({
            "content": [{"type": "text", "text": message}],
            "structuredContent": structured,
            "isError": true,
        })
    }
}

impl fmt::Display for ToolCallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnknownTool => "unknown pmux tool",
            Self::Failure(PublicFailure::InvalidArguments) => "invalid tool arguments",
            // The composed sentence goes to `result()`, which is the channel a
            // model reads; `Display` is this binary's own log line.
            Self::Failure(PublicFailure::InvalidConfig { .. }) => "invalid tool configuration",
            Self::Failure(PublicFailure::DaemonRejected { .. }) => "pmuxd rejected the request",
            Self::Failure(PublicFailure::TransportUnavailable) => "pmuxd transport unavailable",
            Self::Failure(PublicFailure::TimedOut) => "pmuxd request timed out",
            Self::Failure(PublicFailure::InvalidDaemonResponse) => {
                "invalid pmuxd protocol response"
            }
        })
    }
}

impl std::error::Error for ToolCallError {}

/// What `tools/list` returns: the provider surface only.
///
/// [`map_tool_call`] admits the same single name. Session and agent tools are
/// not in this catalogue and are not dispatchable.
pub fn published_tool_definitions() -> Vec<Value> {
    vec![tool(
        "run_stateless",
        concat!(
            "Ask the daemon's stateless token engine one question and get the text and token ",
            "usage back. The whole contract is (model, effort, prompt) -> text + usage: model ",
            "is required, effort is optional and is validated against the resolved model by ",
            "the daemon, and prompt is the question. ",
            "THE CALLER NAMES NO RESOURCE: there is no cwd, no configuration root, no system ",
            "prompt and no session id in this tool's schema, because the daemon mints every ",
            "one of them from its own configuration. ",
            "Refused with unsupported_feature when the daemon was started without a pool ",
            "(--pool-parent)."
        ),
        run_stateless_schema(),
        // Not read-only (it spends tokens), not destructive (it creates and
        // destroys nothing a caller can name), not idempotent (two calls
        // are two answers and two bills), open-world (it reaches Claude).
        annotations(false, false, false, true),
    )]
}

fn tool(name: &str, description: &str, input_schema: Value, annotations: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
        "annotations": annotations,
    })
}

fn annotations(read_only: bool, destructive: bool, idempotent: bool, open_world: bool) -> Value {
    json!({
        "readOnlyHint": read_only,
        "destructiveHint": destructive,
        "idempotentHint": idempotent,
        "openWorldHint": open_world,
    })
}

/// Execute exactly one native request and unwrap only its corresponding result type.
pub async fn handle_tool(
    client: &PmuxClient,
    name: &str,
    arguments: &Value,
) -> Result<Value, ToolCallError> {
    let request = map_tool_call(name, arguments)?;
    let result = client.request(request).await.map_err(redact_client_error)?;
    let structured = extract_result(result)?;
    // `structuredContent` is the one canonical success representation. Keeping
    // `content` empty avoids duplicating a near-frame-limit native result into
    // both a JSON string and structured JSON in the enclosing MCP frame.
    Ok(successful_tool_result(structured))
}

fn successful_tool_result(structured: Value) -> Value {
    json!({
        "content": [],
        "structuredContent": structured,
        "isError": false,
    })
}

pub(crate) fn map_tool_call(name: &str, arguments: &Value) -> Result<Request, ToolCallError> {
    match name {
        "run_stateless" => Ok(Request::RunStateless(decode(arguments)?)),
        _ => Err(ToolCallError::UnknownTool),
    }
}

/// Decodes one tool's arguments, FORWARDING the refusal when pmux wrote it.
///
/// `caller_actionable_decode_refusal` is the same function
/// `bin/pmuxd/src/handler.rs` calls on the wire, so the sentence a model reads
/// through MCP and the sentence a socket caller reads are one sentence, written
/// once. A refusal serde wrote out of the caller's own values stays
/// [`PublicFailure::InvalidArguments`].
fn decode<T: DeserializeOwned>(arguments: &Value) -> Result<T, ToolCallError> {
    if !arguments.is_object() {
        return Err(ToolCallError::Failure(PublicFailure::InvalidArguments));
    }
    serde_json::from_value(arguments.clone()).map_err(|error| {
        match pseudomux_protocol::v1::caller_actionable_decode_refusal(&error) {
            Some(reason) => ToolCallError::Failure(PublicFailure::InvalidConfig { reason }),
            // pmux did not write this one, so it may contain the caller's own
            // values and stays content-free.
            None => ToolCallError::Failure(PublicFailure::InvalidArguments),
        }
    })
}

fn extract_result(result: ResponseResult) -> Result<Value, ToolCallError> {
    match result {
        ResponseResult::StatelessResult(value) => serde_json::to_value(value)
            .map_err(|_| ToolCallError::Failure(PublicFailure::InvalidDaemonResponse)),
        _ => Err(ToolCallError::Failure(PublicFailure::InvalidDaemonResponse)),
    }
}

fn redact_client_error(error: ClientError) -> ToolCallError {
    let failure = match error {
        ClientError::Server(error) => PublicFailure::DaemonRejected {
            code: error.code,
            retryable: error.retryable,
            recommendation: error.recommendation().map(ToOwned::to_owned),
        },
        ClientError::Io(_) => PublicFailure::TransportUnavailable,
        ClientError::Timeout { .. } => PublicFailure::TimedOut,
        ClientError::InvalidOptions(_) => PublicFailure::TransportUnavailable,
        ClientError::Json(_)
        | ClientError::FrameTooLarge { .. }
        | ClientError::UnsupportedProtocolVersion { .. }
        | ClientError::InvalidProtocolVersion
        | ClientError::MismatchedRequestId { .. }
        | ClientError::UnexpectedResult { .. }
        | ClientError::ResultSessionMismatch { .. }
        | ClientError::ResultGenerationMismatch { .. }
        | ClientError::ResultTurnMismatch { .. }
        | ClientError::EventSessionMismatch { .. }
        | ClientError::EventGenerationMismatch { .. }
        | ClientError::InvalidEventSequence { .. }
        | ClientError::EventCursorOverflow { .. }
        | ClientError::InvalidBatchCursor { .. }
        | ClientError::ReplayGapRequestMismatch { .. }
        | ClientError::InvalidReplayGap { .. } => PublicFailure::InvalidDaemonResponse,
    };
    ToolCallError::Failure(failure)
}

/// The whole provider surface, as a JSON schema.
///
/// `additionalProperties: false` is doing product work here, not hygiene. Every
/// field this schema omits -- `cwd`, `config_isolation`, `system_prompt`,
/// `session_id`, `claude`, `environment` -- is a resource the daemon mints, and
/// a schema that merely ignored them would let an agent believe it had set one.
/// The daemon's own DTO is `deny_unknown_fields` for the same reason; this is
/// the same refusal one hop earlier, where the agent can read it.
fn run_stateless_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "model": {
                "type": "string",
                "minLength": 1,
                "description": "Claude model alias or exact id, e.g. `opus`, `sonnet`, `claude-opus-5`. Required: it is half the pool's class key, so an absent model would partition the pool on whatever the daemon happens to default to."
            },
            "effort": {
                "enum": ["low", "medium", "high", "xhigh", "max"],
                "description": "Reasoning depth. Omit for the resolved model's own default. Validated against the RESOLVED model by the daemon, never against this list alone -- tiers are not uniform across Claude models."
            },
            "prompt": {
                "type": "string",
                "minLength": 1,
                "maxLength": 1048576,
                // The prefix list is RENDERED from the shipped set, not typed
                // out. This sentence used to name `/` alone, which was a true
                // statement about one of the two characters the composer takes
                // as a mode switch and a false description of the guard.
                "description": format!(
                    "The question. Refused with unsupported_feature if its first character is one \
                     of {}: those switch the Claude composer into a mode instead of being sent to \
                     the model, and a typed control API for them does not exist yet.",
                    pseudomux_client::prompt::COMPOSER_MODE_PREFIXES
                        .map(|prefix| format!("`{prefix}`"))
                        .join(" ")
                )
            },
            "deadline_unix_ms": {
                "type": "integer",
                "minimum": 0,
                "description": "Absolute wall-clock deadline for the answer. Omit for daemon policy. It may only SHORTEN the wait; nothing here lengthens one."
            }
        },
        "required": ["model", "prompt"]
    })
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;

    const SESSION: &str = "00000000-0000-0000-0000-000000000002";
    const TURN: &str = "00000000-0000-0000-0000-000000000003";

    const UNPUBLISHED_TOOLS: &[&str] = &[
        "start_session",
        "run_turn",
        "inspect_session",
        "cancel_turn",
        "close_session",
        "run_once",
        "subscribe_events",
        "attach_session",
        "create_agent",
        "get_agent",
        "list_agents",
        "update_agent",
        "clear_session",
        "diagnose",
        "legacy_screen_text",
    ];

    fn start_session_arguments() -> Value {
        json!({
            "identity": {"mode": "new", "session_id": SESSION},
            "cwd": "/work/project",
            "claude": {"executable": "/opt/claude/bin/claude"}
        })
    }

    #[test]
    fn tools_list_is_the_provider_surface_only() {
        let published = published_tool_definitions();
        let names = published
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(names, ["run_stateless"]);
        for definition in &published {
            assert_eq!(definition["inputSchema"]["type"], "object");
            assert_eq!(definition["inputSchema"]["additionalProperties"], false);
            let description = definition["description"].as_str().unwrap();
            assert!(
                !description
                    .to_ascii_lowercase()
                    .contains("interactive session"),
                "published tool descriptions must not teach starting an interactive session: \
                 {description:?}"
            );
        }
    }

    /// Every name other than `run_stateless` is `unknown_tool`, including the
    /// former session and agent catalogue, and including a well-formed
    /// `start_session` body. Mapping any of those would reach the native
    /// socket.
    #[test]
    fn unpublished_tool_names_are_unknown_tool() {
        for name in UNPUBLISHED_TOOLS {
            for arguments in [json!({}), start_session_arguments()] {
                let error = map_tool_call(name, &arguments).unwrap_err();
                assert!(
                    error.is_unknown_tool(),
                    "{name} must be unknown_tool, got {error:?}"
                );
                let rendered = error.result();
                assert_eq!(
                    rendered["structuredContent"]["error"]["kind"],
                    "unknown_tool"
                );
                assert!(!rendered.to_string().contains(name), "{rendered}");
            }
        }
    }

    /// `run_stateless_schema` is `additionalProperties: false`, so a field the
    /// Rust DTO gained and this schema did not would be UNREACHABLE from every
    /// MCP caller, silently. For the provider there is a second reading of the same
    /// comparison, and it runs the other way: a field this SCHEMA gained and
    /// the DTO did not would be a resource name an agent could write and the
    /// daemon would refuse -- and the whole product statement is that there is
    /// no such field.
    #[test]
    fn the_run_stateless_schema_and_the_rust_request_carry_the_same_fields() {
        let populated = pseudomux_protocol::v1::RunStatelessRequest {
            model: "claude-opus-5".into(),
            // Both optional fields are populated, or the inventory is short by
            // exactly the fields most likely to be forgotten: they are
            // `skip_serializing_if = "Option::is_none"`.
            effort: Some(pseudomux_protocol::v1::EffortLevel::XHigh),
            prompt: "what is two plus two".into(),
            deadline_unix_ms: Some(1_700_000_000_000),
        };
        let encoded = serde_json::to_value(&populated).unwrap();
        let rust_fields = encoded
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let schema = run_stateless_schema();
        let schema_fields = schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            rust_fields, schema_fields,
            "the MCP run_stateless schema drifted from RunStatelessRequest"
        );
        assert!(matches!(
            map_tool_call("run_stateless", &encoded).unwrap(),
            Request::RunStateless(_)
        ));
    }

    /// The protocol crate's source, so a variant added there is visible here.
    ///
    /// Deliberately the SOURCE and not a Rust list: a Rust list of variants is
    /// the hand-written inventory this whole family of defects is made of, and
    /// it is exactly what `permission_mode` already was. Same technique as
    /// `pool::refusal`'s census over `include_str!("refusal.rs")`. A moved or
    /// renamed protocol file is a compile error here, not a silent pass.
    const PROTOCOL_SOURCE: &str = include_str!("../../../crates/protocol/src/v1.rs");

    /// Every wire spelling of `pub enum {name}`, read out of the protocol's own
    /// source.
    ///
    /// Handles the two things this file's enums actually use: `rename_all =
    /// "snake_case"` on the enum, and `#[serde(rename = "...")]` on a variant
    /// (which is how `XHigh` spells itself `xhigh`). A field-carrying or
    /// internally tagged enum is not one of these and is not asked for.
    fn protocol_enum_spellings(name: &str) -> BTreeSet<String> {
        let header = format!("pub enum {name} {{");
        let (_, rest) = PROTOCOL_SOURCE
            .split_once(&header)
            .unwrap_or_else(|| panic!("crates/protocol/src/v1.rs declares no `{header}`"));
        let (body, _) = rest
            .split_once("\n}")
            .unwrap_or_else(|| panic!("`{header}` is not terminated"));

        let mut spellings = BTreeSet::new();
        let mut renamed: Option<String> = None;
        for line in body.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("#[serde(rename = \"") {
                renamed = rest.split('"').next().map(ToOwned::to_owned);
                continue;
            }
            if line.is_empty() || line.starts_with("//") || line.starts_with("#[") {
                continue;
            }
            let identifier: String = line
                .chars()
                .take_while(|character| character.is_alphanumeric() || *character == '_')
                .collect();
            if identifier.is_empty() {
                continue;
            }
            spellings.insert(renamed.take().unwrap_or_else(|| snake_case(&identifier)));
        }
        assert!(
            !spellings.is_empty(),
            "no variants were parsed out of `{header}`"
        );
        spellings
    }

    /// `serde(rename_all = "snake_case")` over a Rust variant identifier.
    fn snake_case(identifier: &str) -> String {
        let mut out = String::new();
        for (index, character) in identifier.char_indices() {
            if character.is_uppercase() {
                if index != 0 {
                    out.push('_');
                }
                out.extend(character.to_lowercase());
            } else {
                out.push(character);
            }
        }
        out
    }

    /// Every `"enum": [...]` anywhere in any tool schema, with its JSON path.
    ///
    /// COLLECTED BY WALKING rather than listed, which is the half that matters:
    /// a schema enum added later has no entry in the table below and turns this
    /// red, instead of joining `permission_mode` as a quiet six-of-seven.
    fn schema_enums() -> BTreeMap<String, BTreeSet<String>> {
        fn walk(path: &str, value: &Value, found: &mut BTreeMap<String, BTreeSet<String>>) {
            match value {
                Value::Object(object) => {
                    for (key, child) in object {
                        if key == "enum"
                            && let Some(values) = child.as_array()
                        {
                            found.insert(
                                path.to_owned(),
                                values
                                    .iter()
                                    .filter_map(|item| item.as_str().map(ToOwned::to_owned))
                                    .collect(),
                            );
                            continue;
                        }
                        walk(&format!("{path}/{key}"), child, found);
                    }
                }
                Value::Array(items) => {
                    for (index, item) in items.iter().enumerate() {
                        walk(&format!("{path}/{index}"), item, found);
                    }
                }
                _ => {}
            }
        }

        let mut found = BTreeMap::new();
        for definition in published_tool_definitions() {
            let name = definition["name"].as_str().unwrap().to_owned();
            walk(&name, &definition["inputSchema"], &mut found);
        }
        found
    }

    /// Every enum an MCP caller reads names exactly the variants the daemon's
    /// own type has.
    ///
    /// THIS IS THE CHECK `permission_mode` NEEDED. The module doc says "Every
    /// input schema mirrors one v1 DTO", and `permission_mode` listed SIX of
    /// `PermissionMode`'s SEVEN variants. `additionalProperties: false` does
    /// not police an enum's contents, and nothing else did either.
    ///
    /// Two directions, so neither side can quietly narrow. Every schema enum
    /// found by the walk must be in the table (a new one cannot slip in
    /// unchecked), and every table entry must equal its protocol enum exactly
    /// (a variant added on either side is red).
    #[test]
    fn every_enum_in_every_tool_schema_names_exactly_its_protocol_variants() {
        // (JSON path of the schema enum) -> (protocol enum it mirrors).
        let table = [("run_stateless/properties/effort", "EffortLevel")];

        let found = schema_enums();
        let expected_paths: BTreeSet<String> =
            table.iter().map(|(path, _)| (*path).to_owned()).collect();
        assert_eq!(
            found.keys().cloned().collect::<BTreeSet<_>>(),
            expected_paths,
            "a tool schema gained or lost an `enum` that this census does not account for"
        );

        for (path, protocol_enum) in table {
            let variants = protocol_enum_spellings(protocol_enum);
            let schema = found.get(path).unwrap();
            assert_eq!(
                schema, &variants,
                "the MCP schema enum at {path} disagrees with protocol-v1 {protocol_enum}"
            );
        }
    }

    /// A description built from an indented Rust string literal carries the
    /// source's own indentation into the text an agent reads.
    ///
    /// `run_stateless` shipped with fourteen consecutive spaces mid-sentence
    /// for exactly this reason. Checked over every description the walk finds
    /// rather than over the one that was wrong.
    #[test]
    fn no_description_an_agent_reads_carries_the_source_indentation() {
        fn walk(path: &str, value: &Value, offenders: &mut Vec<String>) {
            match value {
                Value::Object(object) => {
                    for (key, child) in object {
                        if key == "description"
                            && let Some(text) = child.as_str()
                            && text.contains("  ")
                        {
                            offenders.push(format!("{path}/{key}"));
                        }
                        walk(&format!("{path}/{key}"), child, offenders);
                    }
                }
                Value::Array(items) => {
                    for (index, item) in items.iter().enumerate() {
                        walk(&format!("{path}/{index}"), item, offenders);
                    }
                }
                _ => {}
            }
        }

        let mut offenders = Vec::new();
        for definition in published_tool_definitions() {
            let name = definition["name"].as_str().unwrap().to_owned();
            let description = definition["description"].as_str().unwrap();
            assert!(
                !description.contains("  "),
                "the {name} tool description carries a run of source indentation: {description:?}"
            );
            walk(&name, &definition["inputSchema"], &mut offenders);
        }
        assert!(
            offenders.is_empty(),
            "these schema descriptions carry runs of source indentation: {offenders:?}"
        );
    }

    /// Every tool has a description, and it is not a bare restatement of its
    /// own name.
    #[test]
    fn every_tool_carries_a_description_an_agent_can_act_on() {
        for definition in published_tool_definitions() {
            let name = definition["name"].as_str().unwrap();
            let description = definition["description"].as_str().unwrap_or_default();
            assert!(
                description.len() > name.len() + 20,
                "the {name} tool description is too thin to be a contract: {description:?}"
            );
            assert!(
                description.ends_with('.'),
                "the {name} tool description is not a sentence: {description:?}"
            );
        }
    }

    /// The tool surface names no resource, checked against the field an agent
    /// would actually try.
    #[test]
    fn the_run_stateless_tool_refuses_every_resource_a_caller_might_name() {
        for named in [
            "cwd",
            "config_isolation",
            "system_prompt",
            "session_id",
            "generation_id",
            "claude",
            "executable",
            "environment",
            "permission_mode",
            "allowed_tools",
            "denied_tools",
            "settings",
            "mcp_configs",
            "plugin_dirs",
            "extra_args",
        ] {
            let mut arguments = json!({"model": "claude-opus-5", "prompt": "hello"});
            arguments[named] = json!("/anything");
            let refused = map_tool_call("run_stateless", &arguments);
            assert!(
                refused.is_err(),
                "run_stateless admitted the field {named:?}, so a caller can name a resource"
            );
        }
    }

    #[test]
    fn maps_run_stateless_directly_to_its_native_request() {
        assert!(matches!(
            map_tool_call(
                "run_stateless",
                &json!({"model": "claude-opus-5", "effort": "xhigh", "prompt": "hello"})
            )
            .unwrap(),
            Request::RunStateless(_)
        ));
    }

    #[test]
    fn defaults_are_owned_by_protocol_dtos_not_mcp_logic() {
        let Request::RunStateless(request) = map_tool_call(
            "run_stateless",
            &json!({"model": "claude-opus-5", "prompt": "hello"}),
        )
        .unwrap() else {
            panic!("wrong request type")
        };
        assert_eq!(request.effort, None);
        assert_eq!(request.deadline_unix_ms, None);
    }

    /// A refusal serde wrote out of the caller's OWN VALUES stays content-free.
    ///
    /// The forwarding above is only safe because it is bounded to the span
    /// `caller_actionable_decode_refusal` returns. This is the other side of
    /// that bound: a `run_stateless` frame carries the caller's prompt.
    #[test]
    fn a_decode_failure_serde_wrote_is_never_forwarded_to_an_mcp_caller() {
        let secret = "must-not-escape";
        let error = map_tool_call(
            "run_stateless",
            &json!({
                "model": "claude-opus-5",
                "prompt": "hello",
                "deadline_unix_ms": secret,
            }),
        )
        .unwrap_err();
        let rendered = error.result().to_string();
        assert!(
            rendered.contains("invalid_arguments"),
            "a refusal pmux did not compose stays classified and content-free: {rendered}"
        );
        assert!(!rendered.contains(secret), "{rendered}");
    }

    #[test]
    fn rejects_unknown_fields_without_echoing_input() {
        let secret = "never-echo-this-prompt";
        let error = map_tool_call(
            "run_stateless",
            &json!({
                "model": "claude-opus-5",
                "prompt": "hello",
                "legacy_prompt_loop": secret
            }),
        )
        .unwrap_err();
        assert!(!error.to_string().contains(secret));
        assert!(!error.result().to_string().contains(secret));
        assert!(
            map_tool_call("legacy_screen_text", &json!({}))
                .unwrap_err()
                .is_unknown_tool()
        );
    }

    #[test]
    fn daemon_errors_are_redacted_to_typed_metadata() {
        let error = redact_client_error(ClientError::Server(
            pseudomux_protocol::v1::ErrorBody::new(
                ErrorCode::InvalidConfig,
                "secret path and prompt",
            )
            .retryable(false)
            .with_details(json!({"secret": "must-not-escape"})),
        ));
        let rendered = error.result().to_string();
        assert!(rendered.contains("invalid_config"));
        assert!(!rendered.contains("secret path"));
        assert!(!rendered.contains("must-not-escape"));
        // A refusal with no advice says nothing where the advice would be,
        // rather than saying `null` there.
        assert_eq!(
            error.result()["structuredContent"]["error"],
            json!({"kind": "daemon_rejected", "code": "invalid_config", "retryable": false})
        );
    }

    /// A refused MCP caller reads the daemon's own advice, and still reads
    /// nothing else out of the refusal.
    ///
    /// **This surface used to render the constant string `"pmuxd rejected the
    /// native request"` for every refusal there is.** `redact_client_error`
    /// kept `code` and `retryable` and dropped `message` and the whole of
    /// `details`, so on the one provider surface whose reader cannot ask a human,
    /// "the stateless token engine is not enabled on this daemon" and "your
    /// prompt starts with `/`" arrived as byte-identical payloads: both
    /// `unsupported_feature`, both not retryable, both that sentence.
    ///
    /// The two halves are deliberately not symmetric. `recommendation` is
    /// written by `ErrorBody::advising` out of pmux's own vocabulary and is
    /// forwarded; `message` can be composed from caller bytes and is not. The
    /// `secret_value` below is the shape that decides it — a real decode
    /// refusal renders the caller's own value into the message.
    #[test]
    fn a_refusals_advice_reaches_the_model_and_the_rest_of_it_does_not() {
        let advised = |code, message: &str, details| {
            redact_client_error(ClientError::Server(
                pseudomux_protocol::v1::ErrorBody::new(code, message).with_details(details),
            ))
        };
        let no_pool = advised(
            ErrorCode::UnsupportedFeature,
            "the stateless token engine is not enabled on this daemon",
            json!({
                "violation": "path_b_not_enabled",
                "recommendation": "restart pmuxd with --pool-parent DIR --pool-claude PATH",
                "attach_token": "attach-capability-token-secret",
            }),
        );
        let mode_prefix = advised(
            ErrorCode::UnsupportedFeature,
            "a prompt whose first character is `/` opens the composer's command menu",
            json!({
                "violation": "composer_mode_prefix",
                "recommendation": "Put a word before it, or ask for the command as text.",
            }),
        );

        for (failure, advice) in [
            (&no_pool, "restart pmuxd with --pool-parent DIR"),
            (&mode_prefix, "Put a word before it, or ask for the command"),
        ] {
            let result = failure.result();
            assert_eq!(
                result["structuredContent"]["error"]["recommendation"],
                json!(advice_of(failure)),
                "the advice must be structured data and not only prose"
            );
            assert!(
                result["content"][0]["text"]
                    .as_str()
                    .is_some_and(|text| text.contains(advice)),
                "the model reads `content`, and the advice was not in it: {result}"
            );
        }

        // The whole point: two refusals that were one payload are now two.
        assert_ne!(no_pool.result(), mode_prefix.result());

        // ...and nothing else came with it.
        let rendered = no_pool.result().to_string();
        assert!(!rendered.contains("attach-capability-token-secret"));
        assert!(!rendered.contains("the stateless token engine is not enabled"));

        // A message that could carry caller bytes is still redacted even when
        // the body beside it carries advice.
        let secret_value = redact_client_error(ClientError::Server(
            pseudomux_protocol::v1::ErrorBody::new(
                ErrorCode::InvalidConfig,
                "invalid type: integer `42`, expected a string",
            )
            .with_details(json!({"recommendation": "send a string"})),
        ));
        let rendered = secret_value.result().to_string();
        assert!(rendered.contains("send a string"));
        assert!(!rendered.contains("42"));
    }

    /// The advice a `ToolCallError` is carrying, read back out of the variant
    /// rather than out of the rendering it is being compared with.
    fn advice_of(error: &ToolCallError) -> &str {
        match error {
            ToolCallError::Failure(PublicFailure::DaemonRejected {
                recommendation: Some(recommendation),
                ..
            }) => recommendation,
            other => panic!("not an advised daemon refusal: {other:?}"),
        }
    }

    #[test]
    fn daemon_identity_and_cursor_failures_are_redacted_as_invalid_responses() {
        let expected_session = SESSION.parse().unwrap();
        let actual_session = "00000000-0000-0000-0000-000000000005".parse().unwrap();
        let expected_generation = pseudomux_protocol::v1::SessionGenerationId::from_u128(4);
        let actual_generation = pseudomux_protocol::v1::SessionGenerationId::from_u128(5);
        let expected_turn = TURN.parse().unwrap();
        let actual_turn = "00000000-0000-0000-0000-000000000006".parse().unwrap();
        let failures = [
            ClientError::ResultSessionMismatch {
                expected: expected_session,
                actual: actual_session,
            },
            ClientError::ResultGenerationMismatch {
                expected: expected_generation,
                actual: actual_generation,
            },
            ClientError::ResultTurnMismatch {
                expected: expected_turn,
                actual: actual_turn,
            },
            ClientError::EventCursorOverflow { cursor: u64::MAX },
        ];

        for failure in failures {
            assert!(matches!(
                redact_client_error(failure),
                ToolCallError::Failure(PublicFailure::InvalidDaemonResponse)
            ));
        }
    }

    #[test]
    fn successful_tool_payloads_are_not_duplicated_as_text() {
        let structured = json!({"large": "x".repeat(1024)});
        let result = successful_tool_result(structured);
        assert_eq!(result["content"], json!([]));
        assert_eq!(
            result["structuredContent"]["large"].as_str().unwrap().len(),
            1024
        );
        assert_eq!(result.to_string().matches(&"x".repeat(1024)).count(), 1);
    }
}
