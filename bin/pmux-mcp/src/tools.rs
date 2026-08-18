//! Thin MCP-to-native-protocol request mapping.
//!
//! This module deliberately contains no prompt loop, completion logic, daemon
//! discovery, process launch, or terminal interpretation.

use std::fmt;

use pseudomux_client::{ClientError, PmuxClient};
use pseudomux_protocol::v1::{
    ErrorCode, MAX_SUBSCRIBE_EVENTS, MAX_SUBSCRIBE_WAIT_MS, RECOMMENDATION_KEY, Request,
    ResponseResult, SubscribeEventsRequest,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolKind {
    StartSession,
    RunTurn,
    InspectSession,
    CancelTurn,
    CloseSession,
    RunOnce,
    SubscribeEvents,
    AttachSession,
    RunStateless,
    CreateAgent,
    GetAgent,
    ListAgents,
    UpdateAgent,
}

#[derive(Debug, PartialEq)]
pub(crate) struct MappedCall {
    kind: ToolKind,
    pub(crate) request: Request,
}

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
    /// `{"environment":{"set":{"SECRET":42}}}` renders as ``invalid type:
    /// integer `42`, expected a string``, and a start frame carries environment
    /// values, inline settings and MCP documents, and system prompts.
    ///
    /// `bin/pmuxd/src/handler.rs` has forwarded exactly this span since the
    /// both-modes rule was written; this surface promised the same thing in its
    /// tool description -- "refused with invalid_config naming the colliding
    /// field" -- and threw it away with `map_err(|_| InvalidArguments)`.
    InvalidConfig {
        reason: String,
    },
    InvalidBounds,
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
        /// MEASURED, `{"environment":{"set":{"SECRET":42}}}` comes back as
        /// ``invalid type: integer `42`, expected a string``, so forwarding
        /// every message would forward caller values from every start frame's
        /// environment, inline settings and system prompts. `recommendation` is
        /// written by `ErrorBody::advising` and by nothing else, always out of
        /// pmux's own vocabulary.
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
            Self::Failure(PublicFailure::InvalidBounds) => (
                "event wait or batch size exceeds the MCP safety bound".to_owned(),
                json!({
                    "error": {
                        "kind": "invalid_bounds",
                        "max_wait_ms": MAX_SUBSCRIBE_WAIT_MS,
                        "max_events": MAX_SUBSCRIBE_EVENTS,
                    }
                }),
            ),
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
            Self::Failure(PublicFailure::InvalidBounds) => "tool arguments exceed safety bounds",
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

/// Native tools the server can dispatch. Every input schema mirrors one v1 DTO.
///
/// [`published_tool_definitions`] is the catalogue `tools/list` returns.
/// Session and agent tools stay callable through `tools/call` so existing
/// clients do not break, but they are not the product surface.
pub fn tool_definitions() -> Vec<Value> {
    vec![
        tool(
            "start_session",
            concat!(
                "Start one interactive Claude session through the native pmux service. Supply ",
                "EITHER `claude` (the inline launch configuration) OR `agent` (a stored id and an ",
                "EXACT version), never both and never neither: a request carrying both is refused ",
                "with invalid_config naming the colliding field, and merging is refused rather ",
                "than resolved because a merge surface needs one documented rule per field and ",
                "nothing derives that list. The input schema states that rule too, as ",
                "dependentSchemas on `agent`, so it can be read rather than discovered by ",
                "failing. `cwd` is always required and is NEVER taken from the ",
                "agent; an agent may only bound it, via containment.workspace_root."
            ),
            start_session_schema(),
            annotations(false, false, false, true),
        ),
        tool(
            "run_turn",
            "Submit one idempotent turn to an existing session; returns acceptance metadata. Use subscribe_events for progress and completion.",
            run_turn_schema(),
            annotations(false, false, true, true),
        ),
        tool(
            "inspect_session",
            "Read the current native session snapshot.",
            inspect_session_schema(),
            annotations(true, false, true, false),
        ),
        tool(
            "cancel_turn",
            "Cancel one exact active turn and report recovery state.",
            cancel_turn_schema(),
            annotations(false, true, true, true),
        ),
        tool(
            "close_session",
            "Close one session and report whether its process tree was reaped. `process_reaped` is false when the daemon could not positively observe the owned process boundary empty; the session is not released until it is true.",
            close_session_schema(),
            annotations(false, true, true, true),
        ),
        tool(
            "run_once",
            "Start, run, and close one interactive session through the canonical native operation.",
            run_once_schema(),
            annotations(false, false, false, true),
        ),
        tool(
            "subscribe_events",
            "Fetch one bounded, sequence-validated long-poll batch. Replay loss is returned in replay_gap with a recovery snapshot.",
            subscribe_events_schema(),
            annotations(true, false, true, false),
        ),
        tool(
            "attach_session",
            "Mint short-lived native attach capability metadata for a writable terminal attachment. The returned token is a bearer credential for the session's terminal and is not consumed by MCP. `read_only: true` is refused with unsupported_feature, and a minified cell refuses writable attachment, so a minified cell cannot be attached at all.",
            attach_session_schema(),
            annotations(false, false, false, false),
        ),
        tool(
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
                "(--path-b-parent)."
            ),
            run_stateless_schema(),
            // Not read-only (it spends tokens), not destructive (it creates and
            // destroys nothing a caller can name), not idempotent (two calls
            // are two answers and two bills), open-world (it reaches Claude).
            annotations(false, false, false, true),
        ),
        tool(
            "create_agent",
            concat!(
                "Store one reusable Claude launch configuration and return its id and version 1. ",
                "AN AGENT MAY NARROW WHAT A SESSION NAMES; IT MAY NEVER NAME A RESOURCE ON THE ",
                "SESSION'S BEHALF. There is deliberately no cwd, no config_isolation root, no ",
                "session identity, no prompt and no environment snapshot in this schema, because ",
                "each of those is per-session and is named on every start_session; ",
                "containment.workspace_root BOUNDS the cwd a session may use and never supplies ",
                "one. Refused with invalid_config when the daemon was started without an agent ",
                "store (--agent-store), when cell is `minified` and ",
                "containment.require_config_isolation is not set, or when environment.set names ",
                "a variable that would move the child's configuration root."
            ),
            create_agent_schema(),
            // Not read-only (it writes a file), not destructive (it removes
            // nothing and every stored version is immutable), not idempotent
            // (two calls mint two agents), closed-world (it reaches no network).
            annotations(false, false, false, false),
        ),
        tool(
            "get_agent",
            concat!(
                "Read one stored agent version. Omit `version` for the current head. Environment ",
                "values and inline settings/MCP document bodies come back as `sha256:` digests ",
                "and never in the clear, while config_digest still identifies the configuration ",
                "exactly. The system prompt is NOT redacted: it is the most important thing about ",
                "an agent and an inspection surface that hid it would be useless."
            ),
            get_agent_schema(),
            annotations(true, false, true, false),
        ),
        tool(
            "list_agents",
            concat!(
                "List every stored agent's id, current version, config digest, name and cell. ",
                "Deliberately does NOT return full specs -- use get_agent for one -- because a ",
                "list is a directory read and returning every spec would spray every stored ",
                "environment key across one frame."
            ),
            list_agents_schema(),
            annotations(true, false, true, false),
        ),
        tool(
            "update_agent",
            concat!(
                "Store a new immutable version of one agent and return it. `expected_version` is ",
                "REQUIRED and is a fence: any value that is not the current head is refused with ",
                "id_conflict, including one stale by exactly one revision, and no update is ever ",
                "answered as 'already landed'. `spec` is a COMPLETE replacement and not a patch. ",
                "Running sessions are unaffected -- each pinned its version at start and never ",
                "reads the store again."
            ),
            update_agent_schema(),
            annotations(false, false, false, false),
        ),
    ]
}

/// What `tools/list` returns: the provider surface only.
///
/// Derived from [`tool_definitions`] so a change to `run_stateless` cannot
/// drift between the catalogue and the dispatcher.
pub fn published_tool_definitions() -> Vec<Value> {
    tool_definitions()
        .into_iter()
        .filter(|tool| tool["name"] == "run_stateless")
        .collect()
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
    let mapped = map_tool_call(name, arguments)?;
    let result = client
        .request(mapped.request)
        .await
        .map_err(redact_client_error)?;
    let structured = extract_result(mapped.kind, result)?;
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

pub(crate) fn map_tool_call(name: &str, arguments: &Value) -> Result<MappedCall, ToolCallError> {
    let (kind, request) = match name {
        "start_session" => (
            ToolKind::StartSession,
            Request::StartSession(decode(arguments)?),
        ),
        "run_turn" => (ToolKind::RunTurn, Request::RunTurn(decode(arguments)?)),
        "inspect_session" => (
            ToolKind::InspectSession,
            Request::InspectSession(decode(arguments)?),
        ),
        "cancel_turn" => (
            ToolKind::CancelTurn,
            Request::CancelTurn(decode(arguments)?),
        ),
        "close_session" => (
            ToolKind::CloseSession,
            Request::CloseSession(decode(arguments)?),
        ),
        "run_once" => (ToolKind::RunOnce, Request::RunOnce(decode(arguments)?)),
        "subscribe_events" => {
            if arguments
                .get("wait_ms")
                .and_then(Value::as_u64)
                .is_some_and(|value| value > MAX_SUBSCRIBE_WAIT_MS)
                || arguments
                    .get("max_events")
                    .and_then(Value::as_u64)
                    .is_some_and(|value| value > u64::from(MAX_SUBSCRIBE_EVENTS))
            {
                return Err(ToolCallError::Failure(PublicFailure::InvalidBounds));
            }
            let request: SubscribeEventsRequest = decode(arguments)?;
            if request.wait_ms > MAX_SUBSCRIBE_WAIT_MS || request.max_events > MAX_SUBSCRIBE_EVENTS
            {
                return Err(ToolCallError::Failure(PublicFailure::InvalidBounds));
            }
            (ToolKind::SubscribeEvents, Request::SubscribeEvents(request))
        }
        "attach_session" => (
            ToolKind::AttachSession,
            Request::AttachSession(decode(arguments)?),
        ),
        "run_stateless" => (
            ToolKind::RunStateless,
            Request::RunStateless(decode(arguments)?),
        ),
        "create_agent" => (
            ToolKind::CreateAgent,
            Request::CreateAgent(decode(arguments)?),
        ),
        "get_agent" => (ToolKind::GetAgent, Request::GetAgent(decode(arguments)?)),
        "list_agents" => (
            ToolKind::ListAgents,
            Request::ListAgents(decode(arguments)?),
        ),
        "update_agent" => (
            ToolKind::UpdateAgent,
            Request::UpdateAgent(decode(arguments)?),
        ),
        _ => return Err(ToolCallError::UnknownTool),
    };
    Ok(MappedCall { kind, request })
}

/// Decodes one tool's arguments, FORWARDING the refusal when pmux wrote it.
///
/// `caller_actionable_decode_refusal` is the same function
/// `bin/pmuxd/src/handler.rs` calls on the wire, so the sentence a model reads
/// through MCP and the sentence a socket caller reads are one sentence, written
/// once. Before this, `map_err(|_| InvalidArguments)` discarded it, and MEASURED
/// against four cases -- `agent` beside `terminal`, `agent` beside `cell`,
/// neither `claude` nor `agent`, and `agent` beside `environment.set` -- this
/// surface answered `arguments do not match the native pmux protocol schema`
/// with `"kind": "invalid_arguments"` for all four, while the tool's own
/// description promised "refused with invalid_config naming the colliding
/// field".
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

fn extract_result(kind: ToolKind, result: ResponseResult) -> Result<Value, ToolCallError> {
    let value = match (kind, result) {
        (ToolKind::StartSession, ResponseResult::SessionStarted(value)) => {
            serde_json::to_value(value)
        }
        (ToolKind::RunTurn, ResponseResult::TurnAccepted(value)) => serde_json::to_value(value),
        (ToolKind::InspectSession, ResponseResult::SessionSnapshot(value)) => {
            serde_json::to_value(value)
        }
        (ToolKind::CancelTurn, ResponseResult::TurnCancelled(value)) => serde_json::to_value(value),
        (ToolKind::CloseSession, ResponseResult::SessionClosed(value)) => {
            serde_json::to_value(value)
        }
        (ToolKind::RunOnce, ResponseResult::TurnResult(value)) => serde_json::to_value(value),
        (ToolKind::SubscribeEvents, ResponseResult::Events(value)) => serde_json::to_value(value),
        (ToolKind::AttachSession, ResponseResult::AttachCapability(value)) => {
            serde_json::to_value(value)
        }
        (ToolKind::RunStateless, ResponseResult::StatelessResult(value)) => {
            serde_json::to_value(value)
        }
        (ToolKind::CreateAgent, ResponseResult::AgentCreated(value)) => serde_json::to_value(value),
        (ToolKind::GetAgent, ResponseResult::Agent(value)) => serde_json::to_value(value),
        (ToolKind::ListAgents, ResponseResult::AgentList(value)) => serde_json::to_value(value),
        (ToolKind::UpdateAgent, ResponseResult::AgentUpdated(value)) => serde_json::to_value(value),
        _ => return Err(ToolCallError::Failure(PublicFailure::InvalidDaemonResponse)),
    };
    value.map_err(|_| ToolCallError::Failure(PublicFailure::InvalidDaemonResponse))
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

fn uuid_schema() -> Value {
    json!({"type": "string", "format": "uuid"})
}

fn string_array_schema() -> Value {
    json!({
        "type": "array",
        "items": {"type": "string"},
        "maxItems": 256,
    })
}

fn config_source_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "source": {"const": "file"},
                    "path": {"type": "string", "minLength": 1}
                },
                "required": ["source", "path"]
            },
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "source": {"const": "inline"},
                    "document": {}
                },
                "required": ["source", "document"]
            }
        ]
    })
}

fn session_identity_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "mode": {"const": "new"},
                    "session_id": uuid_schema()
                },
                "required": ["mode"]
            },
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "mode": {"const": "resume"},
                    "session_id": uuid_schema()
                },
                "required": ["mode", "session_id"]
            }
        ]
    })
}

fn system_prompt_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {"mode": {"const": "default"}},
                "required": ["mode"]
            },
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "mode": {"const": "append"},
                    "prompt": {"type": "string"}
                },
                "required": ["mode", "prompt"]
            },
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "mode": {"const": "replace"},
                    "prompt": {"type": "string"}
                },
                "required": ["mode", "prompt"]
            }
        ]
    })
}

fn claude_config_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "executable": {"type": "string", "minLength": 1},
            "model": {"type": "string", "minLength": 1},
            "effort": {"enum": ["low", "medium", "high", "xhigh", "max"]},
            // SEVEN, not six. `dangerously_skip_permissions` is a
            // `PermissionMode` variant the daemon accepts and this schema
            // omitted, so every agent caller reading the schema was told the
            // mode did not exist -- while `pmux --permission-mode
            // dangerously-skip-permissions` offered it and the daemon ran it.
            // Pinned against the protocol enum by
            // `every_enum_in_every_tool_schema_names_exactly_its_protocol_variants`.
            "permission_mode": {
                "enum": [
                    "default",
                    "accept_edits",
                    "plan",
                    "auto",
                    "bypass_permissions",
                    "dont_ask",
                    "dangerously_skip_permissions"
                ],
                "description": "How Claude asks before acting. `dangerously_skip_permissions` launches Claude with --dangerously-skip-permissions and makes every turn of the session carry the `dangerous_permission_bypass` warning."
            },
            "allowed_tools": string_array_schema(),
            "denied_tools": string_array_schema(),
            "settings": {
                "type": "array",
                "items": config_source_schema(),
                "maxItems": 64
            },
            "mcp_configs": {
                "type": "array",
                "items": config_source_schema(),
                "maxItems": 64
            },
            "plugin_dirs": string_array_schema(),
            "system_prompt": system_prompt_schema(),
            "extra_args": string_array_schema()
        },
        "required": ["executable"]
    })
}

fn environment_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "snapshot": {
                "type": "object",
                "additionalProperties": {"type": "string"},
                "maxProperties": 8192
            },
            "set": {
                "type": "object",
                "additionalProperties": {"type": "string"},
                "maxProperties": 1024
            },
            "unset": {
                "type": "array",
                "items": {"type": "string"},
                "uniqueItems": true,
                "maxItems": 1024
            }
        }
    })
}

fn terminal_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "rows": {"type": "integer", "minimum": 1, "maximum": 65535},
            "cols": {"type": "integer", "minimum": 1, "maximum": 65535},
            "profile": {
                "enum": ["transparent", "rmux_standard"],
                "description": "`transparent` is the default and the only implemented identity; `rmux_standard` is reserved and every start naming it is refused with unsupported_feature."
            },
            "input_transport": {
                "enum": ["auto", "sdk", "attached_stream"],
                "description": "`auto` (the default) resolves to `sdk`, the only implemented transport; `attached_stream` is reserved and every start naming it is refused with unsupported_feature."
            }
        },
        "required": ["rows", "cols"]
    })
}

fn lifecycle_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {"mode": {"const": "transcript"}},
                "required": ["mode"]
            },
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "mode": {"const": "hybrid"},
                    "hook_timeout_ms": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 600000,
                        "default": 5000
                    }
                },
                "required": ["mode"]
            }
        ]
    })
}

/// Neither branch means what a reader would assume, so both are described.
///
/// `start_session` REFUSES `one_shot` (`native.rs:3049`), and `run_once`
/// OVERWRITES whatever the caller sent with `one_shot` (`native.rs:1475`).
/// One schema serves both tools, because `run_once_schema` embeds
/// `start_session_schema`.
fn retention_schema() -> Value {
    json!({
        "description": "start_session accepts only `persistent`; `one_shot` is refused there with unsupported_feature because it is reserved for run_once. run_once ignores this field entirely and always uses one_shot.",
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {"mode": {"const": "one_shot"}},
                "required": ["mode"]
            },
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "mode": {"const": "persistent"},
                    "idle_ttl_ms": {"type": "integer", "minimum": 1}
                },
                "required": ["mode"]
            }
        ]
    })
}

/// The start schema, WITH THE BOTH-MODES RULE IN IT.
///
/// A model reading this used to see `claude`, `agent`, `terminal`, `lifecycle`,
/// `retention`, `compatibility`, `cell` and `auth_policy` as plain optional
/// siblings with `required: ["identity", "cwd"]` -- no `oneOf`, no
/// `dependentSchemas` -- so the schema said a start naming both an agent and a
/// terminal was well-formed, and the daemon then refused it. A rule enforced at
/// the door and absent from the map is a rule a model can only learn by failing.
///
/// **THE FORBIDDEN LIST IS `agent_supplied_start_paths()`**, not a copy of it:
/// a path added there appears in this schema with no second edit, and
/// `the_start_schema_forbids_exactly_the_agent_supplied_paths` asserts the two
/// agree. The nested paths (`environment.set`, `environment.unset`) are
/// expressed where JSON Schema can express them -- inside `environment` -- and
/// the top-level ones as `false` schemas under `dependentSchemas.agent`, which
/// is what "this property may not appear beside `agent`" is spelled as.
fn start_session_schema() -> Value {
    // `dependentSchemas` fires only when `agent` is present, so this whole
    // object is the "an agent supplies the launch policy" half of the rule.
    let mut beside_an_agent = serde_json::Map::new();
    let mut environment_beside_an_agent = serde_json::Map::new();
    for path in pseudomux_protocol::v1::agent_supplied_start_paths() {
        let refusal = json!({
            "not": {},
            "description": format!(
                "`{path}` is supplied by the stored agent, so it may not appear beside `agent`: an \
                 agent supplies the whole launch policy, and merging is refused rather than \
                 resolved"
            )
        });
        match path.split_once('.') {
            Some(("environment", leaf)) => {
                environment_beside_an_agent.insert((*leaf).to_owned(), refusal);
            }
            Some((parent, _)) => unreachable!(
                "agent_supplied_start_paths gained a nested path under {parent:?} and this schema \
                 has no place for it"
            ),
            None => {
                beside_an_agent.insert((*path).to_owned(), refusal);
            }
        }
    }
    // `environment.snapshot` is deliberately still allowed: it is the one launch
    // input an agent structurally cannot carry.
    beside_an_agent.insert(
        "environment".to_owned(),
        json!({"properties": Value::Object(environment_beside_an_agent)}),
    );

    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "description": "Supply EITHER `claude` (the inline launch configuration) OR `agent` (a stored id and an exact version), never both and never neither. `cwd` is always required and is never taken from the agent.",
        "properties": {
            "identity": session_identity_schema(),
            "cwd": {"type": "string", "minLength": 1},
            "claude": claude_config_schema(),
            "agent": agent_ref_schema(),
            "environment": environment_schema(),
            "auth_policy": {"enum": ["subscription", "inherit"]},
            "config_isolation": config_isolation_schema(),
            "terminal": terminal_schema(),
            "lifecycle": lifecycle_schema(),
            "retention": retention_schema(),
            "compatibility": {"enum": ["require_tested", "allow_untested"]},
            "cell": {"enum": ["full", "minified"]}
        },
        "required": ["identity", "cwd"],
        // NEVER NEITHER. `claude` and `agent` are both optional properties on
        // their own, and exactly one of them must be there.
        "anyOf": [
            {"required": ["claude"]},
            {"required": ["agent"]}
        ],
        "dependentSchemas": {"agent": {"properties": Value::Object(beside_an_agent)}}
    })
}

fn config_isolation_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "root": {"type": "string", "minLength": 1}
        },
        "required": ["root"]
    })
}

fn turn_request_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "turn_id": uuid_schema(),
            "prompt": {"type": "string", "minLength": 1, "maxLength": 1048576},
            "deadline_unix_ms": {"type": "integer", "minimum": 0},
            // NOT IMPLEMENTED, and stated here rather than discovered by a
            // refused turn. `native.rs:2371` and `v1/actor.rs:1267` both
            // refuse any `on_disconnect` other than `continue` and any
            // `heartbeat_timeout_ms` at all, with unsupported_feature. The
            // fields stay in the schema because they are protocol-v1 fields
            // and the daemon owns the verdict; the descriptions stop the
            // schema from reading as an offer.
            "lease": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "on_disconnect": {
                        "enum": ["continue", "cancel_turn", "close_session"],
                        "description": "Only `continue` is implemented and it is the default. `cancel_turn` and `close_session` are refused with unsupported_feature: disconnect actions require a future leased connection API. Use cancel_turn (the tool) to stop a turn."
                    },
                    "heartbeat_timeout_ms": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "NOT IMPLEMENTED: any value is refused with unsupported_feature. Bound a turn with deadline_unix_ms instead, which the daemon does enforce."
                    }
                }
            }
        },
        "required": ["turn_id", "prompt"]
    })
}

fn run_turn_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "session_id": uuid_schema(),
            "generation_id": uuid_schema(),
            "turn": turn_request_schema()
        },
        "required": ["session_id", "generation_id", "turn"]
    })
}

fn inspect_session_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "session_id": uuid_schema(),
            "generation_id": uuid_schema()
        },
        "required": ["session_id", "generation_id"]
    })
}

fn cancel_turn_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "session_id": uuid_schema(),
            "generation_id": uuid_schema(),
            "turn_id": uuid_schema()
        },
        "required": ["session_id", "generation_id", "turn_id"]
    })
}

fn close_session_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "session_id": uuid_schema(),
            "generation_id": uuid_schema(),
            "policy": {"enum": ["graceful", "force"]}
        },
        "required": ["session_id", "generation_id"]
    })
}

fn run_once_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "session": start_session_schema(),
            "turn": turn_request_schema()
        },
        "required": ["session", "turn"]
    })
}

fn subscribe_events_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "session_id": uuid_schema(),
            "generation_id": uuid_schema(),
            "after_sequence": {"type": "integer", "minimum": 0, "default": 0},
            "wait_ms": {
                "type": "integer",
                "minimum": 0,
                "maximum": MAX_SUBSCRIBE_WAIT_MS,
                "default": 0
            },
            "max_events": {
                "type": "integer",
                "minimum": 0,
                "maximum": MAX_SUBSCRIBE_EVENTS,
                "default": 0
            }
        },
        "required": ["session_id", "generation_id"]
    })
}

fn attach_session_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "session_id": uuid_schema(),
            "generation_id": uuid_schema(),
            "read_only": {
                "type": "boolean",
                "default": false,
                "description": "NOT IMPLEMENTED: `true` is refused with unsupported_feature on every session, because the pinned rmux stream protocol has no view-only mode."
            },
            "size": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "rows": {"type": "integer", "minimum": 1, "maximum": 65535},
                    "cols": {"type": "integer", "minimum": 1, "maximum": 65535}
                },
                "required": ["rows", "cols"]
            }
        },
        "required": ["session_id", "generation_id"]
    })
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

/// The agent's own environment policy, and deliberately NOT
/// [`environment_schema`].
///
/// `snapshot` is a complete caller snapshot -- a fact about the calling process
/// at call time -- and there is no version of "an agent stores one" that is not
/// either stale the moment the caller's shell changes or a file of environment
/// values at rest. The field is absent from the daemon's own type, so it is
/// absent here; an agent caller reading this schema is told the truth rather
/// than told to leave a field empty.
fn agent_environment_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "set": {
                "type": "object",
                "additionalProperties": {"type": "string"},
                "maxProperties": 1024,
                "description": "Delivered to the child verbatim; this channel bypasses the launch allowlist. A name that would move the child's Claude configuration root (CLAUDE_CONFIG_DIR, CLAUDE_SECURESTORAGE_CONFIG_DIR, HOME, USERPROFILE, XDG_CONFIG_HOME) is refused with invalid_config: an agent is shared by every session started from it, so a stored root would make the agent id a contention key. Values are returned as sha256 digests by get_agent and never in the clear."
            },
            "unset": {
                "type": "array",
                "items": {"type": "string"},
                "uniqueItems": true,
                "maxItems": 1024
            }
        }
    })
}

/// What an agent may say about the resources a session names.
fn agent_containment_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "workspace_root": {
                "type": "string",
                "minLength": 1,
                "description": "Absolute directory every session's cwd must resolve INSIDE. It BOUNDS a cwd and never supplies one -- the caller still writes --cwd on every start -- and it composes with AND against the checks that already run, so no value here makes an otherwise-refused cwd admissible. Containment is decided on the resource (symlinks and the /tmp to /private/tmp rewrite included), not on a path prefix."
            },
            "require_config_isolation": {
                "type": "boolean",
                "default": false,
                "description": "Whether a session started from this agent MUST name a config_isolation root. The agent does not name one and cannot. REQUIRED to be true when cell is `minified`: a minified cell needs a configuration root of its own, so `false` there is a value the agent could never honour and create_agent refuses it rather than silently overriding it."
            }
        }
    })
}

/// Everything an agent stores.
///
/// One schema serves `create_agent` and `update_agent`, because the update is a
/// COMPLETE replacement rather than a patch: a partial-update surface needs one
/// documented merge rule per field and one test per rule, and nothing derives
/// that list.
fn agent_spec_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "name": {
                "type": "string",
                "minLength": 1,
                "maxLength": 200,
                "description": "A human label. It has no filesystem role and no uniqueness requirement, and it is not the id: the daemon mints a UUID, so a name can never become a path component."
            },
            "description": {"type": "string", "maxLength": 4000},
            "claude": claude_config_schema(),
            "environment": agent_environment_schema(),
            "auth_policy": {"enum": ["subscription", "inherit"]},
            "terminal": terminal_schema(),
            "lifecycle": lifecycle_schema(),
            "retention": {
                "description": "Only `persistent` may be stored. `one_shot` is refused at create_agent with unsupported_feature, because start_session refuses it and run_once overwrites it -- so a stored `one_shot` is a value no start could ever honour.",
                "oneOf": [
                    {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {"mode": {"const": "one_shot"}},
                        "required": ["mode"]
                    },
                    {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "mode": {"const": "persistent"},
                            "idle_ttl_ms": {"type": "integer", "minimum": 1}
                        },
                        "required": ["mode"]
                    }
                ]
            },
            "compatibility": {"enum": ["require_tested", "allow_untested"]},
            "cell": {"enum": ["full", "minified"]},
            "containment": agent_containment_schema()
        },
        "required": ["name", "claude"]
    })
}

fn create_agent_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {"spec": agent_spec_schema()},
        "required": ["spec"]
    })
}

fn get_agent_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "agent_id": uuid_schema(),
            "version": {
                "type": "integer",
                "minimum": 1,
                "description": "Omit for the current head. Absent means `whatever is current now`, which is honest for a READ and is exactly what start_session's `agent` refuses for a LAUNCH: a read reports, a launch commits."
            }
        },
        "required": ["agent_id"]
    })
}

fn list_agents_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {},
        "required": []
    })
}

fn update_agent_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "agent_id": uuid_schema(),
            "expected_version": {
                "type": "integer",
                "minimum": 1,
                "description": "The version you believe is current. REQUIRED, and a fence rather than a routing key: any value that is not the current head is refused with id_conflict, including one stale by exactly one revision, and nothing is ever answered as `your update already landed`. Recovering a lost response costs one get_agent and never a wrong answer."
            },
            "spec": agent_spec_schema()
        },
        "required": ["agent_id", "expected_version", "spec"]
    })
}

/// The stored agent one session runs, named on `start_session`.
fn agent_ref_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "agent_id": uuid_schema(),
            "version": {
                "type": "integer",
                "minimum": 1,
                "description": "The exact stored version this session runs. REQUIRED: there is deliberately no `omit for latest`, because `latest at start time` would make the launch a function of WHEN the request arrived. Call get_agent once and log the version you got."
            }
        },
        "required": ["agent_id", "version"]
    })
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use pseudomux_protocol::v1::{
        AttachSessionRequest, CancelTurnRequest, CloseSessionRequest, CompatibilityReport,
        EventBatch, InputTransport, InspectSessionRequest, ReplayGap, RunOnceRequest,
        RunTurnRequest, SessionSnapshot, SessionState, StartSessionRequest, TerminalProfile,
    };

    use super::*;

    const SESSION: &str = "00000000-0000-0000-0000-000000000002";
    const GENERATION: &str = "00000000-0000-0000-0000-000000000004";
    const TURN: &str = "00000000-0000-0000-0000-000000000003";

    fn start() -> Value {
        json!({
            "identity": {"mode": "new", "session_id": SESSION},
            "cwd": "/work/project",
            "claude": {"executable": "/opt/claude/bin/claude"}
        })
    }

    fn turn() -> Value {
        json!({
            "turn_id": TURN,
            "prompt": "inspect the repository",
            "lease": {"on_disconnect": "continue"}
        })
    }

    #[test]
    fn exposes_only_native_v1_tools_with_closed_schemas() {
        let definitions = tool_definitions();
        let names = definitions
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "start_session",
                "run_turn",
                "inspect_session",
                "cancel_turn",
                "close_session",
                "run_once",
                "subscribe_events",
                "attach_session",
                "run_stateless",
                "create_agent",
                "get_agent",
                "list_agents",
                "update_agent",
            ]
        );
        for definition in definitions {
            assert_eq!(definition["inputSchema"]["type"], "object");
            assert_eq!(definition["inputSchema"]["additionalProperties"], false);
        }
    }

    #[test]
    fn tools_list_is_the_provider_surface_only() {
        let published = published_tool_definitions();
        let names = published
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(names, ["run_stateless"]);
        let full = tool_definitions()
            .into_iter()
            .find(|tool| tool["name"] == "run_stateless")
            .expect("run_stateless must stay in the dispatch catalogue");
        assert_eq!(published[0], full);
    }

    /// `start_session_schema` is `additionalProperties: false`, so a field the
    /// Rust DTO gained and this schema did not is UNREACHABLE from every MCP
    /// caller -- silently, and without a compile error anywhere. `cell` reached
    /// the wire that way. Pin the two field sets against each other so the next
    /// one cannot.
    ///
    /// **TWO FIXTURES, UNIONED, because no single request can carry both
    /// shapes.** `claude` and `agent` are mutually exclusive by construction --
    /// the serializer refuses a launch-policy field beside an agent, and the
    /// deserializer refuses one on the way in -- so a single "populated"
    /// request would necessarily omit one of the two fields this schema must
    /// offer, and would then pass while the schema hid whichever one it left
    /// out. The union is the field set the schema is responsible for.
    #[test]
    fn the_start_session_schema_admits_every_field_the_rust_request_carries() {
        let inline = StartSessionRequest {
            identity: pseudomux_protocol::v1::SessionIdentity::New { session_id: None },
            cwd: "/work".into(),
            agent: None,
            claude: Some(pseudomux_protocol::v1::ClaudeLaunchConfig {
                executable: "/opt/claude".into(),
                model: None,
                effort: None,
                permission_mode: None,
                allowed_tools: Vec::new(),
                denied_tools: Vec::new(),
                settings: Vec::new(),
                mcp_configs: Vec::new(),
                plugin_dirs: Vec::new(),
                system_prompt: pseudomux_protocol::v1::SystemPromptPolicy::Default,
                extra_args: Vec::new(),
            }),
            environment: pseudomux_protocol::v1::EnvironmentSpec::default(),
            auth_policy: pseudomux_protocol::v1::AuthPolicy::Subscription,
            // Both fields that are skipped when default must be present here,
            // or the inventory this compares against is short by exactly the
            // fields most likely to be forgotten.
            config_isolation: Some(pseudomux_protocol::v1::ConfigIsolation {
                root: "/private/root".into(),
            }),
            terminal: pseudomux_protocol::v1::TerminalSpec::default(),
            lifecycle: pseudomux_protocol::v1::LifecycleMode::Transcript,
            retention: pseudomux_protocol::v1::RetentionPolicy::OneShot,
            compatibility: pseudomux_protocol::v1::CompatibilityPolicy::RequireTested,
            cell: pseudomux_protocol::v1::SessionCell::Minified,
        };
        let named = StartSessionRequest {
            claude: None,
            agent: Some(pseudomux_protocol::v1::AgentRef {
                agent_id: pseudomux_protocol::v1::AgentId::from_u128(6),
                version: pseudomux_protocol::v1::AgentVersion::FIRST,
            }),
            auth_policy: pseudomux_protocol::v1::AuthPolicy::default(),
            terminal: pseudomux_protocol::v1::TerminalSpec::default(),
            lifecycle: pseudomux_protocol::v1::LifecycleMode::default(),
            retention: pseudomux_protocol::v1::RetentionPolicy::default(),
            compatibility: pseudomux_protocol::v1::CompatibilityPolicy::default(),
            cell: pseudomux_protocol::v1::SessionCell::default(),
            ..inline.clone()
        };

        let encoded = serde_json::to_value(&inline).unwrap();
        let named_encoded = serde_json::to_value(&named).unwrap();
        let rust_fields = encoded
            .as_object()
            .unwrap()
            .keys()
            .chain(named_encoded.as_object().unwrap().keys())
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let schema = start_session_schema();
        let schema_fields = schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            rust_fields, schema_fields,
            "the MCP start_session schema drifted from StartSessionRequest"
        );
        // And the schema must actually accept both shapes.
        assert!(matches!(
            map_tool_call("start_session", &encoded).unwrap().request,
            Request::StartSession(_)
        ));
        assert!(matches!(
            map_tool_call("start_session", &named_encoded)
                .unwrap()
                .request,
            Request::StartSession(_)
        ));
    }

    /// The MCP surface delivers the sentence its own description promises.
    ///
    /// MEASURED before this, over four cases -- `agent` beside `terminal`,
    /// `agent` beside `cell`, neither `claude` nor `agent`, and `agent` beside
    /// `environment.set` -- every one answered
    ///
    /// ```text
    /// {"content":[{"text":"arguments do not match the native pmux protocol schema"}],
    ///  "structuredContent":{"error":{"kind":"invalid_arguments"}}}
    /// ```
    ///
    /// while `tool_definitions` promised "refused with invalid_config naming
    /// the colliding field". The colliding paths are walked from
    /// `agent_supplied_start_paths()` so a path added there is exercised here
    /// with nobody remembering this test exists.
    #[test]
    fn a_both_modes_refusal_reaches_an_mcp_caller_naming_the_colliding_field() {
        let base = json!({
            "identity": {"mode": "new"},
            "cwd": "/work/project",
            "agent": {"agent_id": "00000000-0000-0000-0000-000000000006", "version": 3},
        });
        // The baseline is admitted, or every assertion below could be passing
        // for a reason that has nothing to do with the collision.
        assert!(matches!(
            map_tool_call("start_session", &base).unwrap().request,
            Request::StartSession(_)
        ));

        let offending = |path: &str| -> Value {
            match path {
                "claude" => json!({"executable": "/opt/claude/bin/claude"}),
                "environment.set" => json!({"set": {"TERM": "dumb"}}),
                "environment.unset" => json!({"unset": ["TERM"]}),
                "auth_policy" => json!("inherit"),
                "terminal" => json!({"rows": 40, "cols": 132}),
                "lifecycle" => json!({"mode": "transcript"}),
                "retention" => json!({"mode": "persistent"}),
                "compatibility" => json!("allow_untested"),
                "cell" => json!("minified"),
                other => panic!(
                    "agent_supplied_start_paths gained {other:?} and this test has no value for it"
                ),
            }
        };

        for path in pseudomux_protocol::v1::agent_supplied_start_paths() {
            let mut arguments = base.clone();
            arguments[path.split('.').next().unwrap()] = offending(path);
            let error = map_tool_call("start_session", &arguments).unwrap_err();
            let rendered = error.result();
            assert_eq!(
                rendered["structuredContent"]["error"]["kind"], "invalid_config",
                "{path} was not classified as a configuration refusal: {rendered}"
            );
            let text = rendered["content"][0]["text"].as_str().unwrap();
            assert!(
                text.contains(path),
                "the MCP refusal for {path} does not name it: {text}"
            );
            // ...and it is still bounded: the span pmux composed, with serde's
            // own position suffix and the caller's values left out.
            assert!(
                !text.contains("at line"),
                "the forwarded span must stop before serde's position: {text}"
            );
        }

        // Neither mode is the other half of the same rule, and it names both.
        let error = map_tool_call(
            "start_session",
            &json!({"identity": {"mode": "new"}, "cwd": "/work/project"}),
        )
        .unwrap_err();
        let rendered = error.result();
        assert_eq!(
            rendered["structuredContent"]["error"]["kind"],
            "invalid_config"
        );
        let text = rendered["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("claude") && text.contains("agent"), "{text}");
    }

    /// A refusal serde wrote out of the caller's OWN VALUES stays content-free.
    ///
    /// The forwarding above is only safe because it is bounded to the span
    /// `caller_actionable_decode_refusal` returns. This is the other side of
    /// that bound, and it is the reason the daemon's transport has the same
    /// two-way split: a start frame carries environment values, inline settings
    /// and MCP documents, and system prompts.
    #[test]
    fn a_decode_failure_serde_wrote_is_never_forwarded_to_an_mcp_caller() {
        let secret = "must-not-escape";
        let error = map_tool_call(
            "start_session",
            &json!({
                "identity": {"mode": "new"},
                "cwd": "/work/project",
                "claude": {"executable": "/opt/claude"},
                "environment": {"set": {"SECRET": secret.len()}},
            }),
        )
        .unwrap_err();
        let rendered = error.result().to_string();
        assert!(
            rendered.contains("invalid_arguments"),
            "a refusal pmux did not compose stays classified and content-free: {rendered}"
        );
        assert!(!rendered.contains("SECRET"), "{rendered}");
    }

    /// The schema STATES the rule, and states exactly the derived list.
    ///
    /// A model gets the rule from the map rather than from a failure. The
    /// forbidden set is read back out of the generated schema and compared to
    /// `agent_supplied_start_paths()` by name, so the two cannot drift: this is
    /// the same derivation the refusal itself uses.
    #[test]
    fn the_start_schema_forbids_exactly_the_agent_supplied_paths() {
        let schema = start_session_schema();
        let beside = &schema["dependentSchemas"]["agent"]["properties"];
        let mut forbidden = std::collections::BTreeSet::new();
        for (name, value) in beside.as_object().expect("an object of properties") {
            if value.get("not").is_some() {
                forbidden.insert(name.clone());
                continue;
            }
            for (leaf, nested) in value["properties"].as_object().expect("nested properties") {
                assert!(nested.get("not").is_some(), "{name}.{leaf} states no rule");
                forbidden.insert(format!("{name}.{leaf}"));
            }
        }
        assert_eq!(
            forbidden,
            pseudomux_protocol::v1::agent_supplied_start_paths()
                .iter()
                .map(|path| (*path).to_owned())
                .collect::<std::collections::BTreeSet<_>>(),
            "the schema's forbidden-beside-an-agent set is not the derived list the daemon refuses \
             on"
        );
        // `environment.snapshot` is the one launch input an agent cannot carry,
        // so it must NOT be forbidden beside one.
        assert!(
            beside["environment"]["properties"]
                .get("snapshot")
                .is_none(),
            "a caller's environment snapshot survives beside an agent and the schema must say so"
        );
        // NEVER NEITHER is stated too, and over both spellings.
        let required: std::collections::BTreeSet<String> = schema["anyOf"]
            .as_array()
            .expect("anyOf")
            .iter()
            .flat_map(|branch| {
                branch["required"]
                    .as_array()
                    .expect("required")
                    .iter()
                    .map(|name| name.as_str().unwrap().to_owned())
            })
            .collect();
        assert_eq!(
            required,
            std::collections::BTreeSet::from(["agent".to_owned(), "claude".to_owned()])
        );
    }

    /// The same pin as `start_session`, for the same reason and against a
    /// stricter requirement.
    ///
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
            map_tool_call("run_stateless", &encoded).unwrap().request,
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
        for definition in tool_definitions() {
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
    /// not police an enum's contents, and nothing else did either: every agent
    /// caller that read the schema was told `dangerously_skip_permissions` did
    /// not exist, while `pmux --permission-mode dangerously-skip-permissions`
    /// offered it and the daemon accepted it.
    ///
    /// Two directions, so neither side can quietly narrow. Every schema enum
    /// found by the walk must be in the table (a new one cannot slip in
    /// unchecked), and every table entry must equal its protocol enum exactly
    /// (a variant added on either side is red).
    #[test]
    fn every_enum_in_every_tool_schema_names_exactly_its_protocol_variants() {
        // (JSON path of the schema enum) -> (protocol enum it mirrors).
        let table = [
            ("start_session/properties/auth_policy", "AuthPolicy"),
            ("start_session/properties/cell", "SessionCell"),
            (
                "start_session/properties/claude/properties/effort",
                "EffortLevel",
            ),
            (
                "start_session/properties/claude/properties/permission_mode",
                "PermissionMode",
            ),
            (
                "start_session/properties/compatibility",
                "CompatibilityPolicy",
            ),
            (
                "start_session/properties/terminal/properties/input_transport",
                "InputTransport",
            ),
            (
                "start_session/properties/terminal/properties/profile",
                "TerminalProfile",
            ),
            ("close_session/properties/policy", "ClosePolicy"),
            (
                "run_turn/properties/turn/properties/lease/properties/on_disconnect",
                "DisconnectAction",
            ),
            ("run_stateless/properties/effort", "EffortLevel"),
        ];

        // Every place one schema is EMBEDDED in another, as a prefix rewrite.
        //
        // `run_once` embeds `start_session_schema` and `turn_request_schema`;
        // `create_agent` and `update_agent` each embed `agent_spec_schema`,
        // which reuses `claude_config_schema` and `terminal_schema` and repeats
        // `start_session`'s own `auth_policy`, `compatibility` and `cell`.
        // Derived rather than listed, or the table above would silently stop
        // covering most of the surface the day an embedding changed -- and with
        // four embeddings rather than two, "silently" is now most of the
        // schemas an agent reads.
        let embeddings = [
            ("start_session/", "run_once/properties/session/"),
            ("run_turn/properties/turn/", "run_once/properties/turn/"),
            (
                "start_session/properties/",
                "create_agent/properties/spec/properties/",
            ),
            (
                "start_session/properties/",
                "update_agent/properties/spec/properties/",
            ),
        ];
        let embedded_paths = |path: &str| -> Vec<String> {
            embeddings
                .iter()
                .filter_map(|(from, to)| path.strip_prefix(from).map(|rest| format!("{to}{rest}")))
                .collect()
        };

        let found = schema_enums();
        let expected_paths: BTreeSet<String> = table
            .iter()
            .flat_map(|(path, _)| std::iter::once((*path).to_owned()).chain(embedded_paths(path)))
            .collect();
        assert_eq!(
            found.keys().cloned().collect::<BTreeSet<_>>(),
            expected_paths,
            "a tool schema gained or lost an `enum` that this census does not account for"
        );

        for (path, protocol_enum) in table {
            let variants = protocol_enum_spellings(protocol_enum);
            for candidate in std::iter::once(path.to_owned()).chain(embedded_paths(path)) {
                let Some(schema) = found.get(&candidate) else {
                    continue;
                };
                assert_eq!(
                    schema, &variants,
                    "the MCP schema enum at {candidate} disagrees with protocol-v1 {protocol_enum}"
                );
            }
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
        for definition in tool_definitions() {
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
        for definition in tool_definitions() {
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
    fn maps_every_tool_directly_to_its_native_request() {
        assert!(matches!(
            map_tool_call("start_session", &start()).unwrap().request,
            Request::StartSession(StartSessionRequest { .. })
        ));
        assert!(matches!(
            map_tool_call(
                "run_turn",
                &json!({"session_id": SESSION, "generation_id": GENERATION, "turn": turn()})
            )
            .unwrap()
            .request,
            Request::RunTurn(RunTurnRequest { .. })
        ));
        assert!(matches!(
            map_tool_call(
                "inspect_session",
                &json!({"session_id": SESSION, "generation_id": GENERATION})
            )
            .unwrap()
            .request,
            Request::InspectSession(InspectSessionRequest { .. })
        ));
        assert!(matches!(
            map_tool_call(
                "cancel_turn",
                &json!({"session_id": SESSION, "generation_id": GENERATION, "turn_id": TURN})
            )
            .unwrap()
            .request,
            Request::CancelTurn(CancelTurnRequest { .. })
        ));
        assert!(matches!(
            map_tool_call(
                "close_session",
                &json!({"session_id": SESSION, "generation_id": GENERATION, "policy": "force"})
            )
            .unwrap()
            .request,
            Request::CloseSession(CloseSessionRequest { .. })
        ));
        assert!(matches!(
            map_tool_call("run_once", &json!({"session": start(), "turn": turn()}))
                .unwrap()
                .request,
            Request::RunOnce(RunOnceRequest { .. })
        ));
        assert!(matches!(
            map_tool_call(
                "subscribe_events",
                &json!({
                    "session_id": SESSION,
                    "generation_id": GENERATION,
                    "after_sequence": 7,
                    "wait_ms": 1000,
                    "max_events": 32
                })
            )
            .unwrap()
            .request,
            Request::SubscribeEvents(SubscribeEventsRequest {
                after_sequence: 7,
                wait_ms: 1000,
                max_events: 32,
                ..
            })
        ));
        assert!(matches!(
            map_tool_call(
                "attach_session",
                &json!({"session_id": SESSION, "generation_id": GENERATION, "read_only": true})
            )
            .unwrap()
            .request,
            Request::AttachSession(AttachSessionRequest {
                read_only: true,
                ..
            })
        ));
    }

    #[test]
    fn defaults_are_owned_by_protocol_dtos_not_mcp_logic() {
        let Request::StartSession(request) =
            map_tool_call("start_session", &start()).unwrap().request
        else {
            panic!("wrong request type")
        };
        assert_eq!(request.terminal.rows, 24);
        assert_eq!(request.terminal.cols, 120);

        let Request::SubscribeEvents(request) = map_tool_call(
            "subscribe_events",
            &json!({"session_id": SESSION, "generation_id": GENERATION}),
        )
        .unwrap()
        .request
        else {
            panic!("wrong request type")
        };
        assert_eq!(request.after_sequence, 0);
        assert_eq!(request.wait_ms, 0);
        assert_eq!(request.max_events, 0);
    }

    #[test]
    fn rejects_unknown_fields_and_unbounded_long_polls_without_echoing_input() {
        let secret = "never-echo-this-prompt";
        let error = map_tool_call(
            "run_turn",
            &json!({
                "session_id": SESSION,
                "generation_id": GENERATION,
                "turn": turn(),
                "legacy_prompt_loop": secret
            }),
        )
        .unwrap_err();
        assert!(!error.to_string().contains(secret));
        assert!(!error.result().to_string().contains(secret));

        let error = map_tool_call(
            "subscribe_events",
            &json!({
                "session_id": SESSION,
                "generation_id": GENERATION,
                "wait_ms": MAX_SUBSCRIBE_WAIT_MS + 1
            }),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            ToolCallError::Failure(PublicFailure::InvalidBounds)
        ));
        assert!(map_tool_call("legacy_screen_text", &json!({})).is_err());
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
                "recommendation": "restart pmuxd with --path-b-parent DIR --path-b-claude PATH",
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
            (&no_pool, "restart pmuxd with --path-b-parent DIR"),
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
    fn replay_gap_is_returned_as_first_class_structured_data() {
        let session_id = SESSION.parse().unwrap();
        let snapshot = SessionSnapshot {
            agent: None,
            session_id,
            generation_id: pseudomux_protocol::v1::SessionGenerationId::from_u128(1),
            transcript_session_id: session_id,
            cell: pseudomux_protocol::v1::SessionCell::Full,
            state: SessionState::Ready,
            cwd: "/work/project".into(),
            active_turn_id: None,
            claude_version: Some("tested".into()),
            compatibility: CompatibilityReport {
                claude_version: "2.1.207".into(),
                os: "macos".into(),
                arch: "aarch64".into(),
                terminal_profile: TerminalProfile::Transparent,
                input_transport: InputTransport::Sdk,
                tested: true,
                transcript_drain_ms: 750,
            },
            created_at_ms: 1,
            updated_at_ms: 2,
            idle_deadline_ms: None,
            resumable: true,
            last_sequence: 9,
            last_turn: None,
            needs_input: None,
        };
        let structured = extract_result(
            ToolKind::SubscribeEvents,
            ResponseResult::Events(EventBatch {
                events: vec![],
                next_sequence: 10,
                replay_gap: Some(ReplayGap {
                    requested_after: 2,
                    oldest_available: 7,
                    next_sequence: 10,
                    snapshot: Box::new(snapshot),
                }),
            }),
        )
        .unwrap();
        assert_eq!(structured["replay_gap"]["requested_after"], 2);
        assert_eq!(structured["replay_gap"]["snapshot"]["last_sequence"], 9);
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
