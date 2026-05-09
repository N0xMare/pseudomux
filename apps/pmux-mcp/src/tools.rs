use anyhow::{Result, anyhow};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use pseudomux_adapters::ClaudeCodeOpts;
use pseudomux_protocol::*;
use pseudomux_service::response::strip_prompt_echo;
use serde::Serialize;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::Instant;
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use uuid::Uuid;

use crate::client::DaemonClient;

#[derive(Debug, Serialize)]
struct ToolErrorBody {
    error: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
}

#[derive(Debug)]
struct ToolError(ToolErrorBody);

impl ToolError {
    fn new(code: &str, message: impl Into<String>, session: Option<SessionId>) -> Self {
        Self(ToolErrorBody {
            error: code.to_string(),
            message: message.into(),
            session_id: session.map(|id| id.to_string()),
            prompt_text: None,
            exit_code: None,
        })
    }

    fn confirmation(message: impl Into<String>, prompt_text: String, session: SessionId) -> Self {
        Self(ToolErrorBody {
            error: "confirmation_required".to_string(),
            message: message.into(),
            session_id: Some(session.to_string()),
            prompt_text: Some(prompt_text),
            exit_code: None,
        })
    }

    fn agent_exited(exit_code: Option<i32>, session: SessionId) -> Self {
        Self(ToolErrorBody {
            error: "agent_exited".to_string(),
            message: format!("agent process exited (code: {exit_code:?})"),
            session_id: Some(session.to_string()),
            prompt_text: None,
            exit_code,
        })
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match serde_json::to_string(&self.0) {
            Ok(body) => f.write_str(&body),
            Err(_) => f.write_str(&self.0.message),
        }
    }
}

impl std::error::Error for ToolError {}

#[derive(Clone, Debug, Serialize)]
struct ToolCall {
    name: Option<String>,
    duration_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
struct PromptResult {
    session_id: String,
    text: String,
    duration_ms: u64,
    state: String,
    tools: Vec<ToolCall>,
}

/// Return tool definitions for MCP tools/list.
pub fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "pseudomux_run",
            "description": "Start a session, wait until ready, send a prompt, wait for the response, and optionally keep the session alive.",
            "inputSchema": start_schema(json!({
                "text": { "type": "string", "description": "Prompt text to send" },
                "timeout_secs": { "type": "integer", "description": "Timeout in seconds (default 120)" },
                "keep_alive": { "type": "boolean", "description": "Keep the session alive after the turn (default false)" }
            }), vec!["text"])
        }),
        json!({
            "name": "pseudomux_start_session",
            "description": "Start a new TUI agent session (claude-code, opencode, shell, or custom).",
            "inputSchema": start_schema(json!({}), Vec::<&str>::new())
        }),
        json!({
            "name": "pseudomux_prompt",
            "description": "Send a prompt to a session and wait for the agent response.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session_id": { "type": "string", "description": "Session UUID or unambiguous UUID prefix" },
                    "text": { "type": "string", "description": "Prompt text to send" },
                    "timeout_secs": { "type": "integer", "description": "Timeout in seconds (default 120)" }
                },
                "required": ["session_id", "text"]
            }
        }),
        json!({
            "name": "pseudomux_get_state",
            "description": "Get the current agent state (Ready, Thinking, ToolRunning, etc.).",
            "inputSchema": session_schema(json!({}), Vec::<&str>::new())
        }),
        json!({
            "name": "pseudomux_get_content",
            "description": "Read raw or filtered content from a session.",
            "inputSchema": session_schema(json!({
                "since_last_input": { "type": "boolean", "description": "Only content since last prompt/input (default true)" },
                "since_seq": { "type": "integer", "description": "Read content since this sequence" },
                "raw": { "type": "boolean", "description": "Return raw content entries instead of filtered text" },
                "response": { "type": "boolean", "description": "Return row-aware filtered assistant response since last input" }
            }), Vec::<&str>::new())
        }),
        json!({
            "name": "pseudomux_list_sessions",
            "description": "List all active pseudomux sessions.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "pseudomux_stop_session",
            "description": "Stop/terminate a session.",
            "inputSchema": session_schema(json!({}), Vec::<&str>::new())
        }),
        json!({
            "name": "pseudomux_send_text",
            "description": "Send text to a session without submitting it.",
            "inputSchema": session_schema(json!({
                "text": { "type": "string", "description": "Text to send" }
            }), vec!["text"])
        }),
        json!({
            "name": "pseudomux_send_key",
            "description": "Send a key to a session, such as Enter, Tab, Escape, Ctrl-c, Up, or F1.",
            "inputSchema": session_schema(json!({
                "key": { "type": "string", "description": "Key name" }
            }), vec!["key"])
        }),
        json!({
            "name": "pseudomux_input_action",
            "description": "Send a named action to a session (submit, interrupt, hard_interrupt, etc.).",
            "inputSchema": session_schema(json!({
                "action": { "type": "string", "description": "Action name" }
            }), vec!["action"])
        }),
        json!({
            "name": "pseudomux_interrupt",
            "description": "Send SIGINT/Ctrl-c to a session.",
            "inputSchema": session_schema(json!({}), Vec::<&str>::new())
        }),
        json!({
            "name": "pseudomux_resize",
            "description": "Resize a session terminal.",
            "inputSchema": session_schema(json!({
                "rows": { "type": "integer", "description": "Terminal rows" },
                "cols": { "type": "integer", "description": "Terminal columns" }
            }), vec!["rows", "cols"])
        }),
        json!({
            "name": "pseudomux_screen_text",
            "description": "Read VTE screen content from a session.",
            "inputSchema": session_schema(json!({
                "status_only": { "type": "boolean", "description": "Only return status bar text" }
            }), Vec::<&str>::new())
        }),
        json!({
            "name": "pseudomux_terminal_state",
            "description": "Get terminal keyboard/capability negotiation state.",
            "inputSchema": session_schema(json!({}), Vec::<&str>::new())
        }),
        json!({
            "name": "pseudomux_content_seq",
            "description": "Get the current content buffer sequence number.",
            "inputSchema": session_schema(json!({}), Vec::<&str>::new())
        }),
        json!({
            "name": "pseudomux_watch_events",
            "description": "Collect watch events for a bounded period and return them as an array.",
            "inputSchema": session_schema(json!({
                "timeout_ms": { "type": "integer", "description": "Maximum collection time in milliseconds (default 30000)" },
                "max_events": { "type": "integer", "description": "Maximum events to collect (default 50)" }
            }), Vec::<&str>::new())
        }),
        json!({
            "name": "pseudomux_events",
            "description": "Collect semantic events for a bounded period and return them as an array.",
            "inputSchema": session_schema(json!({
                "timeout_ms": { "type": "integer", "description": "Maximum collection time in milliseconds (default 30000)" },
                "max_events": { "type": "integer", "description": "Maximum events to collect (default 50)" }
            }), Vec::<&str>::new())
        }),
        json!({
            "name": "pseudomux_confirm",
            "description": "Respond to a confirmation/permission prompt (accept or reject).",
            "inputSchema": session_schema(json!({
                "accept": { "type": "boolean", "description": "true to accept, false to reject" }
            }), vec!["accept"])
        }),
    ]
}

/// Dispatch a tool call and return the result as a JSON string.
pub async fn handle_tool(client: &DaemonClient, name: &str, args: &Value) -> Result<String> {
    match name {
        "pseudomux_run" => run(client, args).await,
        "pseudomux_start_session" => start_session(client, args).await,
        "pseudomux_prompt" => prompt(client, args).await,
        "pseudomux_get_state" => get_state(client, args).await,
        "pseudomux_get_content" => get_content(client, args).await,
        "pseudomux_list_sessions" => list_sessions(client).await,
        "pseudomux_stop_session" => stop_session(client, args).await,
        "pseudomux_send_text" => send_text(client, args).await,
        "pseudomux_send_key" => send_key(client, args).await,
        "pseudomux_input_action" => input_action(client, args).await,
        "pseudomux_interrupt" => interrupt(client, args).await,
        "pseudomux_resize" => resize(client, args).await,
        "pseudomux_screen_text" => screen_text(client, args).await,
        "pseudomux_terminal_state" => terminal_state(client, args).await,
        "pseudomux_content_seq" => content_seq(client, args).await,
        "pseudomux_watch_events" => collect_events(client, args, EventStream::Watch).await,
        "pseudomux_events" => collect_events(client, args, EventStream::Semantic).await,
        "pseudomux_confirm" => confirm(client, args).await,
        _ => Err(anyhow!("unknown tool: {name}")),
    }
}

fn start_schema(extra: Value, extra_required: Vec<&str>) -> Value {
    let mut properties = json!({
        "agent": { "type": "string", "description": "Agent type: claude-code, opencode, shell, or custom program" },
        "cwd": { "type": "string", "description": "Working directory" },
        "profile": { "type": "string", "description": "TOML profile name" },
        "name": { "type": "string", "description": "Human-readable session name" },
        "rows": { "type": "integer", "description": "Terminal rows (default 24)" },
        "cols": { "type": "integer", "description": "Terminal cols (default 80)" },
        "args": { "type": "array", "items": { "type": "string" }, "description": "Extra args for the agent" },
        "env": {
            "description": "Environment overrides as an object or array of [key, value] pairs",
            "oneOf": [
                { "type": "object", "additionalProperties": { "type": "string" } },
                { "type": "array" }
            ]
        },
        "logging_mode": { "type": "string", "description": "Logging mode" },
        "record_path": { "type": "string", "description": "Record raw PTY bytes to this file" },
        "model": { "type": "string", "description": "Claude Code model" },
        "permission_mode": { "type": "string", "description": "Claude Code permission mode" },
        "allowed_tools": { "type": "string", "description": "Claude Code allowed tools" },
        "disallowed_tools": { "type": "string", "description": "Claude Code disallowed tools" },
        "system_prompt": { "type": "string", "description": "Claude Code system prompt override" },
        "append_system_prompt": { "type": "string", "description": "Append to Claude Code default system prompt" },
        "effort": { "type": "string", "description": "Claude Code effort level" },
        "max_budget": { "type": "number", "description": "Claude Code max budget in USD" }
    });
    if let Some(map) = properties.as_object_mut()
        && let Some(extra_map) = extra.as_object()
    {
        for (k, v) in extra_map {
            map.insert(k.clone(), v.clone());
        }
    }
    let mut required = extra_required
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    required.sort();
    json!({
        "type": "object",
        "properties": properties,
        "required": required
    })
}

fn session_schema(extra: Value, extra_required: Vec<&str>) -> Value {
    let mut properties = json!({
        "session_id": { "type": "string", "description": "Session UUID or unambiguous UUID prefix" }
    });
    if let Some(map) = properties.as_object_mut()
        && let Some(extra_map) = extra.as_object()
    {
        for (k, v) in extra_map {
            map.insert(k.clone(), v.clone());
        }
    }
    let mut required = vec!["session_id".to_string()];
    required.extend(extra_required.into_iter().map(str::to_string));
    required.sort();
    json!({
        "type": "object",
        "properties": properties,
        "required": required
    })
}

async fn run(client: &DaemonClient, args: &Value) -> Result<String> {
    let text = required_string(args, "text")?;
    let timeout_secs = optional_u64(args, "timeout_secs").unwrap_or(120);
    let keep_alive = args["keep_alive"].as_bool().unwrap_or(false);
    let params = build_start_params(args, Some("claude-code"));

    let session = start_session_inner(client, params).await?;
    if let Err(err) = wait_for_ready(client, session, timeout_secs).await {
        let _ = client
            .send(Request::Terminate(TerminateParams { session }))
            .await;
        return Err(err);
    }

    let result = execute_prompt(client, session, text, timeout_secs).await;

    if !keep_alive {
        let _ = client
            .send(Request::Terminate(TerminateParams { session }))
            .await;
    }

    result.map(|result| json_string(&result))
}

async fn start_session(client: &DaemonClient, args: &Value) -> Result<String> {
    let params = build_start_params(args, None);
    let session = start_session_inner(client, params).await?;
    Ok(json!({"session_id": session.to_string(), "status": "started"}).to_string())
}

async fn start_session_inner(
    client: &DaemonClient,
    params: StartSessionParams,
) -> Result<SessionId> {
    let resp = client.send(Request::StartSession(params)).await?;
    match resp {
        Response::StartSession { session } => Ok(session),
        Response::Error { code, message } => Err(ToolError::new(&code, message, None).into()),
        other => {
            Err(ToolError::new("transport", format!("unexpected response: {other:?}"), None).into())
        }
    }
}

async fn prompt(client: &DaemonClient, args: &Value) -> Result<String> {
    let session = resolve_session(client, args).await?;
    let text = required_string(args, "text")?;
    let timeout_secs = optional_u64(args, "timeout_secs").unwrap_or(120);
    let result = execute_prompt(client, session, text, timeout_secs).await?;
    Ok(json_string(&result))
}

async fn execute_prompt(
    client: &DaemonClient,
    session: SessionId,
    text: String,
    timeout_secs: u64,
) -> Result<PromptResult> {
    let timeout_ms = timeout_secs.saturating_mul(1000);
    let prompt_text = text.clone();

    let watch_request = Request::SubscribeWatchEvents(SubscribeEventsParams {
        session,
        timeout_ms,
        max_events: 0,
    });
    let watch_payload = serde_json::to_vec(&watch_request)
        .map_err(|e| ToolError::new("transport", format!("serde: {e}"), Some(session)))?;
    let watch_stream = client
        .connect_stream()
        .await
        .map_err(|e| ToolError::new("transport", e.to_string(), Some(session)))?;
    let mut watch_framed = Framed::new(watch_stream, LengthDelimitedCodec::new());
    watch_framed
        .send(Bytes::from(watch_payload))
        .await
        .map_err(|e| ToolError::new("transport", e.to_string(), Some(session)))?;

    let send_resp = client
        .send(Request::SendPrompt(SendPromptParams { session, text }))
        .await
        .map_err(|e| ToolError::new("transport", e.to_string(), Some(session)))?;
    expect_ack(send_resp, session)?;

    let start = Instant::now();
    let deadline = Duration::from_millis(timeout_ms);
    let mut saw_thinking = false;
    let mut turn_duration_ms: Option<u64> = None;
    let mut tools: Vec<ToolCall> = Vec::new();
    let mut active_tool_start: Option<Instant> = None;

    loop {
        let remaining = deadline
            .checked_sub(start.elapsed())
            .unwrap_or(Duration::ZERO);
        if remaining.is_zero() {
            return Err(ToolError::new(
                "timeout",
                format!("timeout waiting for agent response ({timeout_secs}s)"),
                Some(session),
            )
            .into());
        }

        let frame = match tokio::time::timeout(remaining, watch_framed.next()).await {
            Ok(Some(Ok(bytes))) => bytes,
            Ok(Some(Err(e))) => {
                return Err(ToolError::new(
                    "transport",
                    format!("stream error: {e}"),
                    Some(session),
                )
                .into());
            }
            Ok(None) => {
                return Err(ToolError::new(
                    "transport",
                    "watch stream ended without completion event",
                    Some(session),
                )
                .into());
            }
            Err(_) => {
                return Err(ToolError::new(
                    "timeout",
                    format!("timeout waiting for agent response ({timeout_secs}s)"),
                    Some(session),
                )
                .into());
            }
        };

        let response: Response = serde_json::from_slice(&frame)
            .map_err(|e| ToolError::new("transport", format!("serde: {e}"), Some(session)))?;
        match response {
            Response::WatchEvent { ref event } => {
                if let Some(ts) = event.get("ToolStarted") {
                    let name = ts.get("name").and_then(Value::as_str).map(str::to_string);
                    tools.push(ToolCall {
                        name,
                        duration_ms: None,
                    });
                    active_tool_start = Some(Instant::now());
                }
                if event.get("ToolFinished").is_some()
                    && let Some(start_inst) = active_tool_start.take()
                    && let Some(last) = tools.last_mut()
                {
                    last.duration_ms = Some(start_inst.elapsed().as_millis() as u64);
                }

                if let Some(tc) = event.get("TurnComplete") {
                    turn_duration_ms = tc.get("duration_ms").and_then(Value::as_u64);
                    break;
                }
                if let Some(sc) = event.get("StateChange")
                    && let Some(to) = sc.get("to").and_then(Value::as_str)
                {
                    if to == "Thinking" {
                        saw_thinking = true;
                    }
                    if to == "Ready" && saw_thinking {
                        break;
                    }
                }
                if let Some(ir) = event.get("InputRequired") {
                    let kind = ir.get("kind").and_then(Value::as_str).unwrap_or("unknown");
                    let prompt_text = ir
                        .get("prompt_text")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string();
                    if kind == "auth" {
                        return Err(ToolError::new(
                            "auth_required",
                            format!("agent requires authentication: {prompt_text}"),
                            Some(session),
                        )
                        .into());
                    }
                    return Err(ToolError::confirmation(
                        format!("agent requires confirmation: {prompt_text}"),
                        prompt_text,
                        session,
                    )
                    .into());
                }
                if let Some(exit) = event.get("SessionExited") {
                    let code = exit
                        .get("exit_code")
                        .and_then(Value::as_i64)
                        .map(|c| c as i32);
                    return Err(ToolError::agent_exited(code, session).into());
                }
            }
            Response::Ack => {
                return Err(ToolError::new(
                    "timeout",
                    format!("agent did not complete within {timeout_secs}s"),
                    Some(session),
                )
                .into());
            }
            Response::Error { code, message } => {
                return Err(ToolError::new(&code, message, Some(session)).into());
            }
            _ => {}
        }
    }

    let effective_duration = turn_duration_ms.unwrap_or(start.elapsed().as_millis() as u64);
    let content_resp = client
        .send(Request::GetFilteredResponseSinceLastInput(
            SessionStateParams { session },
        ))
        .await
        .map_err(|e| ToolError::new("transport", e.to_string(), Some(session)))?;
    let text = match content_resp {
        Response::FilteredContent { text, .. } => text,
        Response::Error { code, message } => {
            return Err(ToolError::new(&code, message, Some(session)).into());
        }
        other => {
            return Err(ToolError::new(
                "transport",
                format!("unexpected response: {other:?}"),
                Some(session),
            )
            .into());
        }
    };

    Ok(PromptResult {
        session_id: session.to_string(),
        text: strip_prompt_echo(text, &prompt_text),
        duration_ms: effective_duration,
        state: "Ready".to_string(),
        tools,
    })
}

async fn wait_for_ready(
    client: &DaemonClient,
    session: SessionId,
    timeout_secs: u64,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        let resp = client
            .send(Request::GetAgentState(SessionStateParams { session }))
            .await
            .map_err(|e| ToolError::new("transport", e.to_string(), Some(session)))?;
        match resp {
            Response::AgentState { state } if state == "Ready" => return Ok(()),
            Response::AgentState { .. } => {}
            Response::Error { code, message } => {
                return Err(ToolError::new(&code, message, Some(session)).into());
            }
            other => {
                return Err(ToolError::new(
                    "transport",
                    format!("unexpected response: {other:?}"),
                    Some(session),
                )
                .into());
            }
        }
        if Instant::now() >= deadline {
            return Err(ToolError::new(
                "timeout",
                format!("timeout waiting for agent to reach Ready ({timeout_secs}s)"),
                Some(session),
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn get_state(client: &DaemonClient, args: &Value) -> Result<String> {
    let session = resolve_session(client, args).await?;
    let resp = client
        .send(Request::GetAgentState(SessionStateParams { session }))
        .await?;
    match resp {
        Response::AgentState { state } => Ok(json!({"state": state}).to_string()),
        Response::Error { code, message } => {
            Err(ToolError::new(&code, message, Some(session)).into())
        }
        other => Err(anyhow!("unexpected response: {other:?}")),
    }
}

async fn get_content(client: &DaemonClient, args: &Value) -> Result<String> {
    let session = resolve_session(client, args).await?;
    let raw = args["raw"].as_bool().unwrap_or(false);
    let response_only = args["response"].as_bool().unwrap_or(false);
    let since_last = args["since_last_input"].as_bool().unwrap_or(true);
    let since_seq = optional_u64(args, "since_seq");

    if response_only {
        let resp = client
            .send(Request::GetFilteredResponseSinceLastInput(
                SessionStateParams { session },
            ))
            .await?;
        return match resp {
            Response::FilteredContent { text, next_seq } => {
                Ok(json!({"text": text, "next_seq": next_seq}).to_string())
            }
            Response::Error { code, message } => {
                Err(ToolError::new(&code, message, Some(session)).into())
            }
            other => Err(anyhow!("unexpected response: {other:?}")),
        };
    }

    if raw {
        let request = match since_seq {
            Some(seq) => Request::GetContentSince(ContentSinceParams { session, seq }),
            None if since_last => Request::GetContentSinceLastInput(SessionStateParams { session }),
            None => Request::GetContentSince(ContentSinceParams { session, seq: 0 }),
        };
        let resp = client.send(request).await?;
        return match resp {
            Response::Content { entries, next_seq } => {
                Ok(json!({"entries": entries, "next_seq": next_seq}).to_string())
            }
            Response::Error { code, message } => {
                Err(ToolError::new(&code, message, Some(session)).into())
            }
            other => Err(anyhow!("unexpected response: {other:?}")),
        };
    }

    let request = match since_seq {
        Some(seq) => Request::GetFilteredContent(ContentSinceParams { session, seq }),
        None if since_last => {
            Request::GetFilteredContentSinceLastInput(SessionStateParams { session })
        }
        None => Request::GetFilteredContent(ContentSinceParams { session, seq: 0 }),
    };
    let resp = client.send(request).await?;
    match resp {
        Response::FilteredContent { text, next_seq } => {
            Ok(json!({"text": text, "next_seq": next_seq}).to_string())
        }
        Response::Error { code, message } => {
            Err(ToolError::new(&code, message, Some(session)).into())
        }
        other => Err(anyhow!("unexpected response: {other:?}")),
    }
}

async fn list_sessions(client: &DaemonClient) -> Result<String> {
    let resp = client.send(Request::ListSessions).await?;
    match resp {
        Response::Sessions { sessions } => {
            let list: Vec<Value> = sessions
                .iter()
                .map(|s| {
                    json!({
                        "session_id": s.session.to_string(),
                        "name": s.name,
                        "agent": s.agent,
                        "status": s.status,
                        "rows": s.rows,
                        "cols": s.cols,
                        "cwd": s.cwd,
                        "profile": s.profile,
                    })
                })
                .collect();
            Ok(json!({"sessions": list}).to_string())
        }
        Response::Error { code, message } => Err(ToolError::new(&code, message, None).into()),
        other => Err(anyhow!("unexpected response: {other:?}")),
    }
}

async fn stop_session(client: &DaemonClient, args: &Value) -> Result<String> {
    let session = resolve_session(client, args).await?;
    let resp = client
        .send(Request::Terminate(TerminateParams { session }))
        .await?;
    ack_result(resp, session)
}

async fn send_text(client: &DaemonClient, args: &Value) -> Result<String> {
    let session = resolve_session(client, args).await?;
    let text = required_string(args, "text")?;
    let resp = client
        .send(Request::SendText(SendTextParams { session, text }))
        .await?;
    ack_result(resp, session)
}

async fn send_key(client: &DaemonClient, args: &Value) -> Result<String> {
    let session = resolve_session(client, args).await?;
    let key = required_string(args, "key")?;
    let resp = client
        .send(Request::SendKey(SendKeyParams { session, key }))
        .await?;
    ack_result(resp, session)
}

async fn input_action(client: &DaemonClient, args: &Value) -> Result<String> {
    let session = resolve_session(client, args).await?;
    let action = required_string(args, "action")?;
    let resp = client
        .send(Request::SendAction(SendActionParams { session, action }))
        .await?;
    ack_result(resp, session)
}

async fn interrupt(client: &DaemonClient, args: &Value) -> Result<String> {
    let session = resolve_session(client, args).await?;
    let resp = client
        .send(Request::Interrupt(InterruptParams { session }))
        .await?;
    ack_result(resp, session)
}

async fn resize(client: &DaemonClient, args: &Value) -> Result<String> {
    let session = resolve_session(client, args).await?;
    let rows = required_u16(args, "rows")?;
    let cols = required_u16(args, "cols")?;
    let resp = client
        .send(Request::Resize(ResizeParams {
            session,
            rows,
            cols,
        }))
        .await?;
    ack_result(resp, session)
}

async fn screen_text(client: &DaemonClient, args: &Value) -> Result<String> {
    let session = resolve_session(client, args).await?;
    let status_only = args["status_only"].as_bool().unwrap_or(false);
    let request = if status_only {
        Request::GetStatusText(SessionStateParams { session })
    } else {
        Request::GetContentText(SessionStateParams { session })
    };
    let resp = client.send(request).await?;
    match resp {
        Response::StatusText { text } | Response::ContentText { text } => {
            Ok(json!({"text": text}).to_string())
        }
        Response::Error { code, message } => {
            Err(ToolError::new(&code, message, Some(session)).into())
        }
        other => Err(anyhow!("unexpected response: {other:?}")),
    }
}

async fn terminal_state(client: &DaemonClient, args: &Value) -> Result<String> {
    let session = resolve_session(client, args).await?;
    let resp = client
        .send(Request::GetTerminalState(SessionStateParams { session }))
        .await?;
    match resp {
        Response::TerminalState {
            keyboard_mode,
            bracketed_paste,
            focus_events,
        } => Ok(json!({
            "keyboard_mode": keyboard_mode,
            "bracketed_paste": bracketed_paste,
            "focus_events": focus_events
        })
        .to_string()),
        Response::Error { code, message } => {
            Err(ToolError::new(&code, message, Some(session)).into())
        }
        other => Err(anyhow!("unexpected response: {other:?}")),
    }
}

async fn content_seq(client: &DaemonClient, args: &Value) -> Result<String> {
    let session = resolve_session(client, args).await?;
    let resp = client
        .send(Request::GetContentCurrentSeq(SessionStateParams {
            session,
        }))
        .await?;
    match resp {
        Response::ContentSeq { seq } => Ok(json!({"seq": seq}).to_string()),
        Response::Error { code, message } => {
            Err(ToolError::new(&code, message, Some(session)).into())
        }
        other => Err(anyhow!("unexpected response: {other:?}")),
    }
}

#[derive(Clone, Copy)]
enum EventStream {
    Watch,
    Semantic,
}

async fn collect_events(
    client: &DaemonClient,
    args: &Value,
    stream_kind: EventStream,
) -> Result<String> {
    let session = resolve_session(client, args).await?;
    let timeout_ms = optional_u64(args, "timeout_ms").unwrap_or(30_000);
    let max_events = optional_u64(args, "max_events").unwrap_or(50).min(1000);
    let params = SubscribeEventsParams {
        session,
        timeout_ms,
        max_events,
    };
    let request = match stream_kind {
        EventStream::Watch => Request::SubscribeWatchEvents(params),
        EventStream::Semantic => Request::SubscribeEvents(params),
    };
    let payload = serde_json::to_vec(&request)?;
    let stream = client.connect_stream().await?;
    let mut framed = Framed::new(stream, LengthDelimitedCodec::new());
    framed.send(Bytes::from(payload)).await?;

    let mut events = Vec::new();
    let deadline = Duration::from_millis(timeout_ms.saturating_add(500));
    let start = Instant::now();
    loop {
        if max_events > 0 && events.len() as u64 >= max_events {
            break;
        }
        let remaining = deadline
            .checked_sub(start.elapsed())
            .unwrap_or(Duration::ZERO);
        if remaining.is_zero() {
            break;
        }
        let frame = match tokio::time::timeout(remaining, framed.next()).await {
            Ok(Some(Ok(bytes))) => bytes,
            Ok(Some(Err(e))) => return Err(anyhow!("stream error: {e}")),
            Ok(None) | Err(_) => break,
        };
        let response: Response = serde_json::from_slice(&frame)?;
        match response {
            Response::WatchEvent { event } | Response::SemanticEvent { event } => {
                events.push(event);
            }
            Response::Ack => break,
            Response::Error { code, message } => {
                return Err(ToolError::new(&code, message, Some(session)).into());
            }
            _ => {}
        }
    }
    Ok(json!({"events": events}).to_string())
}

async fn confirm(client: &DaemonClient, args: &Value) -> Result<String> {
    let session = resolve_session(client, args).await?;
    let accept = args["accept"]
        .as_bool()
        .ok_or_else(|| anyhow!("accept required"))?;
    let action = if accept { "confirm_yes" } else { "confirm_no" }.to_string();
    let resp = client
        .send(Request::SendAction(SendActionParams { session, action }))
        .await?;
    match resp {
        Response::Ack => Ok(
            json!({"success": true, "action": if accept { "accepted" } else { "rejected" }})
                .to_string(),
        ),
        Response::Error { code, message } => {
            Err(ToolError::new(&code, message, Some(session)).into())
        }
        other => Err(anyhow!("unexpected response: {other:?}")),
    }
}

async fn resolve_session(client: &DaemonClient, args: &Value) -> Result<SessionId> {
    let raw = args["session_id"]
        .as_str()
        .ok_or_else(|| anyhow!("session_id required"))?;
    resolve_session_id(client, raw).await
}

async fn resolve_session_id(client: &DaemonClient, value: &str) -> Result<SessionId> {
    if let Ok(session) = Uuid::parse_str(value) {
        return Ok(session);
    }

    let prefix = value.trim().to_ascii_lowercase();
    if prefix.is_empty() {
        return Err(anyhow!("session_id prefix cannot be empty"));
    }

    let resp = client.send(Request::ListSessions).await?;
    let sessions = match resp {
        Response::Sessions { sessions } => sessions,
        Response::Error { code, message } => {
            return Err(ToolError::new(&code, message, None).into());
        }
        other => {
            return Err(anyhow!(
                "unexpected response resolving session prefix: {other:?}"
            ));
        }
    };
    let matches: Vec<_> = sessions
        .into_iter()
        .map(|summary| summary.session)
        .filter(|session| session.to_string().starts_with(&prefix))
        .collect();

    match matches.as_slice() {
        [session] => Ok(*session),
        [] => Err(anyhow!("no session id starts with prefix {value:?}")),
        many => {
            let ids = many
                .iter()
                .map(SessionId::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            Err(anyhow!("session prefix {value:?} is ambiguous: {ids}"))
        }
    }
}

fn build_start_params(args: &Value, default_agent: Option<&str>) -> StartSessionParams {
    let agent = optional_string(args, "agent").or_else(|| default_agent.map(str::to_string));
    let mut extra_args = Vec::new();
    if is_claude_agent(agent.as_deref()) {
        let opts = ClaudeCodeOpts {
            model: optional_string(args, "model"),
            permission_mode: optional_string(args, "permission_mode"),
            allowed_tools: optional_string(args, "allowed_tools"),
            disallowed_tools: optional_string(args, "disallowed_tools"),
            system_prompt: optional_string(args, "system_prompt"),
            append_system_prompt: optional_string(args, "append_system_prompt"),
            effort: optional_string(args, "effort"),
            max_budget: args["max_budget"].as_f64(),
            settings_json: None,
        };
        extra_args.extend(opts.to_args());
    }
    extra_args.extend(string_array(args, "args"));

    StartSessionParams {
        agent,
        profile: optional_string(args, "profile"),
        args: extra_args,
        env: env_pairs(&args["env"]),
        cwd: optional_string(args, "cwd"),
        rows: optional_u16(args, "rows"),
        cols: optional_u16(args, "cols"),
        logging_mode: optional_string(args, "logging_mode"),
        record_path: optional_string(args, "record_path"),
        name: optional_string(args, "name"),
    }
}

fn is_claude_agent(agent: Option<&str>) -> bool {
    agent
        .map(|agent| {
            matches!(
                agent.to_lowercase().as_str(),
                "claude-code" | "claude" | "claudecode"
            )
        })
        .unwrap_or(false)
}

fn expect_ack(resp: Response, session: SessionId) -> Result<()> {
    match resp {
        Response::Ack => Ok(()),
        Response::Error { code, message } => {
            Err(ToolError::new(&code, message, Some(session)).into())
        }
        other => Err(ToolError::new(
            "transport",
            format!("unexpected response: {other:?}"),
            Some(session),
        )
        .into()),
    }
}

fn ack_result(resp: Response, session: SessionId) -> Result<String> {
    expect_ack(resp, session)?;
    Ok(json!({"success": true}).to_string())
}

fn required_string(args: &Value, key: &str) -> Result<String> {
    args[key]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("{key} required"))
}

fn required_u16(args: &Value, key: &str) -> Result<u16> {
    optional_u16(args, key).ok_or_else(|| anyhow!("{key} required"))
}

fn optional_string(args: &Value, key: &str) -> Option<String> {
    args[key]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn optional_u64(args: &Value, key: &str) -> Option<u64> {
    args[key].as_u64()
}

fn optional_u16(args: &Value, key: &str) -> Option<u16> {
    args[key]
        .as_u64()
        .and_then(|value| u16::try_from(value).ok())
}

fn string_array(args: &Value, key: &str) -> Vec<String> {
    args[key]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn env_pairs(value: &Value) -> Vec<(String, String)> {
    if let Some(map) = value.as_object() {
        return map
            .iter()
            .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.to_string())))
            .collect();
    }
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    if let Some(pair) = item.as_array()
                        && pair.len() == 2
                    {
                        return Some((
                            pair[0].as_str()?.to_string(),
                            pair[1].as_str()?.to_string(),
                        ));
                    }
                    let key = item.get("key")?.as_str()?.to_string();
                    let value = item.get("value")?.as_str()?.to_string();
                    Some((key, value))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn json_string<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|e| {
        json!({
            "error": "transport",
            "message": format!("failed to serialize result: {e}")
        })
        .to_string()
    })
}
