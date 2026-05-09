use axum::{
    Json, Router,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, StatusCode, header::AUTHORIZATION},
    middleware::{self, Next},
    response::{
        IntoResponse, Response,
        sse::{Event, Sse},
    },
    routing::{delete, get, post},
};
use futures::stream::Stream;
use pseudomux_core::session::state::TerminalSize;
use pseudomux_core::vte::WatchEvent;
use pseudomux_protocol::{SessionId, StartSessionParams};
use pseudomux_service::Service;
use pseudomux_service::response::strip_prompt_echo;
use serde::Deserialize;
use serde_json::json;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::time::{Duration, Instant};
use tracing::{info, warn};
use uuid::Uuid;

use crate::session::start_session;
use crate::util::summarize;

pub async fn run_http_server(
    service: Arc<Service>,
    host: String,
    port: u16,
    token: Option<String>,
) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/health", get(health))
        .route("/run", post(run_oneshot))
        .route("/sessions", post(create_session))
        .route("/sessions", get(list_sessions))
        .route("/sessions/{id}", get(get_session))
        .route("/sessions/{id}", delete(delete_session))
        .route("/sessions/{id}/prompt", post(send_prompt))
        .route("/sessions/{id}/prompt-sync", post(prompt_sync))
        .route("/sessions/{id}/input/text", post(send_text))
        .route("/sessions/{id}/input/key", post(send_key))
        .route("/sessions/{id}/input/action", post(send_action))
        .route("/sessions/{id}/input/enter", post(send_enter))
        .route("/sessions/{id}/state", get(get_agent_state))
        .route("/sessions/{id}/content", get(get_content))
        .route("/sessions/{id}/screen", get(get_screen))
        .route("/sessions/{id}/events", get(get_events))
        .route("/sessions/{id}/watch", get(get_watch))
        .route("/sessions/{id}/resize", post(resize_session))
        .route("/sessions/{id}/interrupt", post(interrupt_session))
        .route("/sessions/{id}/confirm", post(confirm_session))
        .route("/sessions/{id}/terminal-state", get(get_terminal_state))
        .with_state(service);

    let token = token
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty());
    let auth_enabled = token.is_some();
    let app = if let Some(token) = token {
        app.layer(middleware::from_fn_with_state(
            Arc::new(token),
            require_token,
        ))
    } else {
        app
    };

    let host = host.trim();
    if host.is_empty() {
        anyhow::bail!("--http-host cannot be empty");
    }
    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!(addr = %addr, auth = auth_enabled, "HTTP server listening");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn require_token(State(token): State<Arc<String>>, req: Request, next: Next) -> Response {
    if auth_headers_match(req.headers(), token.as_str()) {
        return next.run(req).await;
    }
    error_response(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "missing or invalid HTTP token",
    )
}

fn auth_headers_match(headers: &HeaderMap, token: &str) -> bool {
    let bearer = format!("Bearer {token}");
    let has_bearer = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == bearer);
    let has_pmux_token = headers
        .get("x-pseudomux-token")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == token);
    has_bearer || has_pmux_token
}

fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({ "error": code, "message": message }))).into_response()
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, header::AUTHORIZATION};

    use super::auth_headers_match;

    #[test]
    fn matches_bearer_token() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer secret"));
        assert!(auth_headers_match(&headers, "secret"));
        assert!(!auth_headers_match(&headers, "other"));
    }

    #[test]
    fn matches_pmux_token_header() {
        let mut headers = HeaderMap::new();
        headers.insert("x-pseudomux-token", HeaderValue::from_static("secret"));
        assert!(auth_headers_match(&headers, "secret"));
        assert!(!auth_headers_match(&headers, "other"));
    }
}

fn service_error(e: anyhow::Error) -> Response {
    error_response(StatusCode::NOT_FOUND, "not_found", &e.to_string())
}

fn task_error(e: tokio::task::JoinError) -> Response {
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        &e.to_string(),
    )
}

async fn health() -> impl IntoResponse {
    Json(json!({ "status": "ok" }))
}

async fn create_session(
    State(svc): State<Arc<Service>>,
    Json(params): Json<StartSessionParams>,
) -> impl IntoResponse {
    match tokio::task::spawn_blocking(move || start_session(&svc, params)).await {
        Ok(Ok(id)) => (
            StatusCode::CREATED,
            Json(json!({ "ok": true, "session": id })),
        )
            .into_response(),
        Ok(Err(e)) => error_response(StatusCode::BAD_REQUEST, "start_failed", &e.to_string()),
        Err(e) => task_error(e),
    }
}

async fn list_sessions(State(svc): State<Arc<Service>>) -> impl IntoResponse {
    match tokio::task::spawn_blocking(move || svc.list_sessions()).await {
        Ok(sessions) => {
            let summaries: Vec<_> = sessions.into_iter().map(summarize).collect();
            Json(json!({ "ok": true, "sessions": summaries })).into_response()
        }
        Err(e) => task_error(e),
    }
}

async fn get_session(State(svc): State<Arc<Service>>, Path(id): Path<Uuid>) -> impl IntoResponse {
    match tokio::task::spawn_blocking(move || svc.state(id)).await {
        Ok(Ok(info)) => Json(json!({ "ok": true, "session": summarize(info) })).into_response(),
        Ok(Err(e)) => service_error(e),
        Err(e) => task_error(e),
    }
}

async fn delete_session(
    State(svc): State<Arc<Service>>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match tokio::task::spawn_blocking(move || svc.terminate(id)).await {
        Ok(Ok(())) => Json(json!({ "ok": true })).into_response(),
        Ok(Err(e)) => service_error(e),
        Err(e) => task_error(e),
    }
}

async fn send_prompt(
    State(svc): State<Arc<Service>>,
    Path(id): Path<Uuid>,
    Json(body): Json<TextBody>,
) -> impl IntoResponse {
    match tokio::task::spawn_blocking(move || svc.send_prompt(id, &body.text)).await {
        Ok(Ok(())) => Json(json!({ "ok": true })).into_response(),
        Ok(Err(e)) => service_error(e),
        Err(e) => task_error(e),
    }
}

#[derive(Deserialize)]
struct TextBody {
    text: String,
}

#[derive(Deserialize)]
struct KeyBody {
    key: String,
}

#[derive(Deserialize)]
struct ActionBody {
    action: String,
}

#[derive(Deserialize)]
struct ConfirmBody {
    accept: bool,
}

#[derive(Deserialize)]
struct ResizeBody {
    rows: u16,
    cols: u16,
}

async fn send_text(
    State(svc): State<Arc<Service>>,
    Path(id): Path<Uuid>,
    Json(body): Json<TextBody>,
) -> impl IntoResponse {
    match tokio::task::spawn_blocking(move || svc.send_text(id, &body.text)).await {
        Ok(Ok(())) => Json(json!({ "ok": true })).into_response(),
        Ok(Err(e)) => service_error(e),
        Err(e) => task_error(e),
    }
}

async fn send_key(
    State(svc): State<Arc<Service>>,
    Path(id): Path<Uuid>,
    Json(body): Json<KeyBody>,
) -> impl IntoResponse {
    match tokio::task::spawn_blocking(move || {
        let key = pseudomux_core::input::parse_key_name(&body.key)
            .map_err(|e| anyhow::anyhow!("invalid key name: {e}"))?;
        svc.send_key(id, key)
    })
    .await
    {
        Ok(Ok(())) => Json(json!({ "ok": true })).into_response(),
        Ok(Err(e)) => error_response(StatusCode::BAD_REQUEST, "bad_request", &e.to_string()),
        Err(e) => task_error(e),
    }
}

async fn send_action(
    State(svc): State<Arc<Service>>,
    Path(id): Path<Uuid>,
    Json(body): Json<ActionBody>,
) -> impl IntoResponse {
    match tokio::task::spawn_blocking(move || svc.send_action(id, &body.action)).await {
        Ok(Ok(())) => Json(json!({ "ok": true })).into_response(),
        Ok(Err(e)) => service_error(e),
        Err(e) => task_error(e),
    }
}

async fn send_enter(State(svc): State<Arc<Service>>, Path(id): Path<Uuid>) -> impl IntoResponse {
    match tokio::task::spawn_blocking(move || svc.send_enter(id)).await {
        Ok(Ok(())) => Json(json!({ "ok": true })).into_response(),
        Ok(Err(e)) => service_error(e),
        Err(e) => task_error(e),
    }
}

async fn get_agent_state(
    State(svc): State<Arc<Service>>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match tokio::task::spawn_blocking(move || svc.agent_state(id)).await {
        Ok(Ok(state)) => Json(json!({ "ok": true, "state": format!("{state:?}") })).into_response(),
        Ok(Err(e)) => service_error(e),
        Err(e) => task_error(e),
    }
}

#[derive(Deserialize)]
struct ContentQuery {
    since_seq: Option<u64>,
    since_last_input: Option<bool>,
}

async fn get_content(
    State(svc): State<Arc<Service>>,
    Path(id): Path<Uuid>,
    Query(q): Query<ContentQuery>,
) -> impl IntoResponse {
    if q.since_last_input.unwrap_or(false) {
        match tokio::task::spawn_blocking(move || {
            let text = svc.filtered_content_since_last_input(id)?;
            let seq = svc.content_current_seq(id)?;
            Ok::<_, anyhow::Error>((text, seq))
        })
        .await
        {
            Ok(Ok((text, next_seq))) => {
                Json(json!({ "ok": true, "text": text, "next_seq": next_seq })).into_response()
            }
            Ok(Err(e)) => service_error(e),
            Err(e) => task_error(e),
        }
    } else {
        let seq = q.since_seq.unwrap_or(0);
        match tokio::task::spawn_blocking(move || {
            let text = svc.filtered_content_since_seq(id, seq)?;
            let current_seq = svc.content_current_seq(id)?;
            Ok::<_, anyhow::Error>((text, current_seq))
        })
        .await
        {
            Ok(Ok((text, next_seq))) => {
                Json(json!({ "ok": true, "text": text, "next_seq": next_seq })).into_response()
            }
            Ok(Err(e)) => service_error(e),
            Err(e) => task_error(e),
        }
    }
}

#[derive(Deserialize)]
struct ScreenQuery {
    status: Option<bool>,
}

async fn get_screen(
    State(svc): State<Arc<Service>>,
    Path(id): Path<Uuid>,
    Query(q): Query<ScreenQuery>,
) -> impl IntoResponse {
    if q.status.unwrap_or(false) {
        match tokio::task::spawn_blocking(move || svc.status_text(id)).await {
            Ok(Ok(text)) => Json(json!({ "ok": true, "text": text })).into_response(),
            Ok(Err(e)) => service_error(e),
            Err(e) => task_error(e),
        }
    } else {
        match tokio::task::spawn_blocking(move || svc.content_text(id)).await {
            Ok(Ok(text)) => Json(json!({ "ok": true, "text": text })).into_response(),
            Ok(Err(e)) => service_error(e),
            Err(e) => task_error(e),
        }
    }
}

#[derive(Deserialize)]
struct EventsQuery {
    timeout_ms: Option<u64>,
    max_events: Option<u64>,
}

async fn get_events(
    State(svc): State<Arc<Service>>,
    Path(id): Path<Uuid>,
    Query(q): Query<EventsQuery>,
) -> impl IntoResponse {
    let rx = match tokio::task::spawn_blocking({
        let svc = Arc::clone(&svc);
        move || svc.subscribe_events(id)
    })
    .await
    {
        Ok(Ok(rx)) => rx,
        Ok(Err(e)) => return service_error(e).into_response(),
        Err(e) => return task_error(e).into_response(),
    };
    sse_stream(
        rx,
        q.timeout_ms.unwrap_or(30_000),
        q.max_events.unwrap_or(u64::MAX),
    )
    .into_response()
}

async fn get_watch(State(svc): State<Arc<Service>>, Path(id): Path<Uuid>) -> impl IntoResponse {
    let rx = match tokio::task::spawn_blocking({
        let svc = Arc::clone(&svc);
        move || svc.subscribe_watch_events(id)
    })
    .await
    {
        Ok(Ok(rx)) => rx,
        Ok(Err(e)) => return service_error(e).into_response(),
        Err(e) => return task_error(e).into_response(),
    };
    sse_stream(rx, 30_000, u64::MAX).into_response()
}

fn sse_stream<T>(
    mut rx: broadcast::Receiver<T>,
    timeout_ms: u64,
    max_events: u64,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>>
where
    T: Clone + Send + serde::Serialize + 'static,
{
    let stream = async_stream::stream! {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        let mut count: u64 = 0;
        loop {
            if count >= max_events {
                break;
            }
            let Ok(result) = tokio::time::timeout_at(deadline, rx.recv()).await else {
                break;
            };
            match result {
                Ok(event) => {
                    if let Ok(data) = serde_json::to_string(&event) {
                        yield Ok(Event::default().data(data));
                        count += 1;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(lagged = n, "SSE subscriber lagged");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Sse::new(stream)
}

async fn resize_session(
    State(svc): State<Arc<Service>>,
    Path(id): Path<Uuid>,
    Json(body): Json<ResizeBody>,
) -> impl IntoResponse {
    match tokio::task::spawn_blocking(move || {
        svc.resize(
            id,
            TerminalSize {
                rows: body.rows,
                cols: body.cols,
            },
        )
    })
    .await
    {
        Ok(Ok(())) => Json(json!({ "ok": true })).into_response(),
        Ok(Err(e)) => service_error(e),
        Err(e) => task_error(e),
    }
}

async fn interrupt_session(
    State(svc): State<Arc<Service>>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match tokio::task::spawn_blocking(move || svc.interrupt(id)).await {
        Ok(Ok(())) => Json(json!({ "ok": true })).into_response(),
        Ok(Err(e)) => service_error(e),
        Err(e) => task_error(e),
    }
}

async fn confirm_session(
    State(svc): State<Arc<Service>>,
    Path(id): Path<Uuid>,
    Json(body): Json<ConfirmBody>,
) -> impl IntoResponse {
    let action = if body.accept {
        "confirm_yes"
    } else {
        "confirm_no"
    }
    .to_string();
    match tokio::task::spawn_blocking(move || svc.send_action(id, &action)).await {
        Ok(Ok(())) => Json(json!({ "ok": true })).into_response(),
        Ok(Err(e)) => service_error(e),
        Err(e) => task_error(e),
    }
}

async fn get_terminal_state(
    State(svc): State<Arc<Service>>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match tokio::task::spawn_blocking(move || svc.terminal_state(id)).await {
        Ok(Ok(state)) => Json(json!({
            "ok": true,
            "keyboard_mode": format!("{:?}", state.keyboard_mode),
            "bracketed_paste": state.bracketed_paste,
            "focus_events": state.focus_events,
        }))
        .into_response(),
        Ok(Err(e)) => service_error(e),
        Err(e) => task_error(e),
    }
}

// ── Blocking prompt endpoints (CLI-parity JSON shape) ───────────────────────

/// Tracked tool call during a prompt turn (mirrors `pmux run` JSON shape).
#[derive(serde::Serialize)]
struct ToolCall {
    name: Option<String>,
    duration_ms: Option<u64>,
}

/// Request body for POST /run (start + wait-Ready + prompt + optionally stop).
#[derive(Deserialize)]
struct RunBody {
    /// Prompt text to send.
    text: String,
    /// Session start params (agent, model, cwd, etc.). Same shape as POST /sessions.
    #[serde(default)]
    session: Option<StartSessionParams>,
    /// Timeout in seconds (default: 120).
    #[serde(default)]
    timeout_secs: Option<u64>,
    /// If true, keep the session alive after the turn (default: false).
    #[serde(default)]
    keep_alive: bool,
}

/// Request body for POST /sessions/:id/prompt-sync.
#[derive(Deserialize)]
struct PromptSyncBody {
    text: String,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

/// One-shot HTTP endpoint: start a session, send a prompt, return the result,
/// and (unless `keep_alive`) terminate the session. Mirrors `pmux run` exactly.
async fn run_oneshot(State(svc): State<Arc<Service>>, Json(body): Json<RunBody>) -> Response {
    let timeout_secs = body.timeout_secs.unwrap_or(120);
    let session_params = body.session.unwrap_or(StartSessionParams {
        agent: None,
        profile: None,
        args: Vec::new(),
        env: Vec::new(),
        cwd: None,
        rows: None,
        cols: None,
        logging_mode: None,
        record_path: None,
        name: None,
    });

    // 1. Create session.
    let svc_for_start = Arc::clone(&svc);
    let session =
        match tokio::task::spawn_blocking(move || start_session(&svc_for_start, session_params))
            .await
        {
            Ok(Ok(id)) => id,
            Ok(Err(e)) => {
                return prompt_error("transport", StatusCode::BAD_GATEWAY, &e.to_string(), None);
            }
            Err(e) => return task_error(e),
        };

    // 2. Wait for Ready.
    if let Err(resp) = wait_for_ready(&svc, session, timeout_secs).await {
        let _ = tokio::task::spawn_blocking({
            let svc = Arc::clone(&svc);
            move || svc.terminate(session)
        })
        .await;
        return resp;
    }

    // 3. Execute prompt.
    let result = execute_blocking_prompt(&svc, session, body.text, timeout_secs).await;

    // 4. Tear down unless keep-alive.
    if !body.keep_alive {
        let _ = tokio::task::spawn_blocking({
            let svc = Arc::clone(&svc);
            move || svc.terminate(session)
        })
        .await;
    }

    result
}

/// Blocking prompt on an existing session.
async fn prompt_sync(
    State(svc): State<Arc<Service>>,
    Path(id): Path<Uuid>,
    Json(body): Json<PromptSyncBody>,
) -> Response {
    let timeout_secs = body.timeout_secs.unwrap_or(120);
    execute_blocking_prompt(&svc, id, body.text, timeout_secs).await
}

/// Poll `agent_state` until the session reaches `Ready` or the timeout elapses.
async fn wait_for_ready(
    svc: &Arc<Service>,
    session: SessionId,
    timeout_secs: u64,
) -> Result<(), Response> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        let svc_for_poll = Arc::clone(svc);
        let state = tokio::task::spawn_blocking(move || svc_for_poll.agent_state(session))
            .await
            .map_err(task_error)?;
        match state {
            Ok(s) if format!("{s:?}") == "Ready" => return Ok(()),
            Ok(_) => {}
            Err(e) => {
                return Err(prompt_error(
                    "transport",
                    StatusCode::BAD_GATEWAY,
                    &e.to_string(),
                    Some(session),
                ));
            }
        }
        if Instant::now() >= deadline {
            return Err(prompt_error(
                "timeout",
                StatusCode::REQUEST_TIMEOUT,
                &format!("timeout waiting for agent to reach Ready ({timeout_secs}s)"),
                Some(session),
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Send a prompt and loop over watch events until TurnComplete (or timeout/
/// error). Returns the CLI-parity JSON shape on success, or a typed error
/// response with the right HTTP status code.
async fn execute_blocking_prompt(
    svc: &Arc<Service>,
    session: SessionId,
    text: String,
    timeout_secs: u64,
) -> Response {
    let prompt_text = text.clone();

    // Subscribe BEFORE sending the prompt.
    let svc_for_sub = Arc::clone(svc);
    let mut rx = match tokio::task::spawn_blocking(move || {
        svc_for_sub.subscribe_watch_events(session)
    })
    .await
    {
        Ok(Ok(rx)) => rx,
        Ok(Err(e)) => {
            return prompt_error(
                "transport",
                StatusCode::BAD_GATEWAY,
                &e.to_string(),
                Some(session),
            );
        }
        Err(e) => return task_error(e),
    };

    // Send the prompt (blocking inside spawn_blocking).
    let svc_for_send = Arc::clone(svc);
    let send_result =
        tokio::task::spawn_blocking(move || svc_for_send.send_prompt(session, &text)).await;
    match send_result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            return prompt_error(
                "transport",
                StatusCode::BAD_GATEWAY,
                &e.to_string(),
                Some(session),
            );
        }
        Err(e) => return task_error(e),
    }

    // Wait for completion.
    let start = Instant::now();
    let deadline = Duration::from_secs(timeout_secs);
    let mut saw_thinking = false;
    let mut turn_duration_ms: Option<u64> = None;
    let mut tools: Vec<ToolCall> = Vec::new();
    let mut active_tool_start: Option<Instant> = None;

    loop {
        let remaining = deadline
            .checked_sub(start.elapsed())
            .unwrap_or(Duration::ZERO);
        if remaining.is_zero() {
            return prompt_error(
                "timeout",
                StatusCode::REQUEST_TIMEOUT,
                &format!("agent did not complete within {timeout_secs}s"),
                Some(session),
            );
        }

        let event = match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(event)) => event,
            Ok(Err(broadcast::error::RecvError::Closed)) => {
                return prompt_error(
                    "transport",
                    StatusCode::BAD_GATEWAY,
                    "watch event stream closed unexpectedly",
                    Some(session),
                );
            }
            Ok(Err(broadcast::error::RecvError::Lagged(n))) => {
                warn!(session = %session, lagged = n, "watch subscriber lagged");
                continue;
            }
            Err(_) => {
                return prompt_error(
                    "timeout",
                    StatusCode::REQUEST_TIMEOUT,
                    &format!("agent did not complete within {timeout_secs}s"),
                    Some(session),
                );
            }
        };

        match event {
            WatchEvent::ToolStarted { name, .. } => {
                tools.push(ToolCall {
                    name,
                    duration_ms: None,
                });
                active_tool_start = Some(Instant::now());
            }
            WatchEvent::ToolFinished { .. } => {
                if let Some(start_inst) = active_tool_start.take()
                    && let Some(last) = tools.last_mut()
                {
                    last.duration_ms = Some(start_inst.elapsed().as_millis() as u64);
                }
            }
            WatchEvent::TurnComplete { duration_ms, .. } => {
                turn_duration_ms = Some(duration_ms);
                break;
            }
            WatchEvent::StateChange { ref to, .. } => {
                if to == "Thinking" {
                    saw_thinking = true;
                }
                if to == "Ready" && saw_thinking {
                    break;
                }
            }
            WatchEvent::InputRequired {
                ref kind,
                ref prompt_text,
                ..
            } => {
                let (code, message) = if kind == "auth" {
                    (
                        "auth_required",
                        format!("agent requires authentication: {prompt_text}"),
                    )
                } else {
                    (
                        "confirmation_required",
                        format!("agent requires confirmation: {prompt_text}"),
                    )
                };
                return prompt_error(
                    code,
                    StatusCode::PRECONDITION_REQUIRED,
                    &message,
                    Some(session),
                );
            }
            WatchEvent::SessionExited { exit_code, .. } => {
                return prompt_error(
                    "agent_exited",
                    StatusCode::BAD_GATEWAY,
                    &format!("agent process exited (code: {exit_code:?})"),
                    Some(session),
                );
            }
            _ => {}
        }
    }

    let effective_duration = turn_duration_ms.unwrap_or(start.elapsed().as_millis() as u64);

    // Fetch the final response.
    let svc_for_fetch = Arc::clone(svc);
    let text = match tokio::task::spawn_blocking(move || {
        svc_for_fetch.filtered_response_since_last_input(session)
    })
    .await
    {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => {
            return prompt_error(
                "transport",
                StatusCode::BAD_GATEWAY,
                &e.to_string(),
                Some(session),
            );
        }
        Err(e) => return task_error(e),
    };
    let text = strip_prompt_echo(text, &prompt_text);

    (
        StatusCode::OK,
        Json(json!({
            "session_id": session.to_string(),
            "text": text,
            "duration_ms": effective_duration,
            "state": "Ready",
            "tools": tools,
        })),
    )
        .into_response()
}

/// Build a typed error response matching the CLI's JSON shape.
fn prompt_error(
    code: &str,
    status: StatusCode,
    message: &str,
    session_id: Option<SessionId>,
) -> Response {
    let mut body = json!({
        "error": code,
        "message": message,
    });
    if let Some(id) = session_id {
        body["session_id"] = json!(id.to_string());
    }
    (status, Json(body)).into_response()
}
