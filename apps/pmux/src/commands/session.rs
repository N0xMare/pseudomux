use anyhow::{Result, bail};
use pseudomux_adapters::{AgentKind, ClaudeCodeOpts, LaunchConfig};
use pseudomux_core::session::state::TerminalSize;
use pseudomux_protocol::{
    InterruptParams, ReadSinceParams, Request, ResizeParams, Response, SendTextParams,
    SessionSummary, StartSessionParams, TerminateParams,
};
use pseudomux_service::Service;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{self, AsyncBufReadExt};
use tokio::time::sleep;

use crate::client::{DaemonClient, expect_ack, resolve_session};

pub(crate) async fn handle_ping(client: &DaemonClient, json: bool) -> Result<()> {
    let resp = client.send(Request::Ping).await?;
    match resp {
        Response::Pong => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "type": "health",
                        "ok": true
                    }))?
                );
            } else {
                println!("ok");
            }
        }
        Response::Error { code, message } => bail!("{code}: {message}"),
        other => bail!("unexpected response: {other:?}"),
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_start(
    client: &DaemonClient,
    profile: Option<String>,
    agent: Option<String>,
    name: Option<String>,
    cwd: Option<String>,
    rows: Option<u16>,
    cols: Option<u16>,
    model: Option<String>,
    permission_mode: Option<String>,
    allowed_tools: Option<String>,
    disallowed_tools: Option<String>,
    system_prompt: Option<String>,
    append_system_prompt: Option<String>,
    effort: Option<String>,
    max_budget: Option<f64>,
    record: Option<std::path::PathBuf>,
    agent_args: Vec<String>,
) -> Result<()> {
    let is_claude = agent
        .as_deref()
        .map(|a| {
            matches!(
                a.to_lowercase().as_str(),
                "claude-code" | "claude" | "claudecode"
            )
        })
        .unwrap_or(false);
    let mut extra_args = Vec::new();
    if is_claude {
        let claude_opts = ClaudeCodeOpts {
            model,
            permission_mode,
            allowed_tools,
            disallowed_tools,
            system_prompt,
            append_system_prompt,
            effort,
            max_budget,
            settings_json: None,
        };
        extra_args.extend(claude_opts.to_args());
    }
    extra_args.extend(agent_args);
    let params = StartSessionParams {
        agent,
        profile,
        args: extra_args,
        env: Vec::new(),
        cwd,
        rows,
        cols,
        logging_mode: None,
        record_path: record.map(|p| p.display().to_string()),
        name,
    };
    let resp = client.send(Request::StartSession(params)).await?;
    let session = match resp {
        Response::StartSession { session } => session,
        Response::Error { code, message } => bail!("{code}: {message}"),
        other => bail!("unexpected response: {other:?}"),
    };
    println!("{session}");
    Ok(())
}

pub(crate) async fn handle_stop(client: &DaemonClient, session: &str) -> Result<()> {
    let session = resolve_session(client, session).await?;
    expect_ack(
        client
            .send(Request::Terminate(TerminateParams { session }))
            .await?,
    )
}

pub(crate) async fn handle_list(client: &DaemonClient) -> Result<()> {
    let resp = client.send(Request::ListSessions).await?;
    match resp {
        Response::Sessions { sessions } => {
            for s in sessions {
                print_summary(&s);
            }
        }
        Response::Error { code, message } => bail!("{code}: {message}"),
        other => bail!("unexpected response: {other:?}"),
    }
    Ok(())
}

pub(crate) async fn handle_attach(client: &DaemonClient, session: &str) -> Result<()> {
    let session = resolve_session(client, session).await?;
    attach_session(client, session).await
}

pub(crate) async fn handle_resize(
    client: &DaemonClient,
    session: &str,
    rows: u16,
    cols: u16,
) -> Result<()> {
    let session = resolve_session(client, session).await?;
    expect_ack(
        client
            .send(Request::Resize(ResizeParams {
                session,
                rows,
                cols,
            }))
            .await?,
    )
}

pub(crate) async fn handle_interrupt(client: &DaemonClient, session: &str) -> Result<()> {
    let session = resolve_session(client, session).await?;
    expect_ack(
        client
            .send(Request::Interrupt(InterruptParams { session }))
            .await?,
    )
}

pub(crate) fn handle_demo(
    agent: String,
    linger_ms: u64,
    command: Vec<String>,
    interactive: bool,
    keep_alive: bool,
) -> Result<()> {
    use std::io::Write;
    let agent_kind = parse_agent(&agent);
    let mut cfg = LaunchConfig {
        size: TerminalSize { rows: 24, cols: 80 },
        ..LaunchConfig::default()
    };
    let mut send_after = None;
    if matches!(agent_kind, AgentKind::OpenCode | AgentKind::ClaudeCode) && !command.is_empty() {
        cfg.args = command.clone();
    } else if !interactive {
        let command_str = if command.is_empty() {
            "echo hello".to_string()
        } else {
            command.join(" ")
        };
        send_after = Some(command_str);
    }
    let svc = Arc::new(Service::new()?);
    let sid = svc.start(agent_kind, cfg, None)?;
    let session_dir = svc.log_root().join(sid.to_string());
    eprintln!("session log: {}", session_dir.display());
    if let Some(text) = send_after {
        svc.send_text(sid, &text)?;
        svc.send_enter(sid)?;
    }
    if interactive {
        interactive_attach(Arc::clone(&svc), sid)?;
    } else {
        let mut seq: u64 = 0;
        let start = std::time::Instant::now();
        let mut idle_iters = 0u8;
        let linger = Duration::from_millis(linger_ms);
        loop {
            let (chunks, next) = svc.read_since(sid, seq)?;
            if chunks.is_empty() {
                idle_iters += 1;
            } else {
                idle_iters = 0;
            }
            for c in chunks {
                print!("{}", String::from_utf8_lossy(&c.bytes));
            }
            std::io::stdout().flush().ok();
            seq = next.saturating_sub(1);
            if idle_iters >= 5 {
                break;
            }
            if start.elapsed() > linger {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        eprintln!("\n-- next_seq: {}", seq + 1);
    }
    if keep_alive {
        eprintln!("session kept alive; id: {sid}");
    } else {
        let _ = svc.terminate(sid);
    }
    Ok(())
}

fn parse_agent(s: &str) -> AgentKind {
    s.parse::<AgentKind>().unwrap()
}

async fn attach_session(
    client: &DaemonClient,
    session: pseudomux_protocol::SessionId,
) -> Result<()> {
    let mut reader = io::BufReader::new(io::stdin());
    let mut input = String::new();
    let mut seq: u64 = 0;
    loop {
        tokio::select! {
            () = sleep(Duration::from_millis(200)) => {
                let resp = client.send(Request::ReadSince(ReadSinceParams { session, seq })).await?;
                match resp {
                    Response::Output { chunks, next_seq } => {
                        for chunk in chunks {
                            print!("{}", String::from_utf8_lossy(&chunk.bytes));
                        }
                        seq = next_seq.saturating_sub(1);
                    }
                    Response::Error { code, message } => bail!("{code}: {message}"),
                    _ => {}
                }
            }
            read = reader.read_line(&mut input) => {
                let n = read?;
                if n == 0 {
                    break;
                }
                if input.trim() == "/exit" {
                    break;
                }
                let text = input.trim_end_matches('\n').to_string();
                client.send(Request::SendText(SendTextParams { session, text })).await?;
                client.send(Request::SendEnter(InterruptParams { session })).await?;
                input.clear();
            }
        }
    }
    Ok(())
}

fn print_summary(summary: &SessionSummary) {
    let profile = summary.profile.as_deref().unwrap_or("-");
    let cwd = summary.cwd.as_deref().unwrap_or("-");
    let name_display = summary
        .name
        .as_deref()
        .map(|n| format!(" name={n}"))
        .unwrap_or_default();
    let args = if summary.args.is_empty() {
        "-".to_string()
    } else {
        summary.args.join(" ")
    };
    println!(
        "{}{} status={} size={}x{} pid={:?} profile={} agent={} program={} cwd={} args={}",
        summary.session,
        name_display,
        summary.status,
        summary.rows,
        summary.cols,
        summary.pid,
        profile,
        summary.agent,
        summary.program,
        cwd,
        args
    );
}

fn interactive_attach(svc: Arc<Service>, sid: pseudomux_protocol::SessionId) -> Result<()> {
    use std::io::{self, Write};
    let mut seq: u64 = 0;
    let stdin = io::stdin();
    let mut line = String::new();
    loop {
        let (chunks, next) = svc.read_since(sid, seq)?;
        for c in chunks {
            print!("{}", String::from_utf8_lossy(&c.bytes));
        }
        std::io::stdout().flush().ok();
        seq = next.saturating_sub(1);
        line.clear();
        stdin.read_line(&mut line)?;
        if line.trim() == "/exit" {
            break;
        }
        svc.send_text(sid, line.trim_end_matches('\n'))?;
        svc.send_enter(sid)?;
    }
    Ok(())
}
