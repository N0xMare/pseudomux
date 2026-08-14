#![cfg(unix)]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use pseudomux_client::{PmuxClient, exact_environment_snapshot, normalize_cli_prompt};
use pseudomux_protocol::v1::{
    AuthPolicy, ClaudeLaunchConfig, CompatibilityPolicy, ConfigSource, EffortLevel,
    EnvironmentSpec, LifecycleMode, MAX_SAFE_JSON_INTEGER, PermissionMode, RetentionPolicy,
    RunOnceRequest, SessionCell, SessionIdentity, StartSessionRequest, SystemPromptPolicy,
    TerminalSpec, TurnOutcome, TurnRequest,
};
use uuid::Uuid;

const MAX_PROMPT_BYTES: u64 = 1024 * 1024;

/// See `bin/pmux/src/cli.rs`: the daemon filters the inherited snapshot with an
/// allowlist, and `EnvironmentSpec::set` is the only channel it does not filter.
/// The facade is bounded, but it is not exempt from that: a `claude -p` caller
/// whose MCP server needs a token would otherwise have no way to deliver it.
const ENVIRONMENT_HELP_HEADING: &str =
    "Launch environment (allowlisted: an inherited name pmux does not recognize is dropped)";

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum OutputFormat {
    #[default]
    Text,
    Json,
    StreamJson,
}

#[derive(Debug, Parser)]
#[command(
    name = "claude-p",
    version,
    about = "Bounded compatibility facade over interactive pmux",
    disable_help_subcommand = true
)]
struct Args {
    /// Compatibility marker only. Claude is never launched with --print.
    #[arg(short = 'p', long = "print")]
    print_marker: bool,

    #[arg(long, env = "PSEUDOMUX_SOCKET")]
    socket: PathBuf,

    #[arg(long, env = "PMUX_CLAUDE_BIN", default_value = "claude")]
    claude_bin: PathBuf,

    #[arg(long, default_value = ".")]
    cwd: PathBuf,

    #[arg(long, conflicts_with = "resume")]
    session_id: Option<Uuid>,

    #[arg(long, conflicts_with = "session_id")]
    resume: Option<Uuid>,

    #[arg(long)]
    model: Option<String>,

    #[arg(long, value_enum)]
    effort: Option<FacadeEffort>,

    #[arg(long, value_enum)]
    permission_mode: Option<FacadePermissionMode>,

    #[arg(long = "allowedTools")]
    allowed_tools: Vec<String>,

    #[arg(long = "disallowedTools")]
    denied_tools: Vec<String>,

    #[arg(long)]
    settings: Vec<PathBuf>,

    #[arg(long = "mcp-config")]
    mcp_configs: Vec<PathBuf>,

    #[arg(long = "plugin-dir")]
    plugin_dirs: Vec<PathBuf>,

    #[arg(long, conflicts_with = "append_system_prompt")]
    system_prompt: Option<String>,

    #[arg(long, conflicts_with = "system_prompt")]
    append_system_prompt: Option<String>,

    #[arg(long, value_enum, default_value_t)]
    output_format: OutputFormat,

    #[arg(long, default_value_t = 300)]
    timeout_seconds: u64,

    /// Deliver KEY=VALUE to Claude verbatim; repeatable. The explicit `set`
    /// channel bypasses the launch allowlist, so this is how a name the
    /// allowlist drops is restored. VALUE is visible in this process's argv
    /// (`ps`); use --env-passthrough for anything secret.
    #[arg(long = "env", value_name = "KEY=VALUE", help_heading = ENVIRONMENT_HELP_HEADING)]
    env: Vec<String>,

    /// Forward KEY from claude-p's own environment to Claude; repeatable. Only
    /// the name is written on the command line, so the value never reaches `ps`
    /// output. Fails when KEY is unset or empty in this process.
    #[arg(long = "env-passthrough", value_name = "KEY", help_heading = ENVIRONMENT_HELP_HEADING)]
    env_passthrough: Vec<String>,

    /// Drop KEY from the inherited snapshot before the launch environment is
    /// built; repeatable. Applied before --env/--env-passthrough.
    #[arg(long = "unset", value_name = "KEY", help_heading = ENVIRONMENT_HELP_HEADING)]
    unset: Vec<String>,

    /// One prompt. When omitted, UTF-8 text is read from stdin.
    prompt: Option<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum FacadeEffort {
    Low,
    Medium,
    High,
    #[value(name = "xhigh")]
    XHigh,
    Max,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum FacadePermissionMode {
    Default,
    AcceptEdits,
    Plan,
    Auto,
    BypassPermissions,
    DontAsk,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let prompt = read_prompt(args.prompt.clone())?;
    let request = build_request(&args, prompt)?;
    let client = PmuxClient::new(args.socket.clone())?;
    let result = client.run_once(request).await?;
    emit_result(args.output_format, &result)?;
    if result.outcome != TurnOutcome::Completed {
        bail!("Claude turn ended with {:?}", result.outcome);
    }
    Ok(())
}

fn read_prompt(argument: Option<String>) -> Result<String> {
    let prompt = match argument {
        Some(prompt) => prompt,
        None if std::io::stdin().is_terminal() => {
            bail!("a positional prompt or piped stdin is required")
        }
        None => {
            let mut bytes = Vec::new();
            std::io::stdin()
                .take(MAX_PROMPT_BYTES + 1)
                .read_to_end(&mut bytes)
                .context("failed to read prompt from stdin")?;
            if bytes.len() as u64 > MAX_PROMPT_BYTES {
                bail!("prompt exceeds the {MAX_PROMPT_BYTES}-byte facade limit");
            }
            String::from_utf8(bytes).context("prompt must be valid UTF-8")?
        }
    };
    if prompt.len() as u64 > MAX_PROMPT_BYTES {
        bail!("prompt exceeds the {MAX_PROMPT_BYTES}-byte facade limit");
    }
    // `echo q | claude-p` is what this facade is for, and the pipe carries the
    // terminator every producer writes. `pmux` measured what arming a turn with
    // it costs; the rule and that measurement are in `normalize_cli_prompt`,
    // which both binaries now call rather than each keeping a copy.
    let prompt = normalize_cli_prompt(&prompt);
    if prompt.trim().is_empty() {
        bail!("prompt must not be empty");
    }
    if prompt.chars().any(|character| {
        character == '\0'
            || character == '\u{1b}'
            || (character.is_control() && !matches!(character, '\n' | '\t'))
    }) {
        bail!("prompt contains an unsafe control character");
    }
    if prompt.trim_start().starts_with('/') {
        bail!("slash commands are unsupported by the compatibility facade");
    }
    Ok(prompt)
}

fn build_request(args: &Args, prompt: String) -> Result<RunOnceRequest> {
    let cwd = args.cwd.canonicalize().context("invalid --cwd")?;
    let executable = resolve_executable(&args.claude_bin)?;
    let identity = match args.resume {
        Some(session_id) => SessionIdentity::Resume { session_id },
        None => SessionIdentity::New {
            session_id: args.session_id,
        },
    };
    let settings = config_files(&args.settings, "--settings")?;
    let mcp_configs = config_files(&args.mcp_configs, "--mcp-config")?;
    let plugin_dirs = args
        .plugin_dirs
        .iter()
        .map(|path| canonical_string(path, "--plugin-dir"))
        .collect::<Result<Vec<_>>>()?;
    let system_prompt = match (&args.system_prompt, &args.append_system_prompt) {
        (Some(prompt), None) => SystemPromptPolicy::Replace {
            prompt: prompt.clone(),
        },
        (None, Some(prompt)) => SystemPromptPolicy::Append {
            prompt: prompt.clone(),
        },
        (None, None) => SystemPromptPolicy::Default,
        (Some(_), Some(_)) => unreachable!("clap enforces conflicts"),
    };
    let now_ms = unix_ms()?;
    let deadline = deadline_from_timeout(now_ms, args.timeout_seconds)?;

    Ok(RunOnceRequest {
        session: StartSessionRequest {
            identity,
            cwd: cwd.to_string_lossy().into_owned(),
            // The `claude -p` façade never names a stored agent: it
            // impersonates one exact command line, and a stored configuration
            // would make the argv a function of something the caller did not
            // type.
            agent: None,
            claude: Some(ClaudeLaunchConfig {
                executable: executable.to_string_lossy().into_owned(),
                model: args.model.clone(),
                effort: args.effort.map(Into::into),
                permission_mode: args.permission_mode.map(Into::into),
                allowed_tools: args.allowed_tools.clone(),
                denied_tools: args.denied_tools.clone(),
                settings,
                mcp_configs,
                plugin_dirs,
                system_prompt,
                extra_args: Vec::new(),
            }),
            environment: launch_environment(args)?,
            auth_policy: AuthPolicy::Subscription,
            // The façade impersonates `claude -p`, which reads the caller's own
            // configuration root. A private root would silently change which
            // CLAUDE.md, settings and MCP servers the command sees.
            config_isolation: None,
            terminal: TerminalSpec::default(),
            lifecycle: LifecycleMode::Transcript,
            retention: RetentionPolicy::OneShot,
            compatibility: CompatibilityPolicy::RequireTested,
            // The `claude -p` façade is a tool-capable one-shot by definition.
            // Path B's cell is a pool concern with an empty tool surface and
            // nothing here would select it.
            cell: SessionCell::Full,
        },
        turn: TurnRequest {
            turn_id: Uuid::new_v4(),
            prompt,
            deadline_unix_ms: Some(deadline),
            lease: Default::default(),
        },
    })
}

/// The exact caller snapshot plus the explicit patch from `--env`,
/// `--env-passthrough`, and `--unset`.
///
/// No environment **value** is echoed in a diagnostic here: a malformed `--env`
/// is identified by position, because the text after `=` may be a credential.
/// The same rule and the same wording are used by `bin/pmux/src/cli.rs`.
fn launch_environment(args: &Args) -> Result<EnvironmentSpec> {
    let (set, unset) = environment_patch(args, &|name| std::env::var_os(name))?;
    let mut environment = exact_environment_snapshot()?;
    environment.set = set;
    environment.unset = unset;
    Ok(environment)
}

/// `lookup` is the caller's own environment. It is a parameter so the unit test
/// can exercise `--env-passthrough` without mutating the process environment,
/// which is `unsafe` and racy under a threaded test harness.
fn environment_patch(
    args: &Args,
    lookup: &dyn Fn(&str) -> Option<OsString>,
) -> Result<(BTreeMap<String, String>, BTreeSet<String>)> {
    let mut set: BTreeMap<String, String> = BTreeMap::new();

    for (index, entry) in args.env.iter().enumerate() {
        let position = index + 1;
        let Some((key, value)) = entry.split_once('=') else {
            bail!(
                "--env argument {position} is not KEY=VALUE: no `=` separator \
                 (the argument text is withheld because it may be a secret)"
            );
        };
        validate_environment_name(key, "--env")?;
        if value.contains('\0') {
            bail!("--env argument {position} ({key}) has a value containing NUL");
        }
        insert_environment_value(&mut set, key, value.to_owned())?;
    }

    for name in &args.env_passthrough {
        validate_environment_name(name, "--env-passthrough")?;
        let Some(value) = lookup(name) else {
            bail!(
                "--env-passthrough {name}: {name} is not set in claude-p's own environment; \
                 export it before launching, or pass --env {name}=VALUE"
            );
        };
        let value = value
            .into_string()
            .map_err(|_| anyhow::anyhow!("--env-passthrough {name}: {name} is not valid UTF-8"))?;
        if value.is_empty() {
            bail!(
                "--env-passthrough {name}: {name} is set but empty; export a non-empty value \
                 before launching"
            );
        }
        insert_environment_value(&mut set, name, value)?;
    }

    let mut unset = BTreeSet::new();
    for name in &args.unset {
        validate_environment_name(name, "--unset")?;
        unset.insert(name.clone());
    }
    if let Some(key) = unset.iter().find(|key| set.contains_key(*key)) {
        bail!("environment variable {key} is both unset and set; choose one");
    }

    Ok((set, unset))
}

fn validate_environment_name(name: &str, flag: &str) -> Result<()> {
    if name.is_empty() {
        bail!("{flag} requires a non-empty environment variable name");
    }
    if name.contains('=') {
        bail!("{flag} name {name:?} may not contain `=`");
    }
    if name.contains('\0') {
        bail!("{flag} name may not contain NUL");
    }
    Ok(())
}

fn insert_environment_value(
    set: &mut BTreeMap<String, String>,
    key: &str,
    value: String,
) -> Result<()> {
    if set.insert(key.to_owned(), value).is_some() {
        bail!("environment variable {key} is provided more than once by --env/--env-passthrough");
    }
    Ok(())
}

fn deadline_from_timeout(now_ms: u64, timeout_seconds: u64) -> Result<u64> {
    if timeout_seconds == 0 {
        bail!("--timeout-seconds must be non-zero");
    }
    let timeout_ms = timeout_seconds
        .checked_mul(1_000)
        .context("turn timeout conversion overflow")?;
    let deadline = now_ms
        .checked_add(timeout_ms)
        .context("turn deadline overflow")?;
    if deadline > MAX_SAFE_JSON_INTEGER {
        bail!("turn deadline exceeds protocol-v1's safe-integer domain");
    }
    Ok(deadline)
}

fn config_files(paths: &[PathBuf], label: &str) -> Result<Vec<ConfigSource>> {
    paths
        .iter()
        .map(|path| {
            Ok(ConfigSource::File {
                path: canonical_string(path, label)?,
            })
        })
        .collect()
}

fn canonical_string(path: &Path, label: &str) -> Result<String> {
    Ok(path
        .canonicalize()
        .with_context(|| format!("invalid {label} path: {}", path.display()))?
        .to_string_lossy()
        .into_owned())
}

fn resolve_executable(value: &Path) -> Result<PathBuf> {
    if value.components().count() > 1 || value.is_absolute() {
        return value
            .canonicalize()
            .with_context(|| format!("Claude executable is unavailable: {}", value.display()));
    }
    let path = std::env::var_os("PATH").context("PATH is unavailable")?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(value);
        if candidate.is_file() {
            return candidate
                .canonicalize()
                .context("failed to resolve Claude executable");
        }
    }
    bail!("Claude executable {value:?} was not found in PATH")
}

fn unix_ms() -> Result<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis()
        .try_into()
        .context("system clock exceeds protocol range")
}

fn emit_result(format: OutputFormat, result: &pseudomux_protocol::v1::TurnResult) -> Result<()> {
    match format {
        OutputFormat::Text => print!("{}", result.text),
        OutputFormat::Json => println!("{}", serde_json::to_string(result)?),
        OutputFormat::StreamJson => {
            println!(
                "{}",
                serde_json::json!({
                    "type": "system",
                    "subtype": "init",
                    "session_id": result.session_id,
                    "provenance": "pmux_interactive_transcript_reconstruction",
                    "claude_version": result.claude_version,
                })
            );
            println!(
                "{}",
                serde_json::json!({
                    "type": "assistant",
                    "message": {
                        "content": result.final_blocks,
                        "model": result.model,
                        "stop_reason": result.stop_reason,
                        "usage": result.usage.main,
                    }
                })
            );
            println!(
                "{}",
                serde_json::json!({
                    "type": "result",
                    "subtype": result_subtype(result.outcome),
                    "is_error": result.outcome == TurnOutcome::Failed,
                    "session_id": result.session_id,
                    "turn_id": result.turn_id,
                    "result": result.text,
                    "usage": result.usage,
                    "provenance": "pmux_interactive_transcript_reconstruction",
                    "token_deltas": false,
                })
            );
        }
    }
    Ok(())
}

const fn result_subtype(outcome: TurnOutcome) -> &'static str {
    match outcome {
        TurnOutcome::Completed => "success",
        TurnOutcome::Cancelled => "cancelled",
        TurnOutcome::Failed => "error",
    }
}

impl From<FacadeEffort> for EffortLevel {
    fn from(value: FacadeEffort) -> Self {
        match value {
            FacadeEffort::Low => Self::Low,
            FacadeEffort::Medium => Self::Medium,
            FacadeEffort::High => Self::High,
            FacadeEffort::XHigh => Self::XHigh,
            FacadeEffort::Max => Self::Max,
        }
    }
}

impl From<FacadePermissionMode> for PermissionMode {
    fn from(value: FacadePermissionMode) -> Self {
        match value {
            FacadePermissionMode::Default => Self::Default,
            FacadePermissionMode::AcceptEdits => Self::AcceptEdits,
            FacadePermissionMode::Plan => Self::Plan,
            FacadePermissionMode::Auto => Self::Auto,
            FacadePermissionMode::BypassPermissions => Self::BypassPermissions,
            FacadePermissionMode::DontAsk => Self::DontAsk,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_validation_rejects_slash_and_escape() {
        assert!(read_prompt(Some("/compact".into())).is_err());
        assert!(read_prompt(Some("unsafe\u{1b}".into())).is_err());
        assert!(read_prompt(Some("unsafe\u{1}".into())).is_err());
        assert!(read_prompt(Some("unsafe\u{7f}".into())).is_err());
        assert!(read_prompt(Some("x".repeat(MAX_PROMPT_BYTES as usize + 1))).is_err());
        assert_eq!(
            read_prompt(Some("hello\r\nworld".into())).unwrap(),
            "hello\nworld"
        );
    }

    #[test]
    fn output_format_defaults_to_text() {
        assert!(matches!(OutputFormat::default(), OutputFormat::Text));
    }

    #[test]
    fn failed_results_are_never_labeled_success() {
        assert_eq!(result_subtype(TurnOutcome::Completed), "success");
        assert_eq!(result_subtype(TurnOutcome::Cancelled), "cancelled");
        assert_eq!(result_subtype(TurnOutcome::Failed), "error");
    }

    fn args(arguments: &[&str]) -> Args {
        let mut argv = vec!["claude-p", "--socket", "/tmp/pmux.sock"];
        argv.extend_from_slice(arguments);
        argv.push("prompt");
        Args::try_parse_from(argv).unwrap()
    }

    /// The facade builds its own `StartSessionRequest`, so the daemon's
    /// launch allowlist drops the same 68-of-78 names here that it drops for
    /// `pmux`. Without these three flags a `claude -p` migration has no way to
    /// deliver an MCP server's token at all.
    fn caller_environment(name: &str) -> Option<OsString> {
        match name {
            "FORWARDED" => Some(OsString::from("forwarded-value")),
            "FORWARDED_EMPTY" => Some(OsString::new()),
            _ => None,
        }
    }

    fn patch(arguments: &[&str]) -> Result<(BTreeMap<String, String>, BTreeSet<String>)> {
        environment_patch(&args(arguments), &caller_environment)
    }

    /// The facade builds its own `StartSessionRequest`, so the daemon's launch
    /// allowlist drops the same names here that it drops for `pmux`. Without
    /// these three flags a `claude -p` migration has no way to deliver an MCP
    /// server's token at all.
    #[test]
    fn the_explicit_environment_channel_is_reachable_from_the_facade() {
        let (set, unset) = patch(&[
            "--env",
            "EXPLICIT=a=b",
            "--env-passthrough",
            "FORWARDED",
            "--unset",
            "LANG",
        ])
        .unwrap();

        assert_eq!(set.get("EXPLICIT").map(String::as_str), Some("a=b"));
        assert_eq!(
            set.get("FORWARDED").map(String::as_str),
            Some("forwarded-value"),
            "the value reaches `set` while only the name is on the command line"
        );
        assert_eq!(unset, BTreeSet::from(["LANG".to_owned()]));
    }

    #[test]
    fn malformed_environment_arguments_are_rejected_without_echoing_a_value() {
        let separator = patch(&["--env", "NO_SEPARATOR"]).unwrap_err().to_string();
        assert!(separator.contains("no `=` separator"), "{separator}");

        let nul = patch(&["--env", "KEY=before\u{0}after"])
            .unwrap_err()
            .to_string();
        assert!(nul.contains("value containing NUL"), "{nul}");
        assert!(!nul.contains("after"), "the value was echoed: {nul}");

        for arguments in [
            vec!["--env", "=orphan"],
            vec!["--env-passthrough", "HAS=EQUALS"],
            vec!["--unset", ""],
            vec!["--env", "DUP=one", "--env", "DUP=two"],
            vec!["--env", "BOTH=one", "--unset", "BOTH"],
        ] {
            assert!(patch(&arguments).is_err(), "{arguments:?} was accepted");
        }

        let absent = patch(&["--env-passthrough", "DEFINITELY_UNSET"])
            .unwrap_err()
            .to_string();
        assert!(absent.contains("DEFINITELY_UNSET"), "{absent}");
        assert!(absent.contains("is not set"), "{absent}");

        let empty = patch(&["--env-passthrough", "FORWARDED_EMPTY"])
            .unwrap_err()
            .to_string();
        assert!(empty.contains("is set but empty"), "{empty}");
    }

    #[test]
    fn timeout_conversion_is_exact_and_fails_before_overflow_or_saturation() {
        assert!(deadline_from_timeout(1, 0).is_err());
        assert_eq!(deadline_from_timeout(1, 1).unwrap(), 1_001);

        let now_ms = MAX_SAFE_JSON_INTEGER % 1_000;
        let maximum_seconds = (MAX_SAFE_JSON_INTEGER - now_ms) / 1_000;
        assert_eq!(
            deadline_from_timeout(now_ms, maximum_seconds).unwrap(),
            MAX_SAFE_JSON_INTEGER
        );
        assert!(deadline_from_timeout(now_ms, maximum_seconds + 1).is_err());
        assert!(deadline_from_timeout(now_ms, u64::MAX).is_err());
        assert!(deadline_from_timeout(u64::MAX, 1).is_err());
    }
}
