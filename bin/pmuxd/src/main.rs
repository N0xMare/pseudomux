mod bounded_log;
mod conversation;
mod handler;
mod messages_http;

use std::io::{self, Write};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use clap::{Parser, Subcommand};
use handler::{RequestDispatcher, ServerLimits, serve_until};
use pseudomux_protocol::v1::EffortLevel;
use pseudomux_protocol::v1::{ErrorBody, Request, ResponseResult};
use pseudomux_service::compatibility::{
    CompatibilityProfileRegistry, DEFAULT_UNTESTED_TRANSCRIPT_DRAIN_MS, TestedCompatibilityProfile,
    validate_transcript_drain_ms,
};
use pseudomux_service::native::{NativeService, NativeServiceConfig};
use pseudomux_service::pool::config::{
    DEFAULT_INSTANCE_IDLE_TTL_MS, DEFAULT_POOL_SIZE, DEFAULT_RECYCLE_TURNS, DEFAULT_SYSTEM_PROMPT,
    RSS_CEILING_MB_PER_INSTANCE,
};
use pseudomux_service::pool::{PoolConfig, PoolSettings, WarmClassSetting};
// The socket directory, the log directory and the Path B pool parent are all
// held to one definition of "private", so the three cannot drift apart.
use pseudomux_service::private_dir::create_private_dir_all;
#[cfg(unix)]
use pseudomux_service::private_dir::owner_only_violation;
use pseudomux_service::runtime::PrivateRuntimeConfig;
use tokio::net::{UnixListener, UnixStream};
use tokio::time::timeout;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

const MAX_DAEMON_LOG_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Parser, Debug)]
#[command(name = "pmuxd", version)]
#[command(about = "native pseudomux protocol daemon", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Bind the socket and serve protocol v1 until SIGINT or SIGTERM.
    ///
    /// Path A (interactive sessions) is always served. Path B, the stateless
    /// token engine `pmux run` reaches, is OFF unless --path-b-parent is given;
    /// every other --path-b-* flag is refused without it.
    ///
    /// Every refusal below happens before the socket is bound, so a rejected
    /// configuration leaves no socket, no runtime directory and no rmux
    /// sidecar behind it.
    Serve {
        /// Explicit owner-only Unix socket path.
        #[arg(long, env = "PMUX_SOCKET")]
        socket: PathBuf,

        /// Maximum number of concurrently serviced client connections.
        #[arg(long, default_value = "64")]
        max_connections: NonZeroUsize,

        /// Grace period for in-flight requests after SIGINT or SIGTERM.
        #[arg(long, default_value_t = 5_000)]
        shutdown_grace_ms: u64,

        /// Private rmux sidecar binary; defaults to `pmux-rmuxd` beside pmuxd.
        #[arg(long)]
        rmuxd: Option<PathBuf>,

        /// Capability launcher binary; defaults to `pmux-launcher` beside pmuxd.
        #[arg(long)]
        launcher: Option<PathBuf>,

        /// Parent for the ephemeral, owner-only private runtime directory.
        #[arg(long)]
        runtime_parent: Option<PathBuf>,

        /// Tested compatibility cell as a JSON object. Repeat for each
        /// version-range/OS/architecture/terminal/input cell.
        ///
        /// `claude_version` alone means that exact version. Add
        /// `claude_version_tested_through` to admit a RANGE, inclusive at both
        /// ends; it may not span a major or minor version. Two cells whose
        /// ranges overlap on one platform are refused at boot as ambiguous.
        #[arg(long = "tested-claude-profile", value_name = "JSON")]
        tested_claude_profiles: Vec<String>,

        /// Conservative transcript drain for an explicit unmatched
        /// `allow_untested` request. The resulting session remains untested.
        #[arg(long, default_value_t = DEFAULT_UNTESTED_TRANSCRIPT_DRAIN_MS)]
        untested_transcript_drain_ms: u64,

        /// Where stored agents live. Defaults to `agents/` beside the socket,
        /// which is how the daemon log directory is derived too.
        ///
        /// Held to the SAME bar as the socket directory and the Path B pool
        /// parent: every level pmuxd creates is 0700 from birth and every file
        /// is 0600, and a directory that already exists and is not owner-only
        /// and owned by this user is REFUSED at boot, naming what is wrong and
        /// what would be right. pmuxd never re-permissions a tree it did not
        /// create.
        ///
        /// An operator who moves --socket keeps their agents by naming this
        /// explicitly.
        #[arg(long = "agent-store", value_name = "DIR")]
        agent_store: Option<PathBuf>,

        /// ENABLES THE STATELESS TOKEN ENGINE. Absolute parent directory for
        /// the pool's per-slot trees; pmux creates `<parent>/<slot>/<epoch>/`
        /// itself, 0700 and empty, and erases each one when its instance is
        /// destroyed. No caller can name any path under it.
        ///
        /// Path B is off unless this is given. Every other `--path-b-*` flag is
        /// refused without it, because a knob that silently does nothing is
        /// worse than an error.
        #[arg(long = "path-b-parent", value_name = "DIR", help_heading = PATH_B_HELP_HEADING)]
        path_b_parent: Option<PathBuf>,

        /// Required with --path-b-parent, and must be ABSOLUTE. There is
        /// deliberately no default and no PATH lookup: the pool launches
        /// unattended, and resolving a bare name through the daemon's PATH is
        /// how it launches the wrong binary.
        #[arg(long = "path-b-claude", value_name = "PATH", help_heading = PATH_B_HELP_HEADING)]
        path_b_claude: Option<PathBuf>,

        /// Live instances the pool may hold. Refused above the owner-set cap of
        /// 15, at boot.
        #[arg(long = "path-b-pool-size", default_value_t = DEFAULT_POOL_SIZE, help_heading = PATH_B_HELP_HEADING)]
        path_b_pool_size: u32,

        /// Turns one instance serves before it is recycled.
        #[arg(long = "path-b-recycle-turns", default_value_t = DEFAULT_RECYCLE_TURNS, help_heading = PATH_B_HELP_HEADING)]
        path_b_recycle_turns: u32,

        /// One warm class to hold, as `MODEL[/EFFORT]=COUNT`, e.g.
        /// `claude-sonnet-5/medium=2` or `haiku=1`. Repeatable, one class per
        /// occurrence. The declared total may not exceed the pool size, and
        /// each class is resolved through the SAME call a live request uses --
        /// a class the pool could never serve is refused at boot rather than
        /// discovered by an operator reading a mint failure.
        #[arg(long = "path-b-warm", value_name = "MODEL[/EFFORT]=COUNT", help_heading = PATH_B_HELP_HEADING)]
        path_b_warm: Vec<String>,

        /// The system prompt every pool instance is launched with, delivered in
        /// REPLACE mode so it survives `/clear`. Bounded at 512 bytes and
        /// refused at boot. Keep it under three sentences: that is an editorial
        /// instruction to you and is deliberately NOT enforced -- a sentence
        /// counter rejects a correct prompt containing "e.g.", which is a rule
        /// pretending to be a proof. 512 bytes is what the daemon enforces.
        #[arg(
            long = "path-b-system-prompt",
            value_name = "TEXT",
            default_value = DEFAULT_SYSTEM_PROMPT,
            conflicts_with = "path_b_system_prompt_file",
            help_heading = PATH_B_HELP_HEADING
        )]
        path_b_system_prompt: String,

        /// Read the system prompt from this UTF-8 file instead of argv.
        #[arg(
            long = "path-b-system-prompt-file",
            value_name = "FILE",
            help_heading = PATH_B_HELP_HEADING
        )]
        path_b_system_prompt_file: Option<PathBuf>,

        /// How long an idle instance is held before the pool's own sweep
        /// destroys it, down to each class's declared warm floor.
        #[arg(long = "path-b-instance-idle-ttl-ms", default_value_t = DEFAULT_INSTANCE_IDLE_TTL_MS, help_heading = PATH_B_HELP_HEADING)]
        path_b_instance_idle_ttl_ms: u64,

        /// Deadline a stateless turn gets when its caller supplies none.
        #[arg(long = "path-b-turn-timeout-ms", default_value_t = DEFAULT_POOL_TURN_TIMEOUT_MS, help_heading = PATH_B_HELP_HEADING)]
        path_b_turn_timeout_ms: u64,

        /// Absolute directory, OUTSIDE the pool parent, where a quarantined
        /// instance's tree is retained as evidence. Omit to erase instead.
        #[arg(long = "path-b-retain-dir", value_name = "DIR", help_heading = PATH_B_HELP_HEADING)]
        path_b_retain_dir: Option<PathBuf>,

        /// Resident-memory budget the pool is sized against, in MB. Checked
        /// once at boot against `pool_size * 1024 MB`; there is no runtime
        /// sampler, because the turn cap already makes the per-instance ceiling
        /// arithmetically unreachable.
        #[arg(long = "path-b-rss-budget-mb", help_heading = PATH_B_HELP_HEADING)]
        path_b_rss_budget_mb: Option<u64>,

        /// Absolute directory, OUTSIDE the pool parent, holding the redacted
        /// Path B evidence corpus. Defaults to `path-b-evidence/` beside the
        /// socket, alongside `logs/` and `agents/`.
        ///
        /// Each destroyed instance's transcripts are mirrored there pruned to
        /// the eight fields `tools/promotion/measure_transcript_drain.py`
        /// reads -- timestamps, row kinds, version, entrypoint -- and NO prompt
        /// or completion text. Point that tool at it to re-check the drain for
        /// a new Claude Code version at zero cost, which is otherwise
        /// impossible: a new version has no `cli` turns to re-analyse until
        /// something has run some.
        #[arg(long = "path-b-evidence-dir", value_name = "DIR", help_heading = PATH_B_HELP_HEADING)]
        path_b_evidence_dir: Option<PathBuf>,

        /// Retain no Path B evidence at all. The pool then erases every
        /// transcript at teardown, as it did before the corpus existed, and a
        /// future promotion has nothing free to read.
        #[arg(long = "path-b-no-evidence", help_heading = PATH_B_HELP_HEADING)]
        path_b_no_evidence: bool,

        /// Loopback Anthropic Messages listener (`HOST:PORT`) in front of
        /// the Path B pool. One conversation pins one warm instance; only the
        /// delta is typed; `/clear` runs on release. Loopback only. Off unless
        /// given. Pi points `api: "anthropic-messages"` at `http://HOST:PORT`.
        #[arg(long = "path-b-messages-bind", value_name = "HOST:PORT", help_heading = PATH_B_HELP_HEADING)]
        path_b_messages_bind: Option<String>,
    },
}

/// Groups every stateless-engine flag in `--help`, so the one thing an operator
/// must know -- that `--path-b-parent` is the enable switch -- is not buried
/// among the session flags.
const PATH_B_HELP_HEADING: &str = "Stateless token engine (Path B; off unless --path-b-parent)";

/// CHOSEN: ten minutes, the same ceiling a Path A turn gets. A stateless turn
/// is one model call with no tool surface, so it is far under this; the bound
/// exists so a wedged instance returns its slot rather than holding it forever.
const DEFAULT_POOL_TURN_TIMEOUT_MS: u64 = 600_000;

struct ServeOptions {
    socket: PathBuf,
    max_connections: NonZeroUsize,
    shutdown_grace: Duration,
    rmuxd: Option<PathBuf>,
    launcher: Option<PathBuf>,
    runtime_parent: Option<PathBuf>,
    tested_claude_profiles: Vec<String>,
    untested_transcript_drain_ms: u64,
    agent_store: Option<PathBuf>,
    path_b: PathBOptions,
    path_b_messages_bind: Option<String>,
}

/// The stateless engine's flags, exactly as parsed. Nothing here is trusted yet.
struct PathBOptions {
    parent: Option<PathBuf>,
    claude: Option<PathBuf>,
    pool_size: u32,
    recycle_turns: u32,
    warm: Vec<String>,
    system_prompt: String,
    system_prompt_file: Option<PathBuf>,
    instance_idle_ttl_ms: u64,
    turn_timeout_ms: u64,
    retain_dir: Option<PathBuf>,
    rss_budget_mb: Option<u64>,
    evidence_dir: Option<PathBuf>,
    no_evidence: bool,
}

/// Turn the parsed flags into a validated [`PoolConfig`], or refuse to boot.
///
/// # The absent-parent rule
///
/// `--path-b-parent` is the enable switch, and every other `--path-b-*` flag is
/// an ERROR without it rather than being ignored. A flag that silently does
/// nothing is the failure mode where an operator sets `--path-b-pool-size 15`,
/// reads no error, and believes they have a pool.
///
/// The check is against what the operator TYPED, not against a value differing
/// from a default: `--path-b-pool-size 15` is indistinguishable from the
/// default by value, and an operator who typed it is exactly the operator who
/// needs to be told.
///
/// The set of flags it checks is DERIVED from the `serve` command's own
/// arguments, not listed here. A hand-kept list of ten names beside eleven
/// declarations is the recurring defect in this tree: the day an eleventh
/// `--path-b-*` flag is added, a list would go on reporting success while the
/// new flag silently did nothing, which is the exact failure this guard exists
/// to prevent.
fn resolve_path_b(
    options: PathBOptions,
    matches: &clap::ArgMatches,
    socket: &Path,
) -> Result<Option<PoolConfig>> {
    let serve = matches
        .subcommand_matches("serve")
        .ok_or_else(|| anyhow!("pmuxd was invoked without its serve subcommand"))?;
    let typed = |name: &str| {
        serve
            .value_source(name)
            .is_some_and(|source| source != clap::parser::ValueSource::DefaultValue)
    };

    let Some(parent) = options.parent else {
        let stray: Vec<String> = path_b_dependent_flag_ids()
            .into_iter()
            .filter(|name| typed(name))
            .map(|name| format!("--{}", name.replace('_', "-")))
            .collect();
        if stray.is_empty() {
            return Ok(None);
        }
        bail!(
            "the stateless token engine is off because --path-b-parent was not given, but {} was: \
give --path-b-parent DIR (an absolute, owner-only directory pmuxd may create per-slot trees \
under) to enable it, or drop the flag",
            stray.join(", ")
        );
    };

    let claude = options.claude.ok_or_else(|| {
        anyhow!(
            "--path-b-parent enables the stateless token engine, which launches Claude unattended, \
             so it also requires --path-b-claude PATH: an ABSOLUTE path to the Claude executable, \
             e.g. --path-b-claude /usr/local/bin/claude. There is no default and no PATH lookup, \
             because resolving a bare name through the daemon's own PATH is how a daemon launches \
             the wrong binary"
        )
    })?;

    let system_prompt = match options.system_prompt_file {
        Some(path) => {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            // Exactly ONE trailing newline is dropped, the POSIX text-file
            // terminator, for the same reason `pmux --prompt-file` drops one:
            // every conventional tool writes it and it is not part of the
            // prompt the operator wrote. Only one, so an operator who
            // deliberately ends with a blank line still gets it, and so the
            // 512-byte bound is measured on the same bytes the child receives.
            match text.strip_suffix('\n') {
                Some(trimmed) => trimmed.to_owned(),
                None => text,
            }
        }
        None => options.system_prompt,
    };

    let mut settings = PoolSettings::defaults(parent, claude);
    settings.pool_size = options.pool_size;
    settings.recycle_turns = options.recycle_turns;
    settings.system_prompt = system_prompt;
    settings.instance_idle_ttl_ms = options.instance_idle_ttl_ms;
    settings.turn_timeout_ms = options.turn_timeout_ms;
    settings.retain_dir = options.retain_dir;
    // ON unless the operator says otherwise, and DERIVED from `--socket` by the
    // same function `logs/` and `agents/` go through, so an operator who moved
    // the socket finds all three in one place. `--path-b-no-evidence` is the
    // whole of the off switch, and it wins over an explicit directory rather
    // than being quietly ignored beside one.
    settings.evidence_dir = match (options.no_evidence, options.evidence_dir) {
        (true, _) => None,
        (false, Some(explicit)) => Some(explicit),
        (false, None) => Some(daemon_sibling_dir(socket, "path-b-evidence")?),
    };
    settings.warm_set = options
        .warm
        .iter()
        .map(|declaration| parse_warm_class(declaration))
        .collect::<Result<Vec<_>>>()?;
    // DERIVED from the pool size unless the operator states a budget, so the
    // boot assertion is "this host was sized for this pool" and not a constant
    // that silently passes.
    settings.rss_budget_mb = options
        .rss_budget_mb
        .unwrap_or_else(|| u64::from(options.pool_size) * RSS_CEILING_MB_PER_INSTANCE);

    settings
        .validate()
        .map(Some)
        .map_err(|refusal| anyhow!("the stateless token engine refused to boot: {refusal}"))
}

/// Every `--path-b-*` flag that is meaningless without `--path-b-parent`.
///
/// Derived from the `serve` command clap actually built, so the guard's set and
/// the flag set are the same set by construction rather than by anyone
/// remembering. Declaration order is preserved, which is the order the operator
/// reads them in `--help`.
fn path_b_dependent_flag_ids() -> Vec<String> {
    <Cli as clap::CommandFactory>::command()
        .find_subcommand("serve")
        .expect("pmuxd declares a serve subcommand")
        .get_arguments()
        .map(|argument| argument.get_id().to_string())
        .filter(|id| id.starts_with("path_b_") && id != "path_b_parent")
        .collect()
}

/// The effort tiers, in the one spelling every surface accepts.
///
/// Rendered from `EffortLevel::as_str`, never from `{:?}`: `XHigh` is a Rust
/// identifier that nothing accepts, and naming it in a refusal beside a literal
/// `--effort` is the defect `EffortLevel::as_str` was added for. Held against
/// the protocol enum's own source by
/// `the_effort_tiers_this_daemon_names_are_every_tier_the_protocol_has`.
const EFFORT_TIERS: [EffortLevel; 5] = [
    EffortLevel::Low,
    EffortLevel::Medium,
    EffortLevel::High,
    EffortLevel::XHigh,
    EffortLevel::Max,
];

fn effort_tier_list() -> String {
    EFFORT_TIERS
        .iter()
        .map(|tier| tier.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// `MODEL[/EFFORT]=COUNT`.
///
/// The effort is parsed through `EffortLevel`'s own wire spelling rather than a
/// local table, so the word an operator types here is the same word the
/// protocol uses and the same word that reaches `--effort`.
fn parse_warm_class(declaration: &str) -> Result<WarmClassSetting> {
    let (class, count) = declaration.rsplit_once('=').ok_or_else(|| {
        anyhow!(
            "--path-b-warm {declaration:?} has no `=`; write MODEL[/EFFORT]=COUNT, \
             e.g. --path-b-warm claude-sonnet-5/medium=2 or --path-b-warm haiku=1"
        )
    })?;
    let count: u32 = count.parse().with_context(|| {
        format!(
            "--path-b-warm {declaration:?} has a non-numeric count; the text after the last `=` \
             must be how many instances of this class to hold warm, e.g. \
             --path-b-warm {class}=2"
        )
    })?;
    let (model, effort) = match class.split_once('/') {
        Some((model, effort)) => {
            let level = serde_json::from_value::<EffortLevel>(serde_json::Value::String(
                effort.to_owned(),
            ))
            .map_err(|_| {
                anyhow!(
                    "--path-b-warm {declaration:?} names effort {effort:?}, which is not a tier: \
                     use one of {}, or drop the `/{effort}` to let the model pick its own default",
                    effort_tier_list()
                )
            })?;
            (model, Some(level))
        }
        None => (class, None),
    };
    if model.is_empty() {
        bail!(
            "--path-b-warm {declaration:?} names an empty model; write MODEL[/EFFORT]=COUNT, \
             e.g. --path-b-warm claude-sonnet-5/medium=2 or --path-b-warm haiku=1"
        );
    }
    Ok(WarmClassSetting {
        model: model.to_owned(),
        effort,
        count,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    #[allow(unsafe_code)]
    #[cfg(unix)]
    // SAFETY: installing SIG_IGN for SIGHUP is process-global but happens once,
    // before the daemon creates its service or accepts work.
    unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }

    // Parsed once into both shapes: the typed struct the daemon runs on, and
    // the raw matches, which are the only place "did the operator TYPE this
    // flag" is answerable. A default and an explicitly typed identical value
    // are the same value and different operator intents.
    let matches = <Cli as clap::CommandFactory>::command().get_matches();
    match Cli::parse().command {
        Command::Serve {
            socket,
            max_connections,
            shutdown_grace_ms,
            rmuxd,
            launcher,
            runtime_parent,
            tested_claude_profiles,
            untested_transcript_drain_ms,
            agent_store,
            path_b_parent,
            path_b_claude,
            path_b_pool_size,
            path_b_recycle_turns,
            path_b_warm,
            path_b_system_prompt,
            path_b_system_prompt_file,
            path_b_instance_idle_ttl_ms,
            path_b_turn_timeout_ms,
            path_b_retain_dir,
            path_b_rss_budget_mb,
            path_b_evidence_dir,
            path_b_no_evidence,
            path_b_messages_bind,
        } => {
            run_server(
                ServeOptions {
                    socket,
                    max_connections,
                    shutdown_grace: Duration::from_millis(shutdown_grace_ms),
                    rmuxd,
                    launcher,
                    runtime_parent,
                    tested_claude_profiles,
                    untested_transcript_drain_ms,
                    agent_store,
                    path_b: PathBOptions {
                        parent: path_b_parent,
                        claude: path_b_claude,
                        pool_size: path_b_pool_size,
                        recycle_turns: path_b_recycle_turns,
                        warm: path_b_warm,
                        system_prompt: path_b_system_prompt,
                        system_prompt_file: path_b_system_prompt_file,
                        instance_idle_ttl_ms: path_b_instance_idle_ttl_ms,
                        turn_timeout_ms: path_b_turn_timeout_ms,
                        retain_dir: path_b_retain_dir,
                        rss_budget_mb: path_b_rss_budget_mb,
                        evidence_dir: path_b_evidence_dir,
                        no_evidence: path_b_no_evidence,
                    },
                    path_b_messages_bind,
                },
                &matches,
            )
            .await?;
        }
    }
    Ok(())
}

struct NativeDispatcher {
    service: Arc<NativeService>,
}

#[async_trait]
impl RequestDispatcher for NativeDispatcher {
    async fn dispatch(&self, request: Request) -> Result<ResponseResult, ErrorBody> {
        self.service.dispatch(request).await
    }
}

async fn run_server(options: ServeOptions, matches: &clap::ArgMatches) -> Result<()> {
    // BEFORE the socket is bound and before the private runtime is started. A
    // pool refusal is an operator error, and an operator error must not leave a
    // socket, a runtime directory and an rmux sidecar behind it.
    let pool = resolve_path_b(options.path_b, matches, &options.socket)?;
    let conversation_config = pool
        .as_ref()
        .map(|config| conversation::ConversationConfig {
            idle_ttl: Duration::from_millis(config.instance_idle_ttl_ms),
            max_leases: config.pool_size,
        });
    let messages_bind = options
        .path_b_messages_bind
        .as_deref()
        .map(messages_http::parse_messages_bind)
        .transpose()?;
    if messages_bind.is_some() && pool.is_none() {
        // The clap id starts with path_b_, so the absent-parent guard already
        // refuses this when the operator typed the flag. This is the belt for
        // a constructed ServeOptions in tests.
        bail!("--path-b-messages-bind requires --path-b-parent");
    }
    let messages_listener = match messages_bind {
        Some(addr) => Some(messages_http::bind_messages(addr).await?),
        None => None,
    };
    let socket_path = resolve_socket_path(options.socket)?;
    let (listener, mut socket_guard) = bind_socket(&socket_path).await?;
    let log_dir = daemon_log_dir(&socket_path)?;
    // Beside `logs/`, and derived from the same parent, so an operator who
    // moved --socket finds both in one place. `NativeService::start` opens it
    // and refuses a tree it did not create and may not trust.
    let agent_store = match options.agent_store {
        Some(explicit) => explicit,
        None => daemon_sibling_dir(&socket_path, "agents")?,
    };
    let _log_guard = init_logging(&log_dir)?;

    let mut runtime_config = PrivateRuntimeConfig::from_current_exe()
        .context("failed to resolve private runtime companion binaries")?;
    let hybrid_hook_client =
        apply_companion_overrides(&mut runtime_config, options.rmuxd, options.launcher)?;
    runtime_config.runtime_parent = options.runtime_parent;

    let tested_claude_profiles = parse_tested_profiles(options.tested_claude_profiles)?;
    validate_transcript_drain_ms(options.untested_transcript_drain_ms)
        .context("invalid --untested-transcript-drain-ms")?;
    let service_config = NativeServiceConfig {
        hybrid_hook_client: Some(hybrid_hook_client),
        tested_claude_profiles,
        untested_transcript_drain_ms: options.untested_transcript_drain_ms,
        agent_store: Some(agent_store),
        pool,
        ..NativeServiceConfig::default()
    };
    // BEFORE `NativeService::start`, and that is the whole point of the line.
    // See [`ShutdownSignals`]: the mint of the warm set runs inside `start`,
    // and until this call SIGTERM carries its default disposition.
    let mut shutdown = ShutdownSignals::install();
    let service = NativeService::start(runtime_config, service_config)
        .await
        .map_err(|error| {
            // LOGGED, not merely returned. `_log_guard` is alive here, so this
            // record reaches the daemon log the operator will read; the string
            // is `ErrorBody::message`, which pmux composes, and never a
            // terminal backend's `Display`.
            tracing::error!(
                operation = "pmuxd_startup",
                code = ?error.code,
                message = %error.message,
                "pmuxd startup failed; no socket is served"
            );
            anyhow!("native service startup failed: {}", error.message)
        })?;
    let dispatcher = Arc::new(NativeDispatcher {
        service: Arc::clone(&service),
    });

    info!(socket = %socket_path.display(), "pmuxd protocol v1 listening");
    let messages_task = match (messages_listener, conversation_config) {
        (Some(listener), Some(config)) => {
            let pool = service
                .pool()
                .cloned()
                .ok_or_else(|| anyhow!("--path-b-messages-bind requires a started Path B pool"))?;
            let book = Arc::new(conversation::ConversationBook::new(config, pool));
            Some(tokio::spawn(async move {
                if let Err(error) = messages_http::serve_messages(listener, book).await {
                    warn!(error = %error, "Path B Messages listener stopped");
                }
            }))
        }
        _ => None,
    };
    let serve_result = serve_until(
        listener,
        dispatcher,
        ServerLimits {
            max_connections: options.max_connections.get(),
            shutdown_grace: options.shutdown_grace,
            ..ServerLimits::default()
        },
        shutdown.requested(),
    )
    .await;
    if let Some(task) = messages_task {
        task.abort();
    }

    let shutdown_result = service.shutdown().await;
    if let Err(error) = &shutdown_result {
        // Error messages can originate in terminal backends, so record only
        // the stable protocol classification in daemon logs.
        warn!(error_code = ?error.code, "native service shutdown failed");
    }
    let cleanup_result = socket_guard.cleanup();
    match &cleanup_result {
        Ok(false) => {
            warn!(socket = %socket_path.display(), "pmuxd socket identity changed; not removing it");
        }
        Err(error) => {
            warn!(error_kind = ?error.kind(), "failed to remove pmuxd socket");
        }
        Ok(true) => {}
    }

    serve_result.context("pmuxd transport failed")?;
    shutdown_result
        .map_err(|error| anyhow!("native service shutdown failed with code {:?}", error.code))?;
    cleanup_result.context("failed to remove pmuxd socket")?;
    info!("pmuxd stopped");
    Ok(())
}

fn apply_companion_overrides(
    runtime_config: &mut PrivateRuntimeConfig,
    rmuxd: Option<PathBuf>,
    launcher: Option<PathBuf>,
) -> Result<PathBuf> {
    if let Some(path) = rmuxd {
        runtime_config.rmuxd = path;
    }
    if let Some(path) = launcher {
        runtime_config.launcher = path;
    }
    sibling_companion(&runtime_config.rmuxd, "pmux-hook")
}

fn sibling_companion(reference: &Path, name: &str) -> Result<PathBuf> {
    let directory = reference
        .parent()
        .context("companion binary path has no parent directory")?;
    #[cfg(windows)]
    let name = format!("{name}.exe");
    Ok(directory.join(name))
}

fn parse_tested_profiles(values: Vec<String>) -> Result<CompatibilityProfileRegistry> {
    let mut profiles = Vec::with_capacity(values.len());
    for value in values {
        if value.trim().is_empty() {
            bail!("--tested-claude-profile must not be empty");
        }
        let profile = serde_json::from_str::<TestedCompatibilityProfile>(&value)
            .context("--tested-claude-profile must be a strict JSON profile object")?;
        profiles.push(profile);
    }
    CompatibilityProfileRegistry::try_from_profiles(profiles)
        .context("invalid --tested-claude-profile")
}

/// The daemon's shutdown signals, held from before the first Path B instance is
/// minted until `serve_until` returns.
///
/// # Why this is a value and not a function
///
/// It was `async fn shutdown_signal()`, called in argument position at
/// `serve_until`. `tokio::signal::unix::signal` is what installs the
/// disposition, and an `async fn` runs none of its body until the future is
/// first polled -- which `serve_until` does only after `NativeService::start`
/// has returned. `NativeService::start` is where `Pool::start` mints the whole
/// declared warm set, so for the width of that mint SIGTERM and SIGINT carried
/// their DEFAULT disposition.
///
/// MEASURED at that shape, macos/aarch64, `--path-b-warm claude-sonnet-5/low=3`
/// against real Claude 2.1.226, SIGTERM 2.6 s in:
///
/// ```text
/// exit 143                       the kernel, not pmux
/// trees left  0/0 1/0 2/0        one epoch tree per instance the mint reached
/// socket      PRESENT            socket_guard::cleanup never ran
/// daemon log  1 line             the raw startup writeln; the WorkerGuard
///                                never dropped, so every buffered record died
/// ```
///
/// Installing here removes the default disposition for the whole window. A
/// signal that arrives during the mint is not lost: tokio's `Signal` coalesces
/// and buffers every delivery after the handle is created, so `recv()` returns
/// immediately when `serve_until` finally polls it, and the daemon takes the
/// ordinary graceful path -- pool drained, sockets removed, log flushed by the
/// guard's `Drop`.
///
/// **What it deliberately does not do: cancel the mint.** The window is
/// shortened to nothing in its consequences and not in its duration; a SIGTERM
/// at 0.2 s still waits for the declared warm set to finish before shutting
/// down. Racing `NativeService::start` against the signal is the one repair
/// that is NOT available: main holds no `NativeService` until it returns, so
/// dropping that future orphans exactly the trees and children
/// `native::start_pool`'s own comment says must never be orphaned.
struct ShutdownSignals {
    #[cfg(unix)]
    terminate: Option<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    interrupt: Option<tokio::signal::unix::Signal>,
}

impl ShutdownSignals {
    /// Install the handlers now.
    ///
    /// A registration failure is reported and survived rather than fatal: a
    /// daemon that cannot install SIGTERM is a daemon whose stop is ungraceful,
    /// which is worse than it was but is not a reason to refuse to serve. The
    /// two are tracked separately so an operator learns which one they lost.
    fn install() -> Self {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};

            let install = |kind: SignalKind, name: &'static str| match signal(kind) {
                Ok(stream) => Some(stream),
                Err(error) => {
                    warn!(
                        error_kind = ?error.kind(),
                        signal = name,
                        "failed to install a shutdown signal handler; that signal keeps its \
                         default disposition and will kill this daemon"
                    );
                    None
                }
            };
            Self {
                terminate: install(SignalKind::terminate(), "SIGTERM"),
                interrupt: install(SignalKind::interrupt(), "SIGINT"),
            }
        }
        #[cfg(not(unix))]
        Self {}
    }

    /// Resolves on the first shutdown signal, including one delivered before
    /// this future was ever polled.
    async fn requested(&mut self) {
        #[cfg(unix)]
        match (self.terminate.as_mut(), self.interrupt.as_mut()) {
            (Some(terminate), Some(interrupt)) => {
                tokio::select! {
                    _ = terminate.recv() => {}
                    _ = interrupt.recv() => {}
                }
            }
            (Some(only), None) | (None, Some(only)) => {
                only.recv().await;
            }
            // Neither installed: both keep their default disposition, so this
            // process is already gone by the time anything would have polled
            // here. Pending rather than "shut down now" -- returning would
            // stop a daemon that was never asked to stop.
            (None, None) => std::future::pending().await,
        }

        #[cfg(not(unix))]
        if let Err(error) = tokio::signal::ctrl_c().await {
            warn!(error_kind = ?error.kind(), "failed to wait for shutdown signal");
        }
    }
}

async fn bind_socket(path: &Path) -> Result<(UnixListener, SocketGuard)> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::FileTypeExt;
                if metadata.file_type().is_socket() && socket_listener_alive(path).await {
                    bail!(
                        "socket {} already has a live listener: another pmuxd is serving it. Stop \
                         that daemon, or give this one a different --socket path",
                        path.display()
                    );
                }
            }
            remove_stale_socket(path)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect socket {}", path.display()));
        }
    }

    let listener = bind_with_private_umask(path)
        .with_context(|| format!("failed to bind socket {}", path.display()))?;
    let identity = SocketIdentity::read(path)?;
    let guard = SocketGuard::new(path.to_path_buf(), identity);
    set_socket_permissions(path)?;
    Ok((listener, guard))
}

async fn socket_listener_alive(path: &Path) -> bool {
    matches!(
        timeout(Duration::from_millis(250), UnixStream::connect(path)).await,
        Ok(Ok(_))
    )
}

fn remove_stale_socket(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};

        let metadata = std::fs::symlink_metadata(path)
            .with_context(|| format!("failed to inspect stale socket {}", path.display()))?;
        if !metadata.file_type().is_socket() {
            bail!(
                "refusing to remove non-socket file at {}: pmuxd only ever replaces a stale socket \
                 it owns. Move that file aside, or give a different --socket path",
                path.display()
            );
        }
        if metadata.uid() != effective_uid() {
            bail!(
                "refusing to remove socket {} owned by uid {}: pmuxd is running as uid {} and only \
                 replaces its own stale socket. Run as that owner, or give a different --socket \
                 path",
                path.display(),
                metadata.uid(),
                effective_uid()
            );
        }
        let identity = SocketIdentity::from_metadata(&metadata);
        let current = SocketIdentity::read(path)?;
        if current != identity {
            bail!("socket {} changed while checking ownership", path.display());
        }
    }

    std::fs::remove_file(path)
        .with_context(|| format!("failed to remove stale socket {}", path.display()))
}

fn bind_with_private_umask(path: &Path) -> io::Result<UnixListener> {
    #[cfg(unix)]
    {
        use std::os::unix::net::UnixListener as StdUnixListener;

        let guard = UmaskGuard::private();
        let listener = StdUnixListener::bind(path);
        drop(guard);
        let listener = listener?;
        listener.set_nonblocking(true)?;
        UnixListener::from_std(listener)
    }

    #[cfg(not(unix))]
    UnixListener::bind(path)
}

struct UmaskGuard {
    previous: libc::mode_t,
}

impl UmaskGuard {
    #[allow(unsafe_code)]
    fn private() -> Self {
        // SAFETY: pmuxd binds its public endpoint before starting any service
        // tasks. The previous process umask is restored by this guard.
        let previous = unsafe { libc::umask(0o077) };
        Self { previous }
    }
}

impl Drop for UmaskGuard {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: `previous` is the value returned by `umask` in this process.
        unsafe {
            libc::umask(self.previous);
        }
    }
}

fn set_socket_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};

        let metadata = std::fs::symlink_metadata(path)
            .with_context(|| format!("failed to inspect socket {}", path.display()))?;
        if !metadata.file_type().is_socket() || metadata.uid() != effective_uid() {
            bail!(
                "new endpoint {} is not an owner-controlled socket",
                path.display()
            );
        }
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to set socket permissions {}", path.display()))?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SocketIdentity {
    device: u64,
    inode: u64,
    owner: u32,
}

impl SocketIdentity {
    #[cfg(unix)]
    fn read(path: &Path) -> Result<Self> {
        use std::os::unix::fs::FileTypeExt;

        let metadata = std::fs::symlink_metadata(path)
            .with_context(|| format!("failed to inspect socket {}", path.display()))?;
        if !metadata.file_type().is_socket() {
            bail!("endpoint {} is not a Unix socket", path.display());
        }
        Ok(Self::from_metadata(&metadata))
    }

    #[cfg(unix)]
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;

        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            owner: metadata.uid(),
        }
    }

    #[cfg(not(unix))]
    fn read(_path: &Path) -> Result<Self> {
        Ok(Self {
            device: 0,
            inode: 0,
            owner: 0,
        })
    }
}

#[derive(Debug)]
struct SocketGuard {
    path: PathBuf,
    identity: SocketIdentity,
    armed: bool,
}

impl SocketGuard {
    fn new(path: PathBuf, identity: SocketIdentity) -> Self {
        Self {
            path,
            identity,
            armed: true,
        }
    }

    fn cleanup(&mut self) -> io::Result<bool> {
        let removed = remove_if_same_socket(&self.path, self.identity)?;
        self.armed = false;
        Ok(removed)
    }
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = remove_if_same_socket(&self.path, self.identity);
        }
    }
}

fn remove_if_same_socket(path: &Path, expected: SocketIdentity) -> io::Result<bool> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error),
    };

    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;

        if !metadata.file_type().is_socket()
            || SocketIdentity::from_metadata(&metadata) != expected
            || expected.owner != effective_uid()
        {
            return Ok(false);
        }
    }

    std::fs::remove_file(path)?;
    Ok(true)
}

fn daemon_log_dir(socket_path: &Path) -> Result<PathBuf> {
    daemon_sibling_dir(socket_path, "logs")
}

/// One owner-only directory beside the socket.
///
/// `logs/` and `agents/` are both derived here rather than each spelling out
/// the same `parent().join(..)`, so the two cannot drift apart the day one of
/// them learns something about the socket path the other does not.
fn daemon_sibling_dir(socket_path: &Path, name: &str) -> Result<PathBuf> {
    Ok(socket_parent(socket_path)?.join(name))
}

/// The socket's parent, checked for the one property every sibling directory
/// inherits from it, and NOTHING created.
///
/// Split out of [`validate_socket_path`] because the Path B evidence directory
/// is derived from it and `resolve_path_b` runs BEFORE the socket directory is
/// created -- deliberately, so a pool refusal leaves no socket directory, no
/// runtime directory and no rmux sidecar behind it. A derivation that had to
/// create something first would have inverted that order.
fn socket_parent(path: &Path) -> Result<&Path> {
    if !path.is_absolute() {
        bail!(
            "--socket {} is relative; give an absolute path, e.g. --socket {}",
            path.display(),
            std::env::current_dir()
                .map(|cwd| cwd.join(path))
                .unwrap_or_else(|_| PathBuf::from("/run/pmux/pmux.sock"))
                .display()
        );
    }
    path.parent()
        .filter(|path| !path.as_os_str().is_empty())
        .context("socket path must have a parent directory")
}

fn init_logging(log_dir: &Path) -> Result<tracing_appender::non_blocking::WorkerGuard> {
    ensure_private_directory(log_dir)?;
    let mut bounded_log = bounded_log::BoundedLogWriter::open(log_dir, MAX_DAEMON_LOG_BYTES)
        .context("failed to open bounded pmuxd log")?;
    writeln!(
        bounded_log,
        "{{\"event\":\"startup\",\"pid\":{}}}",
        std::process::id()
    )
    .context("failed to write pmuxd startup record")?;
    let (file_writer, guard) = tracing_appender::non_blocking(bounded_log);
    // Keep dependency tracing disabled even when an ambient `RUST_LOG` is
    // present: terminal backends may handle prompts, environment variables,
    // and capability tokens. pmuxd's own call sites log transport metadata
    // only, so this target-scoped filter cannot expose request content.
    //
    // The two first-party pmux crates are admitted at WARN and no lower. Both
    // are audited to log only static operation names, session ids, and
    // content-free failure classifications -- never an rmux error's `Display`,
    // which can carry session names, paths, wait matchers, and rendered screen
    // text. Without them, a private control-plane loss is classified and then
    // recorded nowhere, which is the diagnostic hole this filter's own comment
    // was accidentally describing. Anything added under these targets must hold
    // the same line; `rmux_sdk` and every other dependency stay off.
    let filter = EnvFilter::new("off,pmuxd=info,pseudomux_service=warn,pseudomux_rmux=warn");
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_ansi(false)
                .with_writer(file_writer),
        )
        .try_init()
        .ok();
    Ok(guard)
}

fn resolve_socket_path(socket: PathBuf) -> Result<PathBuf> {
    validate_socket_path(&socket)?;
    Ok(socket)
}

fn validate_socket_path(path: &Path) -> Result<()> {
    ensure_private_directory(socket_parent(path)?)
}

/// The socket directory and the log directory, private at every level pmuxd
/// creates.
///
/// The creation half is `create_private_dir_all` and no longer
/// `create_dir_all` + one `chmod`. MEASURED on this host at umask `022`, with
/// `--socket /tmp/pmux-14th/deep/run/pmux.sock`, before the change:
///
/// ```text
/// drwxr-xr-x  /tmp/pmux-14th
/// drwxr-xr-x  /tmp/pmux-14th/deep
/// drwx------  /tmp/pmux-14th/deep/run
/// ```
///
/// The guard's own refusal says "directory {} must be owner-only", and the two
/// directories pmuxd itself had just created were not. `create_dir_all` creates
/// every missing ancestor at `0o777 & !umask`; the single `set_permissions`
/// reached only the leaf.
///
/// The check half is unchanged in strength and is what the pool parent is now
/// held to as well (`pseudomux_service::pool`): a directory pmuxd did not
/// create is REFUSED when it is not owner-only or not the caller's, never
/// silently re-permissioned.
fn ensure_private_directory(path: &Path) -> Result<()> {
    let existed = match std::fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect directory {}", path.display()));
        }
    };
    if !existed {
        create_private_dir_all(path)
            .with_context(|| format!("failed to create directory {}", path.display()))?;
    }

    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect directory {}", path.display()))?;
    if !metadata.file_type().is_dir() {
        bail!("{} is not a directory", path.display());
    }

    #[cfg(unix)]
    if let Some(reason) = owner_only_violation(&metadata, effective_uid()) {
        bail!(
            "directory {} must be owner-only: {reason}. pmuxd never re-permissions a directory it \
             did not create; fix it with `chown {} {}` and `chmod 700 {}`, or point --socket at a \
             path pmuxd may create itself",
            path.display(),
            effective_uid(),
            path.display(),
            path.display()
        );
    }
    Ok(())
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn effective_uid() -> u32 {
    // SAFETY: `geteuid` has no preconditions and does not dereference pointers.
    unsafe { libc::geteuid() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_relative_socket_is_rejected() {
        let error = resolve_socket_path(PathBuf::from("relative/pmux.sock")).unwrap_err();
        assert!(error.to_string().contains("absolute"));
    }

    /// The one thing an operator meets first: `pmuxd serve --help`.
    ///
    /// Derived over clap's own argument list rather than over a list of flags
    /// somebody remembered, for the same reason the stray-flag guard is.
    #[test]
    fn every_serve_flag_an_operator_can_type_carries_help_text() {
        let command = <Cli as clap::CommandFactory>::command();
        let serve = command.find_subcommand("serve").unwrap();
        assert!(
            serve.get_about().is_some(),
            "`pmuxd serve` renders in `pmuxd --help` with no description"
        );
        let missing: Vec<String> = serve
            .get_arguments()
            .filter(|argument| argument.get_id() != "help")
            .filter(|argument| argument.get_help().is_none() && argument.get_long_help().is_none())
            .map(|argument| argument.get_id().to_string())
            .collect();
        assert!(
            missing.is_empty(),
            "these serve flags render with no description: {missing:?}"
        );
    }

    /// The absent-parent guard covers EVERY `--path-b-*` flag, one boot at a
    /// time, with the flag set derived from clap on both sides.
    ///
    /// The guard used to hold a hand-written list of ten names next to eleven
    /// declarations. It happened to be complete; the shape is the defect. This
    /// drives one real `resolve_path_b` per flag, so a flag whose id the guard
    /// misses is a red test naming that flag rather than a knob that silently
    /// does nothing.
    #[test]
    fn every_path_b_flag_is_refused_by_name_when_the_engine_is_off() {
        // A value each flag accepts. Derived shape, hand-supplied value: clap
        // cannot invent a valid value for an arbitrary type, but it CAN tell us
        // the full set of flags that must appear here, which is the half that
        // rots.
        let value_for = |id: &str| -> Vec<String> {
            let flag = format!("--{}", id.replace('_', "-"));
            match id {
                "path_b_claude" => vec![flag, "/bin/sh".to_owned()],
                "path_b_pool_size" | "path_b_recycle_turns" => vec![flag, "2".to_owned()],
                "path_b_warm" => vec![flag, "sonnet=1".to_owned()],
                "path_b_system_prompt" => vec![flag, "be brief".to_owned()],
                "path_b_system_prompt_file" => vec![flag, "/dev/null".to_owned()],
                "path_b_instance_idle_ttl_ms" | "path_b_turn_timeout_ms" => {
                    vec![flag, "1000".to_owned()]
                }
                "path_b_retain_dir" | "path_b_evidence_dir" => vec![flag, "/tmp".to_owned()],
                "path_b_rss_budget_mb" => vec![flag, "4096".to_owned()],
                "path_b_no_evidence" => vec![flag],
                "path_b_messages_bind" => vec![flag, "127.0.0.1:0".to_owned()],
                other => panic!(
                    "`--{}` is a --path-b-* flag with no value in this test; add one so the \
                     absent-parent guard is proven for it",
                    other.replace('_', "-")
                ),
            }
        };

        let ids = path_b_dependent_flag_ids();
        assert!(
            ids.len() >= 10,
            "the derivation found only {ids:?}; it is not reading the serve command's arguments"
        );
        for id in ids {
            let mut argv = vec![
                "pmuxd".to_owned(),
                "serve".to_owned(),
                "--socket".to_owned(),
                "/tmp/pmux.sock".to_owned(),
            ];
            argv.extend(value_for(&id));
            let matches = <Cli as clap::CommandFactory>::command().get_matches_from(&argv);
            let Command::Serve {
                path_b_parent,
                path_b_claude,
                path_b_pool_size,
                path_b_recycle_turns,
                path_b_warm,
                path_b_system_prompt,
                path_b_system_prompt_file,
                path_b_instance_idle_ttl_ms,
                path_b_turn_timeout_ms,
                path_b_retain_dir,
                path_b_rss_budget_mb,
                path_b_evidence_dir,
                path_b_no_evidence,
                ..
            } = Cli::try_parse_from(&argv).unwrap().command;
            let result = resolve_path_b(
                PathBOptions {
                    parent: path_b_parent,
                    claude: path_b_claude,
                    pool_size: path_b_pool_size,
                    recycle_turns: path_b_recycle_turns,
                    warm: path_b_warm,
                    system_prompt: path_b_system_prompt,
                    system_prompt_file: path_b_system_prompt_file,
                    instance_idle_ttl_ms: path_b_instance_idle_ttl_ms,
                    turn_timeout_ms: path_b_turn_timeout_ms,
                    retain_dir: path_b_retain_dir,
                    rss_budget_mb: path_b_rss_budget_mb,
                    evidence_dir: path_b_evidence_dir,
                    no_evidence: path_b_no_evidence,
                },
                &matches,
                Path::new("/tmp/pmux.sock"),
            );
            let flag = format!("--{}", id.replace('_', "-"));
            let error = match result {
                Err(error) => error.to_string(),
                Ok(_) => panic!(
                    "`pmuxd serve {flag} ...` booted with the stateless engine OFF and no \
                     complaint, so {flag} silently did nothing: the absent-parent guard does not \
                     cover it"
                ),
            };
            assert!(
                error.contains(&flag),
                "`pmuxd serve {flag} ...` without --path-b-parent was not refused by name: {error}"
            );
            assert!(
                error.contains("--path-b-parent DIR"),
                "the refusal for {flag} does not say what would be right: {error}"
            );
        }

        // And the guard stays silent when nothing Path B was typed, or Path A
        // could never boot.
        let argv = ["pmuxd", "serve", "--socket", "/tmp/pmux.sock"];
        let matches = <Cli as clap::CommandFactory>::command().get_matches_from(argv);
        let Command::Serve {
            path_b_pool_size,
            path_b_recycle_turns,
            path_b_system_prompt,
            path_b_instance_idle_ttl_ms,
            path_b_turn_timeout_ms,
            ..
        } = Cli::try_parse_from(argv).unwrap().command;
        assert!(
            resolve_path_b(
                PathBOptions {
                    parent: None,
                    claude: None,
                    pool_size: path_b_pool_size,
                    recycle_turns: path_b_recycle_turns,
                    warm: Vec::new(),
                    system_prompt: path_b_system_prompt,
                    system_prompt_file: None,
                    instance_idle_ttl_ms: path_b_instance_idle_ttl_ms,
                    turn_timeout_ms: path_b_turn_timeout_ms,
                    retain_dir: None,
                    rss_budget_mb: None,
                    evidence_dir: None,
                    no_evidence: false,
                },
                &matches,
                Path::new("/tmp/pmux.sock"),
            )
            .unwrap()
            .is_none()
        );
    }

    /// `--path-b-parent` without `--path-b-claude` must name the flag AND what
    /// a right value looks like.
    #[test]
    fn the_missing_claude_refusal_names_the_flag_and_a_usable_value() {
        let argv = [
            "pmuxd",
            "serve",
            "--socket",
            "/tmp/pmux.sock",
            "--path-b-parent",
            "/tmp/pool",
        ];
        let matches = <Cli as clap::CommandFactory>::command().get_matches_from(argv);
        let Command::Serve {
            path_b_parent,
            path_b_pool_size,
            path_b_recycle_turns,
            path_b_system_prompt,
            path_b_instance_idle_ttl_ms,
            path_b_turn_timeout_ms,
            ..
        } = Cli::try_parse_from(argv).unwrap().command;
        let error = resolve_path_b(
            PathBOptions {
                parent: path_b_parent,
                claude: None,
                pool_size: path_b_pool_size,
                recycle_turns: path_b_recycle_turns,
                warm: Vec::new(),
                system_prompt: path_b_system_prompt,
                system_prompt_file: None,
                instance_idle_ttl_ms: path_b_instance_idle_ttl_ms,
                turn_timeout_ms: path_b_turn_timeout_ms,
                retain_dir: None,
                rss_budget_mb: None,
                evidence_dir: None,
                no_evidence: false,
            },
            &matches,
            Path::new("/tmp/pmux.sock"),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("--path-b-claude PATH"), "{error}");
        assert!(error.contains("ABSOLUTE"), "{error}");
        assert!(error.contains("/usr/local/bin/claude"), "{error}");
    }

    /// The tiers this daemon names in a refusal are every tier the protocol
    /// has, derived from the protocol's own source.
    ///
    /// The message used to hand-write "low, medium, high, xhigh, max" -- with
    /// twenty-two spaces of Rust indentation folded into the middle of the
    /// sentence, which is what an operator saw. A hand-written list beside an
    /// enum is the defect this tree keeps finding; this is the same census
    /// `pool::refusal` uses over its own source.
    #[test]
    fn the_effort_tiers_this_daemon_names_are_every_tier_the_protocol_has() {
        const PROTOCOL_SOURCE: &str = include_str!("../../../crates/protocol/src/v1.rs");
        let (_, rest) = PROTOCOL_SOURCE
            .split_once("pub enum EffortLevel {")
            .expect("crates/protocol/src/v1.rs declares EffortLevel");
        let (body, _) = rest.split_once("\n}").expect("EffortLevel is terminated");
        let declared = body
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with("//") && !line.starts_with("#["))
            .count();
        assert_eq!(
            declared,
            EFFORT_TIERS.len(),
            "protocol-v1 EffortLevel has {declared} variants and this daemon names \
             {} of them in --path-b-warm's refusal",
            EFFORT_TIERS.len()
        );

        // And every named tier is one the parser actually accepts, spelled the
        // way the operator must type it.
        for tier in EFFORT_TIERS {
            let warm = parse_warm_class(&format!("sonnet/{}=1", tier.as_str())).unwrap();
            assert_eq!(warm.effort, Some(tier));
        }
        let error = parse_warm_class("sonnet/XHigh=1").unwrap_err().to_string();
        assert!(error.contains(&effort_tier_list()), "{error}");
        assert!(
            !error.contains("  "),
            "the refusal carries a run of source indentation: {error}"
        );
    }

    #[test]
    fn explicit_rmuxd_override_selects_hook_from_the_same_candidate_directory() {
        let mut runtime_config = PrivateRuntimeConfig {
            rmuxd: PathBuf::from("/candidate-a/pmux-rmuxd"),
            launcher: PathBuf::from("/candidate-a/pmux-launcher"),
            runtime_parent: None,
            startup_timeout: Duration::from_secs(1),
            operation_timeout: Duration::from_secs(1),
            lease_ttl: Duration::from_secs(1),
        };

        let hook = apply_companion_overrides(
            &mut runtime_config,
            Some(PathBuf::from("/candidate-b/pmux-rmuxd")),
            Some(PathBuf::from("/candidate-c/pmux-launcher")),
        )
        .unwrap();

        assert_eq!(
            runtime_config.rmuxd,
            PathBuf::from("/candidate-b/pmux-rmuxd")
        );
        assert_eq!(
            runtime_config.launcher,
            PathBuf::from("/candidate-c/pmux-launcher")
        );
        assert_eq!(hook, PathBuf::from("/candidate-b/pmux-hook"));
    }

    /// EVERY directory pmuxd creates for the socket is owner-only, not just the
    /// last one.
    ///
    /// The previous version of this test used `<tempdir>/state`: one level, and
    /// its parent already existed. A fixture with no intermediate level cannot
    /// observe an unsealed intermediate level, so it passed against a
    /// `create_dir_all` + one `chmod` that left every ancestor at
    /// `0o777 & !umask`. MEASURED with `--socket
    /// /tmp/pmux-14th/deep/run/pmux.sock` and umask `022`:
    ///
    /// ```text
    /// drwxr-xr-x  /tmp/pmux-14th
    /// drwxr-xr-x  /tmp/pmux-14th/deep
    /// drwx------  /tmp/pmux-14th/deep/run
    /// ```
    ///
    /// The path is nested now, and the levels walked are derived from it by an
    /// ancestor walk rather than listed, so a level pmuxd creates cannot be
    /// absent from the check.
    #[test]
    fn every_socket_parent_level_pmuxd_creates_is_owner_only() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("deep/run/state");
        let socket = parent.join("pmux.sock");
        validate_socket_path(&socket).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let created: Vec<&Path> = parent
                .ancestors()
                .take_while(|ancestor| *ancestor != directory.path())
                .collect();
            assert_eq!(
                created.len(),
                3,
                "the fixture must have intermediate levels or it cannot see the defect: {created:?}"
            );
            for level in created {
                let metadata = std::fs::metadata(level).unwrap();
                assert_eq!(metadata.uid(), effective_uid(), "{}", level.display());
                assert_eq!(
                    metadata.permissions().mode() & 0o777,
                    0o700,
                    "{} is not owner-only",
                    level.display()
                );
            }
        }
    }

    #[test]
    fn permissive_existing_socket_parent_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("state");
        std::fs::create_dir(&parent).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
            let error = validate_socket_path(&parent.join("pmux.sock")).unwrap_err();
            assert!(error.to_string().contains("owner-only"));
        }
    }

    #[tokio::test]
    async fn bound_socket_is_private_and_removed_by_guard() {
        let directory = private_tempdir();
        let socket = directory.path().join("pmux.sock");
        let (listener, mut guard) = bind_socket(&socket).await.unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let metadata = std::fs::metadata(&socket).unwrap();
            assert_eq!(metadata.uid(), effective_uid());
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
        drop(listener);
        assert!(guard.cleanup().unwrap());
        assert!(!socket.exists());
    }

    #[tokio::test]
    async fn live_socket_is_never_replaced() {
        let directory = private_tempdir();
        let socket = directory.path().join("pmux.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let error = bind_socket(&socket).await.unwrap_err();
        assert!(error.to_string().contains("live listener"));
        assert!(socket.exists());
        drop(listener);
    }

    #[tokio::test]
    async fn stale_owned_socket_is_replaced() {
        let directory = private_tempdir();
        let socket = directory.path().join("pmux.sock");
        drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());

        let (listener, mut guard) = bind_socket(&socket).await.unwrap();
        assert!(socket.exists());
        drop(listener);
        assert!(guard.cleanup().unwrap());
    }

    #[tokio::test]
    async fn non_socket_at_endpoint_is_never_removed() {
        let directory = private_tempdir();
        let path = directory.path().join("pmux.sock");
        std::fs::write(&path, b"do not remove").unwrap();

        let error = bind_socket(&path).await.unwrap_err();
        assert!(error.to_string().contains("non-socket"));
        assert_eq!(std::fs::read(path).unwrap(), b"do not remove");
    }

    #[tokio::test]
    async fn cleanup_guard_does_not_remove_replacement() {
        let directory = private_tempdir();
        let path = directory.path().join("pmux.sock");
        let (listener, mut guard) = bind_socket(&path).await.unwrap();
        drop(listener);
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"replacement").unwrap();

        assert!(!guard.cleanup().unwrap());
        assert_eq!(std::fs::read(path).unwrap(), b"replacement");
    }

    #[test]
    fn tested_profiles_are_strict_structured_and_ambiguity_free() {
        let profile = serde_json::json!({
            "claude_version": "2.1.207",
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "terminal_profile": "transparent",
            "input_transport": "sdk",
            "transcript_drain_ms": 875,
        })
        .to_string();
        let profiles = parse_tested_profiles(vec![profile.clone()]).unwrap();
        assert_eq!(profiles.len(), 1);
        assert!(parse_tested_profiles(vec![profile.clone(), profile]).is_err());
        assert!(parse_tested_profiles(vec!["  ".to_owned()]).is_err());
        assert!(
            parse_tested_profiles(vec![
                serde_json::json!({
                    "claude_version": "2.1.207",
                    "os": std::env::consts::OS,
                    "arch": std::env::consts::ARCH,
                    "terminal_profile": "transparent",
                    "input_transport": "sdk",
                    "transcript_drain_ms": 875,
                    "unexpected": true,
                })
                .to_string()
            ])
            .is_err()
        );
    }

    /// An operator may state a RANGE, and a profile that does not is still an
    /// exact match.
    ///
    /// The optional field is the whole of the operator-visible change: every
    /// `--tested-claude-profile` written before `claude_version_tested_through`
    /// existed still parses and still means one version. What the range adds is
    /// refused when it is ambiguous -- two cells that could both admit one
    /// version are as ambiguous as two copies of one cell, and `insert` says so
    /// at boot rather than at turn 200.
    #[test]
    fn an_operator_may_state_a_version_range_and_overlapping_ones_are_refused() {
        let cell = |extra: serde_json::Value| {
            let mut profile = serde_json::json!({
                "claude_version": "2.1.207",
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "terminal_profile": "transparent",
                "input_transport": "sdk",
                "transcript_drain_ms": 875,
            });
            for (key, value) in extra.as_object().unwrap() {
                profile[key] = value.clone();
            }
            profile.to_string()
        };

        let ranged = cell(serde_json::json!({"claude_version_tested_through": "2.1.215"}));
        assert_eq!(
            parse_tested_profiles(vec![ranged.clone()]).unwrap().len(),
            1
        );

        // Exact, and inside the range above: overlapping, therefore refused.
        assert!(
            parse_tested_profiles(vec![ranged.clone(), cell(serde_json::json!({}))]).is_err(),
            "an exact cell inside another cell's range is ambiguous"
        );
        // Adjacent, therefore admissible: two measurements that partition the
        // line are a legitimate thing for an operator to hold.
        assert_eq!(
            parse_tested_profiles(vec![
                ranged,
                cell(serde_json::json!({
                    "claude_version": "2.1.216",
                    "claude_version_tested_through": "2.1.220",
                })),
            ])
            .unwrap()
            .len(),
            2
        );
        // A range across a minor is refused: patch drift is tolerated, a minor
        // is not.
        assert!(
            parse_tested_profiles(vec![cell(
                serde_json::json!({"claude_version_tested_through": "2.2.0"})
            )])
            .is_err(),
            "a tested range may not span a minor version"
        );
        // ...and so is an inverted one.
        assert!(
            parse_tested_profiles(vec![cell(
                serde_json::json!({"claude_version_tested_through": "2.1.206"})
            )])
            .is_err(),
            "a ceiling below its floor admits nothing and means nothing"
        );
    }

    #[test]
    fn cli_replaces_bare_version_admission_with_structured_profiles() {
        let profile = serde_json::json!({
            "claude_version": "2.1.207",
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "terminal_profile": "transparent",
            "input_transport": "sdk",
            "transcript_drain_ms": 875,
        })
        .to_string();
        let parsed = Cli::try_parse_from([
            "pmuxd",
            "serve",
            "--socket",
            "/tmp/pmux.sock",
            "--tested-claude-profile",
            &profile,
        ])
        .unwrap();
        let Command::Serve {
            tested_claude_profiles,
            untested_transcript_drain_ms,
            ..
        } = parsed.command;
        assert_eq!(tested_claude_profiles, vec![profile]);
        assert_eq!(
            untested_transcript_drain_ms,
            DEFAULT_UNTESTED_TRANSCRIPT_DRAIN_MS
        );

        assert!(
            Cli::try_parse_from([
                "pmuxd",
                "serve",
                "--socket",
                "/tmp/pmux.sock",
                "--tested-claude-version",
                "2.1.207",
            ])
            .is_err()
        );
    }

    fn private_tempdir() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        directory
    }
}
