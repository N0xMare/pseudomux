use anyhow::{Result, bail};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use pseudomux_protocol::{
    Request, Response, SendPromptParams, SessionId, SessionStateParams, SubscribeEventsParams,
};
use std::io::Write;
use std::time::Duration;
use tokio::time::Instant;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use crate::client::{DaemonClient, connect_to_daemon, expect_ack, resolve_session};

/// Typed error from `execute_prompt`. Maps to a stable exit code:
///
/// - **0** — success (no error)
/// - **1** — timeout
/// - **2** — agent or transport failure (subprocess crash, daemon error, IO)
/// - **3** — agent needs human input (auth or confirmation)
///
/// In `--json` mode, errors emit `{"error": "<code>", "message": "...",
/// "session_id": "..."}` to stdout. In plain mode, they go to stderr.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub(crate) enum PromptError {
    Timeout {
        message: String,
        session_id: Option<String>,
    },
    AgentExited {
        exit_code: Option<i32>,
        message: String,
        session_id: Option<String>,
    },
    AuthRequired {
        message: String,
        session_id: Option<String>,
    },
    ConfirmationRequired {
        prompt_text: String,
        message: String,
        session_id: Option<String>,
    },
    Transport {
        message: String,
        session_id: Option<String>,
    },
}

impl PromptError {
    pub(crate) fn exit_code(&self) -> i32 {
        match self {
            Self::Timeout { .. } => 1,
            Self::AgentExited { .. } | Self::Transport { .. } => 2,
            Self::AuthRequired { .. } | Self::ConfirmationRequired { .. } => 3,
        }
    }

    pub(crate) fn message(&self) -> &str {
        match self {
            Self::Timeout { message, .. }
            | Self::AgentExited { message, .. }
            | Self::AuthRequired { message, .. }
            | Self::ConfirmationRequired { message, .. }
            | Self::Transport { message, .. } => message,
        }
    }

    fn with_session(mut self, session: SessionId) -> Self {
        let s = Some(session.to_string());
        match &mut self {
            Self::Timeout { session_id, .. }
            | Self::AgentExited { session_id, .. }
            | Self::AuthRequired { session_id, .. }
            | Self::ConfirmationRequired { session_id, .. }
            | Self::Transport { session_id, .. } => *session_id = s,
        }
        self
    }
}

impl std::fmt::Display for PromptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message())
    }
}

impl std::error::Error for PromptError {}

impl From<anyhow::Error> for PromptError {
    fn from(e: anyhow::Error) -> Self {
        Self::Transport {
            message: e.to_string(),
            session_id: None,
        }
    }
}
impl From<std::io::Error> for PromptError {
    fn from(e: std::io::Error) -> Self {
        Self::Transport {
            message: e.to_string(),
            session_id: None,
        }
    }
}
impl From<serde_json::Error> for PromptError {
    fn from(e: serde_json::Error) -> Self {
        Self::Transport {
            message: format!("serde: {e}"),
            session_id: None,
        }
    }
}

/// Print a [`PromptError`] and exit with its associated code.
pub(crate) fn print_error_and_exit(err: PromptError, json: bool) -> ! {
    let code = err.exit_code();
    if json {
        let out = serde_json::to_string(&err).unwrap_or_else(|_| {
            format!(r#"{{"error":"transport","message":{:?}}}"#, err.message())
        });
        println!("{out}");
    } else {
        eprintln!("error: {}", err.message());
    }
    std::process::exit(code);
}

/// Resolve `--text` and `--file` into the prompt text. Exactly one must be set;
/// `--file -` reads from stdin.
pub(crate) fn resolve_prompt_text(text: Option<String>, file: Option<String>) -> Result<String> {
    match (text, file) {
        (Some(t), None) => Ok(t),
        (None, Some(path)) => {
            if path == "-" {
                use std::io::Read;
                let mut buf = String::new();
                std::io::stdin()
                    .read_to_string(&mut buf)
                    .map_err(|e| anyhow::anyhow!("failed to read prompt from stdin: {e}"))?;
                Ok(buf)
            } else {
                std::fs::read_to_string(&path)
                    .map_err(|e| anyhow::anyhow!("failed to read prompt file '{path}': {e}"))
            }
        }
        (Some(_), Some(_)) => bail!("--text and --file are mutually exclusive"),
        (None, None) => bail!("one of --text or --file is required"),
    }
}

/// A tool invocation observed during a prompt turn.
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct ToolCall {
    pub name: Option<String>,
    pub duration_ms: Option<u64>,
}

/// The result of a single prompt → response turn.
#[derive(Clone, Debug)]
pub(crate) struct PromptResult {
    pub session_id: SessionId,
    pub text: String,
    pub duration_ms: u64,
    pub state: String,
    pub tools: Vec<ToolCall>,
}

// ── Core execution primitive ────────────────────────────────────────────────

/// Send a prompt to an already-running session and block until the agent
/// responds. Returns the clean response text, duration, state, and tool calls.
///
/// Callers: both `handle_sdk_prompt` (the `pmux prompt` CLI command) and
/// `handle_run` (the `pmux run` one-shot command) share this function.
pub(crate) async fn execute_prompt(
    client: &DaemonClient,
    session: SessionId,
    text: String,
    timeout_secs: u64,
    stream: bool,
) -> Result<PromptResult, PromptError> {
    execute_prompt_inner(client, session, text, timeout_secs, stream)
        .await
        .map_err(|e| e.with_session(session))
}

async fn execute_prompt_inner(
    client: &DaemonClient,
    session: SessionId,
    text: String,
    timeout_secs: u64,
    stream: bool,
) -> Result<PromptResult, PromptError> {
    let timeout_ms = timeout_secs * 1000;
    let prompt_text = text.clone();

    // Subscribe to watch events BEFORE sending prompt.
    let watch_request = Request::SubscribeWatchEvents(SubscribeEventsParams {
        session,
        timeout_ms,
        max_events: 0,
    });
    let watch_payload = serde_json::to_vec(&watch_request)?;
    let watch_stream = connect_to_daemon(&client.sockets).await?;
    let mut watch_framed = Framed::new(watch_stream, LengthDelimitedCodec::new());
    watch_framed.send(Bytes::from(watch_payload)).await?;

    // Send the prompt.
    let send_resp = client
        .send(Request::SendPrompt(SendPromptParams { session, text }))
        .await?;
    expect_ack(send_resp)?;

    let start = Instant::now();
    let deadline = Duration::from_millis(timeout_ms);
    let mut saw_thinking = false;
    let mut turn_duration_ms: Option<u64> = None;
    #[allow(unused_assignments)]
    let mut final_state = String::new();
    let mut tools: Vec<ToolCall> = Vec::new();
    let mut active_tool_start: Option<Instant> = None;

    loop {
        let remaining = deadline
            .checked_sub(start.elapsed())
            .unwrap_or(Duration::ZERO);
        if remaining.is_zero() {
            return Err(PromptError::Timeout {
                message: format!("timeout waiting for agent response ({timeout_secs}s)"),
                session_id: None,
            });
        }

        let frame = match tokio::time::timeout(remaining, watch_framed.next()).await {
            Ok(Some(Ok(bytes))) => bytes,
            Ok(Some(Err(e))) => {
                return Err(PromptError::Transport {
                    message: format!("stream error: {e}"),
                    session_id: None,
                });
            }
            Ok(None) => {
                return Err(PromptError::Transport {
                    message: "watch stream ended without completion event".into(),
                    session_id: None,
                });
            }
            Err(_) => {
                return Err(PromptError::Timeout {
                    message: format!("timeout waiting for agent response ({timeout_secs}s)"),
                    session_id: None,
                });
            }
        };

        let response: Response = serde_json::from_slice(&frame)?;
        match response {
            Response::WatchEvent { ref event } => {
                if stream {
                    let mut stderr = std::io::stderr().lock();
                    let _ = writeln!(stderr, "{}", serde_json::to_string(event)?);
                    let _ = stderr.flush();
                }

                if let Some(ts) = event.get("ToolStarted") {
                    let name = ts
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
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
                    turn_duration_ms = tc.get("duration_ms").and_then(serde_json::Value::as_u64);
                    final_state = "Ready".to_string();
                    break;
                }
                if let Some(sc) = event.get("StateChange")
                    && let Some(to) = sc.get("to").and_then(|v| v.as_str())
                {
                    if to == "Thinking" {
                        saw_thinking = true;
                    }
                    if to == "Ready" && saw_thinking {
                        final_state = "Ready".to_string();
                        break;
                    }
                }
                if let Some(ir) = event.get("InputRequired") {
                    let kind = ir.get("kind").and_then(|v| v.as_str()).unwrap_or("unknown");
                    let prompt_text = ir
                        .get("prompt_text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    if kind == "auth" {
                        return Err(PromptError::AuthRequired {
                            message: format!("agent requires authentication: {prompt_text}"),
                            session_id: None,
                        });
                    }
                    return Err(PromptError::ConfirmationRequired {
                        prompt_text: prompt_text.clone(),
                        message: format!("agent requires confirmation: {prompt_text}"),
                        session_id: None,
                    });
                }
                if let Some(exit) = event.get("SessionExited") {
                    let code = exit
                        .get("exit_code")
                        .and_then(|v| v.as_i64())
                        .map(|c| c as i32);
                    return Err(PromptError::AgentExited {
                        exit_code: code,
                        message: format!("agent process exited (code: {code:?})"),
                        session_id: None,
                    });
                }
            }
            Response::Ack => {
                // Ack on this stream means the daemon closed the subscription —
                // typically because its timeout (which we set to match ours)
                // elapsed before TurnComplete fired. From the user's
                // perspective this is a timeout, not a transport failure.
                return Err(PromptError::Timeout {
                    message: format!(
                        "agent did not complete within {timeout_secs}s (server-side stream timeout)"
                    ),
                    session_id: None,
                });
            }
            Response::Error { code, message } => {
                return Err(PromptError::Transport {
                    message: format!("{code}: {message}"),
                    session_id: None,
                });
            }
            _ => {}
        }
    }

    let elapsed_ms = start.elapsed().as_millis() as u64;
    let effective_duration = turn_duration_ms.unwrap_or(elapsed_ms);

    // Row-aware snapshot of the assistant's response.
    let content_resp = client
        .send(Request::GetFilteredResponseSinceLastInput(
            SessionStateParams { session },
        ))
        .await?;
    let text = match content_resp {
        Response::FilteredContent { text, .. } => text,
        Response::Error { code, message } => {
            return Err(PromptError::Transport {
                message: format!("{code}: {message}"),
                session_id: None,
            });
        }
        other => {
            return Err(PromptError::Transport {
                message: format!("unexpected response: {other:?}"),
                session_id: None,
            });
        }
    };
    let text = strip_prompt_echo(text, &prompt_text);

    Ok(PromptResult {
        session_id: session,
        text,
        duration_ms: effective_duration,
        state: final_state,
        tools,
    })
}

/// Print a [`PromptResult`] as JSON or plain text.
pub(crate) fn print_result(result: &PromptResult, json: bool) {
    if json {
        let out = serde_json::to_string(&serde_json::json!({
            "session_id": result.session_id.to_string(),
            "text": result.text,
            "duration_ms": result.duration_ms,
            "state": result.state,
            "tools": result.tools,
        }))
        .expect("JSON serialization failed");
        println!("{out}");
    } else {
        print!("{}", result.text);
    }
}

// ── CLI entry point (pmux prompt) ───────────────────────────────────────────

pub(crate) async fn handle_sdk_prompt(
    client: &DaemonClient,
    session: &str,
    text: String,
    timeout: u64,
    json: bool,
    stream: bool,
) -> Result<()> {
    let session = resolve_session(client, session).await?;
    match execute_prompt(client, session, text, timeout, stream).await {
        Ok(result) => print_result(&result, json),
        Err(err) => print_error_and_exit(err, json),
    }
    Ok(())
}

// ── Response cleanup ────────────────────────────────────────────────────────

// Response post-processing moved to `pseudomux_service::response` so both
// the CLI and the HTTP handler in pmuxd can use the same stripping logic.
use pseudomux_service::response::strip_prompt_echo;

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::strip_prompt_echo;

    #[test]
    fn strips_wrapped_prompt_echo() {
        let prompt = "You are running inside pseudomux as a sub-agent. Please review this \
                      codebase and report what you find in under 300 words.";
        let response = [
            "You are running inside pseudomux as a sub-agent. Please review this",
            "codebase and report what you find in under 300 words.",
            "Pseudomux Review",
            "Pseudomux is an SDK-grade PTY multiplexer.",
        ]
        .join("\n");
        let stripped = strip_prompt_echo(response, prompt);
        assert_eq!(
            stripped,
            "Pseudomux Review\nPseudomux is an SDK-grade PTY multiplexer."
        );
    }

    #[test]
    fn preserves_response_when_no_echo() {
        let prompt = "What is 2+2?";
        let response = "4. It's a simple addition.".to_string();
        assert_eq!(strip_prompt_echo(response.clone(), prompt), response);
    }

    #[test]
    fn ignores_short_word_false_match() {
        let prompt = "What do you think about the dog and the cat?";
        let response = "the dog".to_string();
        assert_eq!(strip_prompt_echo(response.clone(), prompt), response);
    }

    #[test]
    fn stops_at_first_non_match() {
        let prompt = "Line one goes here. Line two is here too.";
        let response = [
            "Line one goes here.",
            "Actual response start.",
            "Line two is here too.",
        ]
        .join("\n");
        let stripped = strip_prompt_echo(response, prompt);
        assert_eq!(stripped, "Actual response start.\nLine two is here too.");
    }

    #[test]
    fn strips_tool_chrome_preamble() {
        let prompt = "Review the codebase and tell me what it does.";
        let response = [
            "[context] $ ls /home/jm/dev/pseudomux",
            "Listing 1 directory… (ctrl+o to expand)",
            "Listing 1 directory…",
            "[context] README.md",
            "Reading 1 file, listing 1 directory… (ctrl+o to expand)",
            "Read 2 files, listed 1 directory (ctrl+o to expand)",
            "Pseudomux Review",
            "Pseudomux is a PTY multiplexer for TUI agents.",
        ]
        .join("\n");
        let stripped = strip_prompt_echo(response, prompt);
        assert_eq!(
            stripped,
            "Pseudomux Review\nPseudomux is a PTY multiplexer for TUI agents."
        );
    }

    #[test]
    fn strips_parenthesized_tool_headers() {
        let prompt = "Fetch a URL and summarize.";
        let response = [
            "Fetch(https://doc.rust-lang.org/std/option/enum.Option.html)",
            "The Option enum represents optional values.",
            "Bash(ls -la)",
            "Task(find all .rs files)",
            "Summary complete.",
        ]
        .join("\n");
        let stripped = strip_prompt_echo(response, prompt);
        assert_eq!(
            stripped,
            "The Option enum represents optional values.\nSummary complete."
        );
    }

    #[test]
    fn strips_tool_chrome_after_prose() {
        let prompt = "Review the repo.";
        let response = [
            "I have enough to write the report.",
            "Pseudomux Review",
            "▘▘ ▝▝    ~/dev/pseudomux",
            "Pseudomux is a PTY multiplexer.",
        ]
        .join("\n");
        let stripped = strip_prompt_echo(response, prompt);
        assert_eq!(
            stripped,
            "I have enough to write the report.\nPseudomux Review\nPseudomux is a PTY multiplexer."
        );
    }

    #[test]
    fn strips_effort_and_welcome_chrome() {
        let prompt = "Review this repo.";
        let response = [
            "◐ medium · /effort",
            "▝▜████",
            "▘▘ ▝▝    ~/dev/pseudomux",
            "[context] README.md",
            "Pseudomux Review",
            "A PTY multiplexer.",
        ]
        .join("\n");
        let stripped = strip_prompt_echo(response, prompt);
        assert_eq!(stripped, "Pseudomux Review\nA PTY multiplexer.");
    }

    #[test]
    fn strips_mixed_prompt_and_tool_chrome() {
        let prompt = "What is in this repo? Please look around.";
        let response = [
            "What is in this repo? Please look around.",
            "[context] README.md",
            "Reading 1 file…",
            "Read 1 file (ctrl+o to expand)",
            "This repo contains a PTY multiplexer called pseudomux.",
        ]
        .join("\n");
        let stripped = strip_prompt_echo(response, prompt);
        assert_eq!(
            stripped,
            "This repo contains a PTY multiplexer called pseudomux."
        );
    }
}
