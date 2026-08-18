//! Loopback Anthropic Messages facade over a pinned minified Claude cell.
//!
//! This is not a clone of api.anthropic.com. Claude Code's tool surface stays
//! denied. The first turn of a conversation is flattened into a primer; later
//! turns type only the new suffix into the same cell (no `/clear`) so
//! Anthropic's prompt cache can hit. Token streaming is reconstructed after
//! the turn commits.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use pseudomux_protocol::v1::{EffortLevel, ErrorBody, StatelessResult};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{info, warn};

use crate::conversation::{ConversationBook, LeaseTurn};

const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
const TOOL_CALL_OPEN: &str = "<tool_call>";
const TOOL_CALL_CLOSE: &str = "</tool_call>";

/// Parse `--path-b-messages-bind`. Loopback only: this is a local provider
/// surface, not something that should be reachable off-box.
pub fn parse_messages_bind(value: &str) -> Result<SocketAddr> {
    let addr: SocketAddr = value
        .parse()
        .with_context(|| format!("--path-b-messages-bind {value:?} is not HOST:PORT"))?;
    if !addr.ip().is_loopback() {
        bail!(
            "--path-b-messages-bind must be loopback (127.0.0.1 or [::1]), not {}",
            addr.ip()
        );
    }
    Ok(addr)
}

/// Bind the loopback listener before the daemon advertises readiness.
pub async fn bind_messages(bind: SocketAddr) -> Result<TcpListener> {
    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("failed to bind Messages listener on {bind}"))?;
    let actual = listener.local_addr().unwrap_or(bind);
    info!(addr = %actual, "pmuxd Path B Messages listening");
    Ok(listener)
}

/// Accept Path B Messages requests until the task is aborted.
pub async fn serve_messages(listener: TcpListener, book: Arc<ConversationBook>) -> Result<()> {
    let sweeper = {
        let book = Arc::clone(&book);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            loop {
                interval.tick().await;
                book.sweep_expired().await;
            }
        })
    };
    let result = accept_loop(listener, book).await;
    sweeper.abort();
    result
}

async fn accept_loop(listener: TcpListener, book: Arc<ConversationBook>) -> Result<()> {
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                let book = Arc::clone(&book);
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(stream, book.as_ref()).await {
                        warn!(peer = %peer, error = %error, "Path B Messages connection failed");
                    }
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                warn!(error = %error, "Path B Messages accept failed");
            }
        }
    }
}

async fn handle_connection(mut stream: TcpStream, book: &ConversationBook) -> Result<()> {
    let (header_bytes, rest) = read_headers(&mut stream).await?;
    let header_text = String::from_utf8_lossy(&header_bytes);
    let (method, path, headers) = parse_request_line_and_headers(&header_text)?;
    if !has_any_auth(&headers) && method != "OPTIONS" {
        write_http(
            &mut stream,
            401,
            "application/json",
            &json!({"type":"error","error":{"type":"authentication_error","message":"x-api-key or Authorization required"}}).to_string(),
        )
        .await?;
        return Ok(());
    }
    if let Some(conversation_id) = release_conversation_id(&method, &path) {
        if let Err(error) = book.release(&conversation_id).await {
            write_dispatch_error(&mut stream, false, &error).await?;
            return Ok(());
        }
        write_http(
            &mut stream,
            200,
            "application/json",
            &json!({"released": true, "conversation": conversation_id}).to_string(),
        )
        .await?;
        return Ok(());
    }
    if method != "POST" || !is_messages_path(&path) {
        write_http(
            &mut stream,
            404,
            "application/json",
            &json!({"type":"error","error":{"type":"not_found_error","message":"POST /v1/messages or POST /v1/conversations/{id}/release"}}).to_string(),
        )
        .await?;
        return Ok(());
    }
    let content_length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    if content_length == 0 || content_length > MAX_BODY_BYTES {
        write_http(
            &mut stream,
            413,
            "application/json",
            &json!({"type":"error","error":{"type":"invalid_request_error","message":"body length out of range"}}).to_string(),
        )
        .await?;
        return Ok(());
    }
    let mut body = rest;
    while body.len() < content_length {
        let mut buf = vec![0; (content_length - body.len()).min(8192)];
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            bail!("client closed before Content-Length");
        }
        body.extend_from_slice(&buf[..n]);
    }
    body.truncate(content_length);

    let request_json: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => {
            write_http(
                &mut stream,
                400,
                "application/json",
                &json!({"type":"error","error":{"type":"invalid_request_error","message":error.to_string()}}).to_string(),
            )
            .await?;
            return Ok(());
        }
    };

    if let Err(message) = reject_unsupported(&request_json) {
        write_http(
            &mut stream,
            400,
            "application/json",
            &json!({"type":"error","error":{"type":"invalid_request_error","message":message}})
                .to_string(),
        )
        .await?;
        return Ok(());
    }

    let stream_wanted = request_json
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let lease = match book.complete(&headers, &request_json).await {
        Ok(lease) => lease,
        Err(error) => {
            write_dispatch_error(&mut stream, stream_wanted, &error).await?;
            return Ok(());
        }
    };

    let completion = parse_completion(&lease.result.text);
    let message = anthropic_message_from_lease(&lease, &completion);
    let extra = lease_headers(&lease);
    if stream_wanted {
        let sse = encode_sse(&message, &completion);
        write_http_extra(&mut stream, 200, "text/event-stream", &sse, &extra).await?;
    } else {
        write_http_extra(
            &mut stream,
            200,
            "application/json",
            &message.to_string(),
            &extra,
        )
        .await?;
    }
    Ok(())
}

fn release_conversation_id(method: &str, path: &str) -> Option<String> {
    if method != "POST" && method != "DELETE" {
        return None;
    }
    let path = path.split('?').next().unwrap_or(path);
    let prefixes = ["/v1/conversations/", "/v1/v1/conversations/"];
    for prefix in prefixes {
        if let Some(rest) = path.strip_prefix(prefix) {
            if let Some(id) = rest.strip_suffix("/release") {
                if !id.is_empty() && !id.contains('/') {
                    return Some(id.to_owned());
                }
            }
        }
    }
    None
}

fn lease_headers(lease: &LeaseTurn) -> Vec<(String, String)> {
    vec![
        (
            "x-pmux-conversation".to_owned(),
            lease.conversation_id.clone(),
        ),
        ("x-pmux-cell".to_owned(), lease.cell.clone()),
        ("x-pmux-lease".to_owned(), lease.kind.as_str().to_owned()),
        (
            "x-pmux-idle-ttl-ms".to_owned(),
            lease.idle_ttl_ms.to_string(),
        ),
    ]
}

fn anthropic_message_from_lease(lease: &LeaseTurn, completion: &ParsedCompletion) -> Value {
    anthropic_message(&lease.model, &lease.result, completion)
}

/// Split `claude-sonnet-5-xhigh` into (`claude-sonnet-5`, Some(XHigh)).
/// Longer suffixes are tried first so `-xhigh` is not read as `-high`.
pub fn split_model_and_effort(model: &str) -> (String, Option<EffortLevel>) {
    const SUFFIXES: &[(&str, EffortLevel)] = &[
        ("-xhigh", EffortLevel::XHigh),
        ("-medium", EffortLevel::Medium),
        ("-high", EffortLevel::High),
        ("-low", EffortLevel::Low),
        ("-max", EffortLevel::Max),
    ];
    for (suffix, level) in SUFFIXES {
        if let Some(stem) = model.strip_suffix(suffix) {
            if !stem.is_empty() {
                return (stem.to_owned(), Some(*level));
            }
        }
    }
    (model.to_owned(), None)
}

fn reject_unsupported(body: &Value) -> Result<(), String> {
    if walk_for_type(body, "image") {
        return Err("image content is not supported on the stateless token engine".to_owned());
    }
    Ok(())
}

fn walk_for_type(value: &Value, type_name: &str) -> bool {
    match value {
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) == Some(type_name) {
                return true;
            }
            object.values().any(|child| walk_for_type(child, type_name))
        }
        Value::Array(items) => items.iter().any(|child| walk_for_type(child, type_name)),
        _ => false,
    }
}

pub fn flatten_prompt(body: &Value) -> Result<String, String> {
    let mut out = String::new();
    let system = system_text(body.get("system"));
    if !system.is_empty() {
        out.push_str("SYSTEM:\n");
        out.push_str(&system);
        out.push_str("\n\n");
    }
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        if !tools.is_empty() {
            out.push_str("TOOLS:\nYou may call tools by emitting one or more blocks of the form\n");
            out.push_str(TOOL_CALL_OPEN);
            out.push_str(r#"{"name":"TOOL_NAME","id":"toolu_...","input":{}}"#);
            out.push_str(TOOL_CALL_CLOSE);
            out.push_str(
                "\nDo not execute anything. The consumer will run the tool and send a tool_result.\nIf you can answer without a tool, answer in plain text and emit no tool_call blocks.\nAvailable tools (JSON Schema):\n",
            );
            out.push_str(&serde_json::to_string_pretty(tools).unwrap_or_else(|_| "[]".to_owned()));
            out.push_str("\n\n");
        }
    }
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| "messages is required".to_owned())?;
    out.push_str("HISTORY:\n");
    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        let rendered = render_content(message.get("content"));
        out.push_str(match role {
            "assistant" => "ASSISTANT:\n",
            "system" => "SYSTEM:\n",
            _ => "USER:\n",
        });
        out.push_str(&rendered);
        out.push_str("\n\n");
    }
    out.push_str(
        "Continue as the assistant. Either answer in plain text or emit tool_call blocks.\n",
    );
    Ok(out)
}

/// Path B's composer refuses tabs and other control characters anywhere in
/// the prompt (Claude rewrites a tab to four spaces and then the row no
/// longer matches). Coding-agent history is full of tabs; rewrite them here
/// rather than failing the turn after HTTP accept.
pub(crate) fn sanitize_prompt(prompt: &str) -> String {
    prompt
        .chars()
        .filter_map(|character| match character {
            '\t' => Some("    ".to_owned()),
            '\n' => Some("\n".to_owned()),
            other if other.is_control() => None,
            other => Some(other.to_string()),
        })
        .collect()
}

pub(crate) fn system_text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| {
                if block.get("type").and_then(Value::as_str) == Some("text") {
                    block.get("text").and_then(Value::as_str).map(str::to_owned)
                } else if let Some(text) = block.as_str() {
                    Some(text.to_owned())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => other.as_str().unwrap_or("").to_owned(),
    }
}

pub(crate) fn render_content(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .map(render_block)
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => other.to_string(),
    }
}

fn render_block(block: &Value) -> String {
    match block.get("type").and_then(Value::as_str) {
        Some("text") => block
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        Some("tool_use") => format!(
            "[tool_use id={} name={}]: {}",
            block.get("id").and_then(Value::as_str).unwrap_or("?"),
            block.get("name").and_then(Value::as_str).unwrap_or("?"),
            block
                .get("input")
                .map(|input| input.to_string())
                .unwrap_or_else(|| "{}".to_owned())
        ),
        Some("tool_result") => {
            let content = match block.get("content") {
                Some(Value::String(text)) => text.clone(),
                Some(Value::Array(items)) => items
                    .iter()
                    .filter_map(|item| item.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n"),
                Some(other) => other.to_string(),
                None => String::new(),
            };
            format!(
                "[tool_result tool_use_id={}{}]: {}",
                block
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .unwrap_or("?"),
                if block.get("is_error").and_then(Value::as_bool) == Some(true) {
                    " is_error=true"
                } else {
                    ""
                },
                content
            )
        }
        Some("thinking") => block
            .get("thinking")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        _ => block
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_default(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedCompletion {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
}

pub fn parse_completion(text: &str) -> ParsedCompletion {
    let mut tool_calls = Vec::new();
    let mut remaining = String::new();
    let mut cursor = text;
    while let Some(start) = cursor.find(TOOL_CALL_OPEN) {
        remaining.push_str(&cursor[..start]);
        let after = &cursor[start + TOOL_CALL_OPEN.len()..];
        let Some(end) = after.find(TOOL_CALL_CLOSE) else {
            remaining.push_str(TOOL_CALL_OPEN);
            remaining.push_str(after);
            cursor = "";
            break;
        };
        let payload = after[..end].trim();
        let raw_block = &cursor[start..start + TOOL_CALL_OPEN.len() + end + TOOL_CALL_CLOSE.len()];
        cursor = &after[end + TOOL_CALL_CLOSE.len()..];
        match serde_json::from_str::<Value>(payload) {
            Ok(value) => {
                let name = value
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                if name.is_empty() {
                    remaining.push_str(raw_block);
                    continue;
                }
                let id = value
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .filter(|id| !id.is_empty())
                    .unwrap_or_else(|| format!("toolu_{}", tool_calls.len() + 1));
                let input = value.get("input").cloned().unwrap_or_else(|| json!({}));
                tool_calls.push(ToolCall { id, name, input });
            }
            Err(_) => remaining.push_str(raw_block),
        }
    }
    remaining.push_str(cursor);
    ParsedCompletion {
        text: remaining.trim().to_owned(),
        tool_calls,
    }
}

fn anthropic_message(
    model: &str,
    result: &StatelessResult,
    completion: &ParsedCompletion,
) -> Value {
    let mut content = Vec::new();
    for call in &completion.tool_calls {
        content.push(json!({
            "type": "tool_use",
            "id": call.id,
            "name": call.name,
            "input": call.input,
        }));
    }
    if !completion.text.is_empty() || content.is_empty() {
        content.push(json!({"type": "text", "text": completion.text}));
    }
    let stop_reason = if !completion.tool_calls.is_empty() {
        "tool_use"
    } else {
        match result.stop_reason.as_ref().map(|reason| reason.kind) {
            Some(pseudomux_protocol::v1::StopReasonKind::MaxTokens) => "max_tokens",
            Some(pseudomux_protocol::v1::StopReasonKind::Refusal) => "refusal",
            _ => "end_turn",
        }
    };
    json!({
        "id": message_id(),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": Value::Null,
        "usage": {
            "input_tokens": result.usage.main.input_tokens,
            "output_tokens": result.usage.main.output_tokens,
            "cache_creation_input_tokens": result.usage.main.cache_creation_input_tokens,
            "cache_read_input_tokens": result.usage.main.cache_read_input_tokens,
        }
    })
}

pub fn encode_sse(message: &Value, completion: &ParsedCompletion) -> String {
    let model = message.get("model").cloned().unwrap_or(json!(""));
    let id = message
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("msg_0")
        .to_owned();
    let usage = message.get("usage").cloned().unwrap_or_else(|| {
        json!({
            "input_tokens": 0,
            "output_tokens": 0,
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 0
        })
    });
    let stop_reason = message
        .get("stop_reason")
        .and_then(Value::as_str)
        .unwrap_or("end_turn");
    let mut events = Vec::new();
    events.push(sse_event(
        "message_start",
        &json!({
            "type": "message_start",
            "message": {
                "id": id,
                "type": "message",
                "role": "assistant",
                "content": [],
                "model": model,
                "stop_reason": Value::Null,
                "stop_sequence": Value::Null,
                "usage": {
                    "input_tokens": usage.get("input_tokens").cloned().unwrap_or(json!(0)),
                    "output_tokens": 1,
                    "cache_creation_input_tokens": usage.get("cache_creation_input_tokens").cloned().unwrap_or(json!(0)),
                    "cache_read_input_tokens": usage.get("cache_read_input_tokens").cloned().unwrap_or(json!(0)),
                }
            }
        }),
    ));
    let mut index = 0usize;
    for call in &completion.tool_calls {
        events.push(sse_event(
            "content_block_start",
            &json!({
                "type": "content_block_start",
                "index": index,
                "content_block": {
                    "type": "tool_use",
                    "id": call.id,
                    "name": call.name,
                    "input": {}
                }
            }),
        ));
        events.push(sse_event(
            "content_block_delta",
            &json!({
                "type": "content_block_delta",
                "index": index,
                "delta": {
                    "type": "input_json_delta",
                    "partial_json": call.input.to_string()
                }
            }),
        ));
        events.push(sse_event(
            "content_block_stop",
            &json!({"type": "content_block_stop", "index": index}),
        ));
        index += 1;
    }
    if !completion.text.is_empty() || completion.tool_calls.is_empty() {
        events.push(sse_event(
            "content_block_start",
            &json!({
                "type": "content_block_start",
                "index": index,
                "content_block": {"type": "text", "text": ""}
            }),
        ));
        events.push(sse_event(
            "content_block_delta",
            &json!({
                "type": "content_block_delta",
                "index": index,
                "delta": {"type": "text_delta", "text": completion.text}
            }),
        ));
        events.push(sse_event(
            "content_block_stop",
            &json!({"type": "content_block_stop", "index": index}),
        ));
    }
    events.push(sse_event(
        "message_delta",
        &json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason, "stop_sequence": Value::Null},
            "usage": {"output_tokens": usage.get("output_tokens").cloned().unwrap_or(json!(0))}
        }),
    ));
    events.push(sse_event("message_stop", &json!({"type": "message_stop"})));
    events.join("")
}

fn sse_event(event: &str, data: &Value) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

fn message_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("msg_{nanos:x}")
}

fn is_messages_path(path: &str) -> bool {
    // Anthropic SDK posts `/v1/messages`. If a client sets baseUrl to
    // `http://host:port/v1`, the joined path becomes `/v1/v1/messages`.
    matches!(
        path.split('?').next().unwrap_or(path),
        "/v1/messages" | "/messages" | "/v1/v1/messages"
    )
}

/// Presence-only. Loopback is the trust boundary: any non-empty
/// `x-api-key` or `Authorization` is accepted. This is not a secret
/// check and must not be described as one. Off-box bind is refused
/// by [`parse_messages_bind`].
fn has_any_auth(headers: &[(String, String)]) -> bool {
    headers.iter().any(|(name, value)| {
        !value.trim().is_empty()
            && (name.eq_ignore_ascii_case("x-api-key")
                || name.eq_ignore_ascii_case("authorization"))
    })
}

async fn read_headers(stream: &mut TcpStream) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            bail!("client closed during headers");
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(index) = find_double_crlf(&buf) {
            let rest = buf.split_off(index + 4);
            return Ok((buf, rest));
        }
        if buf.len() > 64 * 1024 {
            bail!("headers too large");
        }
    }
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_request_line_and_headers(text: &str) -> Result<(String, String, Vec<(String, String)>)> {
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_owned();
    let path = parts.next().unwrap_or("").to_owned();
    if method.is_empty() || path.is_empty() {
        bail!("malformed request line");
    }
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        headers.push((name.trim().to_owned(), value.trim().to_owned()));
    }
    Ok((method, path, headers))
}

async fn write_http(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &str,
) -> Result<()> {
    write_http_extra(stream, status, content_type, body, &[]).await
}

async fn write_http_extra(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &str,
    extra_headers: &[(String, String)],
) -> Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        413 => "Payload Too Large",
        _ => "Error",
    };
    let mut header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close",
        body.len()
    );
    for (name, value) in extra_headers {
        header.push_str(&format!("\r\n{name}: {value}"));
    }
    header.push_str("\r\n\r\n");
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

async fn write_dispatch_error(
    stream: &mut TcpStream,
    stream_wanted: bool,
    error: &ErrorBody,
) -> Result<()> {
    // The turn has not started streaming: a lone `event: error` without
    // message_start/message_stop is not a valid pi stream. Report request
    // failure as JSON regardless of the requested stream flag.
    let _ = stream_wanted;
    write_http(stream, 400, "application/json", &dispatch_error_body(error)).await
}

fn dispatch_error_body(error: &ErrorBody) -> String {
    json!({
        "type": "error",
        "error": {
            "type": "api_error",
            "message": error.message,
            "details": error.details,
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_only() {
        assert!(parse_messages_bind("127.0.0.1:0").is_ok());
        assert!(parse_messages_bind("[::1]:9").is_ok());
        assert!(parse_messages_bind("0.0.0.0:8080").is_err());
        assert!(parse_messages_bind("192.168.1.4:8080").is_err());
    }

    #[test]
    fn auth_is_presence_only() {
        assert!(!has_any_auth(&[]));
        assert!(!has_any_auth(&[("x-api-key".to_owned(), "   ".to_owned())]));
        assert!(has_any_auth(&[(
            "x-api-key".to_owned(),
            "anything".to_owned()
        )]));
        assert!(has_any_auth(&[(
            "Authorization".to_owned(),
            "Bearer x".to_owned()
        )]));
    }

    #[test]
    fn flatten_includes_system_tools_history_and_tool_results() {
        let body = json!({
            "model": "claude-sonnet-5",
            "system": [{"type":"text","text":"Be terse."}],
            "tools": [{
                "name": "bash",
                "description": "run a command",
                "input_schema": {"type":"object","properties":{"command":{"type":"string"}}}
            }],
            "messages": [
                {"role":"user","content":"list files"},
                {"role":"assistant","content":[
                    {"type":"tool_use","id":"toolu_1","name":"bash","input":{"command":"ls"}}
                ]},
                {"role":"user","content":[
                    {"type":"tool_result","tool_use_id":"toolu_1","content":"a.rs\n"}
                ]}
            ]
        });
        let prompt = flatten_prompt(&body).unwrap();
        assert!(prompt.contains("SYSTEM:\nBe terse."));
        assert!(prompt.contains("Available tools"));
        assert!(prompt.contains("bash"));
        assert!(prompt.contains("[tool_use id=toolu_1 name=bash]"));
        assert!(prompt.contains("[tool_result tool_use_id=toolu_1]"));
        assert!(prompt.contains("<tool_call>"));
        assert!(!prompt.contains("\"type\":\"image\""));
    }

    #[test]
    fn parse_tool_call_blocks_and_leftover_text() {
        let parsed = parse_completion(
            "thinking aloud\n<tool_call>{\"name\":\"bash\",\"id\":\"toolu_9\",\"input\":{\"command\":\"pwd\"}}</tool_call>\n",
        );
        assert_eq!(parsed.text, "thinking aloud");
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].id, "toolu_9");
        assert_eq!(parsed.tool_calls[0].name, "bash");
        assert_eq!(parsed.tool_calls[0].input["command"], "pwd");
    }

    #[test]
    fn sse_has_required_pi_events_and_stop_reason() {
        let completion = ParsedCompletion {
            text: "hello".to_owned(),
            tool_calls: Vec::new(),
        };
        let message = json!({
            "id": "msg_test",
            "model": "claude-sonnet-5",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 3, "output_tokens": 1, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}
        });
        let sse = encode_sse(&message, &completion);
        for event in [
            "event: message_start",
            "event: content_block_start",
            "event: content_block_delta",
            "event: content_block_stop",
            "event: message_delta",
            "event: message_stop",
        ] {
            assert!(sse.contains(event), "missing {event} in {sse}");
        }
        assert!(sse.contains("\"stop_reason\":\"end_turn\""));
        assert!(sse.contains("hello"));
    }

    #[test]
    fn tool_use_sse_uses_tool_use_stop_reason() {
        let completion = ParsedCompletion {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "toolu_1".to_owned(),
                name: "bash".to_owned(),
                input: json!({"command":"ls"}),
            }],
        };
        let message = json!({
            "id": "msg_test",
            "model": "claude-sonnet-5",
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 1, "output_tokens": 2, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}
        });
        let sse = encode_sse(&message, &completion);
        assert!(sse.contains("\"type\":\"tool_use\""));
        assert!(sse.contains("\"stop_reason\":\"tool_use\""));
        assert!(sse.contains("input_json_delta"));
    }

    #[test]
    fn images_are_rejected() {
        let body = json!({
            "messages": [{"role":"user","content":[{"type":"image","source":{"type":"base64"}}]}]
        });
        assert!(reject_unsupported(&body).is_err());
    }

    #[test]
    fn malformed_tool_call_stays_in_text() {
        let parsed = parse_completion("before <tool_call>not-json</tool_call> after");
        assert!(parsed.tool_calls.is_empty());
        assert!(parsed.text.contains("<tool_call>not-json</tool_call>"));
        assert!(parsed.text.contains("before"));
        assert!(parsed.text.contains("after"));
    }

    #[test]
    fn model_id_carries_effort_suffix() {
        use EffortLevel::*;
        assert_eq!(
            split_model_and_effort("claude-sonnet-5-xhigh"),
            ("claude-sonnet-5".to_owned(), Some(XHigh))
        );
        assert_eq!(
            split_model_and_effort("claude-fable-5-max"),
            ("claude-fable-5".to_owned(), Some(Max))
        );
        assert_eq!(
            split_model_and_effort("claude-sonnet-5"),
            ("claude-sonnet-5".to_owned(), None)
        );
    }

    #[test]
    fn tabs_become_spaces_before_path_b() {
        let cleaned = sanitize_prompt("fn\tmain() {\n\tlet x = 1;\n}");
        assert!(!cleaned.contains('\t'));
        assert!(cleaned.contains("    main()"));
        assert!(cleaned.contains('\n'));
    }
}
