//! Client-side agent profiles: one JSON document expanded into launch fields.
//!
//! A profile supplies the repetitive parts of a `StartSessionRequest` — model,
//! effort, permission mode, MCP configs, plugin directories, terminal geometry —
//! so a caller does not retype fifteen flags per invocation. Expansion happens
//! **entirely in the client, before the request is framed**. pmuxd never learns
//! that a profile existed, so child argv stays a pure function of the request
//! the daemon received, and no server-side registry has to be kept in sync.
//!
//! Four things are deliberately inexpressible here, because a config file that
//! silently redirects them is exactly the ambient resolution this product
//! refuses everywhere else: `cwd`, session identity, the prompt, and the turn
//! deadline. Naming any of them is a parse error, not a silent no-op.
//!
//! The document deserializes into the protocol's own types (`EffortLevel`,
//! `PermissionMode`, `ConfigSource`, `LifecycleMode`, …) so there is no second
//! schema to drift. Only the composition operators are defined here:
//!
//! * **Scalars** — absent inherits, present replaces. A literal JSON `null` is
//!   an error: v1 has no unset operator, and "null means unset" would be a
//!   second, invisible one.
//! * **Lists** — append, parent first. That is exactly how argv is built (one
//!   repeated flag per element), so composition and launch agree by
//!   construction.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use pseudomux_protocol::v1::launch_environment;
use pseudomux_protocol::v1::{
    AuthPolicy, CompatibilityPolicy, ConfigIsolation, ConfigSource, EffortLevel, EnvironmentSpec,
    InputTransport, LifecycleMode, PermissionMode, RetentionPolicy, SessionCell,
    SystemPromptPolicy, TerminalProfile,
};
use serde_json::Value;
use thiserror::Error;

/// Maximum number of documents in one `extends` chain, counting the requested
/// agent itself. A longer chain is rejected, never silently truncated.
pub const MAX_AGENT_CHAIN_DEPTH: usize = 4;

/// The only supported document version.
pub const AGENT_PROFILE_VERSION: u64 = 1;

const AGENT_KEYS: &[&str] = &[
    "extends",
    "claude",
    "terminal",
    "auth_policy",
    "lifecycle",
    "retention",
    "compatibility",
    "cell",
    "config_isolation",
    "require_env",
];

const CLAUDE_KEYS: &[&str] = &[
    "model",
    "effort",
    "permission_mode",
    "allowed_tools",
    "denied_tools",
    "settings",
    "mcp_configs",
    "plugin_dirs",
    "system_prompt",
    "extra_args",
];

const TERMINAL_KEYS: &[&str] = &["rows", "cols", "profile", "input_transport"];

/// Names a profile may never carry. They are per-invocation and always explicit.
const PER_INVOCATION_KEYS: &[&str] = &[
    "cwd",
    "deadline",
    "deadline_unix_ms",
    "environment",
    "executable",
    "identity",
    "prompt",
    "prompt_file",
    "resume",
    "session_id",
    "turn",
    "turn_id",
];

#[derive(Debug, Error)]
#[error("{0}")]
pub struct AgentProfileError(String);

type Result<T> = std::result::Result<T, AgentProfileError>;

fn refuse(message: impl Into<String>) -> AgentProfileError {
    AgentProfileError(message.into())
}

/// One agent expanded through its whole `extends` chain.
///
/// Scalars are `Option` so a caller can tell "the profile said nothing" from
/// "the profile said the default value"; an explicit CLI flag overrides a
/// `Some`, and appends to the lists.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentProfile {
    pub model: Option<String>,
    pub effort: Option<EffortLevel>,
    pub permission_mode: Option<PermissionMode>,
    pub system_prompt: Option<SystemPromptPolicy>,
    pub allowed_tools: Vec<String>,
    pub denied_tools: Vec<String>,
    pub settings: Vec<ConfigSource>,
    pub mcp_configs: Vec<ConfigSource>,
    pub plugin_dirs: Vec<String>,
    pub extra_args: Vec<String>,
    pub auth_policy: Option<AuthPolicy>,
    pub compatibility: Option<CompatibilityPolicy>,
    /// Which cell a session launched from this profile is driven as. Expressible
    /// here because every other launch policy is, and because a Path B profile
    /// is exactly the kind of thing a profile exists to name once.
    pub cell: Option<SessionCell>,
    /// A pmux-owned Claude configuration root for sessions launched from this
    /// profile. Named here for the same reason `cell` is: a Path B profile that
    /// says "minified" and then borrows the operator's `~/.claude` has only
    /// half of what it needs.
    pub config_isolation: Option<ConfigIsolation>,
    pub lifecycle: Option<LifecycleMode>,
    pub retention: Option<RetentionPolicy>,
    pub rows: Option<u16>,
    pub cols: Option<u16>,
    pub terminal_profile: Option<TerminalProfile>,
    pub input_transport: Option<InputTransport>,
    pub require_env: Vec<String>,
}

#[derive(Clone, Debug, Default)]
struct RawAgent {
    extends: Option<String>,
    profile: AgentProfile,
}

macro_rules! scalar {
    ($map:expr, $key:literal, $context:expr) => {
        match $map.remove($key) {
            None => None,
            Some(value) => {
                Some(serde_json::from_value(value).map_err(|error| {
                    refuse(format!("{}: invalid `{}`: {error}", $context, $key))
                })?)
            }
        }
    };
}

macro_rules! list {
    ($map:expr, $key:literal, $context:expr) => {
        match $map.remove($key) {
            None => Vec::new(),
            Some(value) => serde_json::from_value(value)
                .map_err(|error| refuse(format!("{}: invalid `{}`: {error}", $context, $key)))?,
        }
    };
}

/// Reads `path` and expands `agent` through its `extends` chain.
///
/// `path` must be absolute: profiles are located by explicit path or the single
/// `PMUX_AGENT_FILE` fallback, never by searching XDG directories or walking up
/// from the working directory.
pub fn load_agent_profile(path: &Path, agent: &str) -> Result<AgentProfile> {
    if !path.is_absolute() {
        return Err(refuse(format!(
            "agent profile path must be absolute, got {}",
            path.display()
        )));
    }
    let display = path.display().to_string();
    let text = fs::read_to_string(path)
        .map_err(|error| refuse(format!("{display}: cannot read agent profile: {error}")))?;
    let mode = fs::metadata(path)
        .map_err(|error| refuse(format!("{display}: cannot stat agent profile: {error}")))?
        .permissions()
        .mode()
        & 0o777;
    expand(&text, mode, &display, agent)
}

/// The whole loader minus the filesystem read, so document rules are testable
/// without a real file. `mode` is the profile file's own permission bits.
pub fn expand(text: &str, mode: u32, display: &str, agent: &str) -> Result<AgentProfile> {
    let document: Value = serde_json::from_str(text)
        .map_err(|error| refuse(format!("{display}: invalid JSON: {error}")))?;
    reject_duplicate_keys(text, display)?;
    reject_nulls(&document, display, "document")?;

    let object = document
        .as_object()
        .ok_or_else(|| refuse(format!("{display}: document must be a JSON object")))?;
    let mut remaining = object.clone();
    match remaining.remove("version").as_ref().and_then(Value::as_u64) {
        Some(AGENT_PROFILE_VERSION) => {}
        Some(other) => {
            return Err(refuse(format!(
                "{display}: unsupported profile version {other}, expected {AGENT_PROFILE_VERSION}"
            )));
        }
        None => {
            return Err(refuse(format!(
                "{display}: missing required `version`: {AGENT_PROFILE_VERSION}"
            )));
        }
    }
    let agents_value = remaining
        .remove("agents")
        .ok_or_else(|| refuse(format!("{display}: missing required `agents` object")))?;
    reject_unknown_keys(&remaining, display, "document")?;

    let agents_object = agents_value
        .as_object()
        .ok_or_else(|| refuse(format!("{display}: `agents` must be a JSON object")))?;
    let mut agents = BTreeMap::new();
    for (name, value) in agents_object {
        validate_agent_name(name, display)?;
        agents.insert(name.clone(), parse_agent(name, value, display)?);
    }
    if agents.is_empty() {
        return Err(refuse(format!("{display}: `agents` defines no agents")));
    }

    // A secret at rest in a world-readable config is made structurally
    // impossible rather than merely discouraged: any inline document anywhere in
    // the file forces the file itself to be owner-only.
    if agents
        .values()
        .flat_map(|raw| raw.profile.settings.iter().chain(&raw.profile.mcp_configs))
        .any(|source| matches!(source, ConfigSource::Inline { .. }))
        && mode & 0o077 != 0
    {
        return Err(refuse(format!(
            "{display}: profile carries an inline settings or MCP document and must be owner-only \
             (mode 0600); mode is {mode:04o}"
        )));
    }

    let chain = resolve_chain(&agents, agent, display)?;
    let mut profile = AgentProfile::default();
    for name in &chain {
        merge_into(&mut profile, agents[name].profile.clone());
    }
    validate_resolved_paths(&profile, display)?;
    Ok(profile)
}

fn parse_agent(name: &str, value: &Value, display: &str) -> Result<RawAgent> {
    let context = format!("{display}: agent `{name}`");
    let object = value
        .as_object()
        .ok_or_else(|| refuse(format!("{context} must be a JSON object")))?;
    let mut remaining = object.clone();
    let mut raw = RawAgent::default();

    if let Some(parent) = remaining.remove("extends") {
        let parent = parent
            .as_str()
            .ok_or_else(|| refuse(format!("{context}: `extends` must be an agent name")))?;
        validate_agent_name(parent, display)?;
        raw.extends = Some(parent.to_owned());
    }
    if let Some(claude) = remaining.remove("claude") {
        parse_claude(&mut raw.profile, &claude, &context)?;
    }
    if let Some(terminal) = remaining.remove("terminal") {
        parse_terminal(&mut raw.profile, &terminal, &context)?;
    }
    raw.profile.auth_policy = scalar!(remaining, "auth_policy", context);
    raw.profile.compatibility = scalar!(remaining, "compatibility", context);
    raw.profile.cell = scalar!(remaining, "cell", context);
    raw.profile.config_isolation = scalar!(remaining, "config_isolation", context);
    raw.profile.lifecycle = scalar!(remaining, "lifecycle", context);
    raw.profile.retention = scalar!(remaining, "retention", context);
    raw.profile.require_env = list!(remaining, "require_env", context);
    reject_unknown_keys(&remaining, &context, "agent")?;
    reject_reserved_values(&raw.profile, &context)?;
    Ok(raw)
}

fn parse_claude(profile: &mut AgentProfile, value: &Value, context: &str) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| refuse(format!("{context}: `claude` must be a JSON object")))?;
    let mut remaining = object.clone();
    profile.model = scalar!(remaining, "model", context);
    profile.effort = scalar!(remaining, "effort", context);
    profile.permission_mode = scalar!(remaining, "permission_mode", context);
    profile.system_prompt = scalar!(remaining, "system_prompt", context);
    profile.allowed_tools = list!(remaining, "allowed_tools", context);
    profile.denied_tools = list!(remaining, "denied_tools", context);
    profile.settings = list!(remaining, "settings", context);
    profile.mcp_configs = list!(remaining, "mcp_configs", context);
    profile.plugin_dirs = list!(remaining, "plugin_dirs", context);
    profile.extra_args = list!(remaining, "extra_args", context);
    reject_unknown_keys(&remaining, context, "claude")
}

fn parse_terminal(profile: &mut AgentProfile, value: &Value, context: &str) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| refuse(format!("{context}: `terminal` must be a JSON object")))?;
    let mut remaining = object.clone();
    profile.rows = scalar!(remaining, "rows", context);
    profile.cols = scalar!(remaining, "cols", context);
    profile.terminal_profile = scalar!(remaining, "profile", context);
    profile.input_transport = scalar!(remaining, "input_transport", context);
    if profile.rows == Some(0) || profile.cols == Some(0) {
        return Err(refuse(format!(
            "{context}: terminal rows and cols must be greater than zero"
        )));
    }
    reject_unknown_keys(&remaining, context, "terminal")
}

/// Values the wire type can represent but the daemon refuses. Rejecting them
/// here means the caller sees the problem at expansion time, naming the profile
/// and the key, instead of as an opaque daemon error one launch later.
fn reject_reserved_values(profile: &AgentProfile, context: &str) -> Result<()> {
    if profile.terminal_profile == Some(TerminalProfile::RmuxStandard) {
        return Err(refuse(format!(
            "{context}: `terminal.profile: rmux_standard` is reserved and has not passed the \
             Phase 0 release gate"
        )));
    }
    if profile.input_transport == Some(InputTransport::AttachedStream) {
        return Err(refuse(format!(
            "{context}: `terminal.input_transport: attached_stream` is reserved and is not \
             enabled by the validated v1 profile"
        )));
    }
    if profile.retention == Some(RetentionPolicy::OneShot) {
        return Err(refuse(format!(
            "{context}: `retention: one_shot` is reserved for run_once and is rejected by \
             start_session"
        )));
    }
    Ok(())
}

fn resolve_chain(
    agents: &BTreeMap<String, RawAgent>,
    agent: &str,
    display: &str,
) -> Result<Vec<String>> {
    let mut chain: Vec<String> = Vec::new();
    let mut seen = BTreeSet::new();
    let mut current = agent.to_owned();
    loop {
        if !seen.insert(current.clone()) {
            chain.push(current);
            return Err(refuse(format!(
                "{display}: agent `{agent}` has a cyclic `extends` chain: {}",
                chain.join(" -> ")
            )));
        }
        let raw = agents.get(&current).ok_or_else(|| {
            refuse(format!(
                "{display}: agent `{current}` is not defined; known agents: {}",
                agents.keys().cloned().collect::<Vec<_>>().join(", ")
            ))
        })?;
        chain.push(current.clone());
        if chain.len() > MAX_AGENT_CHAIN_DEPTH {
            return Err(refuse(format!(
                "{display}: agent `{agent}` has an `extends` chain deeper than \
                 {MAX_AGENT_CHAIN_DEPTH}: {}",
                chain.join(" -> ")
            )));
        }
        match &raw.extends {
            Some(parent) => current = parent.clone(),
            None => break,
        }
    }
    chain.reverse();
    Ok(chain)
}

/// Applies one document over an accumulated ancestor result. Destructuring is
/// exhaustive on purpose: a new field cannot be added without choosing an
/// operator for it here.
fn merge_into(base: &mut AgentProfile, overlay: AgentProfile) {
    let AgentProfile {
        model,
        effort,
        permission_mode,
        system_prompt,
        mut allowed_tools,
        mut denied_tools,
        mut settings,
        mut mcp_configs,
        mut plugin_dirs,
        mut extra_args,
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
        mut require_env,
    } = overlay;

    // Scalars: present replaces, absent inherits.
    replace(&mut base.model, model);
    replace(&mut base.effort, effort);
    replace(&mut base.permission_mode, permission_mode);
    replace(&mut base.system_prompt, system_prompt);
    replace(&mut base.auth_policy, auth_policy);
    replace(&mut base.compatibility, compatibility);
    replace(&mut base.cell, cell);
    replace(&mut base.config_isolation, config_isolation);
    replace(&mut base.lifecycle, lifecycle);
    replace(&mut base.retention, retention);
    replace(&mut base.rows, rows);
    replace(&mut base.cols, cols);
    replace(&mut base.terminal_profile, terminal_profile);
    replace(&mut base.input_transport, input_transport);

    // Lists: append, parent first.
    base.allowed_tools.append(&mut allowed_tools);
    base.denied_tools.append(&mut denied_tools);
    base.settings.append(&mut settings);
    base.mcp_configs.append(&mut mcp_configs);
    base.plugin_dirs.append(&mut plugin_dirs);
    base.extra_args.append(&mut extra_args);
    base.require_env.append(&mut require_env);
}

fn replace<T>(base: &mut Option<T>, overlay: Option<T>) {
    if overlay.is_some() {
        *base = overlay;
    }
}

/// Every path a profile names is absolute (a profile carries no `cwd` to
/// resolve against) and every referenced config file is owner-only. This mirrors
/// `ensure_private_directory` (`bin/pmuxd/src/main.rs:578`) for files the daemon
/// itself never mode-checks.
fn validate_resolved_paths(profile: &AgentProfile, display: &str) -> Result<()> {
    for (label, sources) in [
        ("settings", &profile.settings),
        ("mcp_configs", &profile.mcp_configs),
    ] {
        for source in sources {
            if let ConfigSource::File { path } = source {
                let path = Path::new(path);
                if !path.is_absolute() {
                    return Err(refuse(format!(
                        "{display}: `{label}` file path must be absolute, got {}",
                        path.display()
                    )));
                }
                let metadata = fs::metadata(path).map_err(|error| {
                    refuse(format!(
                        "{display}: `{label}` file {} is unreadable: {error}",
                        path.display()
                    ))
                })?;
                if !metadata.is_file() {
                    return Err(refuse(format!(
                        "{display}: `{label}` path {} is not a regular file",
                        path.display()
                    )));
                }
                let mode = metadata.permissions().mode() & 0o777;
                if mode & 0o077 != 0 {
                    return Err(refuse(format!(
                        "{display}: `{label}` file {} must be owner-only (mode 0600); \
                         mode is {mode:04o}",
                        path.display()
                    )));
                }
            }
        }
    }
    for directory in &profile.plugin_dirs {
        if !Path::new(directory).is_absolute() {
            return Err(refuse(format!(
                "{display}: `plugin_dirs` entry must be absolute, got {directory}"
            )));
        }
    }
    Ok(())
}

/// Asserts every `require_env` name is present and non-empty, and returns one
/// warning per name the launch will not deliver to the child.
///
/// Two independent mechanisms can drop a name, and the warning says which:
///
/// 1. **The inheritance allowlist** ([`launch_environment::inherits`]) filters
///    the caller snapshot, so a name that is merely *present* — `GITHUB_TOKEN`,
///    `MY_API_KEY`, anything outside the admitted set — is dropped at launch.
///    This is the dominant cause and the reason this check exists.
/// 2. **The policy denylists** — `auth_policy: subscription` and the
///    `transparent` terminal profile — remove specific names *after* the
///    caller's explicit patch is applied.
///
/// The distinction is not cosmetic: `--env-passthrough NAME` (which lands in
/// `EnvironmentSpec::set`) defeats (1) and is powerless against (2), because the
/// denylist passes run after `set`. So the remedy is only offered when the
/// allowlist is the sole reason, and a name already delivered through `set` is
/// never reported as allowlist-dropped.
///
/// **This warns rather than errors, deliberately.** The presence and emptiness
/// checks above fail hard because they observe the caller's own environment
/// directly and cannot be wrong. Everything below is a *prediction* made from
/// [`pseudomux_protocol::v1::launch_environment`], which is the one definition
/// of the policy and the reason this client no longer keeps a copy of it. The
/// prediction can still be wrong for a reason no shared definition can fix: a
/// client is separately versioned from the pmuxd it talks to, so an older
/// protocol crate facing a newer daemon that has since admitted the name would
/// turn a launch the daemon accepts into a client-side hard failure. A
/// prediction must never be able to break a correct launch; it may only ever be
/// noise. The warning goes to stderr on every `pmux start`
/// (`bin/pmux/src/cli.rs:311-317`), which is the product's chosen surface for
/// "this is not what you think it is".
///
/// The value is never read, copied, or printed: only presence is observed.
pub fn verify_required_environment(
    require_env: &[String],
    environment: &EnvironmentSpec,
    auth_policy: AuthPolicy,
    terminal_profile: TerminalProfile,
) -> Result<Vec<String>> {
    let mut effective = environment.snapshot.clone();
    for key in &environment.unset {
        effective.remove(key);
    }
    for (key, value) in &environment.set {
        effective.insert(key.clone(), value.clone());
    }

    let mut warnings = Vec::new();
    for name in require_env {
        if name.is_empty() || name.contains('=') || name.contains('\0') {
            return Err(refuse(format!(
                "require_env entry {name:?} is not a usable environment variable name"
            )));
        }
        match effective.get(name) {
            Some(value) if !value.is_empty() => {}
            Some(_) => {
                return Err(refuse(format!(
                    "required environment variable {name} is set but empty; export it before \
                     launching this agent"
                )));
            }
            None => {
                return Err(refuse(format!(
                    "required environment variable {name} is not set; export it before launching \
                     this agent"
                )));
            }
        }
        let assessment = strip_reasons(
            name,
            environment.set.contains_key(name),
            auth_policy,
            terminal_profile,
        );
        if !assessment.reasons.is_empty() {
            let remedy = if assessment.escape_hatch_restores {
                format!(
                    "; pass it through explicitly with `--env-passthrough {name}`, the one \
                     channel the allowlist does not filter"
                )
            } else {
                "; `--env-passthrough` cannot restore it, because those removals run after the \
                 caller's explicit environment set"
                    .to_owned()
            };
            warnings.push(format!(
                "warning: require_env {name} is present but the launched Claude will not see it: \
                 {}{remedy}",
                assessment.reasons.join(" and ")
            ));
        }
    }
    Ok(warnings)
}

/// Why one `require_env` name will not reach the child, and whether the
/// documented escape hatch fixes it.
struct StripAssessment {
    /// One phrase per independent mechanism, in launch order.
    reasons: Vec<&'static str>,
    /// True only when the allowlist is the *sole* reason, so routing the name
    /// through `EnvironmentSpec::set` would deliver it. False when a denylist
    /// pass also applies, because those run after `set` and would strip it
    /// again — advertising the escape hatch there would be wrong advice.
    escape_hatch_restores: bool,
}

fn strip_reasons(
    name: &str,
    delivered_explicitly: bool,
    auth_policy: AuthPolicy,
    terminal_profile: TerminalProfile,
) -> StripAssessment {
    let mut reasons = Vec::new();

    // 1. The allowlist filters the inherited snapshot only. A name the caller
    //    states explicitly bypasses it, so it is not a reason in that case.
    let allowlist_drops = !delivered_explicitly && !launch_environment::inherits(name, auth_policy);
    if allowlist_drops {
        reasons.push("the launch inheritance allowlist does not admit it");
    }

    // 2/3. The denylists, which run after the explicit set and therefore apply
    //      however the name arrived.
    if launch_environment::subscription_policy_removes(name, auth_policy) {
        reasons.push("auth_policy=subscription removes it");
    }
    if terminal_profile == TerminalProfile::Transparent
        && launch_environment::transparent_profile_removes(name)
    {
        reasons.push("terminal profile=transparent removes it");
    }

    StripAssessment {
        escape_hatch_restores: allowlist_drops && reasons.len() == 1,
        reasons,
    }
}

fn validate_agent_name(name: &str, display: &str) -> Result<()> {
    let acceptable = !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        });
    if acceptable {
        Ok(())
    } else {
        Err(refuse(format!(
            "{display}: agent name {name:?} must be 1-64 characters of [A-Za-z0-9._-]"
        )))
    }
}

fn reject_unknown_keys(
    remaining: &serde_json::Map<String, Value>,
    context: &str,
    section: &str,
) -> Result<()> {
    let Some(key) = remaining.keys().next() else {
        return Ok(());
    };
    if PER_INVOCATION_KEYS.contains(&key.as_str()) {
        return Err(refuse(format!(
            "{context}: `{key}` is per-invocation and is never expressible in a profile; pass it \
             explicitly on every call"
        )));
    }
    let known = match section {
        "document" => ["version", "agents"].join(", "),
        "agent" => AGENT_KEYS.join(", "),
        "claude" => CLAUDE_KEYS.join(", "),
        _ => TERMINAL_KEYS.join(", "),
    };
    Err(refuse(format!(
        "{context}: unknown {section} key `{key}`; expected one of: {known}"
    )))
}

/// A literal `null` is a parse error rather than an unset operator. Inline
/// settings and MCP documents are exempt: their contents belong to Claude, not
/// to this schema.
fn reject_nulls(value: &Value, display: &str, location: &str) -> Result<()> {
    match value {
        Value::Null => Err(refuse(format!(
            "{display}: {location} is null; absent means inherit and v1 has no unset operator"
        ))),
        Value::Array(items) => items.iter().enumerate().try_for_each(|(index, item)| {
            reject_nulls(item, display, &format!("{location}[{index}]"))
        }),
        Value::Object(map) => {
            let inline = map.get("source").and_then(Value::as_str) == Some("inline");
            map.iter().try_for_each(|(key, item)| {
                if inline && key == "document" {
                    return Ok(());
                }
                reject_nulls(item, display, &format!("{location}.{key}"))
            })
        }
        _ => Ok(()),
    }
}

/// Rejects any object that repeats a key. `serde_json` keeps the last
/// occurrence silently, so a document defining `yolo` twice would resolve to
/// whichever definition the author did not read. Keys are compared as literal
/// source text; agent names are separately restricted to `[A-Za-z0-9._-]`, so no
/// accepted name has a second spelling.
fn reject_duplicate_keys(text: &str, display: &str) -> Result<()> {
    let bytes = text.as_bytes();
    let mut frames: Vec<Option<BTreeSet<&str>>> = Vec::new();
    let mut expecting_key = false;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'{' => {
                frames.push(Some(BTreeSet::new()));
                expecting_key = true;
                index += 1;
            }
            b'[' => {
                frames.push(None);
                expecting_key = false;
                index += 1;
            }
            b'}' | b']' => {
                frames.pop();
                expecting_key = false;
                index += 1;
            }
            b',' => {
                expecting_key = matches!(frames.last(), Some(Some(_)));
                index += 1;
            }
            b':' => {
                expecting_key = false;
                index += 1;
            }
            b'"' => {
                let start = index + 1;
                let end = string_end(bytes, start).ok_or_else(|| {
                    refuse(format!("{display}: unterminated JSON string literal"))
                })?;
                if expecting_key
                    && let Some(Some(keys)) = frames.last_mut()
                    && !keys.insert(&text[start..end])
                {
                    return Err(refuse(format!(
                        "{display}: duplicate key \"{}\"; a repeated key would silently resolve to \
                         its last definition",
                        &text[start..end]
                    )));
                }
                expecting_key = false;
                index = end + 1;
            }
            _ => index += 1,
        }
    }
    Ok(())
}

fn string_end(bytes: &[u8], mut index: usize) -> Option<usize> {
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'"' => return Some(index),
            _ => index += 1,
        }
    }
    None
}
