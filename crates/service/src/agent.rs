//! The agent resource: one stored, versioned, immutable launch configuration.
//!
//! # The one invariant
//!
//! > **An agent may narrow what a session may name. It may never name a
//! > resource on the session's behalf.**
//!
//! Every rule in this module is an instance of it, and every field of
//! [`AgentSpec`] classifies mechanically under it. `cwd`, `config_isolation`
//! and `identity` name resources pmux CLAIMS and stay per-session;
//! `environment.snapshot` is a fact about the calling process at call time and
//! stays per-session structurally, because [`AgentEnvironmentSpec`] deletes the
//! field rather than documenting that it must be empty; everything else is
//! launch policy a caller retypes identically on every call.
//!
//! # An agent is not a security boundary, and must never be documented as one
//!
//! The daemon and its clients run as the same uid (`docs/spec.md` Sec. 10.2), so
//! anything an agent would refuse the caller can send directly as an inline
//! DTO. The value of the resource is deduplication, pinning and auditability.
//! Every containment rule here is a NARROWING of what one request may say,
//! composed with `AND` against the checks that already run, and never a
//! capability. If any sentence in this module reads as though an agent
//! *constrains* a caller who could not otherwise reach the same launch, that
//! sentence is wrong.
//!
//! # Why child argv is still a pure function of the request
//!
//! `docs/spec.md` Sec. 4.4 requires it, and a server-side registry threatens it:
//! a registry makes the launch a function of the request *and* of server-held
//! state, so one DTO could produce two different launches. Four properties
//! answer that, and without all four this module would not be worth building:
//!
//! 1. The reference is **pinned by version**, and the version is **named in the
//!    request** ([`AgentRef::version`] is required). "Latest at start time" is
//!    refused, because that makes the launch a function of *when the request
//!    arrived*, which is exactly the impurity Sec. 4.4 forbids.
//! 2. A stored version is **immutable**. [`AgentStore::update`] mints a new
//!    version and never opens an existing one for write, so `(agent_id,
//!    version)` denotes one byte-string for all time. That is enforced by
//!    `publish_version_exclusively`, where `link(2)` both names the file and
//!    refuses to replace one, so two concurrent writers holding the same fence
//!    cannot both publish. MEASURED two ways, because this one is load-bearing
//!    for the other three: eight writers on one fence, 30 rounds, leave exactly
//!    one new version file; and 40 SIGKILL crash-and-restart cycles on one
//!    store publish 185 versions of which 0 later change their bytes. It is
//!    also why a crashed update's orphan version is ADOPTED and never
//!    reclaimed -- see `AgentStore::published_head`, where discarding it is
//!    refused precisely because it would put two byte-strings behind one
//!    `(agent_id, version)`.
//! 3. Resolution is a **pure function** run once at admission:
//!    [`resolve_agent_start`] takes `(AgentSpec, per-session request)` and
//!    returns a `StartSessionRequest` that everything downstream --
//!    `resolve_claude_launch`, `admit_bound_resources` -- cannot distinguish
//!    from one a caller typed inline. It reads no clock, no store and no daemon
//!    state.
//! 4. The resolved configuration's **digest is echoed** on the response
//!    ([`SessionAgentPin`]), so a caller can check what it actually launched
//!    rather than trust that resolution did what it said.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pseudomux_protocol::v1::{
    AgentContainment, AgentDescriptor, AgentEnvironmentSpec, AgentId, AgentList, AgentListFailure,
    AgentRef, AgentSpec, AgentSummary, AgentVersion, ClaudeLaunchConfig, ConfigIsolation,
    ConfigSource, EnvironmentSpec, ErrorBody, ErrorCode, SessionAgentPin, SessionCell,
    StartSessionRequest, TimestampMs, agent_supplied_start_paths,
};
use serde_json::json;
use sha2::{Digest, Sha256};

/// The mode every agent file is born with and kept at.
///
/// `0600` for the identical reason directories use `0700`: `umask` can only
/// CLEAR bits, and `0600` has no group or other bits to clear, so the file is
/// never observable at a wider mode and there is no create-then-chmod window a
/// local user can open a handle in.
pub const AGENT_FILE_MODE: u32 = 0o600;

/// Resolves one stored agent version into the start request a caller would have
/// typed inline.
///
/// **PURE.** It reads no clock, no store, no environment and no daemon state;
/// its output is a function of its two arguments and nothing else. That is what
/// keeps `docs/spec.md` Sec. 4.4 literally true rather than approximately true,
/// and `agent_resolution_is_a_pure_function_of_the_spec_and_the_session_fields`
/// pins it with an equality assertion against a hand-written inline request
/// rather than with a description.
///
/// `agent` is CLEARED on the way out, and the pin travels separately. The
/// resolved DTO is not "a start that named an agent"; it is the start that
/// agent means, and a request still carrying `agent` beside a `claude` would be
/// refused by the both-modes rule if anything re-decoded it.
///
/// The per-session request reaching here has already been refused if it carried
/// any path in [`agent_supplied_start_paths`], so every field this function
/// overwrites was at its type default and nothing a caller wrote is discarded.
///
/// **BOTH DOORS, NOT ONE.** That sentence used to rest on `Deserialize` alone,
/// which no in-process caller runs: `validate_v1_serializable` only serializes,
/// and the serializer checked five of the nine derived paths. An embedder
/// sending `cell: "minified"` beside a `full` agent reached here with a value
/// this function then overwrote in silence. The serializer now walks the same
/// derived list, so the claim holds for a Rust embedder as well as for a socket
/// caller.
#[must_use]
pub fn resolve_agent_start(
    spec: &AgentSpec,
    config_digest: &str,
    reference: AgentRef,
    request: StartSessionRequest,
) -> (StartSessionRequest, SessionAgentPin) {
    // Destructured without `..`: a field added to `AgentSpec` stops this
    // compiling until resolution says what it does with it.
    let AgentSpec {
        // The agent's own identity and its narrowing rules. Neither is a start
        // field: `name`/`description` are labels, and `containment` is enforced
        // by `admit_agent_containment` as an ADDITIONAL refusal rather than by
        // writing a value into the request.
        name: _,
        description: _,
        containment: _,
        claude,
        environment,
        auth_policy,
        terminal,
        lifecycle,
        retention,
        compatibility,
        cell,
    } = spec;
    let AgentEnvironmentSpec { set, unset } = environment;

    let StartSessionRequest {
        identity,
        cwd,
        config_isolation,
        // Replaced wholesale, and provably empty on arrival: see the doc above.
        claude: _,
        agent: _,
        environment: caller_environment,
        auth_policy: _,
        terminal: _,
        lifecycle: _,
        retention: _,
        compatibility: _,
        cell: _,
    } = request;

    let resolved = StartSessionRequest {
        identity,
        cwd,
        claude: Some(claude.clone()),
        // Not "the agent, resolved"; the start that agent MEANS.
        agent: None,
        environment: EnvironmentSpec {
            // The caller's snapshot survives untouched. It is the one launch
            // input an agent structurally cannot carry.
            snapshot: caller_environment.snapshot,
            set: set.clone(),
            unset: unset.clone(),
        },
        auth_policy: *auth_policy,
        config_isolation,
        terminal: terminal.clone(),
        lifecycle: lifecycle.clone(),
        retention: retention.clone(),
        compatibility: *compatibility,
        cell: *cell,
    };
    let pin = SessionAgentPin {
        agent_id: reference.agent_id,
        version: reference.version,
        config_digest: config_digest.to_owned(),
    };
    (resolved, pin)
}

/// Every containment rule an agent narrows a start with, applied as an
/// ADDITIONAL refusal.
///
/// **THE COMPOSITION DIRECTION IS THE WHOLE RULE.** This runs BEFORE
/// `admit_bound_resources`, which then runs unchanged, so there is no value of
/// any field here that makes an otherwise-refused start admissible. It can only
/// refuse more. `containment_can_only_refuse_more_never_admit_more` proves that
/// by taking a cwd the existing rules already refuse and asserting it stays
/// refused under every `workspace_root`, including one that contains it. It
/// lives in `native.rs`'s test module, because `admit_bound_resources` -- the
/// other half of the composition -- is private there and the composition is
/// only testable where both halves are visible.
///
/// # Errors
///
/// [`ErrorCode::InvalidConfig`] naming the rule that refused, the value that
/// broke it, and what would satisfy it.
pub fn admit_agent_containment(
    containment: &AgentContainment,
    agent_id: AgentId,
    cwd: &Path,
    config_isolation: Option<&ConfigIsolation>,
) -> Result<(), ErrorBody> {
    // Destructured without `..`: a rule added to `AgentContainment` stops this
    // compiling until it is enforced here.
    let AgentContainment {
        workspace_root,
        require_config_isolation,
    } = containment;

    if let Some(root) = workspace_root {
        let root = Path::new(root);
        // THE RESOLVING PREDICATE, and asymmetrically.
        //
        // `claude_launch::one_directory_contains_the_other` is symmetric, and
        // symmetry is WRONG here: with `workspace_root` at
        // `/Users/x/proj`, a cwd of `/Users/x` CONTAINS the root, so the
        // symmetric predicate would admit it -- and this field's own
        // documentation promises "every session's cwd must resolve INSIDE" the
        // root. A guard whose message promises more than its predicate tests is
        // the exact defect this design exists not to ship, so the direction is
        // named in the call.
        //
        // It is still the same walk, not a fresh `starts_with`: that comparison
        // is wrong under symlinks and under the `/tmp` -> `/private/tmp` rewrite
        // this host performs, and `directory_lies_within` resolves the
        // descendant and stat-identifies each ancestor exactly as the symmetric
        // form does.
        if !crate::claude_launch::directory_lies_within(root, cwd) {
            return Err(ErrorBody::new(
                ErrorCode::InvalidConfig,
                format!(
                    "agent {agent_id} bounds every session to workspace root {}, and cwd {} does \
                     not resolve inside it",
                    root.display(),
                    cwd.display()
                ),
            )
            .with_details(json!({
                "recommendation": format!(
                    "start with a --cwd inside {}, or use an agent whose containment.workspace_root covers it",
                    root.display()
                )
            })));
        }
    }

    if *require_config_isolation && config_isolation.is_none() {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            format!(
                "agent {agent_id} requires every session to name a pmux-owned Claude \
                 configuration root, and this start named none"
            ),
        )
        .with_details(json!({
            "recommendation": "pass --config-isolation-root DIR (an existing, owner-only 0700 \
                               directory the daemon owns); the agent deliberately does not name \
                               one, because an agent that named a root would make its id a \
                               contention key for every session started from it"
        })));
    }
    Ok(())
}

/// Everything an [`AgentSpec`] must satisfy to be STORED.
///
/// The bar is deliberately "a stored agent is one that can start". A spec that
/// every `start_session` would refuse is a configuration that can never launch,
/// and storing one only moves the refusal to a place the caller reads later.
///
/// The launch-policy checks are the SERVICE's own, called rather than restated,
/// so a rule that moves in `compatibility.rs` or `native.rs` moves here too.
///
/// # Errors
///
/// [`ErrorCode::InvalidConfig`] or [`ErrorCode::UnsupportedFeature`], with the
/// reason and the fix.
pub fn validate_agent_spec(spec: &AgentSpec) -> Result<(), ErrorBody> {
    // Destructured without `..`: a field added to `AgentSpec` stops this
    // compiling until its admission rule -- or its explicit absence of one --
    // is written down.
    let AgentSpec {
        name,
        description,
        claude,
        environment,
        auth_policy: _,
        terminal,
        lifecycle,
        retention,
        compatibility: _,
        cell,
        containment,
    } = spec;

    if name.trim().is_empty() {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            "agent name must not be empty: it is the label `pmux agent list` prints",
        ));
    }
    if name.len() > MAX_AGENT_LABEL_BYTES {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            format!("agent name must be at most {MAX_AGENT_LABEL_BYTES} bytes"),
        ));
    }
    if let Some(description) = description
        && description.len() > MAX_AGENT_DESCRIPTION_BYTES
    {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            format!("agent description must be at most {MAX_AGENT_DESCRIPTION_BYTES} bytes"),
        ));
    }

    // The SERVICE's own refusals, called and not restated. A `terminal` or a
    // `retention` a start would refuse is one this agent could never launch.
    crate::compatibility::validate_v1_terminal_support(terminal.profile, terminal.input_transport)?;
    crate::native::validate_public_start_retention(retention)?;
    if *lifecycle == (pseudomux_protocol::v1::LifecycleMode::Hybrid { hook_timeout_ms: 0 }) {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            "hybrid hook timeout must be greater than zero",
        ));
    }
    if terminal.rows == 0 || terminal.cols == 0 {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            "terminal rows and columns must be greater than zero",
        ));
    }

    validate_agent_claude(claude)?;
    validate_agent_environment(environment)?;

    if let Some(root) = &containment.workspace_root {
        if !Path::new(root).is_absolute() {
            return Err(ErrorBody::new(
                ErrorCode::InvalidConfig,
                format!("agent containment.workspace_root must be absolute, found {root:?}"),
            ));
        }
        if crate::claude_launch::traverses_a_parent_component(Path::new(root)) {
            return Err(ErrorBody::new(
                ErrorCode::InvalidConfig,
                format!(
                    "agent containment.workspace_root must be spelled without a `..` component, \
                     found {root:?}"
                ),
            ));
        }
    }

    // ACCEPTED-AND-IGNORED IS REFUSED HERE. The minified cell requires a
    // configuration root of its own, and an agent that said
    // `require_config_isolation: false` beside `cell: minified` would be
    // overridden by that requirement without ever being told.
    if *cell == SessionCell::Minified && !containment.require_config_isolation {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            "an agent with cell `minified` must also set containment.require_config_isolation: \
             every minified cell needs a Claude configuration root of its own, so \
             `require_config_isolation: false` is a value this agent could never honour",
        )
        .with_details(json!({
            "recommendation": "set containment.require_config_isolation to true, or set cell to \
                               `full`"
        })));
    }
    Ok(())
}

const MAX_AGENT_LABEL_BYTES: usize = 200;
const MAX_AGENT_DESCRIPTION_BYTES: usize = 4_000;

fn validate_agent_claude(claude: &ClaudeLaunchConfig) -> Result<(), ErrorBody> {
    if !Path::new(&claude.executable).is_absolute() {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            format!(
                "agent claude.executable must be an absolute path, found {:?}",
                claude.executable
            ),
        ));
    }
    // `extra_args` is held to the launch validator's closed allowlist rather
    // than to a second copy of it, so a driver-owned flag added there is
    // refused here on the same day.
    crate::claude_launch::validate_public_extra_args(&claude.extra_args)
        .map_err(|error| ErrorBody::new(ErrorCode::InvalidConfig, format!("{error:#}")))
}

/// Refuses every `environment.set` name through which an agent would NAME a
/// resource.
///
/// This is the central invariant applied to the one channel that bypasses the
/// launch allowlist. `environment.set["CLAUDE_CONFIG_DIR"] = <a directory>` is
/// a stored value that redirects where a session's configuration lives, and an
/// agent is by construction shared by N sessions -- so a stored config root is
/// exactly the contention key `AgentContainment::require_config_isolation`
/// exists to avoid. It is also, verbatim, the door through which a live
/// minified cell's root was once aliased.
///
/// THE TABLE IS THE SERVICE'S OWN. `claude_launch::CONFIG_ROOT_ENV_DOORS` is
/// the single list of these names, and a name added there is refused here with
/// no second edit. Unlike the minified-cell rule, this applies to EVERY cell:
/// the hazard is that an agent id becomes a name two sessions share, and that
/// is true of the full cell too.
fn validate_agent_environment(environment: &AgentEnvironmentSpec) -> Result<(), ErrorBody> {
    let AgentEnvironmentSpec { set, unset } = environment;
    for name in set.keys().chain(unset.iter()) {
        if name.is_empty() {
            return Err(ErrorBody::new(
                ErrorCode::InvalidConfig,
                "agent environment names must not be empty",
            ));
        }
        if name.contains('=') || name.contains('\0') {
            return Err(ErrorBody::new(
                ErrorCode::InvalidConfig,
                format!("agent environment name {name:?} may not contain `=` or NUL"),
            ));
        }
    }
    for value in set.values() {
        if value.contains('\0') {
            return Err(ErrorBody::new(
                ErrorCode::InvalidConfig,
                "an agent environment value may not contain NUL (the name is withheld with the \
                 value, because the text after `=` may be a credential)",
            ));
        }
    }
    if let Some(door) = crate::claude_launch::CONFIG_ROOT_ENV_DOORS
        .iter()
        .find(|door| set.contains_key(**door))
    {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            format!(
                "an agent may not set {door}: that name moves the child's Claude configuration \
                 root, which is a resource a session BINDS, and an agent may narrow what a \
                 session names but never name one on its behalf"
            ),
        )
        .with_details(json!({
            "recommendation": "set containment.require_config_isolation on the agent and pass \
                               --config-isolation-root DIR on each start, which names the root \
                               per session instead of once for every session at a time"
        })));
    }
    if let Some(key) = unset.iter().find(|key| set.contains_key(*key)) {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            format!("agent environment variable {key} is both set and unset; choose one"),
        ));
    }
    Ok(())
}

/// Lowercase hex SHA-256 over the canonical serialization of an UNREDACTED
/// spec.
///
/// This is identity, where [`AgentVersion`] is only order. It is computed over
/// the unredacted spec precisely so that it still distinguishes two agents that
/// differ only in a redacted environment value, while the frame that carries it
/// discloses neither value.
///
/// # Errors
///
/// The serializer's own error, which is also the numeric-domain preflight every
/// other v1 value goes through.
pub fn config_digest(spec: &AgentSpec) -> Result<String, serde_json::Error> {
    let bytes = serde_json::to_vec(spec)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

/// The digest of one opaque byte string, in the form the wire uses.
fn value_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

/// The spec as an inspection surface may see it.
///
/// Every environment VALUE and every inline settings/MCP document BODY is
/// replaced by `sha256:<hex>` of its bytes. This mirrors what `pmux probe`
/// already promises -- "Environment values, inline settings and MCP documents,
/// and the system prompt are never printed" -- and [`config_digest`] is why it
/// costs nothing: the digest is computed over the unredacted spec, so it still
/// identifies the configuration exactly while the frame discloses nothing.
///
/// **`system_prompt` is deliberately NOT redacted**, and that is a considered
/// divergence from `probe` rather than an oversight. `probe` redacts it because
/// `probe` prints to a terminal; an agent's system prompt is the single most
/// important thing about that agent, and an inspection surface that hides it is
/// useless. Stated here so nobody "fixes" it later.
///
/// A `ConfigSource::File` path survives too: a path is not a secret, and
/// `probe` prints paths for the same reason.
#[must_use]
pub fn redact_agent_spec(spec: &AgentSpec) -> AgentSpec {
    let mut redacted = spec.clone();
    redacted.environment.set = spec
        .environment
        .set
        .iter()
        .map(|(name, value)| (name.clone(), value_digest(value.as_bytes())))
        .collect::<BTreeMap<_, _>>();
    for source in redacted
        .claude
        .settings
        .iter_mut()
        .chain(redacted.claude.mcp_configs.iter_mut())
    {
        if let ConfigSource::Inline { document } = source {
            let digest = value_digest(
                serde_json::to_vec(document)
                    .unwrap_or_else(|_| b"<unserializable>".to_vec())
                    .as_slice(),
            );
            *document = serde_json::Value::String(digest);
        }
    }
    redacted
}

/// The persisted form of one immutable agent version.
///
/// [`AgentDescriptor`]'s fields with the spec UNREDACTED and STRICTLY TYPED.
/// Storing the descriptor's shape rather than the bare spec is what lets
/// `get_agent` answer `created_at_ms` for version N without reading version 1,
/// and what makes the digest a property of the stored bytes rather than
/// something recomputed by whoever happens to read them.
///
/// The spec is an `AgentSpec` here and a `serde_json::Value` on the wire, and
/// that asymmetry is deliberate: a FILE pmux wrote is one pmux must be able to
/// refuse if it does not understand it, while a RESPONSE must tolerate a field
/// a newer daemon added. See [`AgentDescriptor::spec`].
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredVersion {
    agent_id: AgentId,
    version: AgentVersion,
    config_digest: String,
    spec: AgentSpec,
    created_at_ms: TimestampMs,
    updated_at_ms: TimestampMs,
}

/// The versioned, owner-only store.
///
/// ```text
/// <root>/                     0700
///   <agent-id>/               0700   canonical hyphenated UUID
///     head                    0600   a LOWER BOUND on the newest version, one line
///     1.json                  0600   immutable
///     2.json                  0600   immutable
/// ```
///
/// `head` is deliberately NOT documented as "the current version": it is a hint
/// that saves a scan, and the newest published version is derived from the files
/// by `AgentStore::published_head`. See there for why, and for what a caller is
/// owed when a crash lands between the two writes.
///
/// Per-version files rather than one file with a history array, so a torn write
/// can only lose the newest version and can never corrupt one a caller pinned.
/// MEASURED: 40 SIGKILL crash-and-restart cycles on one store published 185
/// versions, of which 0 changed their bytes afterwards and 0 stopped reading.
#[derive(Clone, Debug)]
pub struct AgentStore {
    root: PathBuf,
}

impl AgentStore {
    /// Opens the store, creating every level owner-only from birth and REFUSING
    /// a directory that is not owner-only and owned by the caller.
    ///
    /// **THE SAME BAR AS THE SOCKET DIRECTORY AND THE PATH B POOL PARENT**, and
    /// deliberately not a lower one: this tree receives every stored
    /// environment value and every inline settings document. A tree pmux
    /// creates is created `0700` from birth via `create_private_dir_all`, which
    /// passes the mode to `mkdir(2)` so it is never observable at a wider mode;
    /// a tree the operator already made is REFUSED rather than silently
    /// re-permissioned, because a `chmod` nobody asked for is a worse surprise
    /// than a boot refusal, and re-permissioning would hide the very
    /// misconfiguration this check exists to report.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidConfig`] naming what was wrong AND what would be
    /// right.
    pub fn open(root: &Path) -> Result<Self, ErrorBody> {
        if !root.is_absolute() {
            return Err(ErrorBody::new(
                ErrorCode::InvalidConfig,
                format!("agent store {} must be an absolute path", root.display()),
            ));
        }
        crate::private_dir::create_private_dir_all(root).map_err(|error| {
            ErrorBody::new(
                ErrorCode::InvalidConfig,
                format!(
                    "agent store {}: could not create it: {error}",
                    root.display()
                ),
            )
        })?;
        let metadata = std::fs::symlink_metadata(root).map_err(|error| {
            ErrorBody::new(
                ErrorCode::InvalidConfig,
                format!(
                    "agent store {}: could not inspect it: {error}",
                    root.display()
                ),
            )
        })?;
        if !metadata.file_type().is_dir() {
            return Err(ErrorBody::new(
                ErrorCode::InvalidConfig,
                format!("agent store {} is not a directory", root.display()),
            ));
        }
        #[cfg(unix)]
        if let Some(reason) = crate::private_dir::owner_only_violation(&metadata, effective_uid()) {
            return Err(ErrorBody::new(
                ErrorCode::InvalidConfig,
                format!(
                    "agent store {} must be owner-only and owned by the daemon's user, the same \
                     bar the daemon's socket directory and the Path B pool parent are held to, \
                     because every stored environment value and inline settings document lives \
                     under it: {reason}",
                    root.display()
                ),
            )
            .with_details(json!({
                "recommendation": format!(
                    "pmuxd never re-permissions a directory it did not create; fix it with `chown {} {}` and `chmod 700 {}`, or point --agent-store at a path pmuxd may create itself",
                    effective_uid(),
                    root.display(),
                    root.display()
                )
            })));
        }
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn agent_dir(&self, agent_id: AgentId) -> PathBuf {
        // The path component is the canonical hyphenated UUID and NEVER
        // `spec.name`. A name is a caller-chosen wire string, and the moment
        // one becomes a path component, `..` is a directory traversal; minting
        // a UUID makes traversal unconstructible rather than filtered.
        self.root.join(agent_id.hyphenated().to_string())
    }

    fn version_file(&self, agent_id: AgentId, version: AgentVersion) -> PathBuf {
        self.agent_dir(agent_id).join(format!("{version}.json"))
    }

    fn head_file(&self, agent_id: AgentId) -> PathBuf {
        self.agent_dir(agent_id).join("head")
    }

    /// Stores one new agent at version 1 and returns it, REDACTED and RE-READ.
    ///
    /// **PUBLISHED WHOLE OR NOT AT ALL.** The agent is built under a staging
    /// name that is not a UUID -- so [`AgentStore::list`] cannot see it -- and
    /// becomes visible at its real name in one `rename(2)`. The first shape of
    /// this function created the agent directory and then wrote into it, which
    /// left a UUID directory with no `head` behind every interrupted create;
    /// that is a record `list` then had to read, and one unreadable record used
    /// to take the whole listing down.
    ///
    /// # Errors
    ///
    /// Any admission failure from [`validate_agent_spec`], or a filesystem
    /// failure, as [`ErrorCode::InvalidConfig`].
    pub fn create(
        &self,
        spec: AgentSpec,
        now_ms: TimestampMs,
    ) -> Result<AgentDescriptor, ErrorBody> {
        validate_agent_spec(&spec)?;
        let agent_id = AgentId::new_v4();
        let stored = StoredVersion {
            agent_id,
            version: AgentVersion::FIRST,
            config_digest: digest_or_refuse(&spec)?,
            spec,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        };
        // The id is minted, so this name is unique to this call and the rename
        // below can never find its destination occupied.
        let staging = self
            .root
            .join(format!(".pending-{}", agent_id.hyphenated()));
        crate::private_dir::create_private_dir_all(&staging).map_err(|error| {
            store_failure(&staging, "could not create the agent directory", &error)
        })?;
        let assembled = (|| {
            publish_version_exclusively(&staging.join("1.json"), &stored)?;
            replace_private_file(
                &staging.join("head"),
                format!("{}\n", stored.version).as_bytes(),
            )
        })();
        if let Err(error) = assembled {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
        std::fs::rename(&staging, self.agent_dir(agent_id)).map_err(|error| {
            let _ = std::fs::remove_dir_all(&staging);
            store_failure(
                &self.agent_dir(agent_id),
                "could not publish the agent directory",
                &error,
            )
        })?;
        sync_parent_directory(&self.agent_dir(agent_id));
        // RE-READ, never `redacted(stored)`: what this returns is what the
        // store holds, read back through every guard `get_agent` applies. See
        // [`AgentStore::update`] for the concurrency that made the difference
        // observable, and for why it is now defence in depth rather than the
        // fence.
        redacted(self.read_version(agent_id, AgentVersion::FIRST)?)
    }

    /// Reads one stored version, or the current head when `version` is `None`.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidConfig`] for an unknown agent or version, with the
    /// fix in `details.recommendation`.
    pub fn get(
        &self,
        agent_id: AgentId,
        version: Option<AgentVersion>,
    ) -> Result<AgentDescriptor, ErrorBody> {
        let version = match version {
            Some(version) => version,
            None => self.published_head(agent_id)?,
        };
        redacted(self.read_version(agent_id, version)?)
    }

    /// Every stored agent's head, as a summary, plus every record that could
    /// not be read.
    ///
    /// **ONE BAD RECORD LOSES ITSELF AND NOTHING ELSE.** This used to propagate
    /// `?` per entry, so a single unreadable agent answered the whole listing
    /// with that agent's refusal -- and `missing_agent` recommends this exact
    /// command, which made the recommendation unreachable in precisely the
    /// state it was offered. A record that cannot be read is reported by id, in
    /// [`AgentList::unreadable`], with the refusal `get_agent` would have given
    /// for it; it is never dropped, because a listing that silently omitted a
    /// record would be worse than one that failed.
    ///
    /// # Errors
    ///
    /// Only a failure of the STORE ITSELF, as [`ErrorCode::InvalidConfig`]: the
    /// root is unreadable, or its directory stream broke mid-walk. Neither is a
    /// property of any one record.
    pub fn list(&self) -> Result<AgentList, ErrorBody> {
        let mut agents = Vec::new();
        let mut unreadable = Vec::new();
        let entries = std::fs::read_dir(&self.root)
            .map_err(|error| store_failure(&self.root, "could not read the agent store", &error))?;
        for entry in entries {
            let entry = entry
                .map_err(|error| store_failure(&self.root, "could not read an entry", &error))?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            // Only entries this store could have minted. A UUID directory name
            // is what `create` publishes, so anything else -- including the
            // `.pending-` staging name a create is assembled under -- is not a
            // record and is not ours to interpret.
            let Ok(agent_id) = AgentId::parse_str(&name) else {
                continue;
            };
            if agent_id.hyphenated().to_string() != name {
                continue;
            }
            match self.summarize(agent_id) {
                Ok(summary) => agents.push(summary),
                Err(error) => unreadable.push(AgentListFailure {
                    agent_id,
                    reason: error.message,
                }),
            }
        }
        agents.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
        unreadable.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
        Ok(AgentList { agents, unreadable })
    }

    /// One stored agent's head as a summary, or the refusal that record earns.
    ///
    /// The head is [`AgentStore::published_head`]'s and never the raw pointer's,
    /// so a record whose newest published version the pointer has not reached is
    /// listed at the version `get_agent` and `update_agent` will both use, and a
    /// record whose newest published version is not readable is REPORTED rather
    /// than summarized at the older one behind it. MEASURED before that change,
    /// 15 of 40 SIGKILL trials left a store this reported as healthy at version
    /// N while N+1 was on disk and every future update was refused forever.
    fn summarize(&self, agent_id: AgentId) -> Result<AgentSummary, ErrorBody> {
        let head = self.published_head(agent_id)?;
        let stored = self.read_version(agent_id, head)?;
        Ok(AgentSummary {
            agent_id: stored.agent_id,
            version: stored.version,
            config_digest: stored.config_digest,
            name: stored.spec.name,
            description: stored.spec.description,
            cell: stored.spec.cell,
            updated_at_ms: stored.updated_at_ms,
        })
    }

    /// Mints a new immutable version and returns it, REDACTED and RE-READ.
    ///
    /// `expected_version` is a fence, not a routing key: any value that is not
    /// the current head is [`ErrorCode::IdConflict`], including one stale by
    /// exactly one revision, and nothing here is ever answered as "your update
    /// already landed". A caller that lost its response reads `get_agent` and
    /// compares `config_digest`, which costs one round trip and never a wrong
    /// answer.
    ///
    /// **THE HEAD THAT FENCE IS COMPARED AGAINST IS
    /// `AgentStore::published_head`'s**, which is a function of the durable
    /// files alone, so two consecutive attempts are compared against the same
    /// value and cannot be told the fence is stale in one direction and then
    /// stale in the other. MEASURED before that change, 15 of 40 SIGKILL trials
    /// produced exactly that pair, forever:
    ///
    /// ```text
    /// trial 20: head=2  published_max=3
    ///   retry@2 -> IdConflict: agent ef7f31ff-... is at version 3, not the expected version 2
    ///   retry@3 -> IdConflict: agent ef7f31ff-... is at version 2, not the expected version 3
    /// ```
    ///
    /// # THE FENCE THAT DECIDES IS THE `link(2)`, NOT THE COMPARISON
    ///
    /// The head comparison below is a courtesy: it produces the good message
    /// for the ordinary stale caller. It cannot be the fence, because there is
    /// no lock between reading `head` and writing the next version and
    /// `bin/pmuxd/src/handler.rs` serves 64 connections at once, so two callers
    /// holding the same `expected_version` both pass it. The decision is made
    /// by `publish_version_exclusively`, where naming the file and refusing
    /// to overwrite one are the SAME syscall; exactly one of the two racing
    /// writers can win it, and the loser is told which version now exists.
    ///
    /// MEASURED before that change, 25 rounds of two concurrent updates on one
    /// fence: 7 rounds left `head` pointing at a version that no longer parsed
    /// (`trailing characters at line 1 column 1497`, which also took
    /// `list_agents` down for the whole store), and 13 more answered the winner
    /// a `config_digest` the store did not hold.
    ///
    /// Resolving the head forward gave a loser a SECOND way to lose -- one that
    /// reads after the winner's `link(2)` now fails the comparison above, where
    /// before it would have reached `link(2)` and failed there -- so the claim
    /// is re-measured at the width the daemon serves rather than at two:
    /// `many_writers_on_one_fence_publish_exactly_one_version_and_never_two`
    /// races eight writers 30 rounds and asserts the agent directory holds
    /// exactly `1.json` and `2.json` afterwards. A third file would mean two
    /// writers each minted a number, which is the fork this ordering could have
    /// traded the wedge for.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::IdConflict`] for a stale fence or a lost race,
    /// [`ErrorCode::InvalidConfig`] for anything else.
    pub fn update(
        &self,
        agent_id: AgentId,
        expected_version: AgentVersion,
        spec: AgentSpec,
        now_ms: TimestampMs,
    ) -> Result<AgentDescriptor, ErrorBody> {
        validate_agent_spec(&spec)?;
        let head = self.published_head(agent_id)?;
        if head != expected_version {
            return Err(stale_fence(agent_id, head, expected_version));
        }
        let current = self.read_version(agent_id, head)?;
        let version = head.next();
        let stored = StoredVersion {
            agent_id,
            version,
            config_digest: digest_or_refuse(&spec)?,
            spec,
            created_at_ms: current.created_at_ms,
            updated_at_ms: now_ms,
        };
        publish_version_exclusively(&self.version_file(agent_id, version), &stored).map_err(
            |error| {
                if error.code != ErrorCode::IdConflict {
                    return error;
                }
                // The other writer won the syscall. It has already published a
                // complete version at this number, so the fence the caller
                // should have held is that one.
                stale_fence(agent_id, version, expected_version)
            },
        )?;
        // `head` moves only after the version it names is durable, which is
        // what keeps it a LOWER BOUND on the newest published version rather
        // than an independent claim about one; the reverse order would leave it
        // naming a file that does not exist. It is NOT a fence, and a crash
        // between the two lines is recovered by [`AgentStore::published_head`]
        // in whoever reads next, never by narrowing this window -- two files
        // cannot be written in one syscall.
        //
        // The sentence that used to sit here said such a crash "reads as 'the
        // update did not land' -- the safe direction". MEASURED FALSE, 15 of 40
        // SIGKILL trials: it read as "this agent can never be updated again",
        // because every later `update` recomputed the same `head.next()` and
        // every later `link(2)` refused it.
        self.advance_head(agent_id, version)?;
        // RE-READ, never `redacted(stored)`. The old shape built the caller's
        // descriptor out of what it SENT, and with an overwriting `rename` two
        // racing writers made that observably false: MEASURED, 13 of 25 rounds
        // answered a winner a `config_digest` the store did not hold. What this
        // buys now is weaker and worth stating plainly, so nobody reads it as a
        // tested claim: with publication exclusive, `stored` and the bytes on
        // disk are equal by construction, and deleting this re-read does NOT
        // redden any test. It stays because a descriptor that is a property of
        // the stored bytes cannot drift from one that is a property of the
        // arguments, and because it puts every read guard between the write and
        // the answer.
        redacted(self.read_version(agent_id, version)?)
    }

    /// Reads the exact stored version a start pinned, UNREDACTED, for
    /// resolution.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidConfig`] naming the missing agent or version, with
    /// the fix in `details.recommendation`.
    pub fn load_for_launch(&self, reference: AgentRef) -> Result<(AgentSpec, String), ErrorBody> {
        let stored = self.read_version(reference.agent_id, reference.version)?;
        Ok((stored.spec, stored.config_digest))
    }

    /// The per-agent directory, held to the store root's own bar on every READ.
    ///
    /// PER-READ, NOT ONLY AT BOOT, for the reason [`read_private_file`] is:
    /// this directory is created `0700` by `create`, but nothing stops an
    /// operator widening it between the boot that opened the store and the
    /// `start_session` that reads a version out of it. Only files used to be
    /// re-checked, which left the directory holding them unexamined.
    fn require_readable_agent_dir(&self, agent_id: AgentId) -> Result<(), ErrorBody> {
        let path = self.agent_dir(agent_id);
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(missing_agent(agent_id));
            }
            Err(error) => return Err(store_failure(&path, "could not inspect it", &error)),
        };
        if !metadata.file_type().is_dir() {
            return Err(ErrorBody::new(
                ErrorCode::InvalidConfig,
                format!(
                    "agent store {} is not a directory; a stored agent is a directory this store \
                     created",
                    path.display()
                ),
            ));
        }
        #[cfg(unix)]
        if let Some(reason) = crate::private_dir::owner_only_violation(&metadata, effective_uid()) {
            return Err(ErrorBody::new(
                ErrorCode::InvalidConfig,
                format!(
                    "agent store {} must be owner-only and owned by the daemon's user, the bar \
                     every level of this tree is held to, because it holds environment values and \
                     inline settings documents: {reason}",
                    path.display()
                ),
            )
            .with_details(json!({
                "recommendation": format!(
                    "pmuxd never re-permissions a directory it did not create; fix it with `chown {} {}` and `chmod 700 {}`",
                    effective_uid(),
                    path.display(),
                    path.display()
                )
            })));
        }
        Ok(())
    }

    /// The newest published version, DERIVED FROM THE FILES and never taken
    /// from the pointer alone.
    ///
    /// # `head` is a durable LOWER BOUND, not the answer
    ///
    /// Publishing a version and moving the pointer are two operations on two
    /// files and cannot be made one syscall, so a crash between them is not a
    /// window that can be narrowed away. It can only be RECOVERED from, and the
    /// recovery has to live in whoever reads next. [`AgentStore::advance_head`]
    /// runs only after the version it names is published and a published
    /// version file is never removed, so the pointer always names a file that
    /// exists and `head <= max published` holds -- including when a descheduled
    /// writer's late `advance_head` moves it BACKWARDS past versions two later
    /// writers already published, which is why this walks forward in a loop and
    /// not by one.
    ///
    /// # The published version is ADOPTED, and this is what a caller is owed
    ///
    /// A version published by an update that was killed before it moved the
    /// pointer is durable, immutable, and digest-verified on the way out of
    /// [`AgentStore::read_version`] -- a version in every sense except the
    /// pointer's -- so it is adopted, and the head a caller reads is it.
    ///
    /// The caller-visible consequence, stated plainly because it is a real one:
    /// **an update interrupted by a crash MAY have landed.** That is not a
    /// weakening of the fence, and it is not avoidable by any ordering -- the
    /// same crash one line later would have moved the pointer, and the response
    /// was never delivered either way. It is exactly the case [`AgentStore::update`]'s
    /// fence documentation already prescribes a recovery for: read `get_agent`
    /// and compare `config_digest`. Adoption is what makes that recovery
    /// TRUTHFUL. Without it the store answered "did not land" and then refused
    /// every subsequent update forever.
    ///
    /// Discarding it was the other defensible answer and is refused here for
    /// two reasons this store has already written down. [`missing_version`]
    /// recommends "a version is never removed, so a version number this store
    /// does not hold was never minted", and unlinking a published file makes
    /// that sentence false for a version a session may already have pinned. And
    /// no reader can distinguish a crashed writer's orphan from a live writer's
    /// version published microseconds ago, so "discard" would let a recovering
    /// reader delete a version another writer is about to point at -- after
    /// which two byte-strings could bear one `(agent_id, version)`, which is the
    /// property this module's header calls load-bearing.
    ///
    /// # The step predicate is `link(2)`'s, exactly
    ///
    /// See [`AgentStore::version_name_is_taken`]. Stepping over exactly the
    /// names `link(2)` refuses is what makes `update`'s target -- this value's
    /// `next()` -- a name that does not exist, so the wedge is
    /// unconstructible rather than recovered from.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidConfig`] for a record with no readable pointer at
    /// all; a name that exists but is not a readable version is NOT refused
    /// here, because refusing it here would answer `get_agent` with the pointer
    /// file's sentence instead of that version's own.
    fn published_head(&self, agent_id: AgentId) -> Result<AgentVersion, ErrorBody> {
        let mut version = self.read_head(agent_id)?;
        while self.version_name_is_taken(agent_id, version.next()) {
            version = version.next();
        }
        Ok(version)
    }

    /// Whether `link(2)` would refuse to publish at this version's name.
    ///
    /// **NOT "is a readable version file", and the difference is the whole
    /// point.** The question being asked is the one
    /// [`publish_version_exclusively`] is about to ask the kernel, and every
    /// narrower predicate -- a regular file, one that parses, one whose digest
    /// checks -- leaves [`AgentStore::update`] minting a number whose name is
    /// already taken and being refused `EEXIST` for it on every attempt for the
    /// rest of the store's life. A symlink at `2.json` is not a version and
    /// `link(2)` still refuses it, so it is taken.
    ///
    /// Only `NotFound` means free. A name that could not be stat'ed at all is
    /// treated as TAKEN, because a name this cannot read is not one it may
    /// assume is available; the record then earns its own refusal from
    /// [`AgentStore::read_version`] and is reported by [`AgentStore::list`],
    /// which is louder than stepping around it and answering with an older
    /// version nobody asked for.
    fn version_name_is_taken(&self, agent_id: AgentId, version: AgentVersion) -> bool {
        match std::fs::symlink_metadata(self.version_file(agent_id, version)) {
            Ok(_) => true,
            Err(error) => error.kind() != std::io::ErrorKind::NotFound,
        }
    }

    /// The RAW pointer, which is a lower bound and not the head. Every caller
    /// that wants the head wants [`AgentStore::published_head`].
    fn read_head(&self, agent_id: AgentId) -> Result<AgentVersion, ErrorBody> {
        self.require_readable_agent_dir(agent_id)?;
        let path = self.head_file(agent_id);
        let bytes = read_private_file(&path).map_err(|error| match error {
            // NOT `missing_agent`. `require_readable_agent_dir` above already
            // answered that for an agent that is not there at all, so reaching
            // here means the directory exists and its pointer does not -- a
            // record no `create` can leave any more, and one whose honest
            // description is "half-made", not "absent".
            PrivateReadError::Missing => ErrorBody::new(
                ErrorCode::InvalidConfig,
                format!(
                    "agent store {} has no head pointer; the agent directory exists but the \
                     version it names does not, so this record was never finished",
                    self.agent_dir(agent_id).display()
                ),
            )
            .with_details(json!({
                "recommendation": format!(
                    "remove {} and create the agent again; every agent pmux publishes is renamed into place complete, so a directory without a head predates that or was written by hand",
                    self.agent_dir(agent_id).display()
                )
            })),
            PrivateReadError::Refused(body) => body,
        })?;
        let text = String::from_utf8(bytes).map_err(|_| {
            ErrorBody::new(
                ErrorCode::InvalidConfig,
                format!(
                    "agent store {} does not hold a version number",
                    path.display()
                ),
            )
        })?;
        text.trim()
            .parse::<u64>()
            .ok()
            .and_then(|value| AgentVersion::new(value).ok())
            .ok_or_else(|| {
                ErrorBody::new(
                    ErrorCode::InvalidConfig,
                    format!(
                        "agent store {} does not hold a version number",
                        path.display()
                    ),
                )
            })
    }

    fn read_version(
        &self,
        agent_id: AgentId,
        version: AgentVersion,
    ) -> Result<StoredVersion, ErrorBody> {
        self.require_readable_agent_dir(agent_id)?;
        let path = self.version_file(agent_id, version);
        let bytes = read_private_file(&path).map_err(|error| match error {
            PrivateReadError::Missing => missing_version(agent_id, version),
            PrivateReadError::Refused(body) => body,
        })?;
        let stored: StoredVersion = serde_json::from_slice(&bytes).map_err(|error| {
            ErrorBody::new(
                ErrorCode::InvalidConfig,
                format!(
                    "agent store {} is not a readable agent version: {error}",
                    path.display()
                ),
            )
        })?;
        if stored.agent_id != agent_id || stored.version != version {
            return Err(ErrorBody::new(
                ErrorCode::InvalidConfig,
                format!(
                    "agent store {} names agent {} version {}, not agent {agent_id} version \
                     {version}",
                    path.display(),
                    stored.agent_id,
                    stored.version
                ),
            ));
        }
        let recomputed = digest_or_refuse(&stored.spec)?;
        if recomputed != stored.config_digest {
            return Err(ErrorBody::new(
                ErrorCode::InvalidConfig,
                format!(
                    "agent store {} records config_digest {} for a configuration whose digest is \
                     {recomputed}; the file was changed after pmux wrote it",
                    path.display(),
                    stored.config_digest
                ),
            ));
        }
        Ok(stored)
    }

    /// Moves the head pointer onto a version that is already published.
    ///
    /// `head` is the only MUTABLE file in the store, so this is the only place
    /// a rename is allowed to replace something.
    ///
    /// It writes an ABSOLUTE value and deliberately does not first check that
    /// the value is larger than what is there. That check would be a
    /// check-then-act with no lock behind it -- the shape this module removed
    /// from `publish_version_exclusively` -- and would read as a guarantee the
    /// pointer never regresses, which it could not deliver. A writer descheduled
    /// between its `link` and this line can land it after two later writers
    /// have moved the pointer past it, and the pointer regresses. That is
    /// harmless BY DESIGN rather than by luck: it stays a lower bound, which is
    /// the only thing [`AgentStore::published_head`] asks of it.
    fn advance_head(&self, agent_id: AgentId, version: AgentVersion) -> Result<(), ErrorBody> {
        replace_private_file(&self.head_file(agent_id), format!("{version}\n").as_bytes())
    }
}

/// One stored version as a caller reads it: redacted, and with the spec carried
/// as the document it is.
///
/// # Errors
///
/// The serializer's own failure, which is also the numeric-domain preflight
/// every other v1 value goes through.
fn redacted(stored: StoredVersion) -> Result<AgentDescriptor, ErrorBody> {
    let spec = serde_json::to_value(redact_agent_spec(&stored.spec)).map_err(|error| {
        ErrorBody::new(
            ErrorCode::InvalidConfig,
            format!("agent configuration is not representable on the wire: {error}"),
        )
    })?;
    Ok(AgentDescriptor {
        agent_id: stored.agent_id,
        version: stored.version,
        config_digest: stored.config_digest,
        spec,
        created_at_ms: stored.created_at_ms,
        updated_at_ms: stored.updated_at_ms,
    })
}

fn digest_or_refuse(spec: &AgentSpec) -> Result<String, ErrorBody> {
    config_digest(spec).map_err(|error| {
        ErrorBody::new(
            ErrorCode::InvalidConfig,
            format!("agent configuration is not serializable: {error}"),
        )
    })
}

fn missing_agent(agent_id: AgentId) -> ErrorBody {
    // NO NEW ERROR CODE. Both shipped clients hard-reject an unknown
    // `ErrorCode`, so adding one is a three-language lockstep release; and
    // "your launch configuration references something that does not exist" is
    // honestly an invalid configuration. The actionable half goes to
    // `details.recommendation`, which is the channel `pmux` renders.
    ErrorBody::new(ErrorCode::InvalidConfig, format!("no agent {agent_id}"))
        .with_details(json!({"recommendation": "list the stored agents with `pmux agent list`"}))
}

fn missing_version(agent_id: AgentId, version: AgentVersion) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::InvalidConfig,
        format!("agent {agent_id} has no version {version}"),
    )
    .with_details(json!({
        "recommendation": format!(
            "read the current version with `pmux agent get {agent_id}`; a version is never removed, so a version number this store does not hold was never minted"
        )
    }))
}

fn store_failure(path: &Path, what: &str, error: &std::io::Error) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::InvalidConfig,
        format!("agent store {}: {what}: {error}", path.display()),
    )
}

/// The refusal a caller earns for a stale fence, or for losing the publication
/// race to another writer holding the same one.
///
/// One function so both say the same sentence: from the caller's side the two
/// are the same event, and the answer to both is "re-read and re-apply".
fn stale_fence(agent_id: AgentId, head: AgentVersion, expected_version: AgentVersion) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::IdConflict,
        format!(
            "agent {agent_id} is at version {head}, not the expected version {expected_version}"
        ),
    )
    .with_details(json!({
        "recommendation": format!(
            "read the current configuration with `pmux agent get {agent_id}`, re-apply your edit to it, and update with --expected-version {head}"
        )
    }))
}

/// A monotonic tiebreaker, so no two temporary names in one process collide.
static TEMPORARY_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A scratch name **no other writer can guess or share**.
///
/// The name used to be `path.with_extension("json.tmp")` -- a pure function of
/// the DESTINATION -- so two writers of the same `(agent_id, version)` opened
/// one file, and `truncate(true)` let the second cut the first's bytes off
/// mid-write. MEASURED, that produced a `2.json` whose tail was another
/// writer's: `trailing characters at line 1 column 1497`.
fn unique_temporary(path: &Path) -> PathBuf {
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos());
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".tmp.{}.{sequence}.{nanos}", std::process::id()));
    path.with_file_name(name)
}

/// Creates one file owner-only from birth, durably, and REFUSES to open an
/// existing one.
///
/// `0600` is passed to `open(2)` rather than chmod'd afterwards for the
/// identical reason `create_private_dir_all` passes `0700` to `mkdir(2)`:
/// `umask` can only CLEAR bits, so the file is never observable at a wider mode
/// and there is no create-then-chmod window.
///
/// `create_new(true)` is what makes the creation EXCLUSIVE, and it is a syscall
/// flag rather than a preceding `path.exists()` check, so there is no window
/// between the question and the answer.
///
/// "Durably" is meant at full strength on the platform this ships on and is
/// checked rather than assumed: rustc 1.88.0's
/// `library/std/src/sys/fs/unix.rs:1212` issues `fcntl(F_FULLFSYNC)` for
/// `sync_all` on Apple targets, not `fsync(2)`, so the bytes are past the
/// drive's own cache before this returns and before [`publish_version_exclusively`]
/// gives the inode its real name. What is NOT at full strength is
/// [`sync_parent_directory`], whose failure is discarded on purpose.
fn create_new_private_file(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    use std::io::Write;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(AGENT_FILE_MODE);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Publishes one immutable version file: fully written before it has its name,
/// and never over an existing one.
///
/// TWO PROPERTIES, AND THEY NEED TWO DIFFERENT MECHANISMS.
///
/// * **Whole.** The bytes are written and `fsync`ed under [`unique_temporary`],
///   a name no other writer shares, so no reader can ever observe a partial
///   `<version>.json` and no second writer can truncate this one's content.
/// * **Exclusive.** `link(2)` gives the finished inode its real name and fails
///   with `EEXIST` rather than replacing what is there. Naming the file and
///   refusing to overwrite one are the SAME syscall, which is why two
///   concurrent updates holding one fence cannot both win. `rename(2)` -- what
///   this used to do -- silently overwrites, and the `path.exists()` guard in
///   front of it was a check-then-act with the whole write in the window.
///
/// # Errors
///
/// [`ErrorCode::IdConflict`] when the version already exists, which is the
/// caller-visible half of a lost race; [`ErrorCode::InvalidConfig`] for a
/// serialization or filesystem failure.
fn publish_version_exclusively(path: &Path, stored: &StoredVersion) -> Result<(), ErrorBody> {
    let bytes = serde_json::to_vec(stored).map_err(|error| {
        ErrorBody::new(
            ErrorCode::InvalidConfig,
            format!("agent version is not serializable: {error}"),
        )
    })?;
    let temporary = unique_temporary(path);
    create_new_private_file(&temporary, &bytes)
        .map_err(|error| store_failure(&temporary, "could not write it", &error))?;
    let published = std::fs::hard_link(&temporary, path);
    // The inode now has its real name; this one was only ever scaffolding.
    let _ = std::fs::remove_file(&temporary);
    match published {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(ErrorBody::new(
                ErrorCode::IdConflict,
                format!(
                    "agent store {} already exists; a stored version is immutable and is never \
                     rewritten",
                    path.display()
                ),
            ));
        }
        Err(error) => return Err(store_failure(path, "could not publish it", &error)),
    }
    sync_parent_directory(path);
    Ok(())
}

/// Writes the one MUTABLE file in the store, replacing what is there.
///
/// Only `head` may go through here, and only after the version it names is
/// published. The temporary name is unique for the same reason
/// [`publish_version_exclusively`]'s is; the `rename(2)` is allowed to replace
/// because replacing is exactly what advancing a pointer means.
fn replace_private_file(path: &Path, bytes: &[u8]) -> Result<(), ErrorBody> {
    let temporary = unique_temporary(path);
    create_new_private_file(&temporary, bytes)
        .map_err(|error| store_failure(&temporary, "could not write it", &error))?;
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(store_failure(path, "could not publish it", &error));
    }
    sync_parent_directory(path);
    Ok(())
}

/// A best-effort directory flush: the publication is already atomic, and a
/// platform that refuses to fsync a directory is not a reason to fail a write
/// that landed.
///
/// **"Best-effort" is the accurate word and the discarded `Result` is the whole
/// of it**, so nobody reads the durability of the version bytes -- which is a
/// real `F_FULLFSYNC` barrier, see [`create_new_private_file`] -- as covering
/// the directory entry that names them. MEASURED on this host, the read-only
/// directory handle opened here does accept `F_FULLFSYNC`; what is not claimed
/// is that it always will, or that anything notices when it does not. Against
/// PROCESS death, which is what the store's crash harness kills with, the
/// directory entry is already visible to every later reader and this call is
/// irrelevant; against power loss it is the one link in the chain without a
/// checked barrier.
fn sync_parent_directory(path: &Path) {
    if let Some(parent) = path.parent()
        && let Ok(directory) = std::fs::File::open(parent)
    {
        let _ = directory.sync_all();
    }
}

/// Why a stored file could not be read.
///
/// `Missing` is kept separate from every other refusal because the two callers
/// turn it into different sentences -- "no agent" and "that agent has no
/// version N" -- and both are actionable in a way "could not open it" is not.
enum PrivateReadError {
    Missing,
    Refused(ErrorBody),
}

/// Reads one stored file, checking the mode and the owner OF THE BYTES IT
/// RETURNS.
///
/// **THE GUARD AND THE READ ARE ONE OPEN FILE.** The old shape called
/// `symlink_metadata` and then `std::fs::read`, which follow different things:
/// `symlink_metadata` describes the LINK and `read` follows it. A symlink's own
/// mode is `umask`-dependent -- under `umask 077` it is born `0700` -- so
/// MEASURED, a `1.json` that was a symlink to a `0666` file outside the store
/// passed the guard and was launched, while under `umask 022` the same file was
/// refused for the wrong reason ("has mode 755", which was the link's).
///
/// Three things close it: `O_NOFOLLOW` refuses a symlink at `open(2)`, the
/// metadata comes from `fstat` on the open handle, and the handle is what is
/// read -- so nothing can be substituted between the check and the read either.
/// `is_file()` is required because a directory or a device at this name is not
/// a stored agent version whatever its mode says.
fn read_private_file(path: &Path) -> Result<Vec<u8>, PrivateReadError> {
    use std::io::Read;

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(PrivateReadError::Missing);
        }
        #[cfg(unix)]
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            return Err(PrivateReadError::Refused(
                ErrorBody::new(
                    ErrorCode::InvalidConfig,
                    format!(
                        "agent file {} is a symbolic link; a stored agent file is a regular file \
                         this store wrote, and a link's own mode says nothing about what it points \
                         at",
                        path.display()
                    ),
                )
                .with_details(json!({
                    "recommendation": format!(
                        "replace {} with the regular file it should be; pmuxd never follows a link out of its own store",
                        path.display()
                    )
                })),
            ));
        }
        Err(error) => {
            return Err(PrivateReadError::Refused(store_failure(
                path,
                "could not read it",
                &error,
            )));
        }
    };
    let metadata = file.metadata().map_err(|error| {
        PrivateReadError::Refused(store_failure(path, "could not inspect it", &error))
    })?;
    require_owner_only_open_file(path, &metadata).map_err(PrivateReadError::Refused)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|error| {
        PrivateReadError::Refused(store_failure(path, "could not read it", &error))
    })?;
    Ok(bytes)
}

/// Refuses anything that is not a regular file owned by the daemon's user with
/// nothing granted to group or other.
///
/// The metadata is the OPEN HANDLE's: see [`read_private_file`].
#[cfg(unix)]
fn require_owner_only_open_file(
    path: &Path,
    metadata: &std::fs::Metadata,
) -> Result<(), ErrorBody> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    if !metadata.file_type().is_file() {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            format!(
                "agent file {} is not a regular file; a stored agent file is one this store wrote",
                path.display()
            ),
        ));
    }
    if metadata.uid() != effective_uid() {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            format!(
                "agent file {} is owned by uid {}, not the daemon's user",
                path.display(),
                metadata.uid()
            ),
        )
        .with_details(json!({
            "recommendation": format!("fix it with `chown {} {}`", effective_uid(), path.display())
        })));
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            format!(
                "agent file {} has mode {mode:o}, which is readable beyond its owner; it holds \
                 environment values and inline settings documents",
                path.display()
            ),
        )
        .with_details(json!({
            "recommendation": format!(
                "pmuxd never re-permissions a file it did not create; fix it with `chmod 600 {}`",
                path.display()
            )
        })));
    }
    Ok(())
}

#[cfg(not(unix))]
fn require_owner_only_open_file(
    _path: &Path,
    _metadata: &std::fs::Metadata,
) -> Result<(), ErrorBody> {
    Ok(())
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn effective_uid() -> u32 {
    // SAFETY: `geteuid` has no preconditions and does not dereference pointers.
    unsafe { libc::geteuid() }
}

/// Every start path a stored agent supplies, re-exported so callers that must
/// name one in a message read the protocol's list and never a copy.
#[must_use]
pub fn supplied_start_paths() -> &'static [&'static str] {
    agent_supplied_start_paths()
}
