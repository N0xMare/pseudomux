//! `pmux run` — one-shot convenience command.
//!
//! Combines `start` + wait-for-Ready + `prompt` + (optionally) `stop` into a
//! single CLI invocation, so the simplest possible external usage is:
//!
//! ```sh
//! pmux run --model opus --text "Review this PR" --json
//! ```
//!
//! With `--keep-alive` the session stays open for follow-up `pmux prompt`
//! calls (Mode 2: persistent orchestrator).

use anyhow::{Result, bail};
use pseudomux_adapters::ClaudeCodeOpts;
use pseudomux_protocol::{
    Request, Response, SessionStateParams, StartSessionParams, TerminateParams,
};
use std::time::Duration;

use super::prompt::{PromptError, execute_prompt, print_error_and_exit, print_result};
use crate::client::DaemonClient;

pub(crate) struct RunOptions {
    pub(crate) profile: Option<String>,
    pub(crate) agent: String,
    pub(crate) cwd: Option<String>,
    pub(crate) rows: Option<u16>,
    pub(crate) cols: Option<u16>,
    pub(crate) model: Option<String>,
    pub(crate) permission_mode: Option<String>,
    pub(crate) allowed_tools: Option<String>,
    pub(crate) disallowed_tools: Option<String>,
    pub(crate) system_prompt: Option<String>,
    pub(crate) append_system_prompt: Option<String>,
    pub(crate) effort: Option<String>,
    pub(crate) max_budget: Option<f64>,
    pub(crate) name: Option<String>,
    pub(crate) text: String,
    pub(crate) timeout: u64,
    pub(crate) json: bool,
    pub(crate) stream: bool,
    pub(crate) keep_alive: bool,
}

pub(crate) async fn handle_run(client: &DaemonClient, options: RunOptions) -> Result<()> {
    let timeout_ms = options.timeout * 1000;

    // ── Step 1: Build args and start the session ────────────────────────────
    let is_claude = matches!(
        options.agent.to_lowercase().as_str(),
        "claude-code" | "claude" | "claudecode"
    );
    let mut extra_args = Vec::new();
    if is_claude {
        let opts = ClaudeCodeOpts {
            model: options.model,
            permission_mode: options.permission_mode,
            allowed_tools: options.allowed_tools,
            disallowed_tools: options.disallowed_tools,
            system_prompt: options.system_prompt,
            append_system_prompt: options.append_system_prompt,
            effort: options.effort,
            max_budget: options.max_budget,
            settings_json: None,
        };
        extra_args.extend(opts.to_args());
    }

    let params = StartSessionParams {
        agent: Some(options.agent),
        profile: options.profile,
        args: extra_args,
        env: Vec::new(),
        cwd: options.cwd,
        rows: options.rows,
        cols: options.cols,
        logging_mode: None,
        record_path: None,
        name: options.name,
    };
    let resp = client.send(Request::StartSession(params)).await?;
    let session = match resp {
        Response::StartSession { session } => session,
        Response::Error { code, message } => bail!("{code}: {message}"),
        other => bail!("unexpected response: {other:?}"),
    };

    // ── Step 2: Wait for the agent to reach Ready ───────────────────────────
    // Poll agent-state until Ready. Using a poll loop rather than watch-event
    // subscription avoids the race where Booting→Ready fires before the
    // broadcast subscriber is registered (tokio broadcast drops events sent
    // before any receiver exists). Boot takes 3–10 seconds, so polling at
    // 500ms is responsive enough.
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let state_resp = client
            .send(Request::GetAgentState(SessionStateParams { session }))
            .await?;
        if matches!(state_resp, Response::AgentState { ref state } if state == "Ready") {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            // Clean up the session we just started.
            let _ = client
                .send(Request::Terminate(TerminateParams { session }))
                .await;
            print_error_and_exit(
                PromptError::Timeout {
                    message: format!(
                        "timeout waiting for agent to reach Ready ({}s)",
                        options.timeout
                    ),
                    session_id: Some(session.to_string()),
                },
                options.json,
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // ── Step 3: Execute the prompt ──────────────────────────────────────────
    let result = execute_prompt(
        client,
        session,
        options.text,
        options.timeout,
        options.stream,
    )
    .await;

    // ── Step 4: Tear down (unless --keep-alive) ─────────────────────────────
    if !options.keep_alive {
        let _ = client
            .send(Request::Terminate(TerminateParams { session }))
            .await;
    }

    // Handle the result after cleanup so errors don't leave a zombie session.
    let result = match result {
        Ok(r) => r,
        Err(err) => print_error_and_exit(err, options.json),
    };

    // ── Step 5: Output ──────────────────────────────────────────────────────
    // print_result always includes session_id in JSON mode, so the caller
    // can send follow-up `pmux prompt <session_id>` calls when --keep-alive.
    print_result(&result, options.json);
    if options.keep_alive && !options.json {
        // Non-JSON callers need the session ID on stderr so they can
        // continue prompting without parsing stdout.
        eprintln!("\nsession_id={session}");
    }
    Ok(())
}
