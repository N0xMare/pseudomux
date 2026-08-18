use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::io::{self, IsTerminal, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use pseudomux_client::{
    AgentProfile, exact_environment_snapshot, load_agent_profile, verify_required_environment,
};
use pseudomux_protocol::v1::launch_environment::{self, SUBSCRIPTION_AUTH_KEYS};
use pseudomux_protocol::v1::{
    AgentContainment, AgentRef, AgentSpec, AgentVersion, AuthPolicy, ClaudeLaunchConfig,
    CompatibilityPolicy, ConfigIsolation, ConfigSource, DisconnectAction, EffortLevel,
    InputTransport, LifecycleMode, PermissionMode, RetentionPolicy, SessionCell, SessionIdentity,
    StartSessionRequest, SystemPromptPolicy, TerminalProfile, TerminalSpec, TurnLeasePolicy,
    TurnRequest,
};
use uuid::Uuid;

pub const MAX_PROMPT_BYTES: u64 = 1024 * 1024;
const MAX_PROMPT_SOURCE_BYTES: u64 = MAX_PROMPT_BYTES * 2 + 1;

/// Terminal and lifecycle defaults. They live here rather than in
/// `default_value_t` because a clap default is indistinguishable from an
/// explicit flag, and an agent profile has to be able to set them.
///
/// THESE TWO ARE NOW DELIVERED. For as long as they have existed, every
/// measured pane rendered 24x80: rmux created the private session at its own
/// `DEFAULT_SESSION_SIZE`, and the pane resize that was supposed to correct it
/// could not make a lone pane wider than its window, so 120 collapsed to 80 and
/// the resize still reported success. `pseudomux_rmux::backend` resizes the
/// WINDOW now, and
/// `crates/service/tests/private_runtime.rs::a_terminal_is_created_at_the_requested_geometry_and_not_the_rmux_default`
/// asserts the delivered snapshot against the request. That test is what keeps
/// these numbers honest: every minified-cell screen predicate is calibrated
/// against a real pane, so a requested geometry that is fiction is a trap for
/// the next calibration rather than a cosmetic wish.
const DEFAULT_ROWS: u16 = 24;
const DEFAULT_COLS: u16 = 120;
const DEFAULT_HOOK_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_IDLE_TTL_SECS: u64 = 1_800;

/// Groups the three flags that reach `EnvironmentSpec` in `--help`, and states
/// the allowlist in the one place a caller is guaranteed to read.
const ENVIRONMENT_HELP_HEADING: &str =
    "Launch environment (allowlisted: an inherited name pmux does not recognize is dropped)";

#[derive(Parser, Debug)]
#[command(
    name = "pmux",
    version,
    about = "Native pmux protocol-v1 CLI",
    long_about = "Native pmux protocol-v1 CLI.

`run` is the product: one stateless `(model, effort, prompt)` turn against a
pool of embedded Claude Code processes. The caller names no resource. `pmuxd`
must have been started with --path-b-parent or every `run` is refused.

`ping` and `doctor` start nothing and spend no tokens.

The other subcommands are Path A (experimental): interactive sessions where
you name the working directory, the Claude executable and the configuration
root. `oneshot`, `start`, `turn`, `inspect`, `cancel`, `close`, `attach` and
`probe` are that surface. `clear` is a Path A call against a Path B cell."
)]
pub struct Cli {
    /// Exact pmuxd Unix socket. No discovery or daemon startup is performed.
    #[arg(long, env = "PMUX_SOCKET")]
    pub socket: PathBuf,

    /// Output representation. `json` is one object; `ndjson` is one
    /// `{"type","data"}` record per line. Only `oneshot` and `turn` stream turn
    /// events ahead of the result; every other subcommand emits exactly one
    /// record in either mode.
    #[arg(long, value_enum, default_value_t, global = true)]
    pub output: OutputMode,

    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    /// Parses argv AND records which launch values came from the environment.
    ///
    /// `Cli::parse()` cannot answer the second half: `ArgMatches` knows whether
    /// a value came from the command line, from `env`, or from a default, and
    /// the derived struct is the same `Option<String>` for all three. This is
    /// the one place that distinction still exists, so it is captured here and
    /// carried on [`LaunchArgs::from_environment`].
    ///
    /// It is not a convenience. `--agent` refuses a launch flag the caller
    /// NAMED; refusing one the caller's shell rc exported is refusing them for
    /// something they did not do on this command, and it locked anyone with
    /// `PMUX_MODEL` set out of `--agent` entirely.
    #[must_use]
    pub fn parse_recording_argument_sources() -> Self {
        let matches = <Self as clap::CommandFactory>::command().get_matches();
        let mut parsed = <Self as clap::FromArgMatches>::from_arg_matches(&matches)
            .unwrap_or_else(|error| error.exit());
        parsed.record_launch_argument_sources(&matches);
        parsed
    }

    /// Fills [`LaunchArgs::from_environment`] from clap's own value sources.
    fn record_launch_argument_sources(&mut self, matches: &clap::ArgMatches) {
        let Some((_, subcommand)) = matches.subcommand() else {
            return;
        };
        let launch = match &mut self.command {
            Command::Oneshot { launch, .. } | Command::Start { launch } => launch,
            _ => return,
        };
        launch.from_environment = subcommand
            .ids()
            .filter(|id| {
                matches!(
                    subcommand.value_source(id.as_str()),
                    Some(clap::parser::ValueSource::EnvVariable)
                )
            })
            .map(|id| id.as_str().to_owned())
            .collect();
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum OutputMode {
    #[default]
    Text,
    Json,
    Ndjson,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Neither path: ask the daemon for its version and protocol number.
    ///
    /// Starts nothing, spends no tokens, and reaches only the accept loop. Use
    /// `pmux doctor` for anything behind it.
    Ping,
    /// Path B: run one stateless turn against the embedded Claude Code pool.
    ///
    /// Requires a `pmuxd` started with `--path-b-parent`; without one every
    /// `run` is refused with `unsupported_feature`.
    ///
    /// THE CALLER NAMES NO RESOURCE. There is no `--cwd`, no
    /// `--config-isolation-root`, no `--claude`, no `--system-prompt`, no
    /// session id and no generation on this subcommand, and their absence is
    /// the product rather than an omission: the daemon mints every one of them
    /// from its own configuration plus a slot identity.
    ///
    /// `(model, effort, prompt) -> text + usage`, and nothing else.
    #[command(alias = "ask")]
    Run {
        /// Claude model alias or exact id, e.g. `opus`, `sonnet`,
        /// `claude-opus-5`. Required: it is half the pool's class key, and an
        /// absent model would partition the pool on whatever the daemon's
        /// configuration happens to default to.
        #[arg(long)]
        model: String,
        /// Reasoning depth. Omit for the resolved model's own default.
        /// Validated against the RESOLVED model by the daemon, never against
        /// this list alone -- tiers are not uniform across Claude models.
        #[arg(long, value_enum)]
        effort: Option<EffortArg>,
        #[command(flatten)]
        prompt: PromptArgs,
        /// Absolute wall-clock deadline for the answer. Omit for daemon policy.
        /// It may only SHORTEN pmux's wait; nothing here lengthens one.
        #[arg(long)]
        deadline_unix_ms: Option<u64>,
    },
    /// Path A (experimental): start, run one turn, and close one interactive Claude session.
    Oneshot {
        #[command(flatten)]
        launch: LaunchArgs,
        #[command(flatten)]
        prompt: PromptArgs,
        #[command(flatten)]
        turn: TurnArgs,
    },
    /// Path A (experimental): start a persistent interactive Claude session.
    ///
    /// Prints the session id and generation id every later Path A subcommand
    /// needs. The session stays alive, holding a Claude process, until `pmux
    /// close` or its idle TTL expires.
    Start {
        #[command(flatten)]
        launch: LaunchArgs,
    },
    /// Path A (experimental): run one turn in an existing session.
    #[command(alias = "prompt")]
    Turn {
        /// Session id printed by `pmux start`.
        session: Uuid,
        /// Opaque process generation returned by `pmux start`.
        #[arg(long)]
        generation: Uuid,
        #[command(flatten)]
        prompt: PromptArgs,
        #[command(flatten)]
        turn: TurnArgs,
    },
    /// Path A (experimental): print one session's current snapshot as JSON.
    ///
    /// This is where `transcript_session_id` is re-read after a lost `pmux
    /// clear` response, and where `state` and `last_turn` are read.
    Inspect {
        /// Session id printed by `pmux start`.
        session: Uuid,
        /// Opaque process generation returned by `pmux start`.
        #[arg(long)]
        generation: Uuid,
    },
    /// Path A (experimental): cancel one exact in-flight turn and report recovery state.
    ///
    /// Idempotent, and never resubmits prompt input. Exits non-zero when the
    /// session could not be recovered, which means it must be closed.
    Cancel {
        /// Session id printed by `pmux start`.
        session: Uuid,
        /// Opaque process generation returned by `pmux start`.
        #[arg(long)]
        generation: Uuid,
        /// Turn id to cancel: `--turn-id` if you supplied one, otherwise the
        /// id `pmux` printed on stderr when the turn was accepted.
        turn: Uuid,
    },
    /// Path A (experimental): close one session and reap its Claude process tree.
    ///
    /// Exits non-zero unless the daemon confirms the process was reaped, so a
    /// zero exit is a released slot and a released process.
    Close {
        /// Session id printed by `pmux start`.
        session: Uuid,
        /// Opaque process generation returned by `pmux start`.
        #[arg(long)]
        generation: Uuid,
        /// Carried to the daemon and recorded on the request. It does NOT
        /// currently change the teardown: every backend in this tree drives
        /// the same close and reports `process_reaped` only after positively
        /// observing the owned process boundary empty. Stated here rather than
        /// implied, because a knob that reads as "try harder" and does nothing
        /// is worse than no knob.
        #[arg(long, value_enum, default_value_t)]
        policy: ClosePolicyArg,
    },
    /// Path A call on a Path B cell: clear one minified-cell session's context
    /// between turns.
    ///
    /// Types `/clear` into the session's composer and rebinds to the transcript
    /// Claude rotates to. The session id and generation are unchanged; what
    /// rotates is `transcript_session_id`, which the result reports and which
    /// the next clear must be fenced against.
    Clear {
        /// Session id printed by `pmux start --cell minified`.
        session: Uuid,
        /// Opaque process generation returned by `pmux start`.
        #[arg(long)]
        generation: Uuid,
        /// The transcript the caller believes is bound. At start this is the
        /// session id; afterwards it is the `transcript_session_id` the
        /// previous clear returned, which `pmux inspect` also reports. It is a
        /// fence: every value other than the currently bound transcript is
        /// refused, including one that is only one rotation stale. To recover
        /// after a lost response, re-read it with `pmux inspect`.
        #[arg(long)]
        expect_transcript: Uuid,
        /// Absolute Unix deadline for submitting the command. Omit to use the
        /// server policy.
        #[arg(long)]
        deadline_unix_ms: Option<u64>,
    },
    /// Path A (experimental): attach a terminal to a live session.
    ///
    /// With `--output text` pmux takes over this terminal until you detach.
    /// With `--output json`/`--output ndjson` it does NOT attach: it prints the
    /// short-lived capability, whose `token` is a bearer credential for the
    /// session's terminal -- treat it as a secret and do not log it.
    ///
    /// A `--cell minified` session cannot be attached at all: a writable attach
    /// is refused because the cell does not grant one, and `--read-only` is
    /// refused because it is unimplemented.
    Attach {
        /// Session id printed by `pmux start`.
        session: Uuid,
        /// Opaque process generation returned by `pmux start`.
        #[arg(long)]
        generation: Uuid,
        /// NOT IMPLEMENTED, and refused by the daemon with
        /// `unsupported_feature` on every session: the pinned rmux stream
        /// protocol has no view-only mode. The flag is still accepted so the
        /// refusal comes from the daemon that owns the answer rather than from
        /// a client guess.
        #[arg(long)]
        read_only: bool,
        /// Resize the session's terminal to this many rows on attach. Requires
        /// --cols; omit both to keep the session's current geometry.
        #[arg(long, requires = "cols")]
        rows: Option<u16>,
        /// Resize the session's terminal to this many columns on attach.
        /// Requires --rows.
        #[arg(long, requires = "rows")]
        cols: Option<u16>,
    },
    /// Neither path: validate the socket, the daemon's health tree, the working
    /// directory, and the Claude executable.
    ///
    /// Starts no session and spends no tokens. Exits 0 only when every check it
    /// lists both ran and passed; `unproven` and `unhealthy` both exit 1, and
    /// the `status` field is the distinction.
    ///
    /// The health tree includes the daemon's own compatibility layer, which
    /// runs the Claude the stateless pool would launch and asks the same
    /// registry a mint asks. That is what stops a green `doctor` from being
    /// followed by a `run` refused with `unsupported_claude_version`: the two
    /// answers now come from one comparison, made where both operands live.
    Doctor {
        /// Claude executable to validate, resolved exactly as `pmux start`
        /// resolves `--claude`.
        ///
        /// PATH A's executable, and only that. Path A starts under
        /// `AllowUntested`, so an unmeasured version is not a fault here; the
        /// version gate is `RequireTested` and applies to the pool's own
        /// `--path-b-claude`, which the daemon reports on in the health tree
        /// above rather than this client guessing at it.
        #[arg(long, env = "PMUX_CLAUDE", default_value = "claude")]
        claude: PathBuf,
        /// Working directory to validate. Defaults to the current directory.
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    /// Path A (experimental): store, read and revise the reusable launch configurations
    /// `pmux start --agent` and `pmux oneshot --agent` name.
    ///
    /// An agent holds LAUNCH POLICY and never a resource: no cwd, no
    /// configuration root, no session identity, no prompt and no environment
    /// snapshot. Those are per-session and are named on every start. An agent
    /// may only NARROW what a session names, through `containment`.
    ///
    /// Requires a `pmuxd` started with an agent store; `pmuxd serve` derives one
    /// beside the socket by default, so this is the ordinary case.
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    /// Path A (experimental): build and print a redacted summary of the exact start DTO;
    /// optionally launch it.
    ///
    /// Without --launch this reaches no daemon at all and starts nothing: it is
    /// the way to read what a launch WOULD send, including every environment
    /// name the child will not receive. Environment values, inline settings and
    /// MCP documents, and the system prompt are never printed.
    Probe {
        #[command(flatten)]
        launch_args: LaunchArgs,
        /// Actually start the session, inspect it, and print the snapshot.
        /// Without this the probe is a dry run and contacts no daemon.
        #[arg(long)]
        launch: bool,
        /// Leave the launched session running instead of closing it. Requires
        /// --launch. YOU then own it: close it with `pmux close` using the
        /// session id and generation this report prints, or it holds a Claude
        /// process until its idle TTL expires.
        #[arg(long, requires = "launch")]
        keep: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum AgentCommand {
    /// Store one launch configuration and print its id and version 1.
    ///
    /// The daemon mints the id. `--spec-file` is the complete stored document;
    /// `--from-profile` authors one from a client-side profile instead, and
    /// refuses by name any profile key an agent may not carry.
    Create {
        /// JSON document holding the complete agent spec. `-` reads stdin.
        #[arg(long, value_name = "FILE", conflicts_with = "from_profile")]
        spec_file: Option<PathBuf>,
        /// Author the spec from this client-side profile instead of a file.
        /// Requires --profile-file and --name.
        #[arg(long, value_name = "PROFILE", requires_all = ["profile_file", "name"])]
        from_profile: Option<String>,
        /// The profile document --from-profile is expanded from. pmux performs
        /// no XDG search and never walks up from the working directory.
        #[arg(long, value_name = "PATH", env = "PMUX_PROFILE_FILE")]
        profile_file: Option<PathBuf>,
        /// The stored agent's human label. Required with --from-profile; with
        /// --spec-file the document's own `name` is used.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// Absolute Claude executable for the stored configuration. Required
        /// with --from-profile: a profile carries no executable, and an agent
        /// may not resolve one through the daemon's PATH.
        #[arg(long, value_name = "PATH", requires = "from_profile")]
        claude: Option<PathBuf>,
    },
    /// List every stored agent's id, current version, digest, name and cell.
    ///
    /// Deliberately not full specs: a list is a directory read, and returning
    /// every spec would spray every stored environment key across one frame.
    /// Use `pmux agent get` for one.
    List,
    /// Read one stored agent version.
    ///
    /// Environment values and inline settings/MCP document bodies come back as
    /// `sha256:` digests and never in the clear; `config_digest` still
    /// identifies the configuration exactly. The system prompt is NOT redacted.
    Get {
        /// The stored agent id.
        #[arg(value_name = "AGENT_ID")]
        agent_id: Uuid,
        /// Omit for the current head.
        #[arg(long, value_name = "N")]
        version: Option<u64>,
    },
    /// Store a new immutable version of one agent and print it.
    ///
    /// `--spec-file` is a COMPLETE replacement and not a patch: read, edit,
    /// write. Running sessions are unaffected -- each pinned its version at
    /// start.
    Update {
        /// The stored agent id.
        #[arg(value_name = "AGENT_ID")]
        agent_id: Uuid,
        /// The version you believe is current. REQUIRED, and a fence: any value
        /// that is not the current head is refused with `id_conflict`,
        /// including one stale by exactly one revision, and no update is ever
        /// answered as "already landed".
        #[arg(long, value_name = "N")]
        expected_version: u64,
        /// JSON document holding the complete replacement spec. `-` reads
        /// stdin.
        #[arg(long, value_name = "FILE")]
        spec_file: PathBuf,
    },
}

#[derive(Clone, Debug, Args)]
pub struct PromptArgs {
    /// Prompt text. If omitted, stdin is read when it is not a terminal.
    #[arg(value_name = "PROMPT", conflicts_with = "prompt_file")]
    pub prompt: Option<String>,
    /// Read the prompt from this UTF-8 file; `-` means stdin. One trailing
    /// newline is dropped, so an ordinary text file works unchanged.
    #[arg(long, conflicts_with = "prompt")]
    pub prompt_file: Option<PathBuf>,
}

#[derive(Clone, Debug, Args)]
pub struct TurnArgs {
    /// Caller idempotency key. A fresh UUID v4 is generated by default.
    /// Resubmitting the same id replays the stored result instead of running a
    /// second turn.
    #[arg(long)]
    pub turn_id: Option<Uuid>,
    /// Absolute server turn deadline is computed from this duration.
    #[arg(long, default_value_t = 120)]
    pub timeout_secs: u64,
    /// `continue` is the only value the daemon implements, and it is the
    /// default: a turn always runs to completion whatever happens to this
    /// connection. `cancel-turn` and `close-session` are refused with
    /// `unsupported_feature` ("disconnect actions and heartbeat leases require
    /// a future leased connection API"). To stop a turn, use `pmux cancel`.
    #[arg(long, value_enum, default_value_t)]
    pub on_disconnect: DisconnectArg,
    /// NOT IMPLEMENTED: any value is refused with `unsupported_feature`, on the
    /// same future leased-connection API as --on-disconnect. Bound a turn with
    /// --timeout-secs instead, which the daemon does enforce.
    #[arg(long)]
    pub heartbeat_timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Args)]
pub struct LaunchArgs {
    /// The clap argument ids clap filled from the ENVIRONMENT rather than from
    /// this command line.
    ///
    /// **NOT A FLAG**, and never parsed: `#[arg(skip)]` keeps it out of clap's
    /// argument set entirely. It is filled by
    /// [`Cli::record_launch_argument_sources`] from `ArgMatches`, which is the
    /// only place the distinction still exists.
    ///
    /// It exists because `--agent` has to tell "you typed a launch flag" from
    /// "your shell rc exports one". Once a `LaunchArgs` is built,
    /// `--model opus` and a `PMUX_MODEL=opus` in the caller's profile are the
    /// same `Some("opus")` -- and MEASURED,
    /// `PMUX_MODEL=opus pmux start --agent ... --cwd /tmp` was refused with
    /// "cannot be combined with --model", naming a flag the caller never typed
    /// and locking anyone with that variable exported out of `--agent`
    /// entirely. [`LaunchArgs::claude`] already makes exactly this argument
    /// about a clap `default_value`, which is why that field takes its default
    /// after parsing; `env` needed the same answer and did not have one.
    ///
    /// Empty when a `LaunchArgs` is built directly, which is the right default:
    /// an embedder that sets a field set it deliberately.
    #[arg(skip)]
    pub from_environment: BTreeSet<String>,
    /// Stored agent to launch this session from, by id. Requires
    /// --agent-version.
    ///
    /// The daemon supplies the WHOLE launch policy from the stored version, so
    /// this is mutually exclusive with every inline launch flag: a command that
    /// names both is refused here, naming the flag that collides. --cwd is
    /// still required and is never taken from the agent; an agent may only
    /// BOUND it.
    ///
    /// NOT the client-side profile. That was renamed to --profile, and the old
    /// spellings are refused with a message naming the new one rather than
    /// silently aliased.
    #[arg(
        long,
        value_name = "AGENT_ID",
        env = "PMUX_AGENT_ID",
        requires = "agent_version"
    )]
    pub agent: Option<String>,
    /// The EXACT stored version to run. Required with --agent.
    ///
    /// There is deliberately no "latest": that would make the launch a function
    /// of when the request arrived. Read one with `pmux agent get` and log the
    /// number you got.
    #[arg(long, value_name = "N", env = "PMUX_AGENT_VERSION", requires = "agent")]
    pub agent_version: Option<u64>,
    /// Client-side profile to expand from --profile-file. Expansion happens
    /// here and the daemon never sees the profile name.
    #[arg(long, env = "PMUX_PROFILE")]
    pub profile: Option<String>,
    /// Absolute path to the client-side profile document. Required with
    /// --profile; pmux performs no XDG search and never walks up from the
    /// working directory.
    #[arg(long, env = "PMUX_PROFILE_FILE")]
    pub profile_file: Option<PathBuf>,
    /// RETIRED SPELLING of --profile-file, kept only so the rename is refused
    /// by name instead of by clap's "unexpected argument".
    ///
    /// `--agent` now means the stored server agent, and a silent alias is
    /// exactly how a caller reaches for one feature and gets the other.
    #[arg(long, hide = true, value_name = "PATH")]
    pub agent_file: Option<PathBuf>,
    /// New Claude session UUID. Generated before launch when omitted.
    #[arg(long, conflicts_with = "resume")]
    pub session_id: Option<Uuid>,
    /// Resume this exact Claude session UUID.
    #[arg(long, conflicts_with = "session_id")]
    pub resume: Option<Uuid>,
    /// Claude executable; resolved to an absolute executable path. Defaults to
    /// `claude` on PATH.
    ///
    /// An `Option` with the default applied after parsing, deliberately: a clap
    /// `default_value` is indistinguishable from an explicit flag, and
    /// `--agent` has to be able to say "you also named a launch flag" without
    /// naming one every caller gets for free.
    #[arg(long, env = "PMUX_CLAUDE")]
    pub claude: Option<PathBuf>,
    /// Working directory; canonicalized before submission. Never expressible in
    /// a profile, and never taken from a stored agent.
    #[arg(long, env = "PMUX_CWD")]
    pub cwd: Option<PathBuf>,
    /// Claude model alias or exact id, e.g. `opus`, `sonnet`, `claude-opus-5`.
    /// Omit for the Claude executable's own default.
    #[arg(long, env = "PMUX_MODEL")]
    pub model: Option<String>,
    /// Reasoning depth. Omit for the resolved model's own default. Which tiers
    /// a given model admits is decided by Claude, not by this list.
    #[arg(long, value_enum, env = "PMUX_EFFORT")]
    pub effort: Option<EffortArg>,
    /// How Claude asks for permission before acting. Omit for Claude's own
    /// default.
    #[arg(long, value_enum, env = "PMUX_PERMISSION_MODE")]
    pub permission_mode: Option<PermissionArg>,
    /// Tool pattern Claude may use without asking, e.g. `Read` or
    /// `Bash(git:*)`. Repeatable, and comma-separated values are split.
    /// Appends to a profile's list rather than replacing it.
    #[arg(long = "allowed-tool", value_delimiter = ',')]
    pub allowed_tools: Vec<String>,
    /// Tool pattern Claude may not use. Repeatable, and comma-separated values
    /// are split. Appends to a profile's list.
    #[arg(long = "denied-tool", value_delimiter = ',')]
    pub denied_tools: Vec<String>,
    /// Existing Claude settings file. May be repeated; hooks remain data.
    #[arg(long = "settings")]
    pub settings_files: Vec<PathBuf>,
    /// Inline Claude settings JSON. May be repeated; hooks remain data.
    #[arg(long = "settings-json")]
    pub settings_json: Vec<String>,
    /// Existing MCP server configuration file. May be repeated.
    #[arg(long = "mcp-config")]
    pub mcp_files: Vec<PathBuf>,
    /// Inline MCP server configuration JSON. May be repeated.
    #[arg(long = "mcp-json")]
    pub mcp_json: Vec<String>,
    /// Claude plugin directory; must exist. May be repeated.
    #[arg(long = "plugin-dir")]
    pub plugin_dirs: Vec<PathBuf>,
    /// Extra Claude argument, repeatable. Still subject to the daemon's closed
    /// allowlist; driver-owned and print-mode flags are rejected there.
    #[arg(long = "extra-arg", allow_hyphen_values = true)]
    pub extra_args: Vec<String>,
    /// REPLACE Claude's system prompt with this text. Visible in this process's
    /// argv (`ps`); use --system-prompt-file to keep it off the command line.
    #[arg(
        long,
        conflicts_with_all = ["append_system_prompt", "system_prompt_file", "append_system_prompt_file"]
    )]
    pub system_prompt: Option<String>,
    /// APPEND this text to Claude's system prompt. Visible in this process's
    /// argv (`ps`); use --append-system-prompt-file to keep it off the command
    /// line.
    #[arg(
        long,
        conflicts_with_all = ["system_prompt", "system_prompt_file", "append_system_prompt_file"]
    )]
    pub append_system_prompt: Option<String>,
    /// Read the REPLACE system prompt from this UTF-8 file; `-` means stdin.
    /// One trailing newline is dropped, so an ordinary text file works
    /// unchanged.
    #[arg(
        long,
        value_name = "FILE",
        conflicts_with_all = ["system_prompt", "append_system_prompt", "append_system_prompt_file"]
    )]
    pub system_prompt_file: Option<PathBuf>,
    /// Read the APPEND system prompt from this UTF-8 file; `-` means stdin.
    /// One trailing newline is dropped.
    #[arg(
        long,
        value_name = "FILE",
        conflicts_with_all = ["system_prompt", "append_system_prompt", "system_prompt_file"]
    )]
    pub append_system_prompt_file: Option<PathBuf>,
    /// `subscription` (the default) withholds every provider API-key variable
    /// from the child so it authenticates as your Claude subscription;
    /// `inherit` passes them through.
    #[arg(long, value_enum)]
    pub auth: Option<AuthArg>,
    /// Deliver KEY=VALUE to Claude verbatim; repeatable. The explicit `set`
    /// channel bypasses the launch allowlist, so this is how a name the
    /// allowlist drops is restored. VALUE is visible in this process's argv
    /// (`ps`); use --env-passthrough for anything secret.
    #[arg(long = "env", value_name = "KEY=VALUE", help_heading = ENVIRONMENT_HELP_HEADING)]
    pub env: Vec<String>,
    /// Forward KEY from pmux's own environment to Claude; repeatable. Only the
    /// name is written on the command line, so the value never reaches `ps`
    /// output. Fails when KEY is unset or empty in this process.
    #[arg(long = "env-passthrough", value_name = "KEY", help_heading = ENVIRONMENT_HELP_HEADING)]
    pub env_passthrough: Vec<String>,
    /// Drop KEY from the inherited snapshot before the launch environment is
    /// built; repeatable. Applied before --env/--env-passthrough.
    #[arg(long = "unset", value_name = "KEY", help_heading = ENVIRONMENT_HELP_HEADING)]
    pub unset: Vec<String>,
    /// Terminal rows the session's pane is created at. Default 24.
    #[arg(long)]
    pub rows: Option<u16>,
    /// Terminal columns the session's pane is created at. Default 120.
    #[arg(long)]
    pub cols: Option<u16>,
    /// `transparent` (the default) is the only implemented identity;
    /// `rmux-standard` is reserved and every start naming it is refused with
    /// `unsupported_feature`.
    #[arg(long, value_enum)]
    pub terminal_profile: Option<TerminalProfileArg>,
    /// How a prompt reaches the composer. `auto` (the default) resolves to
    /// `sdk`, which is the only implemented transport; `attached-stream` is
    /// reserved and every start naming it is refused with
    /// `unsupported_feature`.
    #[arg(long, value_enum)]
    pub input_transport: Option<InputTransportArg>,
    /// How a turn's end is corroborated. `transcript` (the default) reads the
    /// transcript alone. `hybrid` additionally merges the daemon's
    /// SessionStart/Stop/StopFailure hooks into the session's settings. The
    /// transcript is the sole completion AUTHORITY either way -- `hybrid` adds
    /// a signal, not a second verdict.
    #[arg(long, value_enum)]
    pub lifecycle: Option<LifecycleArg>,
    /// How long a hybrid lifecycle hook may take. Requires --lifecycle;
    /// default 5000.
    #[arg(long, requires = "lifecycle")]
    pub hook_timeout_ms: Option<u64>,
    /// `persistent` is the only value `run`, `start` and `probe --launch`
    /// accept; `one-shot` is reserved for the daemon's own `run_once` operation
    /// and every start naming it is refused with `unsupported_feature`.
    #[arg(long, value_enum)]
    pub retention: Option<RetentionArg>,
    /// Close the session automatically after this many idle seconds. Default
    /// 1800. Implies persistent retention.
    #[arg(long)]
    pub idle_ttl_secs: Option<u64>,
    /// `require-tested` (the default) admits only a Claude version, OS,
    /// architecture, terminal profile and input transport the daemon was
    /// started with evidence for. `allow-untested` runs anyway on the daemon's
    /// conservative fallback drain, and the session reports `tested: false`.
    #[arg(long, value_enum)]
    pub compatibility: Option<CompatibilityArg>,
    /// Which cell the session is driven as. `minified` is Path B: no tool
    /// surface, cleared between turns, and admitted only on a tested
    /// compatibility profile. It REQUIRES --config-isolation-root, refuses
    /// terminal attachment, and is the only cell `pmux clear` accepts.
    #[arg(long, value_enum)]
    pub cell: Option<CellArg>,
    /// Run this session against a pmux-owned Claude configuration root instead
    /// of the caller's. Must already exist, be owned by the daemon, and have
    /// mode 0700; pmux never creates it. The daemon computes the credential-store
    /// pin itself, so the session still authenticates as the same account.
    /// Mutually exclusive with `--env CLAUDE_CONFIG_DIR=` and
    /// `--env CLAUDE_SECURESTORAGE_CONFIG_DIR=`; with `--cell minified` those
    /// two `--env` names are refused outright and this flag is the only way to
    /// name the cell's configuration root.
    #[arg(long, value_name = "DIR", help_heading = ENVIRONMENT_HELP_HEADING)]
    pub config_isolation_root: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum EffortArg {
    Low,
    Medium,
    High,
    #[value(name = "xhigh")]
    XHigh,
    Max,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum PermissionArg {
    Default,
    AcceptEdits,
    Plan,
    Auto,
    BypassPermissions,
    DontAsk,
    /// Launches Claude with `--dangerously-skip-permissions`. Every turn of the
    /// session then carries the `dangerous_permission_bypass` warning.
    DangerouslySkipPermissions,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum AuthArg {
    #[default]
    Subscription,
    Inherit,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum TerminalProfileArg {
    #[default]
    Transparent,
    RmuxStandard,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum InputTransportArg {
    #[default]
    Auto,
    Sdk,
    AttachedStream,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum LifecycleArg {
    #[default]
    Transcript,
    Hybrid,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum RetentionArg {
    OneShot,
    Persistent,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum CompatibilityArg {
    #[default]
    RequireTested,
    AllowUntested,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum CellArg {
    #[default]
    Full,
    Minified,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum DisconnectArg {
    #[default]
    Continue,
    CancelTurn,
    CloseSession,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum ClosePolicyArg {
    #[default]
    Graceful,
    Force,
}

pub fn build_start_request(args: &LaunchArgs) -> Result<StartSessionRequest> {
    let (request, diagnostics) = build_start_request_with_diagnostics(args)?;
    // Diagnostics go to stderr so stdout stays exactly one machine-readable
    // record, and so precedence is never silent.
    for line in diagnostics {
        eprintln!("pmux: {line}");
    }
    Ok(request)
}

/// Expands the agent profile, applies CLI precedence, and returns the request
/// plus every diagnostic line the caller should see on stderr.
pub fn build_start_request_with_diagnostics(
    args: &LaunchArgs,
) -> Result<(StartSessionRequest, Vec<String>)> {
    refuse_retired_profile_spellings(args)?;
    if args.agent.is_some() {
        return build_agent_start_request(args);
    }
    let mut notes = Vec::new();
    let profile = resolve_agent_profile(args, &mut notes)?;

    let cwd = resolve_cwd(args.cwd.as_deref())?;
    let executable = resolve_executable(args.claude.as_deref())?;
    let identity = match args.resume {
        Some(session_id) => SessionIdentity::Resume { session_id },
        None => SessionIdentity::New {
            session_id: Some(args.session_id.unwrap_or_else(Uuid::new_v4)),
        },
    };

    // Lists append, profile first, because argv repeats one flag per element.
    let mut settings = profile.settings.clone();
    settings.extend(config_sources(
        &args.settings_files,
        &args.settings_json,
        "settings",
    )?);
    let mut mcp_configs = profile.mcp_configs.clone();
    mcp_configs.extend(config_sources(
        &args.mcp_files,
        &args.mcp_json,
        "MCP config",
    )?);
    let mut plugin_dirs = profile.plugin_dirs.clone();
    for path in &args.plugin_dirs {
        plugin_dirs.push(absolute_utf8_path(path, true, "plugin directory")?);
    }
    let mut allowed_tools = profile.allowed_tools.clone();
    allowed_tools.extend(args.allowed_tools.iter().cloned());
    let mut denied_tools = profile.denied_tools.clone();
    denied_tools.extend(args.denied_tools.iter().cloned());
    let mut extra_args = profile.extra_args.clone();
    extra_args.extend(args.extra_args.iter().cloned());

    let cli_system_prompt = resolve_system_prompt(args)?;
    let cli_lifecycle = args.lifecycle.map(|lifecycle| match lifecycle {
        LifecycleArg::Transcript => LifecycleMode::Transcript,
        LifecycleArg::Hybrid => LifecycleMode::Hybrid {
            hook_timeout_ms: args.hook_timeout_ms.unwrap_or(DEFAULT_HOOK_TIMEOUT_MS),
        },
    });
    let cli_retention = match (args.retention, args.idle_ttl_secs) {
        (Some(RetentionArg::OneShot), _) => Some(RetentionPolicy::OneShot),
        (Some(RetentionArg::Persistent), idle_ttl_secs) => Some(persistent_retention(
            idle_ttl_secs.unwrap_or(DEFAULT_IDLE_TTL_SECS),
        )?),
        (None, Some(idle_ttl_secs)) => Some(persistent_retention(idle_ttl_secs)?),
        (None, None) => None,
    };

    let model = override_scalar("model", args.model.clone(), profile.model, &mut notes);
    let effort = override_scalar(
        "effort",
        args.effort.map(Into::into),
        profile.effort,
        &mut notes,
    );
    let permission_mode = override_scalar(
        "permission-mode",
        args.permission_mode.map(Into::into),
        profile.permission_mode,
        &mut notes,
    );
    let system_prompt = override_scalar(
        "system-prompt",
        cli_system_prompt,
        profile.system_prompt,
        &mut notes,
    )
    .unwrap_or_default();
    let auth_policy = override_scalar(
        "auth",
        args.auth.map(Into::into),
        profile.auth_policy,
        &mut notes,
    )
    .unwrap_or_default();
    let cell = override_scalar("cell", args.cell.map(Into::into), profile.cell, &mut notes)
        .unwrap_or_default();
    let cli_config_isolation = args
        .config_isolation_root
        .as_deref()
        .map(|root| {
            Ok::<_, anyhow::Error>(ConfigIsolation {
                root: absolute_utf8_path(root, true, "config isolation root")?,
            })
        })
        .transpose()?;
    let config_isolation = override_scalar(
        "config-isolation-root",
        cli_config_isolation,
        profile.config_isolation,
        &mut notes,
    );
    let compatibility = override_scalar(
        "compatibility",
        args.compatibility.map(Into::into),
        profile.compatibility,
        &mut notes,
    )
    .unwrap_or_default();
    let lifecycle = override_scalar("lifecycle", cli_lifecycle, profile.lifecycle, &mut notes)
        .unwrap_or_default();
    let retention = override_scalar("retention", cli_retention, profile.retention, &mut notes)
        .map_or_else(|| persistent_retention(DEFAULT_IDLE_TTL_SECS), Ok)?;
    let rows = override_scalar("rows", args.rows, profile.rows, &mut notes).unwrap_or(DEFAULT_ROWS);
    let cols = override_scalar("cols", args.cols, profile.cols, &mut notes).unwrap_or(DEFAULT_COLS);
    let terminal_profile = override_scalar(
        "terminal-profile",
        args.terminal_profile.map(Into::into),
        profile.terminal_profile,
        &mut notes,
    )
    .unwrap_or_default();
    let input_transport = override_scalar(
        "input-transport",
        args.input_transport.map(Into::into),
        profile.input_transport,
        &mut notes,
    )
    .unwrap_or_default();

    if rows == 0 || cols == 0 {
        bail!("terminal rows and columns must be greater than zero");
    }
    if lifecycle == (LifecycleMode::Hybrid { hook_timeout_ms: 0 }) {
        bail!("hybrid hook timeout must be greater than zero");
    }
    // NAMED IN THE CALLER'S OWN VOCABULARY. The daemon refuses this too, and
    // its refusal is the authority (`claude_launch::validate_config_isolation`),
    // but it names `config_isolation` -- a wire field nothing on the command
    // line is spelled that way. A CLI caller learns here, before a session is
    // started, and is told the flag to add rather than the field they did not
    // set.
    if cell == SessionCell::Minified && config_isolation.is_none() {
        bail!(
            "--cell minified requires --config-isolation-root DIR: a minified cell's transcripts, \
             prompt history and paste cache must not accumulate in the caller's own Claude \
             configuration root. Give it an existing, owner-only (mode 0700) directory, or drop \
             --cell minified to run the ordinary full cell"
        );
    }

    let mut environment = exact_environment_snapshot()?;
    let (set, unset) = build_environment_patch(args)?;
    environment.set = set;
    environment.unset = unset;
    notes.extend(verify_required_environment(
        &profile.require_env,
        &environment,
        auth_policy,
        terminal_profile,
    )?);

    Ok((
        StartSessionRequest {
            identity,
            cwd: path_to_utf8(&cwd, "working directory")?,
            agent: None,
            claude: Some(ClaudeLaunchConfig {
                executable: path_to_utf8(&executable, "Claude executable")?,
                model,
                effort,
                permission_mode,
                allowed_tools,
                denied_tools,
                settings,
                mcp_configs,
                plugin_dirs,
                system_prompt,
                extra_args,
            }),
            environment,
            auth_policy,
            config_isolation,
            terminal: TerminalSpec {
                rows,
                cols,
                profile: terminal_profile,
                input_transport,
            },
            lifecycle,
            retention,
            compatibility,
            cell,
        },
        notes,
    ))
}

/// Reads the complete stored document one `pmux agent create/update` sends.
///
/// The whole spec comes from one file rather than from thirty flags,
/// deliberately: an agent is stored, versioned and diffed, and a document is
/// the artefact a caller can keep under version control beside the code it
/// launches. `-` reads stdin, exactly as `--prompt-file` does.
///
/// # Errors
///
/// Any read failure, or a document that is not a v1 `AgentSpec` -- which is
/// `deny_unknown_fields`, so a misspelled key is refused BY NAME here rather
/// than silently stored as a default.
pub fn read_agent_spec(path: &Path) -> Result<AgentSpec> {
    let text = if path == Path::new("-") {
        let mut buffer = String::new();
        io::stdin()
            .read_to_string(&mut buffer)
            .context("failed to read the agent spec from stdin")?;
        buffer
    } else {
        fs::read_to_string(path)
            .with_context(|| format!("failed to read agent spec {}", path.display()))?
    };
    serde_json::from_str(&text).with_context(|| {
        format!(
            "{} is not a v1 agent spec (unknown fields are refused by name so a misspelled launch \
             policy is never stored as a default)",
            path.display()
        )
    })
}

/// Authors an [`AgentSpec`] from a client-side profile.
///
/// **THIS IS WHAT THE PROFILE IS FOR NOW.** `extends` chains, composition and
/// `require_env` are how a human WRITES a configuration; the agent is where one
/// is STORED. Server-side inheritance is deliberately absent, so the chain is
/// flattened here, at authoring time, and the stored version is fully resolved.
///
/// Two profile keys have no agent form and are refused BY NAME rather than
/// dropped:
///
/// * `config_isolation` names a directory. An agent that named one would make
///   its id a contention key for every session started from it, and the seed
///   disposition would begin refusing starts as a function of how popular the
///   agent is. Use `containment.require_config_isolation` and name the root per
///   session.
/// * `require_env` is a precondition checked against the CALLING process's
///   environment before a request is sent. A daemon has no calling process to
///   check it against, so a stored one would be a rule nothing runs.
///
/// The destructuring has no `..`: a field added to `AgentProfile` stops this
/// compiling until it is carried or refused.
fn agent_spec_from_profile(
    profile: AgentProfile,
    name: String,
    claude_executable: &Path,
) -> Result<AgentSpec> {
    let AgentProfile {
        model,
        effort,
        permission_mode,
        system_prompt,
        allowed_tools,
        denied_tools,
        settings,
        mcp_configs,
        plugin_dirs,
        extra_args,
        auth_policy,
        compatibility,
        cell,
        config_isolation,
        lifecycle,
        retention,
        rows,
        cols,
        terminal_profile,
        input_transport,
        require_env,
    } = profile;

    if let Some(isolation) = config_isolation {
        bail!(
            "this profile sets `config_isolation` ({}), which an agent may not carry: an agent may \
             narrow what a session names but never name a resource on its behalf. Set \
             `containment.require_config_isolation` on the agent and pass --config-isolation-root \
             on each start",
            isolation.root
        );
    }
    if !require_env.is_empty() {
        bail!(
            "this profile sets `require_env` ({}), which an agent may not carry: it is a check \
             against the CALLING process's environment, and the daemon has no calling process to \
             check it against. Keep the profile for authoring and run its check client-side",
            require_env.join(", ")
        );
    }

    let cell = cell.unwrap_or_default();
    let containment = AgentContainment {
        workspace_root: None,
        // The minified cell needs a configuration root of its own, and
        // `create_agent` refuses the pair rather than overriding it silently.
        // Authoring sets the value the cell requires instead of storing one the
        // daemon would reject.
        require_config_isolation: cell == SessionCell::Minified,
    };

    Ok(AgentSpec {
        name,
        description: None,
        claude: ClaudeLaunchConfig {
            executable: path_to_utf8(claude_executable, "Claude executable")?,
            model,
            effort,
            permission_mode,
            allowed_tools,
            denied_tools,
            settings,
            mcp_configs,
            plugin_dirs,
            system_prompt: system_prompt.unwrap_or_default(),
            extra_args,
        },
        environment: Default::default(),
        auth_policy: auth_policy.unwrap_or_default(),
        terminal: TerminalSpec {
            rows: rows.unwrap_or(DEFAULT_ROWS),
            cols: cols.unwrap_or(DEFAULT_COLS),
            profile: terminal_profile.unwrap_or_default(),
            input_transport: input_transport.unwrap_or_default(),
        },
        lifecycle: lifecycle.unwrap_or_default(),
        retention: retention.map_or_else(|| persistent_retention(DEFAULT_IDLE_TTL_SECS), Ok)?,
        compatibility: compatibility.unwrap_or_default(),
        cell,
        containment,
    })
}

/// The spec one `pmux agent create` sends, from whichever source the caller
/// named.
///
/// # Errors
///
/// Any read or authoring failure.
pub fn build_agent_create_spec(
    spec_file: Option<&Path>,
    from_profile: Option<&str>,
    profile_file: Option<&Path>,
    name: Option<&str>,
    claude: Option<&Path>,
) -> Result<AgentSpec> {
    match (spec_file, from_profile) {
        (Some(path), None) => {
            let mut spec = read_agent_spec(path)?;
            if let Some(name) = name {
                spec.name = name.to_owned();
            }
            Ok(spec)
        }
        (None, Some(profile)) => {
            // `requires_all` makes these unreachable from a parsed command
            // line; this function is `pub` and is also called by tests.
            let profile_file = profile_file
                .context("--from-profile requires --profile-file PATH or PMUX_PROFILE_FILE")?;
            let name = name.context("--from-profile requires --name NAME")?;
            let claude = claude.context(
                "--from-profile requires --claude PATH: a profile carries no executable, and an \
                 agent may not resolve one through the daemon's PATH",
            )?;
            let executable = resolve_executable(Some(claude))?;
            let expanded = load_agent_profile(profile_file, profile)?;
            agent_spec_from_profile(expanded, name.to_owned(), &executable)
        }
        (None, None) => bail!(
            "`pmux agent create` needs a spec: pass --spec-file FILE (or `-` for stdin), or \
             --from-profile NAME --profile-file PATH --name NAME --claude PATH"
        ),
        // clap's `conflicts_with` makes this unreachable from a command line.
        (Some(_), Some(_)) => bail!("--spec-file and --from-profile are mutually exclusive"),
    }
}

/// Every launch flag this caller typed that a stored agent also supplies.
///
/// **DERIVED BY DESTRUCTURING `LaunchArgs` WITHOUT `..`.** A flag added to the
/// subcommand stops this compiling until it is classified as agent-supplied
/// (and named, in the spelling a caller types) or as per-session. The
/// classification is checked from the other side too:
/// `every_launch_flag_is_either_per_session_or_named_as_agent_supplied` walks
/// clap's own argument ids for `start` and asserts the two classes partition
/// them exactly, so a flag that is added and silently dropped from both lists
/// is red.
///
/// The refusal exists at all because the daemon's is correct but late and in
/// the wrong vocabulary: it names `terminal`, a wire field nothing on the
/// command line is spelled that way. This one names `--rows`.
fn launch_flags_a_stored_agent_supplies(args: &LaunchArgs) -> Vec<(&'static str, &'static str)> {
    let LaunchArgs {
        // NOT A FLAG. It is the SOURCE of the flags below, and it is applied by
        // `agent_supplied_launch_flags_by_source` rather than classified here.
        from_environment: _,
        // CONTROL AND PER-SESSION. `agent`/`agent_version` are the reference
        // itself; `profile*`/`agent_file` are refused separately and by name;
        // `session_id`/`resume` are the session's identity; `cwd` and
        // `config_isolation_root` name directories pmux CLAIMS, and an agent
        // may bound them but never supply one.
        agent: _,
        agent_version: _,
        profile,
        profile_file: _,
        agent_file: _,
        session_id: _,
        resume: _,
        cwd: _,
        config_isolation_root: _,
        // AGENT-SUPPLIED, each with the flag a caller types.
        claude,
        model,
        effort,
        permission_mode,
        allowed_tools,
        denied_tools,
        settings_files,
        settings_json,
        mcp_files,
        mcp_json,
        plugin_dirs,
        extra_args,
        system_prompt,
        append_system_prompt,
        system_prompt_file,
        append_system_prompt_file,
        auth,
        env,
        env_passthrough,
        unset,
        rows,
        cols,
        terminal_profile,
        input_transport,
        lifecycle,
        hook_timeout_ms,
        retention,
        idle_ttl_secs,
        compatibility,
        cell,
    } = args;
    // Each row is `(clap argument id, the flag a caller types, whether it was
    // typed)`. The id is what
    // `every_launch_flag_is_either_per_session_or_named_as_agent_supplied`
    // partitions clap's own argument set with; the spelling is what a refusal
    // prints, because `--allowed-tool` is what a caller wrote and
    // `allowed_tools` is not.
    [
        // A profile expands into exactly the launch policy an agent stores, so
        // naming both is naming the launch twice.
        ("profile", "--profile", profile.is_some()),
        ("claude", "--claude", claude.is_some()),
        ("model", "--model", model.is_some()),
        ("effort", "--effort", effort.is_some()),
        (
            "permission_mode",
            "--permission-mode",
            permission_mode.is_some(),
        ),
        ("allowed_tools", "--allowed-tool", !allowed_tools.is_empty()),
        ("denied_tools", "--denied-tool", !denied_tools.is_empty()),
        ("settings_files", "--settings", !settings_files.is_empty()),
        (
            "settings_json",
            "--settings-json",
            !settings_json.is_empty(),
        ),
        ("mcp_files", "--mcp-config", !mcp_files.is_empty()),
        ("mcp_json", "--mcp-json", !mcp_json.is_empty()),
        ("plugin_dirs", "--plugin-dir", !plugin_dirs.is_empty()),
        ("extra_args", "--extra-arg", !extra_args.is_empty()),
        ("system_prompt", "--system-prompt", system_prompt.is_some()),
        (
            "append_system_prompt",
            "--append-system-prompt",
            append_system_prompt.is_some(),
        ),
        (
            "system_prompt_file",
            "--system-prompt-file",
            system_prompt_file.is_some(),
        ),
        (
            "append_system_prompt_file",
            "--append-system-prompt-file",
            append_system_prompt_file.is_some(),
        ),
        ("auth", "--auth", auth.is_some()),
        ("env", "--env", !env.is_empty()),
        (
            "env_passthrough",
            "--env-passthrough",
            !env_passthrough.is_empty(),
        ),
        ("unset", "--unset", !unset.is_empty()),
        ("rows", "--rows", rows.is_some()),
        ("cols", "--cols", cols.is_some()),
        (
            "terminal_profile",
            "--terminal-profile",
            terminal_profile.is_some(),
        ),
        (
            "input_transport",
            "--input-transport",
            input_transport.is_some(),
        ),
        ("lifecycle", "--lifecycle", lifecycle.is_some()),
        (
            "hook_timeout_ms",
            "--hook-timeout-ms",
            hook_timeout_ms.is_some(),
        ),
        ("retention", "--retention", retention.is_some()),
        ("idle_ttl_secs", "--idle-ttl-secs", idle_ttl_secs.is_some()),
        ("compatibility", "--compatibility", compatibility.is_some()),
        ("cell", "--cell", cell.is_some()),
    ]
    .into_iter()
    .filter_map(|(id, flag, present)| present.then_some((id, flag)))
    .collect()
}

/// The agent-supplied launch flags this caller carries, SPLIT BY SOURCE.
///
/// **A FLAG A CALLER TYPED AND A VARIABLE A CALLER'S SHELL EXPORTS ARE NOT THE
/// SAME EVENT**, and `--agent` has to answer them differently:
///
/// * TYPED: refused, by the flag's own spelling. The caller named the launch
///   twice on one command and pmux cannot tell which one they meant.
/// * FROM THE ENVIRONMENT: overridden, and reported. `env` is clap's lowest
///   precedence source below argv -- an ambient default, not an instruction on
///   this command -- and an explicit `--agent` is the higher-precedence choice.
///   Refusing instead meant that exporting `PMUX_MODEL` in a shell rc locked
///   that shell out of `--agent` for good, for a value the caller did not name
///   here; and applying it silently would have been the merge this whole design
///   refuses.
///
/// The environment VARIABLE NAME comes from clap's own argument metadata rather
/// than from a list written here, so a flag whose `env =` binding is added,
/// renamed or removed is reported correctly with no second edit.
fn agent_supplied_launch_flags_by_source(
    args: &LaunchArgs,
) -> (Vec<&'static str>, Vec<(&'static str, String)>) {
    let mut typed = Vec::new();
    let mut ambient = Vec::new();
    for (id, flag) in launch_flags_a_stored_agent_supplies(args) {
        if !args.from_environment.contains(id) {
            typed.push(flag);
            continue;
        }
        let variable = launch_argument_environment_variable(id)
            .unwrap_or_else(|| "an environment variable".to_owned());
        ambient.push((flag, variable));
    }
    (typed, ambient)
}

/// The `env =` binding clap holds for one `start` argument id.
///
/// Read out of `Cli::command()` rather than restated, for the reason every list
/// in this file is derived: a second copy of an attribute is a copy that drifts.
fn launch_argument_environment_variable(id: &str) -> Option<String> {
    <Cli as clap::CommandFactory>::command()
        .find_subcommand_mut("start")?
        .get_arguments()
        .find(|argument| argument.get_id() == id)?
        .get_env()
        .map(|name| name.to_string_lossy().into_owned())
}

/// A start that names a stored agent.
///
/// It carries the caller's own environment SNAPSHOT and nothing else of the
/// launch: the snapshot is a fact about this process at this moment and is the
/// one launch input an agent structurally cannot store.
fn build_agent_start_request(args: &LaunchArgs) -> Result<(StartSessionRequest, Vec<String>)> {
    let agent = args.agent.as_deref().expect("checked by the caller");
    let agent_id = Uuid::parse_str(agent)
        .ok()
        .filter(|parsed| parsed.hyphenated().to_string().eq_ignore_ascii_case(agent))
        .with_context(|| {
            format!(
                "--agent {agent} is not a stored agent id. It must be the canonical hyphenated \
                 UUID `pmux agent create` printed; list them with `pmux agent list`. If you meant \
                 the client-side profile, that flag is now --profile"
            )
        })?;
    // `requires` in clap makes this unreachable from a parsed command line;
    // `LaunchArgs` is `pub` and is also built directly by tests and embedders.
    let version = args.agent_version.with_context(|| {
        "--agent requires --agent-version N: there is deliberately no `latest`, because that \
         would make the launch a function of when the request arrived"
    })?;
    let version = AgentVersion::new(version)
        .map_err(|_| anyhow::anyhow!("--agent-version starts at 1; there is no version 0"))?;

    let (typed, ambient) = agent_supplied_launch_flags_by_source(args);
    if !typed.is_empty() {
        bail!(
            "--agent supplies the whole launch policy, so it cannot be combined with {}. Drop \
             those flags, or drop --agent and name the launch inline. To vary one of them, mint a \
             new version with `pmux agent update` and pin it",
            typed.join(", ")
        );
    }
    // OVERRIDDEN, NEVER SILENTLY. Same channel and same reason as the profile
    // precedence notes: stdout stays one machine-readable record, and no
    // precedence decision is taken without saying so.
    let mut notes = Vec::new();
    for (flag, variable) in ambient {
        notes.push(format!(
            "{variable} in this environment supplies {flag}, and --agent supplies the whole launch \
             policy; the stored agent's value is used. Unset it, or drop --agent, if you meant the \
             environment's"
        ));
    }

    let cwd = resolve_cwd(args.cwd.as_deref())?;
    let identity = match args.resume {
        Some(session_id) => SessionIdentity::Resume { session_id },
        None => SessionIdentity::New {
            session_id: Some(args.session_id.unwrap_or_else(Uuid::new_v4)),
        },
    };
    let config_isolation = args
        .config_isolation_root
        .as_deref()
        .map(|root| {
            Ok::<_, anyhow::Error>(ConfigIsolation {
                root: absolute_utf8_path(root, true, "config isolation root")?,
            })
        })
        .transpose()?;

    Ok((
        StartSessionRequest {
            identity,
            cwd: path_to_utf8(&cwd, "working directory")?,
            claude: None,
            agent: Some(AgentRef { agent_id, version }),
            environment: exact_environment_snapshot()?,
            auth_policy: AuthPolicy::default(),
            config_isolation,
            terminal: TerminalSpec::default(),
            lifecycle: LifecycleMode::default(),
            retention: RetentionPolicy::default(),
            compatibility: CompatibilityPolicy::default(),
            cell: SessionCell::default(),
        },
        notes,
    ))
}

/// The system prompt, from argv or from a file, in exactly one of two modes.
///
/// A system prompt IS a prompt: it is long, it is often generated, and on argv
/// it is visible in `ps`. Every other prompt this CLI takes has had a file form
/// since `--prompt-file`, and `pmuxd` grew `--path-b-system-prompt-file` for
/// the same reason; this closes the last argv-only prompt on the client.
///
/// The four flags are pairwise exclusive in clap, so only the four single-flag
/// arms are reachable from a parsed `Cli`. `LaunchArgs` is `pub` and is also
/// constructed directly by tests and by `pseudomux_client` embedders, which is
/// why the combination is refused here as well rather than trusted.
fn resolve_system_prompt(args: &LaunchArgs) -> Result<Option<SystemPromptPolicy>> {
    let stated = [
        args.system_prompt.is_some(),
        args.append_system_prompt.is_some(),
        args.system_prompt_file.is_some(),
        args.append_system_prompt_file.is_some(),
    ]
    .into_iter()
    .filter(|stated| *stated)
    .count();
    if stated > 1 {
        bail!(
            "system prompt modes are mutually exclusive: state exactly one of \
             --system-prompt, --append-system-prompt, --system-prompt-file, \
             --append-system-prompt-file"
        );
    }
    // The file forms are read through the same reader `--prompt-file` uses, so
    // the byte budget, the UTF-8 requirement and the one-trailing-newline rule
    // are the same rules rather than a second set stated over the same shapes.
    let from_file = |path: &Path| -> Result<String> {
        read_prompt_text(
            &PromptArgs {
                prompt: None,
                prompt_file: Some(path.to_path_buf()),
            },
            "system prompt",
            ComposerBound::No,
        )
    };
    Ok(match args {
        LaunchArgs {
            system_prompt: Some(prompt),
            ..
        } => Some(SystemPromptPolicy::Replace {
            prompt: prompt.clone(),
        }),
        LaunchArgs {
            append_system_prompt: Some(prompt),
            ..
        } => Some(SystemPromptPolicy::Append {
            prompt: prompt.clone(),
        }),
        LaunchArgs {
            system_prompt_file: Some(path),
            ..
        } => Some(SystemPromptPolicy::Replace {
            prompt: from_file(path)?,
        }),
        LaunchArgs {
            append_system_prompt_file: Some(path),
            ..
        } => Some(SystemPromptPolicy::Append {
            prompt: from_file(path)?,
        }),
        _ => None,
    })
}

/// An explicit CLI flag replaces the profile's scalar and says so. Silence here
/// would make the effective launch depend on a file the caller cannot see in
/// the command they typed.
fn override_scalar<T>(
    flag: &str,
    cli: Option<T>,
    profile: Option<T>,
    notes: &mut Vec<String>,
) -> Option<T> {
    match (cli, profile) {
        (Some(value), Some(_)) => {
            notes.push(format!("--{flag} overrides the profile value"));
            Some(value)
        }
        (Some(value), None) => Some(value),
        (None, profile) => profile,
    }
}

/// Every retired spelling of the client-side profile flags, refused by name.
///
/// **NEVER SILENTLY ALIASED.** `--agent` and `PMUX_AGENT` used to mean the
/// client-side profile; they now mean the stored server agent, which is a
/// different resource resolved by a different party. An alias would let a
/// caller reach for one feature and get the other, and the two disagree about
/// the single most consequential thing a launch configuration can say. So each
/// retired spelling is refused with the new one named in the message.
///
/// `PMUX_AGENT` and `PMUX_AGENT_FILE` are read from the process environment
/// rather than through clap, because clap no longer binds them to anything: an
/// operator who exported them once in a shell profile would otherwise have them
/// silently ignored, which is the accepted-and-ignored defect this whole change
/// exists not to ship.
fn refuse_retired_profile_spellings(args: &LaunchArgs) -> Result<()> {
    if let Some(path) = &args.agent_file {
        bail!(
            "--agent-file was renamed to --profile-file (it names a CLIENT-SIDE profile document, \
             and --agent now names a stored server agent by id): pass --profile-file {} instead",
            path.display()
        );
    }
    for (retired, replacement) in [
        ("PMUX_AGENT", "PMUX_PROFILE"),
        ("PMUX_AGENT_FILE", "PMUX_PROFILE_FILE"),
    ] {
        if std::env::var_os(retired).is_some() {
            bail!(
                "{retired} was renamed to {replacement} (it selects a CLIENT-SIDE profile, and \
                 --agent/PMUX_AGENT_ID now names a stored server agent by id): export \
                 {replacement} instead, and unset {retired}"
            );
        }
    }
    Ok(())
}

fn resolve_agent_profile(args: &LaunchArgs, notes: &mut Vec<String>) -> Result<AgentProfile> {
    refuse_retired_profile_spellings(args)?;
    match (&args.profile, &args.profile_file) {
        (None, None) => Ok(AgentProfile::default()),
        // A file without a name means "profiles live here, but not this time".
        // This is deliberately NOT an error: `PMUX_PROFILE_FILE` is meant to be
        // exported once in a shell profile, so erroring here would break every
        // invocation that does not want a profile. The invariant being protected
        // is that pmux never *selects* a profile on its own, and that still
        // holds -- no name, no profile.
        (None, Some(_)) => Ok(AgentProfile::default()),
        (Some(profile), None) => bail!(
            "--profile {profile} requires --profile-file PATH or PMUX_PROFILE_FILE; pmux performs no profile discovery"
        ),
        (Some(profile), Some(path)) => {
            let expanded = load_agent_profile(path, profile)?;
            notes.push(format!(
                "profile `{profile}` expanded from {}",
                path.display()
            ));
            Ok(expanded)
        }
    }
}

/// Builds the caller's explicit environment patch — `EnvironmentSpec::set` and
/// `EnvironmentSpec::unset` — from `--env`, `--env-passthrough`, and `--unset`.
///
/// `set` is the one channel the launch allowlist does not filter, so these three
/// flags are the whole recourse for a caller whose workflow needs a name the
/// allowlist drops. No environment **value** is ever echoed in an error here: a
/// diagnostic identifies the offending `--env` argument by position, because the
/// text after `=` may be a credential.
fn build_environment_patch(
    args: &LaunchArgs,
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
        let Some(value) = std::env::var_os(name) else {
            bail!(
                "--env-passthrough {name}: {name} is not set in pmux's own environment; \
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

    // `snapshot - unset + set` would silently let `set` win. A caller who wrote
    // both meant one of them, and pmux cannot tell which.
    if let Some(key) = unset.iter().find(|key| set.contains_key(*key)) {
        bail!("environment variable {key} is both unset and set; choose one");
    }

    Ok((set, unset))
}

/// Rejects the names `crates/service/src/claude_launch.rs::validate_environment`
/// rejects, but at the command line where the caller can still fix it.
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

// ---------------------------------------------------------------------------
// Launch-environment policy, evaluated locally so `pmux probe` can report drops.
//
// `spec.md` §4.5 requires `pmux probe` to render, by name only, the set of
// variables the launched Claude does not receive, and `spec.md` §9.1 requires a
// dry-run probe to reach no daemon at all. The policy therefore has to be
// evaluated in this process.
//
// It used to be evaluated from a hand-kept copy of the daemon's tables, pinned
// by a source-text-parsing fence in `bin/pmux/tests/launch_environment.rs`. Both
// are gone: `pseudomux_protocol::v1::launch_environment` is the one definition,
// and the CLI reaches it through a dependency it already had. That the client
// can predict the daemon's answer at all is precisely why the policy is protocol
// rather than service detail -- protocol v1 carries no field for the daemon's
// own `ResolvedClaudeLaunch::removed_environment_keys`, so `probe --launch`
// reports this same locally computed set rather than the daemon's answer.
//
// Nothing here can change a launch: the daemon decides, and this only names.
// Matching is case-sensitive in both forms; the protocol module documents why.

/// Every name the request offers that the launched Claude will not receive,
/// evaluated with the same ordering `claude_launch.rs::build_environment` uses:
/// `allowlist(snapshot) - unset + set - policy_removals + profile_changes`.
///
/// Names only. Values are never returned, logged, or serialized (`spec.md:461`).
pub fn dropped_environment_names(request: &StartSessionRequest) -> Vec<String> {
    let spec = &request.environment;
    let auth_policy = request.auth_policy;
    let mut removed = BTreeSet::new();
    let mut delivered = BTreeSet::new();

    for key in spec.snapshot.keys() {
        if launch_environment::inherits(key, auth_policy) {
            delivered.insert(key.clone());
        } else {
            removed.insert(key.clone());
        }
    }

    for key in &spec.unset {
        delivered.remove(key);
    }
    for key in spec.set.keys() {
        delivered.insert(key.clone());
    }

    if auth_policy == AuthPolicy::Subscription {
        for key in SUBSCRIPTION_AUTH_KEYS {
            if delivered.remove(*key) {
                removed.insert((*key).to_owned());
            }
        }
    }

    if request.terminal.profile == TerminalProfile::Transparent {
        delivered.retain(|key| {
            let strip = launch_environment::transparent_profile_removes(key);
            if strip {
                removed.insert(key.clone());
            }
            !strip
        });
        // The transparent profile writes TERM back after the removals.
        delivered.insert("TERM".into());
    }

    // A name the allowlist dropped but `set` restored is not a removal.
    removed.retain(|key| !delivered.contains(key));
    removed.into_iter().collect()
}

pub fn build_turn_request(args: &TurnArgs, prompt: String) -> Result<TurnRequest> {
    if args.timeout_secs == 0 {
        bail!("turn timeout must be greater than zero");
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_millis();
    let timeout_ms = u128::from(args.timeout_secs)
        .checked_mul(1_000)
        .context("turn timeout is too large")?;
    let deadline = now_ms
        .checked_add(timeout_ms)
        .and_then(|value| u64::try_from(value).ok())
        .context("turn deadline does not fit protocol timestamp")?;
    Ok(TurnRequest {
        turn_id: args.turn_id.unwrap_or_else(Uuid::new_v4),
        prompt,
        deadline_unix_ms: Some(deadline),
        lease: TurnLeasePolicy {
            on_disconnect: args.on_disconnect.into(),
            heartbeat_timeout_ms: args.heartbeat_timeout_ms,
        },
    })
}

pub fn read_prompt(args: &PromptArgs) -> Result<String> {
    read_prompt_text(args, "prompt", ComposerBound::Yes)
}

/// Whether the text being read will be typed into Claude's composer.
///
/// THE ONE GUARD A SYSTEM PROMPT DOES NOT GET. A turn prompt is typed into the
/// composer, where a leading solidus is a slash command; a system prompt is
/// delivered as a `--system-prompt` argument to the Claude process and never
/// reaches a composer at all. Applying the slash rule to a system prompt would
/// refuse a legitimate document -- "/usr/bin is on PATH" -- with a message
/// naming a typed control API that has nothing to do with it.
#[derive(Clone, Copy, Eq, PartialEq)]
enum ComposerBound {
    Yes,
    No,
}

/// Reads, normalizes and bounds one prompt-shaped document from argv, a file or
/// stdin.
///
/// `label` names the thing in every refusal, so a caller who pointed
/// `--system-prompt-file` at an unreadable path is told which prompt failed.
/// Every other rule here is about the BYTES rather than about the composer,
/// which is why the system prompt shares them: UTF-8, one line-ending
/// convention, one trailing terminator, a byte budget, non-empty, and no
/// terminal control characters.
///
/// The composer guard stays exactly where it was relative to the control-
/// character scan. U+0085 NEXT LINE is both `White_Space` and `char::is_control`,
/// so `"\u{85}/clear"` is refused by whichever of the two runs first -- and only
/// one of the two refusals tells the caller what they actually did.
///
/// A prompt ENDING in U+0085 now reaches that scan instead of losing the
/// character to the normalization in front of it: `is_trimmed_from_the_end`
/// deletes nothing `is_refused_wherever_it_stands` names, and Claude Code
/// 2.1.227 was MEASURED recording a trailing U+0085 verbatim
/// (`docs/path-b-adversarial.md` sec. 12), so there was never a version of this
/// where deleting it matched the composer.
fn read_prompt_text(args: &PromptArgs, label: &str, composer: ComposerBound) -> Result<String> {
    let bytes = match (&args.prompt, &args.prompt_file) {
        (Some(prompt), None) => prompt.as_bytes().to_vec(),
        (None, Some(path)) if path == Path::new("-") => read_limited(io::stdin().lock())?,
        (None, Some(path)) => {
            let file = fs::File::open(path)
                .with_context(|| format!("failed to open {label} file {}", path.display()))?;
            read_limited(file)?
        }
        (None, None) if !io::stdin().is_terminal() => read_limited(io::stdin().lock())?,
        (None, None) => bail!("provide PROMPT, --prompt-file, or pipe UTF-8 prompt text on stdin"),
        (Some(_), Some(_)) => bail!("{label} sources are mutually exclusive"),
    };
    let prompt =
        String::from_utf8(bytes).with_context(|| format!("{label} must be valid UTF-8"))?;
    // Line endings folded, NFC, and the composer's WHOLE trailing trim applied
    // -- not one terminator, all of them, plus every other character
    // `pseudomux_claude::is_trimmed_from_the_end` names.
    //
    // This comment said "exactly one text-file terminator dropped" and called
    // it "the same `exactly one` rule `crates/claude/src/cursor.rs:196` applies
    // to a trailing CR". Both halves stopped being true in `48aee00`: the
    // one-newline rule was measured to be half of what the composer does (two
    // newlines, and one trailing space, each destroyed the instance that proved
    // it), and `crates/client/src/prompt.rs` now carries the retraction at
    // length. The cursor's CR rule really is exactly-one, about a different
    // boundary, and the equivalence is what would invite the `strip_suffix`
    // back.
    //
    // The rule and the measurement behind it live in `normalize_cli_prompt`,
    // which `claude-p` calls on the same bytes; a second copy here is how the
    // facade came to keep the terminator that killed every turn it armed.
    let normalized = pseudomux_client::normalize_cli_prompt(&prompt);
    if normalized.len() as u64 > MAX_PROMPT_BYTES {
        bail!("{label} exceeds the {MAX_PROMPT_BYTES}-byte CLI limit");
    }
    if normalized.is_empty() {
        bail!("{label} must not be empty");
    }
    if composer == ComposerBound::Yes
        && let Some(refusal) = pseudomux_client::prompt::composer_refusal(&normalized)
    {
        bail!("{}", refusal.describe());
    }
    // `\t` is deliberately absent from this exception where it once stood: the
    // composer rule above has already refused it for a composer-bound prompt,
    // with a message that says what the composer does to it. A system prompt is
    // not composer-bound and never reaches a composer, so a tab in one is
    // ordinary text and stays admitted here.
    for character in normalized.chars() {
        if character == '\0'
            || character == '\u{1b}'
            || (character.is_control() && !matches!(character, '\n' | '\t'))
        {
            bail!("{label} contains an unsafe control character");
        }
    }
    Ok(normalized)
}

pub fn resolve_cwd(cwd: Option<&Path>) -> Result<PathBuf> {
    let requested = match cwd {
        Some(path) if path.is_absolute() => path.to_path_buf(),
        Some(path) => std::env::current_dir()?.join(path),
        None => std::env::current_dir()?,
    };
    let resolved = fs::canonicalize(&requested)
        .with_context(|| format!("working directory does not exist: {}", requested.display()))?;
    if !resolved.is_dir() {
        bail!(
            "working directory is not a directory: {}",
            resolved.display()
        );
    }
    Ok(resolved)
}

/// The default Claude executable, applied here rather than by clap.
///
/// See [`LaunchArgs::claude`] for why the flag has no `default_value`.
const DEFAULT_CLAUDE_EXECUTABLE: &str = "claude";

pub fn resolve_executable(requested: Option<&Path>) -> Result<PathBuf> {
    let requested = requested.unwrap_or_else(|| Path::new(DEFAULT_CLAUDE_EXECUTABLE));
    let candidate = if requested.is_absolute() {
        requested.to_path_buf()
    } else if requested.components().count() > 1 {
        std::env::current_dir()?.join(requested)
    } else {
        find_on_path(requested.as_os_str()).with_context(|| {
            format!(
                "Claude executable not found on PATH: {}",
                requested.display()
            )
        })?
    };
    let resolved = fs::canonicalize(&candidate)
        .with_context(|| format!("Claude executable does not exist: {}", candidate.display()))?;
    let metadata = fs::metadata(&resolved)?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        bail!(
            "Claude path is not an executable file: {}",
            resolved.display()
        );
    }
    Ok(resolved)
}

fn config_sources(files: &[PathBuf], inline: &[String], label: &str) -> Result<Vec<ConfigSource>> {
    let mut result = Vec::with_capacity(files.len() + inline.len());
    for path in files {
        result.push(ConfigSource::File {
            path: absolute_utf8_path(path, false, label)?,
        });
    }
    for document in inline {
        result.push(ConfigSource::Inline {
            document: serde_json::from_str(document)
                .with_context(|| format!("invalid inline {label} JSON"))?,
        });
    }
    Ok(result)
}

fn absolute_utf8_path(path: &Path, directory: bool, label: &str) -> Result<String> {
    let requested = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let resolved = fs::canonicalize(&requested)
        .with_context(|| format!("{label} does not exist: {}", requested.display()))?;
    if directory != resolved.is_dir() {
        bail!("{label} has the wrong file type: {}", resolved.display());
    }
    path_to_utf8(&resolved, label)
}

fn persistent_retention(seconds: u64) -> Result<RetentionPolicy> {
    let idle_ttl_ms = seconds
        .checked_mul(1_000)
        .context("idle TTL is too large")?;
    if idle_ttl_ms == 0 {
        bail!("persistent idle TTL must be greater than zero");
    }
    Ok(RetentionPolicy::Persistent { idle_ttl_ms })
}

fn path_to_utf8(path: &Path, label: &str) -> Result<String> {
    path.to_str()
        .map(ToOwned::to_owned)
        .with_context(|| format!("{label} is not valid UTF-8: {}", path.display()))
}

fn find_on_path(executable: &OsStr) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(executable))
        .find(|candidate| {
            fs::metadata(candidate)
                .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        })
}

fn read_limited(reader: impl Read) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_PROMPT_SOURCE_BYTES)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

impl From<EffortArg> for EffortLevel {
    fn from(value: EffortArg) -> Self {
        match value {
            EffortArg::Low => Self::Low,
            EffortArg::Medium => Self::Medium,
            EffortArg::High => Self::High,
            EffortArg::XHigh => Self::XHigh,
            EffortArg::Max => Self::Max,
        }
    }
}

impl From<PermissionArg> for PermissionMode {
    fn from(value: PermissionArg) -> Self {
        match value {
            PermissionArg::Default => Self::Default,
            PermissionArg::AcceptEdits => Self::AcceptEdits,
            PermissionArg::Plan => Self::Plan,
            PermissionArg::Auto => Self::Auto,
            PermissionArg::BypassPermissions => Self::BypassPermissions,
            PermissionArg::DontAsk => Self::DontAsk,
            PermissionArg::DangerouslySkipPermissions => Self::DangerouslySkipPermissions,
        }
    }
}

impl From<AuthArg> for AuthPolicy {
    fn from(value: AuthArg) -> Self {
        match value {
            AuthArg::Subscription => Self::Subscription,
            AuthArg::Inherit => Self::Inherit,
        }
    }
}

impl From<TerminalProfileArg> for TerminalProfile {
    fn from(value: TerminalProfileArg) -> Self {
        match value {
            TerminalProfileArg::Transparent => Self::Transparent,
            TerminalProfileArg::RmuxStandard => Self::RmuxStandard,
        }
    }
}

impl From<InputTransportArg> for InputTransport {
    fn from(value: InputTransportArg) -> Self {
        match value {
            InputTransportArg::Auto => Self::Auto,
            InputTransportArg::Sdk => Self::Sdk,
            InputTransportArg::AttachedStream => Self::AttachedStream,
        }
    }
}

impl From<CellArg> for SessionCell {
    fn from(value: CellArg) -> Self {
        match value {
            CellArg::Full => Self::Full,
            CellArg::Minified => Self::Minified,
        }
    }
}

impl From<CompatibilityArg> for CompatibilityPolicy {
    fn from(value: CompatibilityArg) -> Self {
        match value {
            CompatibilityArg::RequireTested => Self::RequireTested,
            CompatibilityArg::AllowUntested => Self::AllowUntested,
        }
    }
}

impl From<DisconnectArg> for DisconnectAction {
    fn from(value: DisconnectArg) -> Self {
        match value {
            DisconnectArg::Continue => Self::Continue,
            DisconnectArg::CancelTurn => Self::CancelTurn,
            DisconnectArg::CloseSession => Self::CloseSession,
        }
    }
}

impl From<ClosePolicyArg> for pseudomux_protocol::v1::ClosePolicy {
    fn from(value: ClosePolicyArg) -> Self {
        match value {
            ClosePolicyArg::Graceful => Self::Graceful,
            ClosePolicyArg::Force => Self::Force,
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    /// The rendered `--help` for one subcommand, exactly as a user sees it.
    ///
    /// `build()` first: clap propagates `global = true` arguments into every
    /// subcommand during the build, so a help rendered off an unbuilt command
    /// is missing exactly the arguments this file made global.
    fn rendered_help(subcommand: &str) -> String {
        let mut command = Cli::command();
        command.build();
        command
            .find_subcommand_mut(subcommand)
            .unwrap_or_else(|| panic!("no {subcommand} subcommand"))
            .render_long_help()
            .to_string()
    }

    /// Every subcommand a user can type, `help` excluded: it is clap's and its
    /// text is not ours to hold to these rules.
    fn user_subcommands() -> Vec<clap::Command> {
        Cli::command()
            .get_subcommands()
            .filter(|command| command.get_name() != "help")
            .cloned()
            .collect()
    }

    #[test]
    fn requires_explicit_socket() {
        let result = Cli::try_parse_from(["pmux", "ping"]);
        assert!(result.is_err());
    }

    /// NOTHING TESTED HELP TEXT BEFORE THIS FILE'S POLISH PASS, and five
    /// subcommands plus twenty-three arguments shipped with none at all --
    /// `ping`, `inspect`, `cancel`, `close` and `attach` had no description in
    /// `pmux --help`, and `--model`, `--effort`, `--rows`, `--read-only`,
    /// `--launch`, `--keep` and the rest rendered as a bare flag name.
    ///
    /// The set walked here is clap's own command tree, so a flag added later
    /// without help is red without anyone remembering to list it. That is the
    /// whole point: a hand-kept inventory of "arguments that must have help" is
    /// the defect this repo keeps finding, one level up.
    #[test]
    fn every_subcommand_and_argument_a_user_can_type_carries_help_text() {
        let mut missing = Vec::new();
        for command in user_subcommands() {
            if command.get_about().is_none() {
                missing.push(command.get_name().to_owned());
            }
            for argument in command.get_arguments() {
                // `-h/--help` is clap's own and is documented by clap.
                if argument.get_id() == "help" {
                    continue;
                }
                if argument.get_help().is_none() && argument.get_long_help().is_none() {
                    missing.push(format!("{} {}", command.get_name(), argument.get_id()));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "these subcommands/arguments render in --help with no description: {missing:?}"
        );
    }

    /// A user must be able to tell, from `pmux --help` alone, which product
    /// they are driving.
    ///
    /// Derived over every subcommand rather than over a list, so a subcommand
    /// added later has to answer the question too.
    #[test]
    fn every_subcommand_says_which_path_it_is_on() {
        for command in user_subcommands() {
            let about = command
                .get_about()
                .expect("about is required above")
                .to_string();
            assert!(
                about.starts_with("Path A")
                    || about.starts_with("Path B")
                    || about.starts_with("Neither path"),
                "`pmux {}` does not say which path it belongs to: {about:?}",
                command.get_name()
            );
        }
    }

    /// Every value this CLI offers that the daemon refuses, named as such in
    /// the help that offers it.
    ///
    /// SEVEN of these shipped silently. `--terminal-profile rmux-standard`,
    /// `--input-transport attached-stream`, `--retention one-shot`,
    /// `--on-disconnect cancel-turn`/`close-session`,
    /// `--heartbeat-timeout-ms` and `attach --read-only` are all accepted by
    /// clap, framed into a request, and refused by the daemon with
    /// `unsupported_feature`; `close --policy` is accepted by both and changes
    /// nothing. Their help said none of that, so `[possible values: ...]` was
    /// the only thing a user had to go on and it was an advertisement.
    ///
    /// THE AUTHORITY IS THE SERVICE, not this table:
    ///
    /// * `compatibility.rs:833` -- "rmux-standard terminal identity is reserved
    ///   and is not implemented in protocol v1"
    /// * `compatibility.rs:839` -- "attached-stream prompt injection is
    ///   reserved and is not implemented in protocol v1"
    /// * `native.rs:3590` -- "one_shot retention is reserved for run_once;
    ///   start_session requires persistent retention"
    /// * `native.rs:2756`, `v1/actor.rs:1269` -- "disconnect actions and
    ///   heartbeat leases require a future leased connection API"
    /// * `native.rs:2012` -- "read-only attach is not implemented by the pinned
    ///   rmux stream protocol"
    /// * `driver_io.rs:1778` -- `close(&self, _session_id, _policy)`: every
    ///   `TerminalControl` implementation in the tree discards the policy.
    ///
    /// Checked from two directions. Each censused value must still be one clap
    /// offers, so a value renamed or withdrawn cannot leave a stale entry
    /// passing; and each must be named in the rendered help of the subcommand
    /// that offers it, so making the help vaguer turns this red.
    #[test]
    fn every_value_this_cli_offers_that_the_daemon_refuses_says_so_in_its_own_help() {
        // (subcommand, argument id, value clap offers or "" for the flag
        // itself, substring the rendered help must contain).
        let census = [
            ("start", "terminal_profile", "rmux-standard", "reserved"),
            ("start", "input_transport", "attached-stream", "reserved"),
            ("start", "retention", "one-shot", "reserved"),
            ("oneshot", "on_disconnect", "cancel-turn", "refused"),
            ("oneshot", "on_disconnect", "close-session", "refused"),
            ("oneshot", "heartbeat_timeout_ms", "", "NOT IMPLEMENTED"),
            ("attach", "read_only", "", "NOT IMPLEMENTED"),
            ("close", "policy", "", "does NOT currently change"),
        ];
        for (subcommand, argument_id, value, promise) in census {
            let command = Cli::command()
                .find_subcommand_mut(subcommand)
                .unwrap_or_else(|| panic!("no {subcommand} subcommand"))
                .clone();
            let argument = command
                .get_arguments()
                .find(|argument| argument.get_id() == argument_id)
                .unwrap_or_else(|| panic!("`pmux {subcommand}` has no {argument_id} argument"));
            if !value.is_empty() {
                let offered: Vec<String> = argument
                    .get_possible_values()
                    .iter()
                    .map(|possible| possible.get_name().to_owned())
                    .collect();
                assert!(
                    offered.iter().any(|name| name == value),
                    "`pmux {subcommand} --{}` no longer offers {value:?} \
                     (it offers {offered:?}); this census entry is stale",
                    argument_id.replace('_', "-")
                );
            }
            let help = argument
                .get_long_help()
                .or_else(|| argument.get_help())
                .map(ToString::to_string)
                .unwrap_or_default();
            assert!(
                help.contains(promise),
                "`pmux {subcommand} --{}` offers {value:?}, which the daemon refuses or ignores, \
                 and its help does not say so (looked for {promise:?} in {help:?})",
                argument_id.replace('_', "-")
            );
        }
    }

    /// EVERY launch flag is either per-session or named as agent-supplied.
    ///
    /// `launch_flags_a_stored_agent_supplies` classifies `LaunchArgs` by
    /// destructuring it without `..`, so a flag added to the struct stops the
    /// binary compiling until it is classified. That forces a DECISION; this
    /// forces the decision to be RIGHT, from the other side: it walks clap's
    /// own argument ids for `start` and asserts that the two classes partition
    /// them exactly. A flag added and quietly dropped from both lists is red
    /// here, and a flag named as agent-supplied that clap does not offer is red
    /// too.
    #[test]
    fn every_launch_flag_is_either_per_session_or_named_as_agent_supplied() {
        // Every launch flag set at once, so `launch_flags_a_stored_agent_supplies`
        // reports the complete agent-supplied class rather than whichever
        // members this fixture happened to set.
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path().to_str().unwrap().to_owned();
        let file = directory.path().join("doc.json");
        fs::write(&file, "{}").unwrap();
        let document = file.to_str().unwrap().to_owned();
        let everything = launch_args(&[
            "--cwd",
            &cwd,
            "--claude",
            "/bin/sh",
            "--model",
            "sonnet",
            "--effort",
            "high",
            "--permission-mode",
            "plan",
            "--allowed-tool",
            "Read",
            "--denied-tool",
            "Bash",
            "--settings",
            &document,
            "--settings-json",
            "{}",
            "--mcp-config",
            &document,
            "--mcp-json",
            "{}",
            "--plugin-dir",
            &cwd,
            "--extra-arg",
            "--debug",
            "--system-prompt",
            "be exact",
            "--auth",
            "inherit",
            "--env",
            "A=1",
            "--env-passthrough",
            "PATH",
            "--unset",
            "TERM",
            "--rows",
            "30",
            "--cols",
            "100",
            "--terminal-profile",
            "transparent",
            "--input-transport",
            "sdk",
            "--lifecycle",
            "hybrid",
            "--hook-timeout-ms",
            "1000",
            "--retention",
            "persistent",
            "--idle-ttl-secs",
            "60",
            "--compatibility",
            "allow-untested",
            "--cell",
            "full",
            "--profile",
            "reviewer",
        ]);
        let supplied: BTreeSet<String> = launch_flags_a_stored_agent_supplies(&everything)
            .into_iter()
            .map(|(id, _)| id.to_owned())
            .collect();

        // clap's own ids for the subcommand, minus the ones this classification
        // is ABOUT rather than over: the reference itself, and the two retired
        // spellings that are refused by name before any of this runs.
        let control = BTreeSet::from([
            "agent".to_owned(),
            "agent_version".to_owned(),
            "agent_file".to_owned(),
            "profile_file".to_owned(),
            "help".to_owned(),
        ]);
        let per_session = BTreeSet::from([
            "session_id".to_owned(),
            "resume".to_owned(),
            "cwd".to_owned(),
            "config_isolation_root".to_owned(),
        ]);
        let offered: BTreeSet<String> = Cli::command()
            .find_subcommand_mut("start")
            .expect("no start subcommand")
            .get_arguments()
            .map(|argument| argument.get_id().to_string())
            .collect();

        // Some agent-supplied flags are alternate spellings of one another and
        // are pairwise exclusive in clap, so a single command line cannot set
        // them all. Those appear in clap's ids and not in `supplied`.
        let alternates = BTreeSet::from([
            "append_system_prompt".to_owned(),
            "system_prompt_file".to_owned(),
            "append_system_prompt_file".to_owned(),
        ]);

        let unclassified: BTreeSet<String> = offered
            .difference(
                &supplied
                    .union(&control)
                    .cloned()
                    .collect::<BTreeSet<_>>()
                    .union(&per_session)
                    .cloned()
                    .collect::<BTreeSet<_>>()
                    .union(&alternates)
                    .cloned()
                    .collect::<BTreeSet<_>>(),
            )
            .cloned()
            .collect();
        assert!(
            unclassified.is_empty(),
            "`pmux start` offers flags that are neither per-session nor named as agent-supplied, \
             so `--agent` would silently discard them: {unclassified:?}"
        );

        let stale: BTreeSet<String> = supplied.difference(&offered).cloned().collect();
        assert!(
            stale.is_empty(),
            "the agent-supplied census names flags `pmux start` no longer offers: {stale:?}"
        );
    }

    /// `--agent` and any inline launch flag is refused HERE, in the caller's own
    /// vocabulary.
    ///
    /// The daemon refuses it too and its refusal is the authority, but it names
    /// `terminal` -- a wire field nothing on the command line is spelled that
    /// way. A caller learns here, before a session is started, and is told the
    /// flag to drop.
    #[test]
    fn naming_a_stored_agent_and_an_inline_launch_flag_is_refused_by_flag_name() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path().to_str().unwrap().to_owned();
        let agent = uuid::Uuid::from_u128(6).hyphenated().to_string();

        let clean = build_start_request(&launch_args(&[
            "--agent",
            &agent,
            "--agent-version",
            "3",
            "--cwd",
            &cwd,
        ]))
        .expect("an agent reference plus a cwd is a complete start");
        assert!(clean.claude.is_none());
        assert_eq!(clean.agent.expect("agent").version.get(), 3);
        assert!(
            !clean.environment.snapshot.is_empty(),
            "the caller's own snapshot is the one launch input an agent cannot carry"
        );

        for (flag, value) in [
            ("--model", Some("sonnet")),
            ("--rows", Some("30")),
            ("--cell", Some("full")),
            ("--env", Some("A=1")),
            ("--claude", Some("/bin/sh")),
            ("--profile", Some("reviewer")),
        ] {
            let mut argv = vec![
                "--agent",
                &agent,
                "--agent-version",
                "3",
                "--cwd",
                &cwd,
                flag,
            ];
            if let Some(value) = value {
                argv.push(value);
            }
            let error = build_start_request(&launch_args(&argv))
                .expect_err("an agent supplies the whole launch policy")
                .to_string();
            assert!(
                error.contains(flag),
                "the refusal must name {flag}: {error}"
            );
        }
    }

    /// A launch value the caller's ENVIRONMENT supplies is overridden by
    /// `--agent` and reported; a flag the caller TYPED is refused.
    ///
    /// MEASURED before this split:
    ///
    /// ```text
    /// $ PMUX_MODEL=opus pmux start --agent ... --agent-version 1 --cwd /tmp
    /// pmux: --agent supplies the whole launch policy, so it cannot be combined with --model. ...
    /// ```
    ///
    /// naming a flag the caller never typed, and locking anyone with
    /// `PMUX_MODEL` exported in a shell rc out of `--agent` entirely. `env` is
    /// clap's lowest precedence source below argv, and `--agent` is a higher
    /// one; the note exists so the precedence is never silent, which is the
    /// same rule the profile diagnostics already follow.
    ///
    /// The five env-backed agent-supplied flags are DERIVED from clap's own
    /// argument metadata, so a binding added or renamed is exercised here with
    /// nobody remembering this test exists.
    #[test]
    fn a_launch_value_from_the_environment_is_overridden_by_agent_and_reported_by_variable() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path().to_str().unwrap().to_owned();
        let agent = uuid::Uuid::from_u128(6).hyphenated().to_string();

        // Every agent-supplied flag clap can fill from the environment, read
        // out of clap rather than listed here.
        let agent_supplied: BTreeSet<String> = {
            let everything = launch_args(&[
                "--cwd",
                "/tmp",
                "--claude",
                "/bin/sh",
                "--model",
                "sonnet",
                "--effort",
                "high",
                "--permission-mode",
                "plan",
                "--profile",
                "reviewer",
            ]);
            launch_flags_a_stored_agent_supplies(&everything)
                .into_iter()
                .map(|(id, _)| id.to_owned())
                .collect()
        };
        let env_backed: Vec<(String, String)> = <Cli as clap::CommandFactory>::command()
            .find_subcommand_mut("start")
            .expect("no start subcommand")
            .get_arguments()
            .filter(|argument| agent_supplied.contains(argument.get_id().as_str()))
            .filter_map(|argument| {
                Some((
                    argument.get_id().to_string(),
                    argument.get_env()?.to_string_lossy().into_owned(),
                ))
            })
            .collect();
        assert_eq!(
            env_backed.len(),
            5,
            "the env-backed agent-supplied class changed; review the note text: {env_backed:?}"
        );

        for (id, variable) in env_backed {
            // Built the way clap builds it when the value came from `env`: the
            // field is set AND the id is recorded as environment-sourced.
            let mut args = launch_args(&["--agent", &agent, "--agent-version", "3", "--cwd", &cwd]);
            match id.as_str() {
                "claude" => args.claude = Some(PathBuf::from("/bin/sh")),
                "model" => args.model = Some("sonnet".to_owned()),
                "effort" => args.effort = Some(EffortArg::High),
                "permission_mode" => args.permission_mode = Some(PermissionArg::Plan),
                "profile" => args.profile = Some("reviewer".to_owned()),
                other => panic!("no fixture value for env-backed launch flag {other}"),
            }

            // TYPED (nothing recorded as environment-sourced): refused.
            let error = build_start_request_with_diagnostics(&args)
                .expect_err("a typed launch flag beside --agent is refused")
                .to_string();
            assert!(error.contains("cannot be combined with"), "{id}: {error}");

            // FROM THE ENVIRONMENT: admitted, and reported by VARIABLE.
            args.from_environment = BTreeSet::from([id.clone()]);
            let (request, notes) =
                build_start_request_with_diagnostics(&args).unwrap_or_else(|error| {
                    panic!("{id} came from {variable}, not from argv: {error}")
                });
            assert!(request.agent.is_some(), "{id}");
            assert!(request.claude.is_none(), "{id}");
            assert_eq!(notes.len(), 1, "{id}: {notes:?}");
            assert!(
                notes[0].contains(&variable),
                "{id}: the note must name the variable the value came from, not a flag the caller \
                 never typed: {}",
                notes[0]
            );
            assert!(!notes[0].starts_with("--"), "{id}: {}", notes[0]);
        }
    }

    /// The retired profile spellings are refused with the new one NAMED, and
    /// never silently aliased.
    #[test]
    fn the_retired_profile_spellings_are_refused_by_name() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path().to_str().unwrap().to_owned();
        let error = build_start_request(&launch_args(&[
            "--cwd",
            &cwd,
            "--agent-file",
            "/nonexistent/profiles.json",
        ]))
        .expect_err("--agent-file was renamed")
        .to_string();
        assert!(error.contains("--profile-file"), "{error}");
        assert!(
            error.contains("--agent now names a stored server agent"),
            "the refusal must say WHY the spelling moved: {error}"
        );
    }

    /// Path B's product statement, held on the CLI the way
    /// `the_run_stateless_tool_refuses_every_resource_a_caller_might_name`
    /// holds it on the MCP schema.
    ///
    /// Derived from clap's own argument ids for `ask`, so a resource flag added
    /// to this subcommand later is red here rather than in a leak report.
    #[test]
    fn the_path_b_subcommand_names_no_resource() {
        let command = Cli::command().find_subcommand_mut("run").unwrap().clone();
        let offered: BTreeSet<String> = command
            .get_arguments()
            .map(|argument| argument.get_id().to_string())
            .collect();
        // Every argument `run` declares of its own. `--socket` and `--output`
        // are global and belong to the binary rather than to this subcommand;
        // neither names a resource the daemon would use for the turn.
        let admitted = BTreeSet::from([
            "model".to_owned(),
            "effort".to_owned(),
            "prompt".to_owned(),
            "prompt_file".to_owned(),
            "deadline_unix_ms".to_owned(),
        ]);
        assert_eq!(
            offered, admitted,
            "`pmux run` gained or lost an argument; every addition here is a resource a Path B \
             caller could name, which is the one thing this subcommand promises it cannot do"
        );
    }

    /// EVERYWHERE a prompt is taken, it is takeable from a file.
    ///
    /// `--system-prompt` and `--append-system-prompt` were argv-only: a system
    /// prompt is long, is usually generated, and on argv it is visible to `ps`
    /// -- and `pmuxd` had already grown `--path-b-system-prompt-file` for
    /// exactly that reason while its own client had not.
    ///
    /// The rule is derived from the argument names rather than stated over a
    /// list: any argument whose id mentions a prompt and is not itself a file
    /// form must have a `<id>_file` sibling in the same subcommand. A future
    /// `--review-prompt` is therefore held to the same rule without anyone
    /// remembering this test exists.
    #[test]
    fn every_prompt_this_cli_takes_is_also_takeable_from_a_file() {
        let mut argv_only = Vec::new();
        for command in user_subcommands() {
            let ids: BTreeSet<String> = command
                .get_arguments()
                .map(|argument| argument.get_id().to_string())
                .collect();
            for id in &ids {
                if !id.contains("prompt") || id.ends_with("_file") {
                    continue;
                }
                if !ids.contains(&format!("{id}_file")) {
                    argv_only.push(format!("{} {id}", command.get_name()));
                }
            }
        }
        assert!(
            argv_only.is_empty(),
            "these prompts can only be given on argv, where `ps` can read them: {argv_only:?}"
        );
    }

    #[test]
    fn a_system_prompt_is_readable_from_a_file_in_both_modes() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("system.md");
        // Written with the trailing newline every conventional tool writes.
        fs::write(&file, "stay concise\n").unwrap();

        let replace = build_start_request(&launch_args(&[
            "--claude",
            "/bin/sh",
            "--cwd",
            directory.path().to_str().unwrap(),
            "--system-prompt-file",
            file.to_str().unwrap(),
        ]))
        .unwrap();
        assert_eq!(
            replace
                .claude
                .as_ref()
                .expect("inline launch")
                .system_prompt,
            SystemPromptPolicy::Replace {
                prompt: "stay concise".into()
            },
            "the POSIX text-file terminator is not part of the system prompt"
        );

        let append = build_start_request(&launch_args(&[
            "--claude",
            "/bin/sh",
            "--cwd",
            directory.path().to_str().unwrap(),
            "--append-system-prompt-file",
            file.to_str().unwrap(),
        ]))
        .unwrap();
        assert_eq!(
            append.claude.as_ref().expect("inline launch").system_prompt,
            SystemPromptPolicy::Append {
                prompt: "stay concise".into()
            }
        );

        // A leading solidus is a composer concern and a system prompt never
        // reaches a composer, so the slash-command refusal must not fire here.
        let slashed = directory.path().join("slashed.md");
        fs::write(&slashed, "/usr/bin is on PATH; prefer absolute paths\n").unwrap();
        let request = build_start_request(&launch_args(&[
            "--claude",
            "/bin/sh",
            "--cwd",
            directory.path().to_str().unwrap(),
            "--system-prompt-file",
            slashed.to_str().unwrap(),
        ]))
        .unwrap();
        assert_eq!(
            request
                .claude
                .as_ref()
                .expect("inline launch")
                .system_prompt,
            SystemPromptPolicy::Replace {
                prompt: "/usr/bin is on PATH; prefer absolute paths".into()
            }
        );

        // A missing file names the system prompt, not "the prompt".
        let error = build_start_request(&launch_args(&[
            "--claude",
            "/bin/sh",
            "--cwd",
            directory.path().to_str().unwrap(),
            "--system-prompt-file",
            directory.path().join("absent.md").to_str().unwrap(),
        ]))
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("failed to open system prompt file"),
            "unexpected refusal: {error}"
        );
    }

    #[test]
    fn the_four_system_prompt_modes_are_mutually_exclusive_in_clap_and_in_the_builder() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("system.md");
        fs::write(&file, "stay concise").unwrap();
        let path = file.to_str().unwrap();
        for pair in [
            ["--system-prompt", "text", "--system-prompt-file", path],
            [
                "--append-system-prompt",
                "text",
                "--system-prompt-file",
                path,
            ],
            [
                "--system-prompt-file",
                path,
                "--append-system-prompt-file",
                path,
            ],
            ["--system-prompt", "text", "--append-system-prompt", "text"],
        ] {
            let mut argv = vec!["pmux", "--socket", "/tmp/pmux.sock", "start"];
            argv.extend_from_slice(&pair);
            assert!(
                Cli::try_parse_from(argv).is_err(),
                "clap accepted two system prompt modes at once: {pair:?}"
            );
        }

        // `LaunchArgs` is `pub`, so the builder refuses the combination too
        // rather than trusting clap to have already done it.
        let mut args = launch_args(&["--claude", "/bin/sh"]);
        args.system_prompt = Some("text".into());
        args.append_system_prompt_file = Some(file.clone());
        let error = resolve_system_prompt(&args).unwrap_err().to_string();
        assert!(error.contains("mutually exclusive"), "{error}");
    }

    /// The daemon refuses this too, and its refusal is the authority. The
    /// client's exists so the message names `--config-isolation-root` -- a flag
    /// the caller can type -- rather than `config_isolation`, a wire field
    /// nothing on the command line is spelled that way.
    #[test]
    fn a_minified_cell_with_no_private_root_is_refused_by_name_before_any_daemon_is_contacted() {
        let directory = tempfile::tempdir().unwrap();
        let error = build_start_request(&launch_args(&[
            "--claude",
            "/bin/sh",
            "--cwd",
            directory.path().to_str().unwrap(),
            "--cell",
            "minified",
        ]))
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("--cell minified requires --config-isolation-root"),
            "the refusal does not name the flag that fixes it: {error}"
        );

        // And it is only the missing root that is refused: with one, the same
        // command builds.
        let root = directory.path().join("private-root");
        fs::create_dir(&root).unwrap();
        let request = build_start_request(&launch_args(&[
            "--claude",
            "/bin/sh",
            "--cwd",
            directory.path().to_str().unwrap(),
            "--cell",
            "minified",
            "--config-isolation-root",
            root.to_str().unwrap(),
        ]))
        .unwrap();
        assert_eq!(request.cell, SessionCell::Minified);
    }

    /// `--output` is `global = true`, so its one help string is rendered on
    /// every subcommand -- including the ten that emit exactly one record and
    /// have no turn events at all. It used to read "NDJSON includes turn events
    /// followed by a result record" under `pmux ping`.
    #[test]
    fn the_global_output_help_does_not_promise_turn_events_to_subcommands_that_have_none() {
        for subcommand in [
            "ping", "inspect", "close", "clear", "attach", "doctor", "run",
        ] {
            let help = rendered_help(subcommand);
            assert!(
                help.contains("every other subcommand emits exactly one record"),
                "`pmux {subcommand} --help` does not scope the --output description: {help}"
            );
        }
    }

    #[test]
    fn prompt_normalizes_and_rejects_terminal_controls() {
        let prompt = read_prompt(&PromptArgs {
            prompt: Some("one\r\ntwo\rthree".into()),
            prompt_file: None,
        })
        .unwrap();
        assert_eq!(prompt, "one\ntwo\nthree");

        let error = read_prompt(&PromptArgs {
            prompt: Some("unsafe\u{1b}[2J".into()),
            prompt_file: None,
        })
        .unwrap_err();
        assert!(error.to_string().contains("unsafe control"));
    }

    #[test]
    fn no_cli_prompt_can_carry_a_composer_mode_command_to_the_daemon() {
        // The rule is `pseudomux_client::prompt::composer_refusal`, which the
        // service's `validate_prompt` also calls; the CLI refusal exists so a
        // caller learns before a session is started. It used to be a second
        // statement of one intention, and the two drifted in the only way that
        // mattered: both named `/` and neither named `!`.
        let read = |text: &str| {
            read_prompt(&PromptArgs {
                prompt: Some(text.into()),
                prompt_file: None,
            })
        };
        // Derived from the shipped mode set, so a character added to it adds
        // its own 22 cases rather than being tested by nobody.
        let invisibles = [
            "",
            " ",
            "\t",
            "\n",
            "\r\n",
            "\r",
            "  \t\n  ",
            "\u{a0}",              // NO-BREAK SPACE
            "\u{85}",              // NEXT LINE
            "\u{2003}",            // EM SPACE
            "\u{202f}",            // NARROW NO-BREAK SPACE
            "\u{3000}",            // IDEOGRAPHIC SPACE
            "\u{feff}",            // ZERO WIDTH NO-BREAK SPACE: stripped by JS `trim`
            "\u{200b}",            // ZERO WIDTH SPACE
            "\u{2060}",            // WORD JOINER
            "\u{ad}",              // SOFT HYPHEN
            "\u{200e}",            // LEFT-TO-RIGHT MARK
            "\u{202e}",            // RIGHT-TO-LEFT OVERRIDE
            "\u{feff} \u{200b}\t", // invisibles and whitespace interleaved
        ];
        let mut attempts = Vec::new();
        for prefix in pseudomux_client::prompt::COMPOSER_MODE_PREFIXES {
            for invisible in invisibles {
                attempts.push(format!("{invisible}{prefix}payload"));
            }
            attempts.push(format!("{prefix}payload\n"));
            attempts.push(format!("{prefix}payload\nand more"));
            attempts.push(format!("{prefix}{prefix}payload"));
            attempts.push(prefix.to_string());
        }
        for attempt in &attempts {
            let attempt = attempt.as_str();
            let error = read(attempt)
                .expect_err(&format!("prompt {attempt:?} must be refused"))
                .to_string();
            assert!(
                error.contains("switches the composer") || error.contains("command menu"),
                "prompt {attempt:?} was refused for the wrong reason: {error}"
            );
        }

        // Shapes that are not slash commands and must keep working. No reading
        // of these puts U+002F in first position -- not Rust's `trim_start`,
        // not JS's `trim`, not the invisible-format rule the guard applies --
        // and refusing them would break ordinary prompts (a pasted path, a
        // quoted command, a lookalike glyph, text carried out of a
        // Windows-authored file) for a threat that does not exist.
        for attempt in [
            "\u{2044}clear",        // FRACTION SLASH
            "\u{2215}clear",        // DIVISION SLASH
            "\u{ff0f}clear",        // FULLWIDTH SOLIDUS
            "\u{29f8}clear",        // BIG SOLIDUS
            "\u{feff}explain this", // a BOM ahead of ordinary text is ordinary text
            "\u{200b}explain this",
            "explain this:\n/clear",
            "explain this:\r\n/clear",
            "src/main.rs",
        ] {
            let prompt = read(attempt)
                .unwrap_or_else(|error| panic!("prompt {attempt:?} was refused: {error}"));
            assert_eq!(
                pseudomux_client::prompt::composer_refusal(&prompt),
                None,
                "prompt {attempt:?} would reach the composer as a mode character"
            );
            // The guard reads past those characters without removing them: the
            // daemon must receive the caller's bytes, since they are the text
            // the typed-prompt acknowledgement is matched against. Only the
            // line-ending normalization every prompt gets is expected here.
            assert_eq!(
                prompt,
                attempt.replace("\r\n", "\n"),
                "prompt {attempt:?} was rewritten on its way to the daemon"
            );
        }
    }

    #[test]
    fn prompt_drops_the_whole_trailing_run_the_composer_drops() {
        // Every conventional tool terminates a text file with a newline, but a
        // composer cannot hold one, so Claude records the typed prompt without
        // it. Keeping it made `expected` unequal to `actual` at engine.rs:127
        // and every --prompt-file turn died in `UnexpectedTypedPrompt`.
        //
        // This test asserted that EXACTLY ONE newline was dropped until
        // 2026-08-09, and named that as the guarantee that "a deliberate
        // trailing blank line survives". It did not survive: at 2.1.226 the
        // composer removes its whole trailing run of whitespace, so `"poem\n\n"`
        // was typed as `"poem\n"`, recorded as `"poem"`, and cost the pooled
        // instance -- as did `"  padded  "`, whose two trailing spaces this
        // rule never looked at (`docs/path-b-adversarial.md` sec. 11).
        let read = |text: &str| {
            read_prompt(&PromptArgs {
                prompt: Some(text.into()),
                prompt_file: None,
            })
            .unwrap()
        };
        assert_eq!(read("Reply with exactly: ok\n"), "Reply with exactly: ok");
        assert_eq!(read("Reply with exactly: ok"), "Reply with exactly: ok");
        assert_eq!(read("poem\n\n"), "poem");
        // CRLF is folded first, so a CRLF-terminated file behaves identically.
        assert_eq!(read("line one\r\nline two\r\n"), "line one\nline two");
        // Trailing whitespace goes; LEADING and INTERIOR whitespace stay. This
        // is the composer's `trimEnd` and not a `trim`, MEASURED both ways.
        assert_eq!(read("  padded  \n"), "  padded");
        assert_eq!(read("line one   \nline two"), "line one   \nline two");
        // The one invisible the composer keeps, so pmux keeps it too.
        assert_eq!(read("ok\u{200b}"), "ok\u{200b}");
        // Nothing but whitespace is nothing: refused as empty rather than typed
        // into a composer whose Enter would do nothing at all.
        let empty = read_prompt(&PromptArgs {
            prompt: Some("   \n ".into()),
            prompt_file: None,
        })
        .expect_err("a whitespace-only prompt is empty");
        assert!(
            empty.to_string().contains("must not be empty"),
            "got {empty}"
        );
    }

    #[test]
    fn the_terminator_is_not_charged_against_the_prompt_byte_budget() {
        // The strip runs before the length check, so a source carrying the full
        // budget of content plus its terminator is accepted and delivers the
        // full budget. Charging the terminator would make a file of exactly
        // MAX_PROMPT_BYTES of content unusable for a reason the caller cannot
        // see in its own content.
        let limit = usize::try_from(MAX_PROMPT_BYTES).unwrap();
        let at_limit_plus_terminator = "x".repeat(limit) + "\n";
        let prompt = read_prompt(&PromptArgs {
            prompt: Some(at_limit_plus_terminator),
            prompt_file: None,
        })
        .unwrap();
        assert_eq!(prompt.len(), limit);

        // One byte of real content past the budget is still refused.
        let over = "x".repeat(limit + 1) + "\n";
        let error = read_prompt(&PromptArgs {
            prompt: Some(over),
            prompt_file: None,
        })
        .unwrap_err();
        assert!(error.to_string().contains("exceeds"), "{error}");
    }

    #[test]
    fn comprehensive_launch_maps_to_native_dto() {
        let directory = tempfile::tempdir().unwrap();
        let settings = directory.path().join("settings.json");
        fs::write(&settings, "{}").unwrap();
        let plugin = directory.path().join("plugin");
        fs::create_dir(&plugin).unwrap();
        let session = Uuid::new_v4();
        let cli = Cli::try_parse_from([
            "pmux",
            "--socket",
            "/tmp/pmux.sock",
            "start",
            "--session-id",
            &session.to_string(),
            "--claude",
            "/bin/sh",
            "--cwd",
            directory.path().to_str().unwrap(),
            "--model",
            "sonnet",
            "--effort",
            "high",
            "--permission-mode",
            "plan",
            "--allowed-tool",
            "Read,Glob",
            "--settings",
            settings.to_str().unwrap(),
            "--settings-json",
            r#"{"hooks":{"Stop":[]}}"#,
            "--plugin-dir",
            plugin.to_str().unwrap(),
            "--append-system-prompt",
            "stay concise",
            "--auth",
            "inherit",
            "--terminal-profile",
            "rmux-standard",
            "--input-transport",
            "attached-stream",
            "--lifecycle",
            "hybrid",
            "--retention",
            "persistent",
            "--idle-ttl-secs",
            "60",
            "--compatibility",
            "allow-untested",
        ])
        .unwrap();
        let Command::Start { launch } = cli.command else {
            panic!("wrong command")
        };
        let request = build_start_request(&launch).unwrap();
        assert_eq!(
            request.identity,
            SessionIdentity::New {
                session_id: Some(session)
            }
        );
        assert_eq!(
            request
                .claude
                .as_ref()
                .expect("inline launch")
                .model
                .as_deref(),
            Some("sonnet")
        );
        assert_eq!(
            request
                .claude
                .as_ref()
                .expect("inline launch")
                .allowed_tools,
            ["Read", "Glob"]
        );
        assert_eq!(
            request
                .claude
                .as_ref()
                .expect("inline launch")
                .settings
                .len(),
            2
        );
        assert_eq!(
            request
                .claude
                .as_ref()
                .expect("inline launch")
                .plugin_dirs
                .len(),
            1
        );
        assert!(matches!(request.auth_policy, AuthPolicy::Inherit));
        assert!(matches!(request.lifecycle, LifecycleMode::Hybrid { .. }));
        assert_eq!(
            request.retention,
            RetentionPolicy::Persistent {
                idle_ttl_ms: 60_000
            }
        );
        assert!(!request.environment.snapshot.is_empty());
    }

    fn launch_args(arguments: &[&str]) -> LaunchArgs {
        let mut argv = vec!["pmux", "--socket", "/tmp/pmux.sock", "start"];
        argv.extend_from_slice(arguments);
        let Command::Start { launch } = Cli::try_parse_from(argv).unwrap().command else {
            panic!("wrong command")
        };
        launch
    }

    /// One document exercising every composition operator the CLI relies on.
    fn write_agent_file(directory: &Path, plugin: &Path) -> PathBuf {
        let path = directory.join("agents.json");
        fs::write(
            &path,
            format!(
                r#"{{
                  "version": 1,
                  "agents": {{
                    "base": {{
                      "claude": {{"model": "sonnet", "allowed_tools": ["Read"]}},
                      "terminal": {{"rows": 48, "cols": 160}}
                    }},
                    "yolo": {{
                      "extends": "base",
                      "claude": {{
                        "permission_mode": "dangerously_skip_permissions",
                        "allowed_tools": ["Bash(git:*)"],
                        "plugin_dirs": ["{plugin}"],
                        "extra_args": ["--debug"]
                      }},
                      "compatibility": "allow_untested"
                    }}
                  }}
                }}"#,
                plugin = plugin.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        path
    }

    #[test]
    fn an_agent_profile_expands_into_the_start_dto_before_the_request_is_framed() {
        let directory = tempfile::tempdir().unwrap();
        let plugin = directory.path().join("plugin");
        fs::create_dir(&plugin).unwrap();
        let agent_file = write_agent_file(directory.path(), &plugin);

        let (request, diagnostics) = build_start_request_with_diagnostics(&launch_args(&[
            "--profile",
            "yolo",
            "--profile-file",
            agent_file.to_str().unwrap(),
            "--claude",
            "/bin/sh",
            "--cwd",
            directory.path().to_str().unwrap(),
        ]))
        .unwrap();

        assert_eq!(
            request
                .claude
                .as_ref()
                .expect("inline launch")
                .model
                .as_deref(),
            Some("sonnet")
        );
        assert_eq!(
            request
                .claude
                .as_ref()
                .expect("inline launch")
                .permission_mode,
            Some(PermissionMode::DangerouslySkipPermissions)
        );
        assert_eq!(
            request
                .claude
                .as_ref()
                .expect("inline launch")
                .allowed_tools,
            ["Read", "Bash(git:*)"]
        );
        assert_eq!(
            request.claude.as_ref().expect("inline launch").plugin_dirs,
            [plugin.display().to_string()]
        );
        assert_eq!(
            request.claude.as_ref().expect("inline launch").extra_args,
            ["--debug"]
        );
        assert_eq!(request.terminal.rows, 48);
        assert_eq!(request.terminal.cols, 160);
        assert_eq!(request.compatibility, CompatibilityPolicy::AllowUntested);
        // Defaults still come from the CLI, not the profile, when unstated.
        assert_eq!(request.auth_policy, AuthPolicy::Subscription);
        assert_eq!(request.terminal.profile, TerminalProfile::Transparent);
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].contains("profile `yolo` expanded from"));
    }

    #[test]
    fn explicit_flags_override_profile_scalars_loudly_and_append_to_profile_lists() {
        let directory = tempfile::tempdir().unwrap();
        let plugin = directory.path().join("plugin");
        fs::create_dir(&plugin).unwrap();
        let agent_file = write_agent_file(directory.path(), &plugin);
        let cli_plugin = directory.path().join("cli-plugin");
        fs::create_dir(&cli_plugin).unwrap();

        let (request, diagnostics) = build_start_request_with_diagnostics(&launch_args(&[
            "--profile",
            "yolo",
            "--profile-file",
            agent_file.to_str().unwrap(),
            "--claude",
            "/bin/sh",
            "--cwd",
            directory.path().to_str().unwrap(),
            "--model",
            "opus",
            "--rows",
            "24",
            "--allowed-tool",
            "Write",
            "--plugin-dir",
            cli_plugin.to_str().unwrap(),
            "--extra-arg",
            "--verbose",
        ]))
        .unwrap();

        assert_eq!(
            request
                .claude
                .as_ref()
                .expect("inline launch")
                .model
                .as_deref(),
            Some("opus")
        );
        assert_eq!(request.terminal.rows, 24);
        assert_eq!(request.terminal.cols, 160, "unstated scalars still inherit");
        assert_eq!(
            request
                .claude
                .as_ref()
                .expect("inline launch")
                .allowed_tools,
            ["Read", "Bash(git:*)", "Write"]
        );
        assert_eq!(
            request
                .claude
                .as_ref()
                .expect("inline launch")
                .plugin_dirs
                .len(),
            2
        );
        assert!(
            request.claude.as_ref().expect("inline launch").plugin_dirs[1].ends_with("cli-plugin")
        );
        assert_eq!(
            request.claude.as_ref().expect("inline launch").extra_args,
            ["--debug", "--verbose"]
        );

        assert!(
            diagnostics
                .iter()
                .any(|line| line == "--model overrides the profile value"),
            "{diagnostics:?}"
        );
        assert!(
            diagnostics
                .iter()
                .any(|line| line == "--rows overrides the profile value"),
            "{diagnostics:?}"
        );
        assert!(
            !diagnostics
                .iter()
                .any(|line| line.contains("--cols overrides")),
            "{diagnostics:?}"
        );
    }

    #[test]
    fn a_config_isolation_root_is_reachable_from_the_flag_and_from_a_profile() {
        let directory = tempfile::tempdir().unwrap();
        let private = directory.path().join("private-root");
        fs::create_dir(&private).unwrap();
        let cwd = directory.path().join("workspace");
        fs::create_dir(&cwd).unwrap();

        let (request, _) = build_start_request_with_diagnostics(&launch_args(&[
            "--claude",
            "/bin/sh",
            "--cwd",
            cwd.to_str().unwrap(),
            "--config-isolation-root",
            private.to_str().unwrap(),
        ]))
        .unwrap();
        assert_eq!(
            request.config_isolation,
            Some(ConfigIsolation {
                root: private
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            }),
            "the CLI canonicalizes the root before it reaches the wire"
        );

        // Absent means inherit, which is what every caller that predates the
        // flag gets.
        let (plain, _) = build_start_request_with_diagnostics(&launch_args(&[
            "--claude",
            "/bin/sh",
            "--cwd",
            cwd.to_str().unwrap(),
        ]))
        .unwrap();
        assert_eq!(plain.config_isolation, None);

        // A missing root fails in the CLI rather than travelling to the daemon.
        let error = build_start_request_with_diagnostics(&launch_args(&[
            "--claude",
            "/bin/sh",
            "--cwd",
            cwd.to_str().unwrap(),
            "--config-isolation-root",
            directory.path().join("never-created").to_str().unwrap(),
        ]))
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("config isolation root does not exist"),
            "unexpected refusal: {error}"
        );

        // And a profile can name it once, like every other launch policy.
        let agent_file = directory.path().join("agents.json");
        fs::write(
            &agent_file,
            format!(
                r#"{{"version": 1, "agents": {{"pool": {{
                     "cell": "minified",
                     "config_isolation": {{"root": "{root}"}}
                   }}}}}}"#,
                root = private.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&agent_file, fs::Permissions::from_mode(0o600)).unwrap();
        let (profiled, _) = build_start_request_with_diagnostics(&launch_args(&[
            "--profile",
            "pool",
            "--profile-file",
            agent_file.to_str().unwrap(),
            "--claude",
            "/bin/sh",
            "--cwd",
            cwd.to_str().unwrap(),
        ]))
        .unwrap();
        assert_eq!(profiled.cell, SessionCell::Minified);
        assert_eq!(
            profiled.config_isolation,
            Some(ConfigIsolation {
                root: private.display().to_string()
            }),
            "a profile value travels verbatim; the daemon canonicalizes and validates it"
        );
    }

    #[test]
    fn agent_selection_is_explicit_on_both_sides() {
        // A file with no agent name is NOT an error: `PMUX_AGENT_FILE` is meant
        // to be exported once in a shell profile, so erroring here would break
        // every invocation that does not want an agent. It must also not read
        // the file -- the path here does not exist and this must still succeed.
        let (request, notes) = build_start_request_with_diagnostics(&launch_args(&[
            "--profile-file",
            "/nonexistent/agents.json",
        ]))
        .expect("a profile file without an agent name selects no profile");
        assert!(
            request
                .claude
                .as_ref()
                .expect("inline launch")
                .permission_mode
                .is_none(),
            "no agent named means no profile applied"
        );
        assert!(
            !notes.iter().any(|note| note.contains("profile `")),
            "no expansion note should be emitted: {notes:?}"
        );

        let missing_file =
            build_start_request_with_diagnostics(&launch_args(&["--profile", "yolo"]))
                .unwrap_err()
                .to_string();
        assert!(
            missing_file.contains("performs no profile discovery"),
            "{missing_file}"
        );

        let unreadable = build_start_request_with_diagnostics(&launch_args(&[
            "--profile",
            "yolo",
            "--profile-file",
            "/nonexistent/agents.json",
        ]))
        .unwrap_err()
        .to_string();
        assert!(
            unreadable.contains("cannot read agent profile"),
            "{unreadable}"
        );
    }

    #[test]
    fn extra_args_and_the_dangerous_bypass_are_reachable_from_the_command_line() {
        let directory = tempfile::tempdir().unwrap();
        let request = build_start_request(&launch_args(&[
            "--claude",
            "/bin/sh",
            "--cwd",
            directory.path().to_str().unwrap(),
            "--permission-mode",
            "dangerously-skip-permissions",
            "--extra-arg",
            "--debug",
            "--extra-arg",
            "--verbose",
        ]))
        .unwrap();
        assert_eq!(
            request
                .claude
                .as_ref()
                .expect("inline launch")
                .permission_mode,
            Some(PermissionMode::DangerouslySkipPermissions)
        );
        assert_eq!(
            request.claude.as_ref().expect("inline launch").extra_args,
            ["--debug", "--verbose"]
        );
    }

    /// `std::process::Command` refuses to build an argv containing NUL, so the
    /// black-box suite physically cannot reach this rejection. It is still the
    /// one the daemon would otherwise reject after the request was framed.
    #[test]
    fn env_rejects_nul_without_echoing_the_value() {
        let value_error = build_environment_patch(&launch_args(&["--env", "KEY=before\u{0}after"]))
            .unwrap_err()
            .to_string();
        assert!(
            value_error.contains("value containing NUL"),
            "{value_error}"
        );
        assert!(
            !value_error.contains("after"),
            "the value was echoed: {value_error}"
        );

        let name_error = build_environment_patch(&launch_args(&["--env", "BEF\u{0}RE=value"]))
            .unwrap_err()
            .to_string();
        assert!(name_error.contains("may not contain NUL"), "{name_error}");

        let unset_error = build_environment_patch(&launch_args(&["--unset", "BEF\u{0}RE"]))
            .unwrap_err()
            .to_string();
        assert!(unset_error.contains("may not contain NUL"), "{unset_error}");
    }

    /// The mirror is exercised end to end by `bin/pmux/tests/launch_environment.rs`;
    /// this pins the ordering `build_environment` fixes, which is the part a
    /// reader is most likely to get wrong when editing either copy.
    #[test]
    fn dropped_names_follow_the_daemon_ordering_and_report_only_undelivered_names() {
        let mut request = build_start_request(&launch_args(&["--claude", "/bin/sh"])).unwrap();
        request.environment.snapshot = [
            ("PATH", "/usr/bin"),
            ("EDITOR", "vim"),
            ("ANTHROPIC_API_KEY", "secret"),
            ("TERM_PROGRAM", "iTerm.app"),
            ("LC_ALL", "C"),
            ("RESTORED", "from-snapshot"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
        request.environment.set = [("RESTORED", "explicit")]
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect();
        request.environment.unset = BTreeSet::from(["PATH".to_owned()]);

        assert_eq!(
            dropped_environment_names(&request),
            ["ANTHROPIC_API_KEY", "EDITOR", "TERM_PROGRAM"],
            "an allowlisted name restored by `set` is not a removal, and `unset` \
             of an allowlisted name is the caller's own doing rather than a policy drop"
        );

        // `Inherit` admits the provider namespace at step 1 and skips step 4.
        request.auth_policy = AuthPolicy::Inherit;
        assert_eq!(
            dropped_environment_names(&request),
            ["EDITOR", "TERM_PROGRAM"]
        );
    }

    #[test]
    fn turn_alias_and_output_mode_parse() {
        let session = Uuid::new_v4();
        let generation = Uuid::new_v4();
        let cli = Cli::try_parse_from([
            "pmux",
            "--socket",
            "/tmp/pmux.sock",
            "--output",
            "ndjson",
            "prompt",
            &session.to_string(),
            "--generation",
            &generation.to_string(),
            "hello",
        ])
        .unwrap();
        assert_eq!(cli.output, OutputMode::Ndjson);
        assert!(matches!(cli.command, Command::Turn { .. }));
    }
}
