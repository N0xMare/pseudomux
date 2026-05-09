mod client;
mod commands;
mod config;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use pseudomux_service::socket_candidates;
use std::path::PathBuf;

use client::DaemonClient;

#[derive(Parser, Debug)]
#[command(name = "pmux")]
#[command(about = "pseudomux operator CLI", long_about = None)]
struct Cli {
    #[arg(long)]
    socket: Option<PathBuf>,
    #[arg(long)]
    profile: Option<String>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub(crate) enum StateFormat {
    Text,
    Json,
}

#[derive(Subcommand, Debug)]
#[allow(clippy::large_enum_variant)]
enum Commands {
    Ping {
        #[arg(long)]
        json: bool,
    },
    Start {
        #[arg(long, env = "PMUX_AGENT")]
        agent: Option<String>,
        #[arg(long, help = "Human-readable session name for pmux list")]
        name: Option<String>,
        #[arg(long, env = "PMUX_CWD")]
        cwd: Option<String>,
        #[arg(long)]
        rows: Option<u16>,
        #[arg(long)]
        cols: Option<u16>,
        #[arg(
            long,
            env = "PMUX_MODEL",
            help_heading = "Claude Code Options",
            help = "Model (e.g. sonnet, opus, haiku, claude-sonnet-4-6)"
        )]
        model: Option<String>,
        #[arg(
            long,
            env = "PMUX_PERMISSION_MODE",
            help_heading = "Claude Code Options",
            help = "Permission mode: default, acceptEdits, bypassPermissions, plan"
        )]
        permission_mode: Option<String>,
        #[arg(
            long,
            help_heading = "Claude Code Options",
            help = "Allowed tools (space-separated)"
        )]
        allowed_tools: Option<String>,
        #[arg(
            long,
            help_heading = "Claude Code Options",
            help = "Disallowed tools (space-separated)"
        )]
        disallowed_tools: Option<String>,
        #[arg(
            long,
            help_heading = "Claude Code Options",
            help = "System prompt override"
        )]
        system_prompt: Option<String>,
        #[arg(
            long,
            help_heading = "Claude Code Options",
            help = "Append to default system prompt"
        )]
        append_system_prompt: Option<String>,
        #[arg(
            long,
            help_heading = "Claude Code Options",
            help = "Effort level: low, medium, high"
        )]
        effort: Option<String>,
        #[arg(
            long,
            help_heading = "Claude Code Options",
            help = "Maximum budget in USD"
        )]
        max_budget: Option<f64>,
        #[arg(long, help = "Record raw PTY bytes to this file path")]
        record: Option<PathBuf>,
        #[arg(trailing_var_arg = true)]
        agent_args: Vec<String>,
    },
    Send {
        session: String,
        #[arg(long)]
        text: String,
    },
    Read {
        session: String,
        #[arg(long, default_value_t = 0)]
        since: u64,
    },
    Resize {
        session: String,
        #[arg(long)]
        rows: u16,
        #[arg(long)]
        cols: u16,
    },
    Interrupt {
        session: String,
    },
    Stop {
        session: String,
    },
    List,
    Attach {
        session: String,
    },
    /// Start a session, send a prompt, wait for the response, and stop.
    /// One-shot convenience command that combines start + prompt + stop.
    /// With --keep-alive the session stays open for follow-up `pmux prompt` calls.
    Run {
        /// Prompt text to send (mutually exclusive with --file)
        #[arg(long, conflicts_with = "file")]
        text: Option<String>,
        /// Read prompt text from a file (use `-` for stdin)
        #[arg(long, conflicts_with = "text")]
        file: Option<String>,
        /// Agent type
        #[arg(long, env = "PMUX_AGENT", default_value = "claude-code")]
        agent: String,
        /// Human-readable session name
        #[arg(long)]
        name: Option<String>,
        #[arg(long, env = "PMUX_CWD")]
        cwd: Option<String>,
        #[arg(long)]
        rows: Option<u16>,
        #[arg(long)]
        cols: Option<u16>,
        #[arg(
            long,
            env = "PMUX_MODEL",
            help_heading = "Claude Code Options",
            help = "Model (e.g. sonnet, opus, haiku, claude-sonnet-4-6)"
        )]
        model: Option<String>,
        #[arg(
            long,
            env = "PMUX_PERMISSION_MODE",
            help_heading = "Claude Code Options",
            help = "Permission mode: default, acceptEdits, bypassPermissions, plan"
        )]
        permission_mode: Option<String>,
        #[arg(
            long,
            help_heading = "Claude Code Options",
            help = "Allowed tools (space-separated)"
        )]
        allowed_tools: Option<String>,
        #[arg(
            long,
            help_heading = "Claude Code Options",
            help = "Disallowed tools (space-separated)"
        )]
        disallowed_tools: Option<String>,
        #[arg(
            long,
            help_heading = "Claude Code Options",
            help = "System prompt override"
        )]
        system_prompt: Option<String>,
        #[arg(
            long,
            help_heading = "Claude Code Options",
            help = "Append to default system prompt"
        )]
        append_system_prompt: Option<String>,
        #[arg(
            long,
            env = "PMUX_EFFORT",
            help_heading = "Claude Code Options",
            help = "Effort level: low, medium, high"
        )]
        effort: Option<String>,
        #[arg(
            long,
            help_heading = "Claude Code Options",
            help = "Maximum budget in USD"
        )]
        max_budget: Option<f64>,
        /// Timeout in seconds (default: 120)
        #[arg(long, env = "PMUX_TIMEOUT", default_value = "120")]
        timeout: u64,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Stream watch events as NDJSON to stderr
        #[arg(long)]
        stream: bool,
        /// Keep session alive after prompt completes (for follow-up prompts)
        #[arg(long)]
        keep_alive: bool,
    },
    /// Create a config file with CLI defaults (model, permissions, cwd, etc.).
    Init {
        #[arg(long, default_value = "opus")]
        model: String,
        #[arg(long, default_value = "bypassPermissions")]
        permission_mode: String,
        #[arg(long, default_value = ".")]
        cwd: String,
        #[arg(long, default_value = "300")]
        timeout: u64,
        #[arg(long, default_value = "high")]
        effort: String,
    },
    /// Show the resolved config (file path + values).
    #[command(name = "config")]
    ConfigShow,
    /// Send a prompt and wait for the agent response (SDK-grade blocking primitive).
    #[command(name = "prompt")]
    SdkPrompt {
        /// Session ID (UUID or prefix)
        session: String,
        /// Prompt text to send (mutually exclusive with --file)
        #[arg(long, conflicts_with = "file")]
        text: Option<String>,
        /// Read prompt text from a file (use `-` for stdin)
        #[arg(long, conflicts_with = "text")]
        file: Option<String>,
        /// Timeout in seconds (default: 120)
        #[arg(long, default_value = "120")]
        timeout: u64,
        /// Output as JSON (includes tools used, duration, state)
        #[arg(long)]
        json: bool,
        /// Stream watch events as NDJSON to stderr while the turn runs
        #[arg(long)]
        stream: bool,
    },
    /// Send a protocol-aware key event (uses terminal capability negotiation).
    InputKey {
        session: String,
        /// Key name (e.g. Enter, Tab, Escape, Ctrl-c, Alt-x, Up, Down, F1, etc.)
        key: String,
    },
    /// Send a named agent action (resolved via input profile).
    InputAction {
        session: String,
        /// Action name (e.g. submit, interrupt, variants, agents, etc.)
        action: String,
    },
    /// Send text and submit (convenience: `send_text` + `send_action("submit`")).
    InputPrompt {
        session: String,
        #[arg(long)]
        text: String,
    },
    /// Get the current negotiated terminal state.
    TerminalState {
        session: String,
        #[arg(long, value_enum, default_value_t = StateFormat::Text)]
        format: StateFormat,
    },
    /// Get current VTE screen content text (no ANSI, content region only).
    ScreenText {
        session: String,
        #[arg(long)]
        status: bool,
    },
    /// Get current VTE-inferred agent state.
    AgentState {
        session: String,
        #[arg(long, value_enum, default_value_t = StateFormat::Text)]
        format: StateFormat,
    },
    /// Stream VTE semantic events for a session.
    Events {
        session: String,
        /// Maximum duration in milliseconds (0 = default 30s).
        #[arg(long, default_value_t = 0)]
        timeout_ms: u64,
        /// Maximum number of events (0 = unlimited).
        #[arg(long, default_value_t = 0)]
        max_events: u64,
    },
    /// Get content buffer entries for a session.
    Content {
        session: String,
        /// Get entries since this sequence number.
        #[arg(long)]
        since_seq: Option<u64>,
        /// Get entries since last user input (default).
        #[arg(long)]
        since_last_input: bool,
        /// Get last N entries.
        #[arg(long)]
        last: Option<usize>,
        /// Output raw (unfiltered) content. Default is filtered.
        #[arg(long)]
        raw: bool,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Stream watch events for a session.
    Watch {
        session: String,
        /// Output as NDJSON.
        #[arg(long)]
        json: bool,
        /// Maximum duration in milliseconds (0 = default 30s).
        #[arg(long, default_value_t = 0)]
        timeout_ms: u64,
        /// Maximum number of events (0 = unlimited).
        #[arg(long, default_value_t = 0)]
        max_events: u64,
    },
    /// Respond to a confirmation/permission prompt.
    Confirm {
        session: String,
        /// Accept the confirmation (yes). Default if neither --yes nor --no given.
        #[arg(long, short = 'y')]
        yes: bool,
        /// Reject the confirmation (no).
        #[arg(long, short = 'n')]
        no: bool,
    },
    /// Get current content buffer sequence number.
    ContentSeq {
        session: String,
    },
    /// [hidden] Run a demo session in-process (no daemon needed)
    #[command(hide = true)]
    Demo {
        #[arg(default_value = "shell")]
        agent: String,
        #[arg(long, default_value_t = 2000)]
        linger_ms: u64,
        #[arg(long)]
        interactive: bool,
        #[arg(long)]
        keep_alive: bool,
        #[arg(trailing_var_arg = true)]
        command: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load ~/.config/pseudomux/config.toml and inject as PMUX_* env vars
    // before Clap parses, so config values become Clap defaults.
    config::load_and_apply();
    let cli = Cli::parse();
    match cli.command {
        Commands::Demo {
            agent,
            linger_ms,
            command,
            interactive,
            keep_alive,
        } => commands::session::handle_demo(agent, linger_ms, command, interactive, keep_alive)?,
        Commands::Init {
            model,
            permission_mode,
            cwd,
            timeout,
            effort,
        } => {
            let dir = config::config_dir();
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("config.toml");
            let text =
                config::default_config_text(&model, &permission_mode, &cwd, timeout, &effort);
            std::fs::write(&path, &text)?;
            eprintln!("Wrote {}", path.display());
            eprintln!("{text}");
        }
        Commands::ConfigShow => match config::config_path() {
            Some(path) => {
                eprintln!("config file: {}", path.display());
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                print!("{text}");
            }
            None => eprintln!("No config file found. Run `pmux init` to create one."),
        },
        other => {
            let sockets = match cli.socket {
                Some(path) => vec![path],
                None => socket_candidates(),
            };
            let client = DaemonClient::new(sockets);
            dispatch(&client, cli.profile, other).await?;
        }
    }
    Ok(())
}

async fn dispatch(client: &DaemonClient, profile: Option<String>, command: Commands) -> Result<()> {
    match command {
        Commands::Ping { json } => commands::session::handle_ping(client, json).await?,
        Commands::Start {
            agent,
            name,
            cwd,
            rows,
            cols,
            model,
            permission_mode,
            allowed_tools,
            disallowed_tools,
            system_prompt,
            append_system_prompt,
            effort,
            max_budget,
            record,
            agent_args,
        } => {
            commands::session::handle_start(
                client,
                profile,
                agent,
                name,
                cwd,
                rows,
                cols,
                model,
                permission_mode,
                allowed_tools,
                disallowed_tools,
                system_prompt,
                append_system_prompt,
                effort,
                max_budget,
                record,
                agent_args,
            )
            .await?
        }
        Commands::Run {
            text,
            file,
            agent,
            name,
            cwd,
            rows,
            cols,
            model,
            permission_mode,
            allowed_tools,
            disallowed_tools,
            system_prompt,
            append_system_prompt,
            effort,
            max_budget,
            timeout,
            json,
            stream,
            keep_alive,
        } => {
            let resolved_text = commands::prompt::resolve_prompt_text(text, file)?;
            commands::run::handle_run(
                client,
                commands::run::RunOptions {
                    profile,
                    agent,
                    cwd,
                    rows,
                    cols,
                    model,
                    permission_mode,
                    allowed_tools,
                    disallowed_tools,
                    system_prompt,
                    append_system_prompt,
                    effort,
                    max_budget,
                    name,
                    text: resolved_text,
                    timeout,
                    json,
                    stream,
                    keep_alive,
                },
            )
            .await?
        }
        Commands::Send { session, text } => {
            commands::input::handle_send(client, &session, text).await?
        }
        Commands::Read { session, since } => {
            commands::query::handle_read(client, &session, since).await?
        }
        Commands::Resize {
            session,
            rows,
            cols,
        } => commands::session::handle_resize(client, &session, rows, cols).await?,
        Commands::Interrupt { session } => {
            commands::session::handle_interrupt(client, &session).await?
        }
        Commands::Stop { session } => commands::session::handle_stop(client, &session).await?,
        Commands::List => commands::session::handle_list(client).await?,
        Commands::Attach { session } => commands::session::handle_attach(client, &session).await?,
        Commands::SdkPrompt {
            session,
            text,
            file,
            timeout,
            json,
            stream,
        } => {
            let resolved_text = commands::prompt::resolve_prompt_text(text, file)?;
            commands::prompt::handle_sdk_prompt(
                client,
                &session,
                resolved_text,
                timeout,
                json,
                stream,
            )
            .await?
        }
        Commands::InputKey { session, key } => {
            commands::input::handle_input_key(client, &session, key).await?
        }
        Commands::InputAction { session, action } => {
            commands::input::handle_input_action(client, &session, action).await?
        }
        Commands::InputPrompt { session, text } => {
            commands::input::handle_input_prompt(client, &session, text).await?
        }
        Commands::TerminalState { session, format } => {
            commands::query::handle_terminal_state(client, &session, format).await?
        }
        Commands::ScreenText { session, status } => {
            commands::query::handle_screen_text(client, &session, status).await?
        }
        Commands::AgentState { session, format } => {
            commands::query::handle_agent_state(client, &session, format).await?
        }
        Commands::Events {
            session,
            timeout_ms,
            max_events,
        } => commands::query::handle_events(client, &session, timeout_ms, max_events).await?,
        Commands::Content {
            session,
            since_seq,
            since_last_input: _,
            last,
            raw,
            json,
        } => commands::query::handle_content(client, &session, since_seq, last, raw, json).await?,
        Commands::Watch {
            session,
            json,
            timeout_ms,
            max_events,
        } => commands::query::handle_watch(client, &session, json, timeout_ms, max_events).await?,
        Commands::Confirm {
            session,
            yes: _,
            no,
        } => commands::input::handle_confirm(client, &session, no).await?,
        Commands::ContentSeq { session } => {
            commands::query::handle_content_seq(client, &session).await?
        }
        Commands::Demo { .. } | Commands::Init { .. } | Commands::ConfigShow => unreachable!(),
    }
    Ok(())
}
