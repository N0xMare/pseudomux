//! Validation and deterministic argv/environment construction for interactive Claude.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use pseudomux_protocol::v1::{
    AuthPolicy, ClaudeLaunchConfig, ConfigIsolation, ConfigSource, EffortLevel, EnvironmentSpec,
    InputTransport, PermissionMode, SessionCell, SessionId, SessionIdentity, StartSessionRequest,
    SystemPromptPolicy, TerminalProfile,
};
use pseudomux_rmux::{EnvironmentSnapshot, LaunchSpec};

// ---------------------------------------------------------------------------
// The launch-environment policy itself lives in
// `pseudomux_protocol::v1::launch_environment`, and this module is its one
// enforcement point.
//
// This used to live here so `pmux probe` and the client's `require_env` check
// could both predict the same answer without contacting a daemon. Three
// hand-kept copies used to satisfy that need, pinned only by source-text-parsing
// drift fences; there is now one definition and no fence to keep honest. The
// remaining reader of this module is the daemon launch path.
//
// `inherits` is imported under this module's own vocabulary
// (`inherited_from_snapshot`), which `docs/spec.md` §4 names as the
// snapshot → allowlist → set/unset fold.
use pseudomux_protocol::v1::launch_environment::{
    SUBSCRIPTION_AUTH_KEYS, inherits as inherited_from_snapshot, transparent_profile_removes,
};

const FORBIDDEN_DRIVER_FLAGS: &[&str] = &[
    "-p",
    "--print",
    "--bg",
    "--background",
    "--session-id",
    "--resume",
    "--continue",
    "--output-format",
    "--input-format",
    "--teammate-mode",
    // Inert in the TUI today -- its own help says "only works with --print" --
    // and forbidden precisely because of that. A flag that does nothing now has
    // no caller depending on it, and if a future release ever honoured it
    // interactively it would stop writing the JSONL transcript, which is the
    // sole semantic authority for turn completion. The failure would not be a
    // slow turn; it would be a session that can never complete one.
    "--no-session-persistence",
];

const SAFE_EXTRA_FLAGS: &[&str] = &["--debug", "--verbose"];

/// The one Claude permission control that is a self-contained flag instead of a
/// `--permission-mode` value.
const DANGEROUSLY_SKIP_PERMISSIONS_FLAG: &str = "--dangerously-skip-permissions";

/// Every `environment.set` name a caller can use to move the child's Claude
/// configuration root, refused outright for `cell: minified`.
///
/// Not a filter and not an allowlist of values: the whole point is that the
/// DOOR is deleted for Path B, so there is no spelling left to get wrong. See
/// the long note at the one use site in `validate_config_isolation` for why
/// each name is here and why `HOME` is refused as a REQUEST key rather than
/// bound as a resource.
pub const CONFIG_ROOT_ENV_DOORS: &[&str] = &[
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_SECURESTORAGE_CONFIG_DIR",
    "HOME",
    "USERPROFILE",
    "XDG_CONFIG_HOME",
];

/// Environment pmux delivers to a minified cell's child, applied at step 7.
///
/// MEASURED, not reasoned. Every private configuration root pmux seeds
/// downloads the official plugin marketplace from GCS on first launch -- 428
/// files, 6.2 MB, 39 plugin directories, 31 `SKILL.md` files and 8+ third-party
/// `.mcp.json` -- starting 11 s after launch and finishing 53 s before the
/// cell's first turn. A cell whose whole claim is that it carries nothing from
/// the caller before it cannot also carry a third-party plugin tree it did not
/// ask for, and the download is a network dependency sitting inside the
/// readiness window.
///
/// `CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL` is the only name in
/// this table, and it is here because it was measured to suppress the download
/// with the cell still passing. THE FOUR NAMES DELIBERATELY ABSENT are the
/// reason this is a table and not a prefix:
/// `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC`, `DISABLE_TELEMETRY`,
/// `DO_NOT_TRACK` and `CLAUDE_CODE_SAFE_MODE` were each MEASURED to BREAK the
/// cell 5/5 by rendering a persistent notice that changes the screen shape and
/// fails startup. Suppressing traffic is not the goal; delivering an instance
/// nothing distinguishes from any other is, and a notice is a distinguishing
/// mark.
///
/// Applied AFTER the terminal profile's denylist for the same reason step 6 is:
/// every name here is `CLAUDE_CODE_*`, and a future `CLAUDE_` prefix entry in
/// [`TRANSPARENT_PREFIXES`] would otherwise silently strip it and quietly
/// restore the download. Applied only for `cell: minified`, because an ordinary
/// caller's plugins are the caller's business.
///
/// [`TRANSPARENT_PREFIXES`]: pseudomux_protocol::v1::launch_environment::TRANSPARENT_PREFIXES
const MINIFIED_CELL_ENVIRONMENT: &[(&str, &str)] =
    &[("CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL", "1")];

/// Argv pmux appends to a minified cell's launch and to no other cell's.
///
/// Driver-owned: no caller supplies these, no caller can suppress them, and
/// `SAFE_EXTRA_FLAGS` cannot express them. (Code spans, not intra-doc links:
/// this constant is `pub` and both of the items it names are private, which
/// `rustdoc -D warnings` refuses. Gate A's `rustdoc` cell was red from `20bf20f`
/// until this was fixed, and neither `cargo test` nor `cargo clippy` can see
/// it -- only `cargo doc` runs the lint.)
///
/// # `--strict-mcp-config`
///
/// MEASURED, and the measurement is why it is here rather than in a document
/// that says it is here. One minified cell, pristine private root, empty
/// `.claude.json`, Claude Code 2.1.226 on macos/aarch64, one variable moved:
///
/// | argv | MCP lines in the child's `--debug-file` log |
/// | --- | --- |
/// | as shipped before this constant | **6** -- `[claudeai-mcp] Fetching from `<!-- -->`https://api.anthropic.com/v1/mcp_servers?limit=1000`, `[mcp-registry] Loaded 294 official MCP URLs`, `MCP configs resolved in 33ms` |
/// | the same, plus `--strict-mcp-config` | **2** -- `Loading MCP configs...`, `MCP configs resolved in 0ms` |
///
/// Both cells reached `state: ready`. So the flag removes an outbound HTTP call
/// to the OPERATOR'S OWN ACCOUNT connector list and costs the cell nothing.
///
/// `docs/path-b.md` §2.2 retracted it as "NO LONGER LOAD-BEARING" on the ground
/// that *"no MCP server process is spawned in any configuration"*. That
/// measurement was a descendant-process inventory and it was correct; a remote
/// connector is an HTTP ENDPOINT and spawns no process, so the inventory was
/// structurally incapable of observing the case the retraction was about. The
/// predicate could not test what the sentence promised, which is why the flag
/// is back and why its evidence is a log line rather than a process table.
///
/// # `--safe-mode` IS DELIBERATELY NOT HERE
///
/// Three sites used to say it was shipped. It was never emitted, and it is not
/// being added now:
///
/// * Nothing it closes is measured open. Its stated job is "no CLAUDE.md,
///   skills, plugins, hooks", and `docs/2.1.226-compatibility.md` §4.2 measured
///   user-scope skill discovery landing on the PRIVATE ROOT, with the
///   operator's 77 `smithers-*` skills absent from the menu. The private root
///   and `--disallowedTools "*"` already deliver it.
/// * Its blast radius is uncalibrated. `claude --help` at 2.1.226 says it also
///   disables "custom themes, keybindings, and more" -- i.e. it moves the TUI's
///   own rendering configuration, and every screen constant Path B's fast path
///   trusts (`driver_io.rs`'s composer geometry, the post-`/clear` preamble,
///   the local-command menu's foreground-only selection) was measured WITHOUT
///   it. `docs/path-b.md` §13 item 3 probed the flag for `ready` and for one
///   answered token, 5/5; it did not probe one `/clear`.
/// * `CLAUDE_CODE_SAFE_MODE`, the environment variable, was MEASURED to BREAK
///   the cell 5/5 (see `MINIFIED_CELL_ENVIRONMENT`). This bullet used to say
///   the flag and the variable "are not interchangeable", and 2.1.226's help
///   says the flag *"Sets `CLAUDE_CODE_SAFE_MODE=1`"* in as many words. Both
///   measurements still stand; what they establish is narrower than the
///   sentence was: the variable is FATAL when pmux puts it in the child's
///   launch environment and inert when the child sets it for itself after argv
///   parsing. The pair is close enough that adding the flag on an argument
///   rather than a measurement is the move this file exists to refuse.
///
/// So the repair for `--safe-mode` was to stop claiming it, and the repair for
/// `--strict-mcp-config` was to start passing it. `minified_launch_flags`
/// is what stops the two from drifting apart again.
pub const MINIFIED_CELL_FLAGS: &[&str] = &["--strict-mcp-config"];

/// Every option token a minified cell's argv carries, in argv order, for the
/// launch `request` describes.
///
/// DERIVED, by performing the same three steps `NativeService::start_session_owned_with_retention`
/// performs -- materialize the sensitive files, resolve the launch, apply the
/// private pathnames -- and keeping the `--` tokens. There is no second list
/// here to keep honest, which is the whole point: three prose sites describe
/// this bundle, all three were wrong about it at once, and a paragraph cannot
/// be compared to argv unless argv is available as data.
///
/// `--system-prompt-file` is why the sensitive step is included rather than
/// skipped: it is appended by `SensitiveLaunchFiles::apply_to` and never by
/// `build_args`, so a derivation that stopped at [`resolve_claude_launch`]
/// would under-report the bundle by the one flag Path B cannot run without.
///
/// # Errors
///
/// Whatever the launch pipeline refuses `request` for.
#[cfg(test)]
pub(crate) fn minified_launch_flags(
    runtime_dir: &Path,
    request: &StartSessionRequest,
) -> Result<Vec<String>> {
    ensure!(
        request.cell == SessionCell::Minified,
        "the minified launch bundle is only defined for cell: minified"
    );
    let mut request = request.clone();
    // The same write-back `start_session_owned` performs, and for the same
    // reason: the pipeline resolves the request twice, and a caller-less
    // `New { session_id: None }` would otherwise mint two different UUIDs.
    let session_id = select_session_id(&request)?;
    if matches!(request.identity, SessionIdentity::New { .. }) {
        request.identity = SessionIdentity::New {
            session_id: Some(session_id),
        };
    }
    let sensitive = crate::sensitive_launch::SensitiveLaunchFiles::prepare(
        runtime_dir,
        session_id,
        request
            .claude
            .as_mut()
            .context("a minified launch carries an inline Claude configuration")?,
    )?;
    let mut resolved = resolve_claude_launch(&request)?;
    sensitive.apply_to(&mut resolved.process);
    Ok(resolved
        .process
        .args
        .into_iter()
        .filter(|token| token.starts_with("--"))
        .collect())
}

/// Validated process launch plus diagnostics safe to expose to callers.
#[derive(Clone, Debug)]
pub struct ResolvedClaudeLaunch {
    pub session_id: SessionId,
    pub resume: bool,
    pub process: LaunchSpec,
    pub removed_environment_keys: BTreeSet<String>,
    /// True when argv carries `--dangerously-skip-permissions`. Sessions
    /// launched this way warn on every turn.
    pub dangerous_permission_bypass: bool,
}

/// Validates driver-owned launch policy and chooses one stable session UUID.
///
/// Callers must write the returned UUID back into `SessionIdentity::New` before
/// performing any preparation that resolves the request a second time.
pub fn select_session_id(request: &StartSessionRequest) -> Result<SessionId> {
    validate_start(request)?;
    Ok(match request.identity {
        SessionIdentity::New { session_id } => session_id.unwrap_or_else(SessionId::new_v4),
        SessionIdentity::Resume { session_id } => session_id,
    })
}

/// Resolve a new/resumed UUID and build the only argv accepted by the native driver.
pub fn resolve_claude_launch(request: &StartSessionRequest) -> Result<ResolvedClaudeLaunch> {
    validate_start(request)?;
    // A start that names `agent` is refused on the public wire; this function
    // launches only an inline `claude` configuration. The refusal rather than
    // an `expect` is because `resolve_claude_launch` is `pub` and a direct
    // embedder can call it with any DTO it can construct.
    let claude = request
        .claude
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("start request carries no inline launch configuration"))?;
    let (session_id, resume) = match request.identity {
        SessionIdentity::New { session_id } => {
            (session_id.unwrap_or_else(SessionId::new_v4), false)
        }
        SessionIdentity::Resume { session_id } => (session_id, true),
    };
    let cwd = canonical_absolute(Path::new(&request.cwd), "cwd", RequiredPathKind::Directory)?;
    let executable = canonical_absolute(
        Path::new(&claude.executable),
        "Claude executable",
        RequiredPathKind::ExecutableFile,
    )?;
    let args = build_args(session_id, resume, claude, request.cell)?;
    let (environment, removed_environment_keys) = build_environment(
        &request.environment,
        request.auth_policy,
        request.terminal.profile,
        request.config_isolation.as_ref(),
        request.cell,
    )?;

    let dangerous_permission_bypass = matches!(
        claude.permission_mode,
        Some(PermissionMode::DangerouslySkipPermissions)
    );

    Ok(ResolvedClaudeLaunch {
        session_id,
        resume,
        process: LaunchSpec {
            executable,
            args,
            cwd,
            environment,
        },
        removed_environment_keys,
        dangerous_permission_bypass,
    })
}

fn validate_start(request: &StartSessionRequest) -> Result<()> {
    if request.terminal.rows == 0 || request.terminal.cols == 0 {
        bail!("terminal rows and columns must be non-zero");
    }
    if matches!(
        request.terminal.input_transport,
        InputTransport::AttachedStream
    ) {
        bail!("attached-stream prompt injection is not enabled by the validated v1 profile");
    }
    // Environment patches are applied as `snapshot - unset + set`. Reject the
    // history-suppression marker only when it survives that exact ordering: a
    // complete caller snapshot may safely carry an ambient value when the
    // request explicitly removes it.
    let skip_history_is_effective = request
        .environment
        .set
        .contains_key("CLAUDE_CODE_SKIP_PROMPT_HISTORY")
        || (request
            .environment
            .snapshot
            .contains_key("CLAUDE_CODE_SKIP_PROMPT_HISTORY")
            && !request
                .environment
                .unset
                .contains("CLAUDE_CODE_SKIP_PROMPT_HISTORY"));
    if skip_history_is_effective {
        bail!("CLAUDE_CODE_SKIP_PROMPT_HISTORY is incompatible with transcript authority");
    }
    if let Some(claude) = &request.claude {
        validate_extra_args(&claude.extra_args)?;
    }
    validate_config_isolation(request)?;
    Ok(())
}

/// The config root a request would resolve to with no isolation applied.
///
/// Mirrors `native.rs::effective_config_root`, but reads the *pre-allowlist*
/// view through [`patched_value`] rather than the delivered map, because that
/// is the view the pin has to reproduce and because step 6 has by then already
/// overwritten the delivered one.
fn pre_isolation_config_root(spec: &EnvironmentSpec) -> Option<PathBuf> {
    patched_value(spec, "CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| patched_value(spec, "HOME").map(|home| Path::new(home).join(".claude")))
}

/// Admission rules for a private configuration root.
///
/// Every one of these refuses rather than repairing. The root is a directory
/// the caller named and pmux is about to write a trust table into; the only
/// safe response to "this is not the shape I was promised" is to not write.
fn validate_config_isolation(request: &StartSessionRequest) -> Result<()> {
    let Some(isolation) = &request.config_isolation else {
        // A minified cell without a private root is a contradiction, not a
        // degraded mode. Its claim is that after `/clear` nothing distinguishes
        // one instance from another; under a shared root every caller's prompts
        // accumulate in one `history.jsonl`, every large pasted prompt in one
        // `paste-cache/`, and every abandoned transcript under one `projects/`.
        // Refused here, on the request alone, before any filesystem work.
        ensure!(
            request.cell != SessionCell::Minified,
            "a minified cell requires config_isolation: without a private configuration root its transcripts, prompt history and paste cache accumulate in the caller's own root"
        );
        return Ok(());
    };
    if isolation.root.is_empty() || isolation.root.contains('\0') {
        bail!("config isolation root must be non-empty and contain no NUL");
    }

    // THE SECOND DOOR, closed for Path B.
    //
    // A minified cell already has a first-class way to name its configuration
    // root: `config_isolation`, whose root is canonicalized (so it must exist,
    // and no alias, trailing slash or `..` survives), owner-checked,
    // shadow-checked and pristine-checked. Letting the same caller ALSO reach
    // the root through `environment.set["CLAUDE_CONFIG_DIR"]` is a redundant
    // second entrance, and it is the one every leak in this family has come
    // through: the plain env value is the only spelling of this directory that
    // nothing canonicalizes.
    //
    // Refused outright rather than filtered. A filter is a rule about a
    // spelling and has now been outlived six times; refusing the door deletes
    // the whole spelling surface for Path B at once.
    //
    // Stated on the CELL and BEFORE the isolation-conflict loop below, so it
    // does not inherit its force from that rule -- which is about isolation
    // rather than about the cell, applies to every cell equally, and could be
    // relaxed for an unrelated reason. It sits after the missing-root gate only
    // because a minified cell with no `config_isolation` at all is already
    // refused there, on a rule that says the same thing more directly.
    //
    // PATH A KEEPS THE DOOR as an internal launch-path control. A full-cell
    // (`cell: full`) `environment.set` of those names can still point one
    // internal start at a different root; pmux seeds nothing there. Public
    // `start_session` is refused. There is no `pmux probe`, `pmux start`, or
    // `--config-isolation-root` on the product CLI. Such an internal start is
    // still admitted against every live session's resources by
    // `native::admit_bound_resources`, on the inode, exactly like any other
    // spelling. Nothing in the tree -- no test, no client, no CLI path --
    // combines `cell: minified` with any name in [`CONFIG_ROOT_ENV_DOORS`].
    //
    // LEAK 9, AND WHY THE LIST GREW BY THREE NAMES.
    //
    // `native::effective_config_root` reads `CLAUDE_CONFIG_DIR` **else**
    // `HOME/.claude`. That `else` is the whole leak: admission sees `HOME` only
    // when `HOME` is the SOURCE of the delivered root. A minified cell always
    // has a `config_isolation` root, so `build_environment`'s step 6 always
    // supplies `CLAUDE_CONFIG_DIR` -- and therefore admission NEVER examines a
    // minified cell's `HOME` at all. MEASURED over the real socket: a start
    // carrying both names was ADMITTED, including the row whose `HOME` named a
    // live minified cell's own private root exactly.
    //
    // BINDING `HOME` AS A RESOURCE IS NOT THE FIX, and this is the one place the
    // "guard the resource" rule is deliberately not applied. A cell's private
    // root is operator-chosen with no default (`bin/pmux/src/cli.rs`), so
    // `~/.pmux/cells/N` is the ordinary place to put one -- which makes the
    // operator's `$HOME` an ANCESTOR of nearly every live cell's root. A
    // containment rule on `HOME` would therefore refuse nearly every ordinary
    // start, in exchange for a reach pmux cannot remove anyway: it does not
    // sandbox the filesystem, and any session's Bash tool can already read any
    // absolute path. `docs/path-b.md` records that decision.
    //
    // The structural close is the one that already worked for
    // `CLAUDE_CONFIG_DIR`: delete the door instead of deciding what is behind
    // it. All three names are here because all three are ways to say "the
    // child's idea of home", and a rule that names only the one measured
    // spelling is the recurring defect this file exists to stop:
    //
    // * `HOME` is the measured door -- `effective_config_root` appends
    //   `.claude` to it, and Claude's own bootstrap writes `$HOME/.claude.as_ref().expect("inline launch").json`.
    // * `USERPROFILE` is the same name on Windows and is what Claude's own
    //   private-root recipe sets alongside `HOME`
    //   (`.context/review/pathb-config-root-spec.md`).
    // * `XDG_CONFIG_HOME` is included even though it was MEASURED not to be a
    //   configuration-root door on 2.1.220 (`docs/path-b.md`: with `HOME` and
    //   `XDG_CONFIG_HOME` both redirected, Claude wrote under `$HOME` and
    //   nothing at all under `$XDG_CONFIG_HOME`). It costs a minified caller
    //   nothing -- a minified cell has no use for it, since its root comes from
    //   `config_isolation` -- and it closes the door before the release that
    //   makes it one, which is exactly how the four `CLAUDE*` markers in
    //   `TRANSPARENT_EXACT_KEYS` each arrived one live failure too late.
    //
    // THE MESSAGE PROMISES ONLY WHAT THE PREDICATE TESTS. It says these names
    // may not appear in `environment.set` for this cell. It does NOT claim the
    // child cannot reach the operator's home directory, because the predicate
    // does not test that and pmux cannot deliver it.
    if request.cell == SessionCell::Minified {
        for key in CONFIG_ROOT_ENV_DOORS {
            if request.environment.set.contains_key(*key) {
                bail!(
                    "a minified cell may not set {key} in environment.set; config_isolation is the supported way to give a minified cell its own configuration root, and it is the only one pmux canonicalizes, owner-checks and pristine-checks"
                );
            }
        }
    }

    // The conflict rules run before any filesystem work: they are about the
    // request alone, and a caller who stated an intent pmux cannot honour
    // should hear that rather than a story about permissions. Silently
    // discarding an explicit instruction is worse than refusing it -- the same
    // principle `validate_environment` already applies to team markers.
    //
    // `snapshot` and `unset` are deliberately NOT conflicts. The snapshot is
    // ambient rather than asked for, and its `CLAUDE_CONFIG_DIR` is the *input*
    // to the securestorage pin; refusing it would make isolation unusable for
    // exactly the operators who already run under a custom config root, which
    // is the population that needs the pin most.
    for key in ["CLAUDE_CONFIG_DIR", "CLAUDE_SECURESTORAGE_CONFIG_DIR"] {
        if request.environment.set.contains_key(key) {
            bail!("config isolation and an explicit {key} are mutually exclusive");
        }
    }

    let root = canonical_absolute(
        Path::new(&isolation.root),
        "config isolation root",
        RequiredPathKind::Directory,
    )?;
    require_owner_only_directory(&root, "config isolation root")?;
    crate::config_isolation::refuse_shadowed_config_file(&root)?;

    // A "private" root that is really the caller's own root would put pmux's
    // trust-table writer on the machine-wide file, which is the single hazard
    // this feature exists to remove.
    //
    // Keyed on the resource rather than on the spelling for the same reason
    // `native::admit_bound_resources` is: a `config_isolation.root` naming
    // `/System/Volumes/Data<HOME>/.claude` is the operator's own root under the
    // firmlink alias, and a comparison of canonicalized strings says it is not.
    if let Some(inherited) = pre_isolation_config_root(&request.environment)
        && must_treat_as_same_directory(&root, &inherited)
    {
        bail!(
            "config isolation root is, or cannot be told apart from, the configuration root this request would have used anyway: {}",
            root.display()
        );
    }

    // A config root inside the workspace makes the cell's own transcripts
    // visible to its file tools; a workspace inside the config root puts caller
    // files under a directory pmux writes.
    let cwd = canonical_absolute(Path::new(&request.cwd), "cwd", RequiredPathKind::Directory)?;
    if one_directory_contains_the_other(&root, &cwd) {
        bail!(
            "config isolation root and cwd may not contain one another: {} vs {}",
            root.display(),
            cwd.display()
        );
    }
    Ok(())
}

/// Whether either of two directories is, or lies beneath, the other.
///
/// This replaces `root.starts_with(cwd) || cwd.starts_with(root)`, which was
/// the same defect class as LEAK 5 on the same two resources: a decision about
/// directories, taken by comparing path PREFIXES.
///
/// `Path::starts_with` at least compares whole components, so it was never
/// open to the bare name-prefix collision a `str::starts_with` would have had
/// (`/x/rootB` does not start with `/x/root` component-wise). It was open to
/// the alias family: both sides here are `Path::canonicalize`d, and MEASURED on
/// macOS `canonicalize` does not collapse the APFS firmlink namespace, so a
/// cwd of `/System/Volumes/Data/private/tmp/W` and a root of
/// `/private/tmp/W/inner` are the SAME containment the rule exists to refuse
/// and neither is a component prefix of the other. Every spelling in the leak-5
/// family defeats it the same way.
///
/// Decided on the resource instead. Containment is an ancestry question rather
/// than an identity one, so it is asked as one: walk the candidate descendant's
/// own ancestors and ask [`must_treat_as_same_directory`] about each. Every
/// ancestor of an existing directory exists, so each of those questions is
/// answered by the kernel on `(st_dev, st_ino)` and no alias survives it.
///
/// Both directions, because the rule refuses both: a config root under the
/// workspace and a workspace under the config root are different hazards and
/// neither is admissible.
///
/// TWO DIRECTIONS ALSO MEANS EVERY PATH IS RESOLVED. [`contains_or_is`] resolves
/// its DESCENDANT and stats its ANCESTOR, so calling it both ways round means
/// each of these two paths is resolved exactly once and stat-identified exactly
/// once. That is what covers a CLAIM whose stored spelling is itself a symlink
/// -- and claims are stored verbatim: `TranscriptLocator` canonicalizes the cwd
/// it is handed but keeps the configuration root as it was given, and the plain
/// `environment.set["CLAUDE_CONFIG_DIR"]` door hands over the caller's own
/// spelling. Callers therefore do not have to canonicalize before asking.
///
/// LEAK 7 IS WHY THIS IS `pub(crate)`. For six leaks this predicate existed,
/// was correct, and was asked about exactly one thing: the root and the cwd of
/// the SAME request. `native::admit_bound_resources` -- the rule that decides
/// what a start may bind against directories a LIVE minified cell already holds
/// -- asked [`must_treat_as_same_directory`] instead, which is an IDENTITY
/// question, so `R/sub` was not `R` and eight measured shapes were admitted
/// straight into a live cell's private root. There is deliberately no second
/// implementation of the walk: both callers ask this one, so a fix to the
/// ancestry rule cannot be applied to one rule and forgotten in the other.
///
/// TERMINATION, AND WHAT IT COST BEFORE LEAK 8. The walk is lexical --
/// [`Path::ancestors`] strictly shortens the path by one component each step and
/// stops at the root, so nothing on disk can make it loop. That property was
/// kept, but it was previously bought by walking the SPELLING, and a lexical
/// walk of a spelling is a walk of the wrong directory's ancestors the moment
/// any component of it is a symlink. `stat` resolves each prefix, so a symlink
/// in the MIDDLE of the path was seen as the directory it points at -- but the
/// walk then continued to that prefix's LEXICAL parents, never the target's
/// real ones. MEASURED over the socket against a live minified cell: a plain
/// `environment.set["CLAUDE_CONFIG_DIR"]` naming a symlink to the cell's own
/// `projects/` was ADMITTED, and the intruder's transcript landed physically
/// inside the cell's root. The same hole through `HOME`.
///
/// The walk therefore runs over [`path_the_child_will_reach`] -- the longest
/// prefix that exists, CANONICALIZED, with whatever does not exist yet appended
/// -- and stays lexical from there. Termination is unchanged: canonicalization
/// is the kernel's own, bounded by `ELOOP`, and the walk that follows shortens
/// by one component per step exactly as before.
///
/// WHAT THIS WALK SEES, AND WHAT IT DOES NOT:
///
/// * It sees every alias of every real ancestor of the directory the child will
///   be launched into or will create, on `(st_dev, st_ino)`.
/// * It does NOT see the future. Every answer here is about the filesystem as
///   it is at admission; a component swapped for a symlink between this
///   question and the child's first write is not covered, and cannot be without
///   pmux holding a descriptor on the directory it hands over. That was equally
///   true of every earlier form of this rule.
/// * It does NOT interpret `..`, and does not try to. Where the kernel can
///   resolve the whole path, `canonicalize` has already answered and no `..`
///   survives; where it cannot, [`path_the_child_will_reach`] stops at the
///   first `..` it would have to strip and hands the walk the spelling
///   UNCHANGED, which is exactly the pre-leak-8 behaviour for that one family.
///   Collapsing `..` lexically is not the kernel's rule, so guessing would be
///   wrong in the direction that leaks. Nothing reaches here with one anyway:
///   `native::effective_config_root` refuses a configuration root spelled with
///   `..`, `require_establishable_identity` refuses a `..` path the kernel
///   calls absent, and every cwd on both sides is canonicalized before it
///   arrives. See [`traverses_a_parent_component`].
/// * It does NOT answer for a directory in another mount namespace, or one the
///   child would reach with different privileges than the daemon has.
///
/// The three filesystem hazards are answered by [`DirectoryIdentity::of`]
/// rather than by the loop: a symlink cycle (`ELOOP`), an unreadable ancestor
/// (`EACCES`) and a name past `PATH_MAX` (`ENAMETOOLONG`) all become
/// [`DirectoryIdentity::Unresolved`], for which [`must_treat_as_same_directory`]
/// answers "treat as the same" -- so every one of them is REPORTED AS
/// CONTAINMENT and refuses the start. `path_the_child_will_reach` cannot
/// weaken that: it only ever rewrites a prefix the kernel successfully
/// resolved, and the unresolvable remainder is carried through unchanged, so
/// the first element of the walk still fails to `stat` and still fails closed.
/// Refusing is the only safe direction: a wrong "contained" costs one refusal,
/// a wrong "disjoint" costs the leak.
pub(crate) fn one_directory_contains_the_other(left: &Path, right: &Path) -> bool {
    contains_or_is(left, right) || contains_or_is(right, left)
}

/// Whether `candidate` IS `root`, or lies beneath it, on the resource.
///
/// **THE DIRECTED FORM, and it exists because the symmetric one is the wrong
/// answer to a bound.** `AgentContainment::workspace_root` promises "every
/// session's cwd must resolve INSIDE this directory". Asked with
/// `one_directory_contains_the_other`, a `workspace_root` of
/// `/Users/x/proj` would ADMIT a cwd of `/Users/x`, because the cwd contains
/// the root -- a guard whose message promises more than its predicate tests,
/// which is precisely the defect the agent resource was built not to ship.
///
/// It is the same walk, not a fresh `starts_with`. `contains_or_is` resolves
/// the descendant with `path_the_child_will_reach` and stat-identifies each
/// of its ancestors with `must_treat_as_same_directory`, which follows
/// symlinks and answers on `(st_dev, st_ino)` -- so the APFS firmlink spelling,
/// the `/tmp` -> `/private/tmp` rewrite, and every other alias in the leak-5
/// family are answered by the kernel rather than by a path comparison.
///
/// Both paths are still resolved, so this is not the reach-past
/// `contains_or_is`'s own doc warns about: leak 8 was getting one direction
/// resolved and not the other in a rule that had to be asked BOTH ways.
/// Containment by a bound is a directed question, and answering it in one
/// direction is the answer, not half of one.
#[must_use]
pub fn directory_lies_within(root: &Path, candidate: &Path) -> bool {
    contains_or_is(root, candidate)
}

/// The public admission for caller-supplied Claude arguments.
///
/// Exposed so a [`pseudomux_protocol::v1::AgentSpec`]-shaped configuration is
/// held to the launcher's own closed allowlist rather than to a second copy of
/// it: a driver-owned flag added to `FORBIDDEN_DRIVER_FLAGS` is refused on the
/// same day, with no second edit.
///
/// # Errors
///
/// The launcher's own refusal, verbatim.
pub fn validate_public_extra_args(args: &[String]) -> Result<()> {
    validate_extra_args(args)
}

/// Whether `descendant` is `ancestor`, or lies beneath it, on the resource.
///
/// Asymmetric in how the two paths are handled, and deliberately so.
/// `descendant` is WALKED, so it must first be turned into the path whose
/// lexical parents really are its parents; `ancestor` is only ever `stat`ed by
/// [`must_treat_as_same_directory`], which follows symlinks already, so
/// resolving it here would be a second syscall for an answer the kernel is
/// about to give anyway.
///
/// Private, and not merely unexported: the resolution above is a PRECONDITION
/// of the walk being about the right directory, and a caller reaching past
/// [`one_directory_contains_the_other`] would be reintroducing leak 8 by
/// getting one direction resolved and not the other.
fn contains_or_is(ancestor: &Path, descendant: &Path) -> bool {
    path_the_child_will_reach(descendant)
        .ancestors()
        .any(|prefix| must_treat_as_same_directory(prefix, ancestor))
}

/// The path a spelling really lands on, once everything missing from it exists.
///
/// The longest prefix the kernel can resolve, CANONICALIZED, with the
/// components that do not exist yet appended verbatim. Both halves are needed:
///
/// * The canonical prefix is what makes the walk above true. A canonical path
///   contains no symlink, so its lexical parents ARE its real parents -- which
///   is exactly the property the spelling the caller sent does not have, and
///   the whole of leak 8.
/// * The missing tail is appended rather than dropped because a configuration
///   root that does not exist yet is the ORDINARY shape of a first start, and
///   because `mkdir -p` -- what pmux and Claude's own bootstrap do to it --
///   creates those components as ordinary directories under the resolved
///   prefix. So from the canonical prefix down, the lexical chain is the chain
///   the child will really stand in.
///
/// TERMINATION. [`Path::ancestors`] shortens by one component per step and
/// stops, and `canonicalize` is the operating system's own resolution, bounded
/// by `ELOOP`. A cycle costs a bounded failure per prefix, never a loop here.
///
/// When NOTHING resolves -- an unreadable `/`, a name too long at every prefix,
/// a `..` the walk-up refuses to strip -- the spelling is returned unchanged.
/// That is not a guess: the caller's next act is to `stat` it, which fails,
/// which is [`DirectoryIdentity::Unresolved`], which is reported as containment
/// and refuses the start. Returning the spelling is therefore never WEAKER than
/// resolving it; it is the pre-leak-8 answer, kept for the cases where there is
/// no honest resolution to give.
fn path_the_child_will_reach(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    for prefix in path.ancestors() {
        if let Ok(resolved) = prefix.canonicalize() {
            let mut reached = resolved;
            reached.extend(missing.iter().rev());
            return reached;
        }
        match prefix.file_name() {
            Some(name) => missing.push(name.to_owned()),
            // `Path::file_name` answers `None` for the root, for a relative
            // path that has run out of components, AND for a path whose last
            // component is `..`. Stopping is right for all three: there is
            // nothing left to strip in the first two, and in the third the
            // component pmux would have to strip is the one whose meaning
            // depends on what the component before it turns out to be.
            None => break,
        }
    }
    path.to_path_buf()
}

/// Refuses a directory pmux is about to write secrets-adjacent state into
/// unless it is owned by this process and readable by nobody else.
///
/// pmux verifies and refuses rather than `chmod`ing. `sensitive_launch.rs`
/// relabels directories it created itself; this one is the caller's, and
/// relaxing someone else's directory into compliance would hide the very
/// misconfiguration the check exists to report.
#[cfg(unix)]
fn require_owner_only_directory(path: &Path, label: &str) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let metadata = path
        .metadata()
        .with_context(|| format!("failed to inspect {label}: {}", path.display()))?;
    #[allow(unsafe_code)]
    // SAFETY: `geteuid` takes no arguments, touches no caller memory, and is
    // documented as always succeeding.
    let euid = unsafe { libc::geteuid() };
    ensure!(
        metadata.uid() == euid,
        "{label} must be owned by the daemon's effective uid: {}",
        path.display()
    );
    let mode = metadata.permissions().mode() & 0o7777;
    ensure!(
        mode == 0o700,
        "{label} must have mode 0700, found {mode:04o}: {}",
        path.display()
    );
    Ok(())
}

#[cfg(not(unix))]
fn require_owner_only_directory(_path: &Path, _label: &str) -> Result<()> {
    Ok(())
}

fn validate_extra_args(args: &[String]) -> Result<()> {
    for arg in args {
        if arg.contains('\0') {
            bail!("Claude extra arguments may not contain NUL");
        }
        if FORBIDDEN_DRIVER_FLAGS
            .iter()
            .any(|flag| arg == flag || arg.starts_with(&format!("{flag}=")))
        {
            bail!("driver-owned or non-interactive Claude flag is forbidden: {arg}");
        }
        if !SAFE_EXTRA_FLAGS.contains(&arg.as_str()) {
            bail!("Claude extra argument is not in the v1 allowlist: {arg}");
        }
    }
    Ok(())
}

fn build_args(
    session_id: SessionId,
    resume: bool,
    config: &ClaudeLaunchConfig,
    cell: SessionCell,
) -> Result<Vec<String>> {
    let mut args = Vec::new();
    if resume {
        args.extend(["--resume".into(), session_id.to_string()]);
    } else {
        args.extend(["--session-id".into(), session_id.to_string()]);
    }
    if let Some(model) = &config.model {
        push_value(&mut args, "--model", model)?;
    }
    if let Some(effort) = config.effort {
        args.extend(["--effort".into(), effort_name(effort).into()]);
    }
    if let Some(permission) = config.permission_mode {
        match permission_mode_argv(permission) {
            PermissionModeArgv::Pair(name) => {
                args.extend(["--permission-mode".into(), name.into()]);
            }
            PermissionModeArgv::Single(flag) => args.push(flag.to_owned()),
        }
    }
    for tool in &config.allowed_tools {
        push_value(&mut args, "--allowedTools", tool)?;
    }
    for tool in &config.denied_tools {
        push_value(&mut args, "--disallowedTools", tool)?;
    }
    for settings in &config.settings {
        push_config_source(&mut args, "--settings", settings)?;
    }
    for mcp in &config.mcp_configs {
        push_config_source(&mut args, "--mcp-config", mcp)?;
    }
    for plugin in &config.plugin_dirs {
        let path = canonical_absolute(
            Path::new(plugin),
            "plugin directory",
            RequiredPathKind::Directory,
        )?;
        push_value(
            &mut args,
            "--plugin-dir",
            canonical_utf8(&path, "plugin directory")?,
        )?;
    }
    match &config.system_prompt {
        SystemPromptPolicy::Default => {}
        SystemPromptPolicy::Append { .. } | SystemPromptPolicy::Replace { .. } => {
            bail!("system prompt text must be materialized into an owner-only file before launch")
        }
    }
    args.extend(config.extra_args.iter().cloned());
    // LAST, and after the caller's own extra arguments, so that what the cell
    // is cannot be changed by what the caller asked for. `validate_extra_args`
    // already makes that unreachable -- [`SAFE_EXTRA_FLAGS`] is two spellings
    // and neither is one of these -- and the ordering costs nothing, so it is
    // stated in argv rather than left resting on the allowlist alone.
    if cell == SessionCell::Minified {
        args.extend(MINIFIED_CELL_FLAGS.iter().map(|flag| (*flag).to_owned()));
    }
    Ok(args)
}

fn push_config_source(args: &mut Vec<String>, flag: &str, source: &ConfigSource) -> Result<()> {
    match source {
        ConfigSource::File { path } => {
            let path = canonical_absolute(Path::new(path), flag, RequiredPathKind::RegularFile)?;
            push_value(args, flag, canonical_utf8(&path, flag)?)
        }
        ConfigSource::Inline { .. } => {
            bail!("inline {flag} config must be materialized into an owner-only file before launch")
        }
    }
}

fn push_value(args: &mut Vec<String>, flag: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.contains('\0') || value.starts_with('-') {
        bail!("{flag} value must be non-empty, contain no NUL, and not resemble an option");
    }
    args.extend([flag.to_owned(), value.to_owned()]);
    Ok(())
}

/// Build the exact replacement environment for the launched Claude process.
///
/// The order is fixed and pinned by
/// `documented_environment_order_is_allowlist_then_unset_then_set_then_removals_then_isolation`:
///
/// ```text
/// effective = allowlist(snapshot) - unset + set - policy_removals + profile_changes
///             + config_isolation
/// ```
///
/// 1. **allowlist(snapshot)** — every inherited name not admitted by
///    [`inherited_from_snapshot`] is dropped and reported. This is the primary
///    defense: unknown means denied.
/// 2. **- unset**, 3. **+ set** — the documented caller patch, unchanged.
///    `set` bypasses the allowlist entirely; it is the explicit channel.
/// 4. **- policy_removals** — [`SUBSCRIPTION_AUTH_KEYS`] under
///    [`AuthPolicy::Subscription`]. Belt and braces: a name that is both
///    allowed and explicitly forbidden is still removed, and a name restored
///    through `set` is still stripped.
/// 5. **profile_changes** — the transparent profile's denylist
///    ([`transparent_profile_removes`]), the tmux-shim `PATH` prune, and
///    `TERM=xterm-256color`.
/// 6. **+ config_isolation** — `CLAUDE_CONFIG_DIR` is replaced by the private
///    root and `CLAUDE_SECURESTORAGE_CONFIG_DIR` is pinned to the root the
///    request would have resolved *without* isolation, read through
///    [`patched_value`] from the pre-allowlist view — the same sentence pattern
///    step 5 already uses for `TMUX_PROGRAM`.
/// 7. **+ cell** — [`MINIFIED_CELL_ENVIRONMENT`] for `cell: minified` only.
///
/// `removed` describes the delivered environment: it carries every name the
/// caller offered that is absent from the result, whether the allowlist, the
/// denylist, or the auth policy dropped it. The two reasons are not
/// distinguished, because doing so would change the public
/// `ResolvedClaudeLaunch::removed_environment_keys` type; completeness is an
/// internal launch invariant, asserted by tests, not a public probe surface.
fn build_environment(
    spec: &EnvironmentSpec,
    auth_policy: AuthPolicy,
    terminal_profile: TerminalProfile,
    config_isolation: Option<&ConfigIsolation>,
    cell: SessionCell,
) -> Result<(EnvironmentSnapshot, BTreeSet<String>)> {
    validate_environment(spec)?;

    let mut removed = BTreeSet::new();

    // 1. The inherited snapshot, filtered by the allowlist.
    let mut variables = BTreeMap::new();
    for (key, value) in &spec.snapshot {
        if inherited_from_snapshot(key, auth_policy) {
            variables.insert(key.clone(), value.clone());
        } else {
            removed.insert(key.clone());
        }
    }

    // 2/3. The documented caller patch. `set` is not filtered.
    for key in &spec.unset {
        variables.remove(key);
    }
    variables.extend(spec.set.clone());

    // 4. Auth-policy removals.
    if auth_policy == AuthPolicy::Subscription {
        for key in SUBSCRIPTION_AUTH_KEYS {
            if variables.remove(*key).is_some() {
                removed.insert((*key).to_owned());
            }
        }
    }

    // 5. Terminal-profile removals and changes.
    match terminal_profile {
        TerminalProfile::Transparent => {
            // The shim directory is read from the pre-allowlist value: the
            // profile deletes `TMUX_PROGRAM` and the allowlist never admits it,
            // but the directory it names must still leave `PATH`.
            remove_tmux_shim_from_path(&mut variables, patched_value(spec, "TMUX_PROGRAM"))?;
            variables.retain(|key, _| {
                let strip = transparent_profile_removes(key);
                if strip {
                    removed.insert(key.clone());
                }
                !strip
            });
            variables.insert("TERM".into(), "xterm-256color".into());
        }
        TerminalProfile::RmuxStandard => {
            bail!("rmux-standard terminal identity has not passed the Phase 0 release gate");
        }
    }

    // 6. Config isolation. Last, and deliberately after the profile denylist.
    //
    // After `set` so a caller patch cannot win -- belt and braces, since
    // `validate_config_isolation` already refuses the collision. After the
    // denylist because that list has acquired a `CLAUDE*` name after each of
    // four live failures, and a future `CLAUDE_` prefix entry would otherwise
    // silently strip the pin and turn every isolated session into a login
    // screen. Running last makes that class of regression unrepresentable.
    //
    // The two values are treated differently on purpose. The ROOT is delivered
    // canonicalized, because it must name the same directory pmux seeds and the
    // transcript locator walks. The PIN is delivered byte-for-byte, because
    // Claude hashes it (`sha256(NFC(value))[0..8]`) to name a keychain item and
    // the isolated child must land on exactly the item the operator's own
    // un-isolated session uses. Normalizing or canonicalizing the pin would
    // hash to a different service name and produce "Not logged in".
    //
    // An ABSENT pre-isolation root pins the empty string, which is a
    // first-class value to Claude and not an accident: its own env filter reads
    // `if (r === "" && t !== "CLAUDE_SECURESTORAGE_CONFIG_DIR") continue;`, i.e.
    // every other name drops an empty value and this one is preserved. Empty
    // selects the default, unsuffixed credential store -- which is exactly what
    // a caller with no `CLAUDE_CONFIG_DIR` would have used.
    if let Some(isolation) = config_isolation {
        let root = canonical_absolute(
            Path::new(&isolation.root),
            "config isolation root",
            RequiredPathKind::Directory,
        )?;
        let root = canonical_utf8(&root, "config isolation root")?.to_owned();
        let pin = patched_value(spec, "CLAUDE_CONFIG_DIR")
            .unwrap_or_default()
            .to_owned();
        variables.insert("CLAUDE_CONFIG_DIR".into(), root);
        variables.insert("CLAUDE_SECURESTORAGE_CONFIG_DIR".into(), pin);
    }

    // 7. The cell's own delivered environment. See `MINIFIED_CELL_ENVIRONMENT`
    // for what is in it, what was measured out of it, and why this runs after
    // the profile denylist rather than before it.
    if cell == SessionCell::Minified {
        for (key, value) in MINIFIED_CELL_ENVIRONMENT {
            variables.insert((*key).to_owned(), (*value).to_owned());
        }
    }

    // Checked here, on the final map, rather than on the caller's snapshot:
    // the question is whether a marker reaches the child, not whether the
    // operator happened to be running under one.
    reject_team_markers_reaching_child(&variables)?;

    // A name the allowlist dropped but the caller's explicit `set` restored is
    // not a removal. Report only what the child does not receive.
    removed.retain(|key| !variables.contains_key(key));
    Ok((EnvironmentSnapshot { variables }, removed))
}

/// The value a name would carry under `snapshot - unset + set`, before the
/// allowlist filter.
///
/// Used only for names the allowlist denies but whose value still governs a
/// profile change — today exactly `TMUX_PROGRAM`, whose parent directory is
/// pruned from `PATH`.
fn patched_value<'a>(spec: &'a EnvironmentSpec, key: &str) -> Option<&'a str> {
    if let Some(value) = spec.set.get(key) {
        return Some(value.as_str());
    }
    if spec.unset.contains(key) {
        return None;
    }
    spec.snapshot.get(key).map(String::as_str)
}

fn remove_tmux_shim_from_path(
    variables: &mut BTreeMap<String, String>,
    tmux_program: Option<&str>,
) -> Result<()> {
    let Some(shim_parent) = tmux_program
        .map(Path::new)
        .filter(|path| path.is_absolute())
        .and_then(Path::parent)
        .map(Path::to_path_buf)
    else {
        return Ok(());
    };
    let Some(path) = variables.get("PATH") else {
        return Ok(());
    };
    let components = std::env::split_paths(path)
        .filter(|component| !must_treat_as_same_directory(component, &shim_parent))
        .collect::<Vec<_>>();
    let rebuilt = std::env::join_paths(components).context("failed to rebuild transparent PATH")?;
    let rebuilt = rebuilt
        .into_string()
        .map_err(|_| anyhow::anyhow!("transparent PATH is not valid UTF-8"))?;
    variables.insert("PATH".into(), rebuilt);
    Ok(())
}

/// The kernel's own name for a directory, on the platform pmux runs on.
///
/// `(st_dev, st_ino)`. A path is a SPELLING; this is the RESOURCE. No alias --
/// symlink, firmlink, trailing slash, `..` traversal, case-insensitive
/// spelling, bind mount -- can produce a different pair for the same directory,
/// which is exactly the property a path string does not have.
#[cfg(unix)]
type ResourceKey = (u64, u64);

/// Nothing in `std` exposes an inode off unix, so this degrades to the resolved
/// path -- weaker, and honestly weaker: it collapses symlinks and `..` but not a
/// namespace alias. Every deployment target of pmux is unix; this branch exists
/// so the crate still compiles where the `cfg(not(unix))` fallbacks elsewhere in
/// this file compile.
#[cfg(not(unix))]
type ResourceKey = PathBuf;

#[cfg(unix)]
fn resource_key(_path: &Path, metadata: &std::fs::Metadata) -> ResourceKey {
    use std::os::unix::fs::MetadataExt;

    (metadata.dev(), metadata.ino())
}

#[cfg(not(unix))]
fn resource_key(path: &Path, _metadata: &std::fs::Metadata) -> ResourceKey {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// What the operating system says one path names, right now.
///
/// The distinction between [`Self::Vacant`] and [`Self::Unresolved`] is the
/// whole point of the type. "There is no such directory" is an ANSWER -- it is
/// what a root pmux has yet to create looks like, and it proves the path is not
/// a directory some live session is holding. "I could not look" is not an
/// answer, and code that treats the two alike admits an applicant on a test it
/// never actually ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DirectoryIdentity {
    /// `stat` answered. This is the resource, as the kernel counts identity.
    Resource(ResourceKey),
    /// `stat` answered `NotFound`: this path names nothing.
    ///
    /// A bare fact about right now, and NOT on its own a licence to admit --
    /// see [`traverses_a_parent_component`] and
    /// `native::require_establishable_identity`, which is where the question
    /// "does this absence prove anything?" is decided.
    Vacant,
    /// `stat` failed for any other reason: an unreadable parent, a symlink
    /// loop, a name too long. pmux does not know what this path names.
    Unresolved,
}

impl DirectoryIdentity {
    /// Asked at the point of the question rather than stored, because the
    /// answer is a property of the filesystem and not of the request.
    ///
    /// Deliberately follows symlinks: so does the child pmux is about to
    /// launch, so the resource that matters is the one at the end of the link.
    ///
    /// AND THAT IS AN ANSWER ABOUT ONE PATH, NOT ABOUT A CHAIN. LEAK 8 lived in
    /// the gap: [`one_directory_contains_the_other`] walked a path's LEXICAL
    /// ancestors and asked this about each of them, so a symlink component was
    /// correctly identified as the directory it points at -- and the walk then
    /// carried on to that component's spelling's parents, which are not the
    /// target's parents. Following the link makes each ANSWER right; it cannot
    /// make the QUESTIONS the right ones. The walk resolves the path before it
    /// starts for exactly that reason.
    ///
    /// This reports what the operating system says and nothing else. Policy
    /// about what an answer is worth belongs to the caller, because the two
    /// callers want different things from the same `Vacant`: admission must
    /// treat it as proof that no live cell holds the path, while the
    /// securestorage-pin comparison in `validate_config_isolation` is
    /// deliberately permissive about it.
    pub(crate) fn of(path: &Path) -> Self {
        match std::fs::metadata(path) {
            Ok(metadata) => Self::Resource(resource_key(path, &metadata)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::Vacant,
            Err(_) => Self::Unresolved,
        }
    }
}

/// Whether a path spells any part of itself with `..`.
///
/// This is the exact and only lexical construct whose meaning depends on what
/// exists: `.` and repeated separators name the same directory under every
/// filesystem arrangement (and `Path::components` already elides them), while
/// `..` names a different directory depending on whether the component before
/// it exists, and on whether that component is a symlink.
///
/// LEAK 5b is what that difference bought. The kernel resolves left-to-right,
/// so `/X/NOPE/../rootA` is `NotFound` when `NOPE` is missing -- even though
/// `/X/rootA` is a live minified cell's root. Reading that `NotFound` as
/// "nothing is there, so creating it creates something new" asserts what the
/// kernel never said: MEASURED, a recursive create of `/X/NOPE/../rootB`
/// creates `rootB`, because the intermediate is created first and only then
/// does `..` resolve. Claude's own `CLAUDE_CONFIG_DIR` bootstrap does exactly
/// that, so the path completes onto the live root and the intruder's transcript
/// lands physically inside it.
///
/// This predicate is consulted only to decide whether an ABSENCE is evidence.
/// It is never used to compute where a path points, and nothing built on it
/// trusts lexical resolution: collapsing `..` lexically is not the kernel's
/// rule -- with `b` a symlink, `a/b/..` is `b`'s target's parent, not `a` -- so
/// a fix that collapsed and then trusted would be wrong in the direction that
/// leaks. pmux declines to answer instead, exactly as it declines for an
/// unreadable parent.
pub(crate) fn traverses_a_parent_component(path: &Path) -> bool {
    path.components()
        .any(|component| component == std::path::Component::ParentDir)
}

/// Whether pmux must treat two paths as ONE directory.
///
/// Named for what it DECIDES, not for what it proves, because in the last arm
/// those differ: `paths_equivalent` -- which this replaces -- claimed path
/// equivalence while running a string comparison whenever `canonicalize` did
/// not answer for both sides, and that gap is most of how LEAK 5 survived
/// review.
///
/// MEASURED: on macOS every directory on the data volume has a second canonical
/// spelling that `Path::canonicalize` does not collapse, because the APFS
/// firmlink namespace is not a symlink. `/private/tmp/X` and
/// `/System/Volumes/Data/private/tmp/X` are `(16777230, 269160739)` either way,
/// and `canonicalize` returns the second one UNCHANGED. Against a live minified
/// cell, five separate starts spelled that way -- two `CLAUDE_CONFIG_DIR`
/// shapes, a `config_isolation` root, and two `cwd`s -- were all ADMITTED, and
/// the third of them made pmux write `skipDangerousModePermissionPrompt` into
/// the live cell's own `settings.json` on the intruder's behalf.
///
/// The arms, and why each is the safe direction:
///
/// * Both resolve: the kernel answered, and its answer is the whole question.
/// * Both vacant: neither path names a directory yet, so there is no resource
///   to compare -- but two identical absolute spellings will CREATE one
///   directory between them. The byte comparison appears here as a sufficient
///   condition on paths that name nothing, never as a substitute for the test.
/// * One vacant, one resolved: nothing is not a directory, so it is not THIS
///   directory. This is the arm that lets a root pmux is about to create
///   through, and it is sound precisely because a live session's directory
///   exists by construction -- PROVIDED the vacancy is evidence at all. LEAK 5b
///   was this arm being handed a `NotFound` from `/X/NOPE/../rootA` that a
///   recursive create completes onto the resolved side, so callers that bind a
///   session to a directory must first pass the applicant through
///   `native::require_establishable_identity`, which refuses exactly that
///   spelling. This predicate is not the place for the rule: its other caller,
///   the securestorage-pin comparison, wants the permissive answer and
///   `the_pin_is_byte_exact_while_the_root_is_canonical` pins that.
/// * Anything unresolved: pmux cannot prove the two are different, so it treats
///   them as the same. A wrong "same" costs a refusal; a wrong "different"
///   costs the leak. Callers that must not merely narrow the applicant --
///   `native::admit_bound_resources` -- additionally refuse an applicant whose
///   own identity is [`DirectoryIdentity::Unresolved`], because "treat as the
///   same" says nothing at all when there is no incumbent to be the same AS.
pub(crate) fn must_treat_as_same_directory(left: &Path, right: &Path) -> bool {
    match (DirectoryIdentity::of(left), DirectoryIdentity::of(right)) {
        (DirectoryIdentity::Resource(left), DirectoryIdentity::Resource(right)) => left == right,
        (DirectoryIdentity::Vacant, DirectoryIdentity::Vacant) => left == right,
        (DirectoryIdentity::Vacant, _) | (_, DirectoryIdentity::Vacant) => false,
        _ => true,
    }
}

fn validate_environment(spec: &EnvironmentSpec) -> Result<()> {
    for (key, value) in spec.snapshot.iter().chain(&spec.set) {
        if key.is_empty() || key.contains(['=', '\0']) || value.contains('\0') {
            bail!("invalid environment name or value");
        }
    }
    if spec
        .unset
        .iter()
        .any(|key| key.is_empty() || key.contains(['=', '\0']))
    {
        bail!("invalid environment unset name");
    }
    // An explicitly `set` marker is refused even though the transparent profile
    // would strip it before the child ever saw it. The caller stated an intent
    // that cannot be honoured, and silently discarding an explicit instruction
    // is worse than refusing it. An AMBIENT marker in the snapshot is a
    // different case entirely -- the caller asked for nothing -- and is handled
    // on the resolved map by `reject_team_markers_reaching_child`.
    if spec.set.keys().any(|key| is_team_marker(key)) {
        bail!("agent-team and teammate environment variables are forbidden");
    }
    Ok(())
}

/// A name that would make the child believe it is part of an agent team.
fn is_team_marker(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.contains("AGENT_TEAM") || upper.contains("TEAMMATE")
}

/// Refuse to launch when a team marker would actually REACH the child.
///
/// This is deliberately checked against the resolved environment rather than
/// the caller's snapshot. The snapshot is whatever the operator happened to be
/// running under, and a marker there is only dangerous if it survives the
/// allowlist, the caller patch, and the profile removals.
/// `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS` is in `TRANSPARENT_EXACT_KEYS`, so
/// the transparent profile already deletes it; rejecting on the raw snapshot
/// made pmux refuse to launch at all from inside any Claude Code session with
/// agent teams enabled -- which is the ordinary environment for developing pmux
/// itself -- over a name the child was never going to see.
///
/// The guard is not weakened: a marker in `set`, or one the policy does not
/// remove, still fails closed here. Only names provably stripped are allowed to
/// pass, and this runs on the exact map handed to the child, so a future policy
/// change that stops stripping one re-arms the refusal automatically.
fn reject_team_markers_reaching_child(variables: &BTreeMap<String, String>) -> Result<()> {
    if variables.keys().any(|key| is_team_marker(key)) {
        bail!("agent-team and teammate environment variables are forbidden");
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum RequiredPathKind {
    Directory,
    RegularFile,
    ExecutableFile,
}

fn canonical_absolute(path: &Path, label: &str, kind: RequiredPathKind) -> Result<PathBuf> {
    if !path.is_absolute() {
        bail!("{label} must be absolute");
    }
    let canonical = path
        .canonicalize()
        .with_context(|| format!("{label} is unavailable: {}", path.display()))?;
    let metadata = canonical
        .metadata()
        .with_context(|| format!("failed to inspect {label}: {}", canonical.display()))?;
    match kind {
        RequiredPathKind::Directory => ensure!(
            metadata.is_dir(),
            "{label} must be a directory: {}",
            canonical.display()
        ),
        RequiredPathKind::RegularFile => ensure!(
            metadata.is_file(),
            "{label} must be a regular file: {}",
            canonical.display()
        ),
        RequiredPathKind::ExecutableFile => {
            ensure!(
                metadata.is_file(),
                "{label} must be a regular file: {}",
                canonical.display()
            );
            ensure!(
                is_executable(&metadata),
                "{label} is not executable: {}",
                canonical.display()
            );
        }
    }
    canonical_utf8(&canonical, label)?;
    Ok(canonical)
}

fn canonical_utf8<'a>(path: &'a Path, label: &str) -> Result<&'a str> {
    path.to_str()
        .with_context(|| format!("canonical {label} path is not UTF-8"))
}

#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    true
}

/// The `--effort` spellings Claude Code accepts as a DEPTH SETTING, measured.
///
/// This constant exists to give the exhaustiveness test something to compare
/// pmux's table AGAINST. The test it serves used to assert only that pmux's
/// spelling table covered pmux's own enum, which is a tautology: it passed for
/// as long as `EffortLevel::Ultracode` existed and could never have said
/// anything about what the child does with the word.
///
/// MEASURED against Claude Code 2.1.220 (aarch64 macOS), 2026-08-04, three ways
/// that agree:
///
/// * `claude --help`: "--effort <level>  Effort level for the current session
///   (low, medium, high, xhigh, max)".
/// * `claude --model sonnet --effort zzzznope --print` exits 0 having written
///   to stderr: "Warning: Unknown --effort value 'zzzznope' — ignoring it and
///   using the default effort. Valid values: low, medium, high, xhigh, max."
/// * Each of the five run live and produced a completed turn with no stderr.
///
/// TWO THINGS THIS IS NOT, both measured rather than assumed:
///
/// * It is NOT the set the child ACCEPTS. The 2.1.220 bundle's validator also
///   admits `ultracode`, `unset` and `auto`, and `--effort ultracode` was
///   MEASURED to run a clean turn with no warning at all. `ultracode` is
///   excluded because of what it MEANS, not because it is refused: the bundle
///   describes it as "xhigh + dynamic workflow orchestration (this session
///   only)", i.e. a subagent-orchestration mode rather than a depth setting.
///   `unset` and `auto` both mean "use the default", which pmux expresses by
///   omitting the flag.
/// * It is NOT a per-model set, and pmux deliberately does not derive one. The
///   API's `output_config.effort` surface does vary by model -- Sonnet 4.6 and
///   Opus 4.6 have no `xhigh`, Haiku 4.5 takes no effort at all -- but pmux does
///   not call that API; it launches this CLI, and the CLI is what mediates.
///   MEASURED 2026-08-04 on 2.1.220, five pairs including both the table calls
///   impossible: `--model claude-haiku-4-5 --effort xhigh` and
///   `--model claude-sonnet-4-6 --effort max` each completed a turn with
///   `is_error: false`, an empty stderr, and `modelUsage` naming the requested
///   model. The child neither refuses an unsupported pair nor passes it through
///   to fail at the API; it absorbs it. Refusing such a start at admission
///   would therefore refuse something that measurably works, which is the
///   recurring defect of this tree run backwards: a rule written against the
///   shape of the API request rather than against the resource pmux actually
///   hands over.
///
/// An unknown spelling is the quiet failure this guards: the child does not
/// exit, it WARNS ON STDERR AND SILENTLY USES THE DEFAULT. pmux never reads the
/// child's stderr, so a spelling that stops being accepted would buy a
/// different amount of thinking than the caller asked for, with no diagnostic
/// anywhere.
#[cfg(test)]
const CLAUDE_ACCEPTED_EFFORT_SPELLINGS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

const fn effort_name(effort: EffortLevel) -> &'static str {
    match effort {
        EffortLevel::Low => "low",
        EffortLevel::Medium => "medium",
        EffortLevel::High => "high",
        EffortLevel::XHigh => "xhigh",
        EffortLevel::Max => "max",
    }
}

/// The argv shape one typed permission mode expands to.
///
/// Almost every mode is a value of Claude's `--permission-mode` option, but
/// `DangerouslySkipPermissions` has no such value: it is its own flag. Keeping
/// the two shapes in one wildcard-free match makes a new variant a compile
/// error rather than a silently mis-spelled command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PermissionModeArgv {
    /// `--permission-mode <value>` — exactly two arguments.
    Pair(&'static str),
    /// One self-contained flag with no value argument following it.
    Single(&'static str),
}

const fn permission_mode_argv(mode: PermissionMode) -> PermissionModeArgv {
    match mode {
        PermissionMode::Default => PermissionModeArgv::Pair("default"),
        PermissionMode::AcceptEdits => PermissionModeArgv::Pair("acceptEdits"),
        PermissionMode::Plan => PermissionModeArgv::Pair("plan"),
        PermissionMode::Auto => PermissionModeArgv::Pair("auto"),
        PermissionMode::BypassPermissions => PermissionModeArgv::Pair("bypassPermissions"),
        PermissionMode::DontAsk => PermissionModeArgv::Pair("dontAsk"),
        PermissionMode::DangerouslySkipPermissions => {
            PermissionModeArgv::Single(DANGEROUSLY_SKIP_PERMISSIONS_FLAG)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pseudomux_protocol::v1::launch_environment::{
        INHERITED_EXACT_KEYS, TRANSPARENT_EXACT_KEYS, TRANSPARENT_PREFIXES,
    };
    use pseudomux_protocol::v1::{
        CompatibilityPolicy, LifecycleMode, RetentionPolicy, SessionCell, TerminalSpec,
    };
    use std::collections::BTreeMap;
    use std::fs;

    fn request(root: &Path) -> StartSessionRequest {
        StartSessionRequest {
            identity: SessionIdentity::New {
                session_id: Some(SessionId::nil()),
            },
            cwd: root.to_string_lossy().into_owned(),
            agent: None,
            claude: Some(ClaudeLaunchConfig {
                executable: "/bin/sh".into(),
                model: Some("sonnet".into()),
                effort: Some(EffortLevel::High),
                permission_mode: Some(PermissionMode::Plan),
                allowed_tools: vec!["Read".into()],
                denied_tools: Vec::new(),
                settings: Vec::new(),
                mcp_configs: Vec::new(),
                plugin_dirs: Vec::new(),
                system_prompt: SystemPromptPolicy::Default,
                extra_args: Vec::new(),
            }),
            environment: EnvironmentSpec {
                snapshot: BTreeMap::from([
                    ("PATH".into(), "/usr/bin".into()),
                    ("ANTHROPIC_API_KEY".into(), "secret".into()),
                    ("TMUX".into(), "stock".into()),
                    ("TMUXIFIER".into(), "nested-marker".into()),
                    ("RMUXCUSTOM".into(), "nested-marker".into()),
                    ("CLAUDE_AGENT_SDK_VERSION".into(), "ambient-sdk".into()),
                    ("CLAUDE_AGENT_SDK_MCP_NO_PREFIX".into(), "1".into()),
                    ("CLAUDE_CODE_SDK_CLIENT_APP".into(), "ambient-sdk".into()),
                ]),
                ..EnvironmentSpec::default()
            },
            auth_policy: AuthPolicy::Subscription,
            config_isolation: None,
            terminal: TerminalSpec::default(),
            lifecycle: LifecycleMode::Transcript,
            retention: RetentionPolicy::OneShot,
            compatibility: CompatibilityPolicy::AllowUntested,
            cell: SessionCell::Full,
        }
    }

    #[cfg(unix)]
    fn write_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;

        fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn argument_value<'a>(args: &'a [String], flag: &str) -> &'a str {
        let position = args.iter().position(|argument| argument == flag).unwrap();
        args.get(position + 1).unwrap()
    }

    #[test]
    fn subscription_launch_is_interactive_and_sanitized() {
        let root = std::env::current_dir().unwrap();
        let launch = resolve_claude_launch(&request(&root)).unwrap();
        assert_eq!(launch.process.args[0], "--session-id");
        assert!(!launch.process.args.iter().any(|arg| arg == "--print"));
        assert!(
            !launch
                .process
                .environment
                .variables
                .contains_key("ANTHROPIC_API_KEY")
        );
        assert!(!launch.process.environment.variables.contains_key("TMUX"));
        assert!(
            !launch
                .process
                .environment
                .variables
                .contains_key("TMUXIFIER")
        );
        assert!(
            !launch
                .process
                .environment
                .variables
                .contains_key("RMUXCUSTOM")
        );
        for key in [
            "CLAUDE_AGENT_SDK_VERSION",
            "CLAUDE_AGENT_SDK_MCP_NO_PREFIX",
            "CLAUDE_CODE_SDK_CLIENT_APP",
        ] {
            assert!(!launch.process.environment.variables.contains_key(key));
            assert!(launch.removed_environment_keys.contains(key));
        }
        assert_eq!(
            launch
                .process
                .environment
                .variables
                .get("TERM")
                .map(String::as_str),
            Some("xterm-256color")
        );
    }

    /// Every spelling of one directory that a caller can put on the wire, with
    /// the kernel's own proof that each IS that directory.
    ///
    /// The list is the alias table LEAK 5 walked through. `identity` is here so
    /// the loop below still means something if every other row were removed;
    /// the firmlink row carries the fact that broke `paths_equivalent`.
    #[cfg(unix)]
    fn aliases_of(directory: &Path) -> Vec<(&'static str, PathBuf)> {
        let name = directory.file_name().expect("the fixture has a file name");
        let mut aliases = vec![
            ("identity", directory.to_path_buf()),
            (
                "trailing slash",
                PathBuf::from(format!("{}/", directory.display())),
            ),
            ("dot-dot traversal", directory.join("..").join(name)),
        ];
        let link = directory.with_file_name(format!(
            "symlink-to-{}",
            name.to_str().expect("the fixture name is UTF-8")
        ));
        if !link.exists() {
            std::os::unix::fs::symlink(directory, &link).unwrap();
        }
        aliases.push(("symlink", link));
        #[cfg(target_os = "macos")]
        {
            // The APFS firmlink namespace: present on every system volume
            // since Catalina, NOT a symlink, and therefore not collapsed by
            // `Path::canonicalize` -- which is the whole of LEAK 5.
            let canonical = directory.canonicalize().unwrap();
            let firmlink = Path::new("/System/Volumes/Data").join(
                canonical
                    .strip_prefix("/")
                    .expect("a canonical path is absolute"),
            );
            assert!(
                firmlink.is_dir(),
                "the firmlink alias must exist for this case to mean anything: {}",
                firmlink.display()
            );
            assert_ne!(
                firmlink.canonicalize().unwrap(),
                canonical,
                "canonicalize must NOT collapse the firmlink alias; if it ever does, \
                 the string comparison this rule replaced was not the bug it was measured to be"
            );
            aliases.push(("firmlink", firmlink));
        }
        aliases
    }

    #[cfg(unix)]
    fn inode_of(path: &Path) -> (u64, u64) {
        use std::os::unix::fs::MetadataExt;

        let metadata = fs::metadata(path)
            .unwrap_or_else(|error| panic!("{} must be inspectable: {error}", path.display()));
        (metadata.dev(), metadata.ino())
    }

    /// LEAK 5, stated as the fact that made the string comparison wrong.
    ///
    /// Asserted on the INODE, not on the spelling: every row is proven to be
    /// the same `(device, inode)` as the directory it aliases before the
    /// predicate is asked about it, so a row that stopped being an alias fails
    /// as a broken fixture rather than passing as a satisfied rule.
    #[cfg(unix)]
    #[test]
    fn every_alias_of_one_directory_is_one_resource() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("root");
        fs::create_dir(&root).unwrap();
        let other = parent.path().join("other");
        fs::create_dir(&other).unwrap();

        let truth = inode_of(&root);
        assert_ne!(
            truth,
            inode_of(&other),
            "two distinct directories must not share an inode, or nothing below separates anything"
        );

        for (label, alias) in aliases_of(&root) {
            assert_eq!(
                inode_of(&alias),
                truth,
                "{label}: the fixture must actually alias the same directory"
            );
            assert!(
                must_treat_as_same_directory(&alias, &root),
                "{label}: {} is inode {truth:?}, the same directory",
                alias.display()
            );
            assert!(
                must_treat_as_same_directory(&root, &alias),
                "{label}: the relation must not depend on argument order"
            );
            assert!(
                !must_treat_as_same_directory(&alias, &other),
                "{label}: a different directory must stay different"
            );
        }
    }

    /// A path that names nothing is an ANSWER, and a different one from a path
    /// that cannot be read.
    #[cfg(unix)]
    #[test]
    fn a_path_that_names_no_directory_is_not_the_directory_something_else_holds() {
        let parent = tempfile::tempdir().unwrap();
        let existing = parent.path().join("existing");
        fs::create_dir(&existing).unwrap();
        let missing = parent.path().join("not-yet-created");
        let also_missing = parent.path().join("also-not-created");

        assert_eq!(DirectoryIdentity::of(&missing), DirectoryIdentity::Vacant);
        assert!(
            !must_treat_as_same_directory(&missing, &existing),
            "a root pmux is about to create is not a root a live session is running in"
        );
        assert!(!must_treat_as_same_directory(&existing, &missing));
        assert!(
            !must_treat_as_same_directory(&missing, &also_missing),
            "two different paths that name nothing will create two directories"
        );
        assert!(
            must_treat_as_same_directory(&missing, &missing),
            "one spelling that names nothing will create exactly one directory"
        );

        // The answer tracks the resource, not the request: the moment the
        // vacant path becomes a way of reaching the live directory, it is that
        // directory.
        std::os::unix::fs::symlink(&existing, &missing).unwrap();
        assert_eq!(inode_of(&missing), inode_of(&existing));
        assert!(must_treat_as_same_directory(&missing, &existing));
    }

    /// LEAK 5b, stated as the two filesystem facts that make an absence
    /// worthless as evidence.
    ///
    /// Both are asserted here so that every rule built on
    /// [`traverses_a_parent_component`] fails as a broken premise rather than
    /// as a satisfied rule if either ever stops being true. The rules
    /// themselves live in `native.rs`, where the admission decisions are.
    #[cfg(unix)]
    #[test]
    fn an_absence_reported_for_a_dot_dot_spelling_proves_nothing() {
        let parent = tempfile::tempdir().unwrap();
        let held = parent.path().join("rootA");
        fs::create_dir(&held).unwrap();
        let applicant = parent.path().join("NOPE").join("..").join("rootA");
        assert!(traverses_a_parent_component(&applicant));

        // FACT 1: the kernel resolves left-to-right and stops at the missing
        // `NOPE`, so a path that lexically names the LIVE directory reports
        // `NotFound` -- and `DirectoryIdentity` reports that faithfully.
        assert_eq!(
            fs::metadata(&applicant).unwrap_err().kind(),
            std::io::ErrorKind::NotFound,
            "{} must be NotFound for this case to mean anything",
            applicant.display()
        );
        assert_eq!(DirectoryIdentity::of(&applicant), DirectoryIdentity::Vacant);
        assert!(
            !must_treat_as_same_directory(&applicant, &held),
            "the identity predicate reports the kernel's answer; the policy about \
             what that answer is worth belongs to the admission gate"
        );

        // FACT 2: and that `NotFound` does not survive a recursive create. The
        // intermediate is created first, and only then does `..` resolve -- so
        // the path completes onto a directory that already existed. This is
        // what `mkdir -p` does and what Claude's own `CLAUDE_CONFIG_DIR`
        // bootstrap does, and it is why FACT 1 is not proof of anything.
        let held_b = parent.path().join("rootB");
        fs::create_dir(&held_b).unwrap();
        let inode_before = inode_of(&held_b);
        let completes_onto_it = parent.path().join("NOPE2").join("..").join("rootB");
        assert_eq!(
            fs::metadata(&completes_onto_it).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
        fs::create_dir_all(&completes_onto_it).unwrap();
        assert_eq!(
            inode_of(&completes_onto_it),
            inode_before,
            "a recursive create through a missing intermediate lands on the EXISTING directory"
        );
        assert_eq!(inode_of(&held_b), inode_before);
    }

    /// The one lexical construct whose meaning depends on what exists.
    #[test]
    fn only_a_parent_component_makes_a_spelling_depend_on_the_filesystem() {
        for spelling in [
            "/x/NOPE/../rootA",
            "/x/..",
            "../relative",
            "/x/rootA/../rootA",
        ] {
            assert!(
                traverses_a_parent_component(Path::new(spelling)),
                "{spelling} carries a `..`"
            );
        }
        // `.` and repeated separators name the same directory under every
        // filesystem arrangement, and `Path::components` already elides them.
        for spelling in ["/x/rootA", "/x/./rootA", "/x//rootA", "/x/rootA/", "/"] {
            assert!(
                !traverses_a_parent_component(Path::new(spelling)),
                "{spelling} carries no `..`"
            );
        }
    }

    /// The arm that is NOT allowed to be a byte comparison.
    #[cfg(unix)]
    #[test]
    fn a_directory_that_cannot_be_inspected_is_never_reported_as_a_different_one() {
        use std::os::unix::fs::PermissionsExt;

        let parent = tempfile::tempdir().unwrap();
        let closed = parent.path().join("closed");
        fs::create_dir(&closed).unwrap();
        let hidden = closed.join("hidden");
        fs::create_dir(&hidden).unwrap();
        let elsewhere = parent.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();

        // Observed under the closed parent, then reopened before any assertion
        // runs, so a failure here cannot leave an unremovable temporary tree.
        fs::set_permissions(&closed, fs::Permissions::from_mode(0o000)).unwrap();
        let identity = DirectoryIdentity::of(&hidden);
        let treated_as_same = must_treat_as_same_directory(&hidden, &elsewhere);
        fs::set_permissions(&closed, fs::Permissions::from_mode(0o700)).unwrap();

        if matches!(identity, DirectoryIdentity::Resource(_)) {
            // A user that ignores the mode bits -- root. There is no
            // unreadable path for this process, so the arm is unreachable here.
            return;
        }
        assert_eq!(identity, DirectoryIdentity::Unresolved);
        assert!(
            treated_as_same,
            "a path pmux cannot inspect must never be reported as a DIFFERENT directory: \
             a wrong `same` costs a refusal, a wrong `different` costs the leak"
        );
    }

    #[test]
    fn transparent_profile_removes_the_injected_tmux_shim_path() {
        let directory = tempfile::tempdir().unwrap();
        let shim = directory.path().join("shim");
        std::fs::create_dir(&shim).unwrap();
        let path = std::env::join_paths([Path::new("/usr/bin"), &shim, Path::new("/bin")])
            .unwrap()
            .into_string()
            .unwrap();
        let spec = EnvironmentSpec {
            snapshot: BTreeMap::from([
                ("PATH".into(), path),
                (
                    "TMUX_PROGRAM".into(),
                    shim.join("tmux").to_string_lossy().into_owned(),
                ),
            ]),
            ..EnvironmentSpec::default()
        };
        let (environment, _) = build_environment(
            &spec,
            AuthPolicy::Subscription,
            TerminalProfile::Transparent,
            None,
            SessionCell::Full,
        )
        .unwrap();
        let components =
            std::env::split_paths(environment.variables.get("PATH").unwrap()).collect::<Vec<_>>();
        assert_eq!(
            components,
            vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")]
        );
        assert!(!environment.variables.contains_key("TMUX_PROGRAM"));
    }

    #[test]
    fn transparent_profile_strips_parent_behavior_but_inherit_keeps_credentials() {
        let spec = EnvironmentSpec {
            snapshot: BTreeMap::from([
                ("ANTHROPIC_API_KEY".into(), "caller-selected-key".into()),
                ("CLAUDECODE".into(), "1".into()),
                ("CLAUDE_CODE_ENTRYPOINT".into(), "sdk".into()),
                ("CLAUDE_CODE_REMOTE".into(), "true".into()),
                ("CLAUDE_CODE_CHILD_SESSION".into(), "1".into()),
                ("CLAUDE_AGENT_SDK_VERSION".into(), "ambient".into()),
            ]),
            ..EnvironmentSpec::default()
        };
        let (environment, removed) = build_environment(
            &spec,
            AuthPolicy::Inherit,
            TerminalProfile::Transparent,
            None,
            SessionCell::Full,
        )
        .unwrap();
        assert_eq!(
            environment
                .variables
                .get("ANTHROPIC_API_KEY")
                .map(String::as_str),
            Some("caller-selected-key")
        );
        for key in [
            "CLAUDECODE",
            "CLAUDE_CODE_ENTRYPOINT",
            "CLAUDE_CODE_REMOTE",
            // Regression, 2026-07-27: inheriting this from a parent Claude Code
            // session makes the child never write its own transcript, so every
            // turn dies at `awaiting_prompt_ack`. Isolated to this one variable
            // against Claude 2.1.215 and 2.1.220.
            "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_AGENT_SDK_VERSION",
        ] {
            assert!(!environment.variables.contains_key(key));
            assert!(removed.contains(key));
        }
    }

    #[test]
    fn resume_uses_resume_instead_of_session_id() {
        let root = std::env::current_dir().unwrap();
        let mut request = request(&root);
        request.identity = SessionIdentity::Resume {
            session_id: SessionId::nil(),
        };
        let launch = resolve_claude_launch(&request).unwrap();
        assert!(launch.resume);
        assert_eq!(
            &launch.process.args[..2],
            ["--resume", &SessionId::nil().to_string()]
        );
    }

    #[test]
    fn print_and_positional_passthrough_are_rejected() {
        let root = std::env::current_dir().unwrap();
        for flag in FORBIDDEN_DRIVER_FLAGS {
            for argument in [(*flag).to_owned(), format!("{flag}=value")] {
                let mut request = request(&root);
                request.claude.as_mut().expect("inline launch").extra_args = vec![argument.clone()];
                let error = resolve_claude_launch(&request).unwrap_err();
                assert!(
                    error.to_string().contains("forbidden"),
                    "{argument:?} was not classified as a forbidden driver flag: {error}"
                );
            }
        }

        for argument in ["prompt text", "--debug=value", "--future-flag"] {
            let mut request = request(&root);
            request.claude.as_mut().expect("inline launch").extra_args = vec![argument.into()];
            let error = resolve_claude_launch(&request).unwrap_err();
            assert!(
                error.to_string().contains("allowlist"),
                "{argument:?} was not rejected by the bounded raw allowlist: {error}"
            );
        }

        let mut safe = request(&root);
        safe.claude.as_mut().expect("inline launch").extra_args = SAFE_EXTRA_FLAGS
            .iter()
            .map(|flag| (*flag).to_owned())
            .collect();
        let launch = resolve_claude_launch(&safe).unwrap();
        assert_eq!(
            &launch.process.args[launch.process.args.len() - SAFE_EXTRA_FLAGS.len()..],
            SAFE_EXTRA_FLAGS
        );
    }

    #[test]
    fn structured_values_cannot_be_reinterpreted_as_driver_options() {
        let root = std::env::current_dir().unwrap();

        let mut model = request(&root);
        model.claude.as_mut().expect("inline launch").model = Some("--print".into());
        assert!(resolve_claude_launch(&model).is_err());

        let mut allowed_tool = request(&root);
        allowed_tool
            .claude
            .as_mut()
            .expect("inline launch")
            .allowed_tools = vec!["--background".into()];
        assert!(resolve_claude_launch(&allowed_tool).is_err());

        let mut denied_tool = request(&root);
        denied_tool
            .claude
            .as_mut()
            .expect("inline launch")
            .denied_tools = vec!["--output-format=stream-json".into()];
        assert!(resolve_claude_launch(&denied_tool).is_err());
    }

    #[test]
    fn generated_new_identity_is_frozen_before_launch_resolution() {
        let root = std::env::current_dir().unwrap();
        let mut request = request(&root);
        request.identity = SessionIdentity::New { session_id: None };

        let selected = select_session_id(&request).unwrap();
        assert_ne!(selected, SessionId::nil());
        request.identity = SessionIdentity::New {
            session_id: Some(selected),
        };

        let launch = resolve_claude_launch(&request).unwrap();
        assert_eq!(launch.session_id, selected);
        assert!(!launch.resume);
        assert_eq!(
            &launch.process.args[..2],
            ["--session-id", &selected.to_string()]
        );
    }

    #[test]
    fn transcript_history_disable_is_rejected() {
        let root = std::env::current_dir().unwrap();
        let mut explicitly_enabled = request(&root);
        explicitly_enabled
            .environment
            .set
            .insert("CLAUDE_CODE_SKIP_PROMPT_HISTORY".into(), "1".into());
        assert!(resolve_claude_launch(&explicitly_enabled).is_err());

        let mut ambient_but_removed = request(&root);
        ambient_but_removed
            .environment
            .snapshot
            .insert("CLAUDE_CODE_SKIP_PROMPT_HISTORY".into(), "ambient".into());
        ambient_but_removed
            .environment
            .unset
            .insert("CLAUDE_CODE_SKIP_PROMPT_HISTORY".into());
        assert!(resolve_claude_launch(&ambient_but_removed).is_ok());

        ambient_but_removed
            .environment
            .set
            .insert("CLAUDE_CODE_SKIP_PROMPT_HISTORY".into(), "restored".into());
        assert!(resolve_claude_launch(&ambient_but_removed).is_err());
    }

    #[test]
    fn environment_patch_order_and_subscription_stripping_are_exact() {
        let mut snapshot = BTreeMap::from([
            ("PATCHED".to_owned(), "snapshot".to_owned()),
            ("REMOVED".to_owned(), "snapshot".to_owned()),
            ("TERM".to_owned(), "ambient".to_owned()),
        ]);
        for key in SUBSCRIPTION_AUTH_KEYS {
            snapshot.insert((*key).to_owned(), format!("secret-{key}"));
        }
        for key in TRANSPARENT_EXACT_KEYS
            .iter()
            .filter(|key| !key.contains("AGENT_TEAM") && !key.contains("TEAMMATE"))
        {
            snapshot.insert((*key).to_owned(), format!("ambient-{key}"));
        }
        for prefix in TRANSPARENT_PREFIXES {
            snapshot.insert(format!("{prefix}_BOUNDARY"), "ambient".to_owned());
        }

        let spec = EnvironmentSpec {
            snapshot,
            set: BTreeMap::from([
                ("PATCHED".to_owned(), "set-wins".to_owned()),
                ("ADDED".to_owned(), "set".to_owned()),
                ("ANTHROPIC_API_KEY".to_owned(), "set-secret".to_owned()),
            ]),
            unset: BTreeSet::from([
                "PATCHED".to_owned(),
                "REMOVED".to_owned(),
                "ANTHROPIC_API_KEY".to_owned(),
            ]),
        };
        let (environment, removed) = build_environment(
            &spec,
            AuthPolicy::Subscription,
            TerminalProfile::Transparent,
            None,
            SessionCell::Full,
        )
        .unwrap();

        assert_eq!(
            environment.variables.get("PATCHED"),
            Some(&"set-wins".to_owned())
        );
        assert_eq!(environment.variables.get("ADDED"), Some(&"set".to_owned()));
        assert!(!environment.variables.contains_key("REMOVED"));
        assert_eq!(
            environment.variables.get("TERM"),
            Some(&"xterm-256color".to_owned())
        );
        for key in SUBSCRIPTION_AUTH_KEYS {
            assert!(!environment.variables.contains_key(*key), "retained {key}");
            assert!(removed.contains(*key), "did not report removal of {key}");
        }
        for key in TRANSPARENT_EXACT_KEYS
            .iter()
            .filter(|key| !key.contains("AGENT_TEAM") && !key.contains("TEAMMATE"))
        {
            assert!(!environment.variables.contains_key(*key), "retained {key}");
            assert!(removed.contains(*key), "did not report removal of {key}");
        }
        for prefix in TRANSPARENT_PREFIXES {
            let key = format!("{prefix}_BOUNDARY");
            assert!(!environment.variables.contains_key(&key), "retained {key}");
            assert!(removed.contains(&key), "did not report removal of {key}");
        }
    }

    fn snapshot_only(snapshot: BTreeMap<String, String>) -> EnvironmentSpec {
        EnvironmentSpec {
            snapshot,
            ..EnvironmentSpec::default()
        }
    }

    #[test]
    fn an_unknown_inherited_name_is_denied_by_construction() {
        for policy in [AuthPolicy::Subscription, AuthPolicy::Inherit] {
            let spec = snapshot_only(BTreeMap::from([
                ("SOME_RANDOM_THING".to_owned(), "1".to_owned()),
                ("PATH".to_owned(), "/usr/bin".to_owned()),
            ]));
            let (environment, removed) = build_environment(
                &spec,
                policy,
                TerminalProfile::Transparent,
                None,
                SessionCell::Full,
            )
            .unwrap();
            assert!(
                !environment.variables.contains_key("SOME_RANDOM_THING"),
                "{policy:?} inherited an unknown variable"
            );
            assert!(
                removed.contains("SOME_RANDOM_THING"),
                "{policy:?} did not report the allowlist drop"
            );
            assert_eq!(
                environment.variables.get("PATH"),
                Some(&"/usr/bin".to_owned())
            );
        }
    }

    #[test]
    fn the_allowlist_denies_nested_claude_markers_without_help_from_the_denylist() {
        // The regression that motivated the inversion: a parent Claude Code
        // session exports `CLAUDE_CODE_CHILD_SESSION`, the child inherited it,
        // rendered a composer, accepted the paste and the Enter, and never wrote
        // a transcript of its own -- so every turn died at `awaiting_prompt_ack`.
        // It is now in `TRANSPARENT_EXACT_KEYS`, but this asserts the *allowlist*
        // stands alone: deleting the denylist entry would change nothing, and
        // the fifth marker Claude invents is already denied.
        for policy in [AuthPolicy::Subscription, AuthPolicy::Inherit] {
            assert!(
                !inherited_from_snapshot("CLAUDE_CODE_CHILD_SESSION", policy),
                "{policy:?}: the allowlist admitted the nested-session marker"
            );
            assert!(
                !inherited_from_snapshot("CLAUDE_CODE_NOT_INVENTED_YET", policy),
                "{policy:?}: the allowlist admitted an unreviewed Claude marker"
            );
            for key in TRANSPARENT_EXACT_KEYS {
                assert!(
                    !inherited_from_snapshot(key, policy),
                    "{policy:?}: the allowlist admitted {key}, so only the denylist removes it"
                );
            }
            for prefix in TRANSPARENT_PREFIXES {
                let key = format!("{prefix}_BOUNDARY");
                assert!(
                    !inherited_from_snapshot(&key, policy),
                    "{policy:?}: the allowlist admitted {key}"
                );
            }
        }
    }

    #[test]
    fn every_allowlisted_name_survives_the_snapshot_filter() {
        let mut snapshot = BTreeMap::new();
        for key in INHERITED_EXACT_KEYS {
            snapshot.insert((*key).to_owned(), format!("value-of-{key}"));
        }
        snapshot.insert("LC_ALL".to_owned(), "C".to_owned());
        snapshot.insert("PMUX_TEST_STATE_DIR".to_owned(), "/tmp/state".to_owned());
        let spec = snapshot_only(snapshot);

        let (environment, removed) = build_environment(
            &spec,
            AuthPolicy::Subscription,
            TerminalProfile::Transparent,
            None,
            SessionCell::Full,
        )
        .unwrap();

        for key in INHERITED_EXACT_KEYS {
            assert!(
                environment.variables.contains_key(*key),
                "the allowlist denied its own entry {key}"
            );
            assert!(!removed.contains(*key), "reported allowlisted {key}");
            if *key != "TERM" {
                let expected = format!("value-of-{key}");
                assert_eq!(
                    environment.variables.get(*key),
                    Some(&expected),
                    "rewrote allowlisted {key}"
                );
            }
        }
        // `TERM` is the one allowlisted name the transparent profile overwrites.
        assert_eq!(
            environment.variables.get("TERM"),
            Some(&"xterm-256color".to_owned())
        );
        assert_eq!(environment.variables.get("LC_ALL"), Some(&"C".to_owned()));
        assert_eq!(
            environment.variables.get("PMUX_TEST_STATE_DIR"),
            Some(&"/tmp/state".to_owned())
        );
        assert!(removed.is_empty(), "unexpected removals: {removed:?}");
    }

    #[test]
    fn allowlist_prefix_and_exact_matching_are_case_sensitive() {
        let spec = snapshot_only(BTreeMap::from([
            ("LC_ALL".to_owned(), "C".to_owned()),
            ("LC_TIME".to_owned(), "en_GB.UTF-8".to_owned()),
            ("LC_A_FUTURE_CATEGORY".to_owned(), "C".to_owned()),
            ("lc_all".to_owned(), "wrong-case".to_owned()),
            ("LCALL".to_owned(), "no-underscore".to_owned()),
            ("XLC_ALL".to_owned(), "not-a-prefix".to_owned()),
        ]));
        let (environment, removed) = build_environment(
            &spec,
            AuthPolicy::Subscription,
            TerminalProfile::Transparent,
            None,
            SessionCell::Full,
        )
        .unwrap();
        for key in ["LC_ALL", "LC_TIME", "LC_A_FUTURE_CATEGORY"] {
            assert!(
                environment.variables.contains_key(key),
                "the LC_ prefix did not admit {key}"
            );
        }
        for key in ["lc_all", "LCALL", "XLC_ALL"] {
            assert!(
                !environment.variables.contains_key(key),
                "prefix matching is anchored and case-sensitive: {key}"
            );
            assert!(removed.contains(key), "did not report {key}");
        }
        // Lowercase proxy spellings are admitted only because both cases are
        // listed by name; nothing is case-folded.
        assert!(inherited_from_snapshot(
            "http_proxy",
            AuthPolicy::Subscription
        ));
        assert!(!inherited_from_snapshot(
            "Http_Proxy",
            AuthPolicy::Subscription
        ));
        assert!(!inherited_from_snapshot("path", AuthPolicy::Subscription));
    }

    #[test]
    fn caller_supplied_set_bypasses_the_allowlist_entirely() {
        // `set` is how a caller passes an MCP server's API token. It is the
        // explicit channel and is never filtered.
        let spec = EnvironmentSpec {
            snapshot: BTreeMap::from([("MCP_SERVER_TOKEN".to_owned(), "from-snapshot".to_owned())]),
            set: BTreeMap::from([
                ("MCP_SERVER_TOKEN".to_owned(), "explicit".to_owned()),
                ("SOME_RANDOM_THING".to_owned(), "1".to_owned()),
            ]),
            unset: BTreeSet::new(),
        };
        let (environment, removed) = build_environment(
            &spec,
            AuthPolicy::Subscription,
            TerminalProfile::Transparent,
            None,
            SessionCell::Full,
        )
        .unwrap();
        assert_eq!(
            environment.variables.get("MCP_SERVER_TOKEN"),
            Some(&"explicit".to_owned())
        );
        assert_eq!(
            environment.variables.get("SOME_RANDOM_THING"),
            Some(&"1".to_owned())
        );
        assert!(
            removed.is_empty(),
            "reported a name the child receives: {removed:?}"
        );
    }

    #[test]
    fn inherit_retains_provider_routing_and_subscription_denies_it() {
        // Bedrock resolves credentials through the AWS SDK's environment,
        // Vertex through Google ADC, Foundry through Azure. Allowing the ten
        // selector keys while denying what they select would leave every
        // `inherit` caller with a broken login.
        const PROVIDER_NAMES: &[&str] = &[
            "ANTHROPIC_VERTEX_PROJECT_ID",
            "ANTHROPIC_CUSTOM_HEADERS",
            "AWS_PROFILE",
            "AWS_REGION",
            "AWS_SESSION_TOKEN",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "GCLOUD_PROJECT",
            "CLOUDSDK_CORE_PROJECT",
            "AZURE_TENANT_ID",
            "VERTEX_REGION_CLAUDE_3_5_HAIKU",
            "CLOUD_ML_REGION",
        ];
        let routing = || SUBSCRIPTION_AUTH_KEYS.iter().chain(PROVIDER_NAMES.iter());

        let mut snapshot = BTreeMap::new();
        for key in routing() {
            snapshot.insert((*key).to_owned(), format!("routing-{key}"));
        }
        let spec = snapshot_only(snapshot);

        let (inherited, inherit_removed) = build_environment(
            &spec,
            AuthPolicy::Inherit,
            TerminalProfile::Transparent,
            None,
            SessionCell::Full,
        )
        .unwrap();
        for key in routing() {
            let expected = format!("routing-{key}");
            assert_eq!(
                inherited.variables.get(*key),
                Some(&expected),
                "inherit dropped provider routing key {key}"
            );
        }
        assert!(
            inherit_removed.is_empty(),
            "inherit reported removals: {inherit_removed:?}"
        );

        let (subscription, subscription_removed) = build_environment(
            &spec,
            AuthPolicy::Subscription,
            TerminalProfile::Transparent,
            None,
            SessionCell::Full,
        )
        .unwrap();
        for key in routing() {
            assert!(
                !subscription.variables.contains_key(*key),
                "subscription retained {key}"
            );
            assert!(
                subscription_removed.contains(*key),
                "subscription did not report {key}"
            );
        }
    }

    #[test]
    fn documented_environment_order_is_allowlist_then_unset_then_set_then_removals_then_isolation()
    {
        // effective = allowlist(snapshot) - unset + set - policy_removals
        //             + profile_changes + config_isolation
        let spec = EnvironmentSpec {
            snapshot: BTreeMap::from([
                ("PATH".to_owned(), "/from-snapshot".to_owned()),
                ("TZ".to_owned(), "UTC".to_owned()),
                ("LANG".to_owned(), "from-snapshot".to_owned()),
                ("SOME_RANDOM_THING".to_owned(), "from-snapshot".to_owned()),
                ("ANTHROPIC_API_KEY".to_owned(), "ambient".to_owned()),
            ]),
            set: BTreeMap::from([
                ("LANG".to_owned(), "set-wins".to_owned()),
                ("SOME_RANDOM_THING".to_owned(), "set-wins".to_owned()),
                ("ANTHROPIC_API_KEY".to_owned(), "set-secret".to_owned()),
            ]),
            unset: BTreeSet::from([
                "TZ".to_owned(),
                "LANG".to_owned(),
                "ANTHROPIC_API_KEY".to_owned(),
            ]),
        };
        let (environment, removed) = build_environment(
            &spec,
            AuthPolicy::Subscription,
            TerminalProfile::Transparent,
            None,
            SessionCell::Full,
        )
        .unwrap();

        assert_eq!(
            environment.variables.get("PATH"),
            Some(&"/from-snapshot".to_owned()),
            "an allowlisted snapshot entry must survive unchanged"
        );
        assert!(
            !environment.variables.contains_key("TZ"),
            "unset must still remove an allowlisted name"
        );
        assert_eq!(
            environment.variables.get("LANG"),
            Some(&"set-wins".to_owned()),
            "set must be applied after unset"
        );
        assert_eq!(
            environment.variables.get("SOME_RANDOM_THING"),
            Some(&"set-wins".to_owned()),
            "the allowlist applies to the snapshot term only"
        );
        assert!(
            !environment.variables.contains_key("ANTHROPIC_API_KEY"),
            "policy removals must run after set"
        );
        assert!(removed.contains("ANTHROPIC_API_KEY"));
        assert!(
            !removed.contains("SOME_RANDOM_THING"),
            "a name restored by set is not a removal"
        );
        assert_eq!(
            environment.variables.get("TERM"),
            Some(&"xterm-256color".to_owned()),
            "profile changes are last"
        );
    }

    #[test]
    fn removed_environment_keys_reports_allowlist_drops_and_nothing_else() {
        let root = std::env::current_dir().unwrap();
        let mut request = request(&root);
        request
            .environment
            .snapshot
            .insert("SOME_RANDOM_THING".to_owned(), "1".to_owned());
        request
            .environment
            .snapshot
            .insert("ANOTHER_UNKNOWN".to_owned(), "2".to_owned());
        request
            .environment
            .set
            .insert("ANOTHER_UNKNOWN".to_owned(), "restored".to_owned());

        let launch = resolve_claude_launch(&request).unwrap();
        assert!(
            launch
                .removed_environment_keys
                .contains("SOME_RANDOM_THING"),
            "internal launch completeness must report allowlist drops, not only denylist drops"
        );
        assert_eq!(
            launch.process.environment.variables.get("ANOTHER_UNKNOWN"),
            Some(&"restored".to_owned())
        );
        for key in &launch.removed_environment_keys {
            assert!(
                !launch.process.environment.variables.contains_key(key),
                "reported {key} as removed while delivering it"
            );
        }
    }

    #[test]
    fn agent_team_and_teammate_environment_markers_never_reach_the_child() {
        let root = std::env::current_dir().unwrap();
        for key in [
            "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS",
            "CLAUDE_CODE_TEAMMATE_MODE",
            "custom_agent_team_marker",
        ] {
            // An explicit `set` bypasses the allowlist, so it genuinely would
            // reach the child and must still fail closed.
            let mut patch = request(&root);
            patch.environment.unset.insert(key.to_owned());
            patch.environment.set.insert(key.to_owned(), "1".to_owned());
            assert!(resolve_claude_launch(&patch).is_err(), "accepted set {key}");

            // An explicitly removed ambient marker is gone either way.
            let mut snapshot = request(&root);
            snapshot
                .environment
                .snapshot
                .insert(key.to_owned(), "1".to_owned());
            snapshot.environment.unset.insert(key.to_owned());
            assert!(
                resolve_claude_launch(&snapshot).is_ok(),
                "an explicitly removed ambient marker remained effective: {key}"
            );

            // The invariant that actually matters: however the launch resolves,
            // no marker is ever delivered.
            let mut ambient = request(&root);
            ambient
                .environment
                .snapshot
                .insert(key.to_owned(), "1".to_owned());
            if let Ok(launch) = resolve_claude_launch(&ambient) {
                assert!(
                    !launch.process.environment.variables.contains_key(key),
                    "delivered {key} to the child"
                );
            }
        }
    }

    #[test]
    fn an_ambient_marker_the_policy_strips_does_not_block_the_launch() {
        // Rejecting on the raw snapshot made pmux refuse to start from inside
        // any Claude Code session with agent teams enabled -- the ordinary
        // environment for developing pmux -- over a name the transparent
        // profile already deletes. The guard is on delivery, not on ambience.
        let root = std::env::current_dir().unwrap();
        let key = "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS";
        assert!(
            TRANSPARENT_EXACT_KEYS.contains(&key),
            "{key} is no longer stripped by the transparent profile; this test's \
             premise is void and the launch must now be refused"
        );
        let mut ambient = request(&root);
        ambient
            .environment
            .snapshot
            .insert(key.to_owned(), "1".to_owned());
        let launch = resolve_claude_launch(&ambient)
            .expect("a marker the profile strips must not block the launch");
        assert!(!launch.process.environment.variables.contains_key(key));
        assert!(launch.removed_environment_keys.iter().any(|k| k == key));
    }

    #[test]
    fn invalid_environment_names_values_and_unset_entries_fail_closed() {
        let root = std::env::current_dir().unwrap();
        for (key, value) in [("", "value"), ("A=B", "value"), ("KEY", "nul\0value")] {
            let mut snapshot = request(&root);
            snapshot
                .environment
                .snapshot
                .insert(key.to_owned(), value.to_owned());
            assert!(resolve_claude_launch(&snapshot).is_err());

            let mut patch = request(&root);
            patch
                .environment
                .set
                .insert(key.to_owned(), value.to_owned());
            assert!(resolve_claude_launch(&patch).is_err());
        }
        for key in ["", "A=B", "nul\0key"] {
            let mut request = request(&root);
            request.environment.unset.insert(key.to_owned());
            assert!(resolve_claude_launch(&request).is_err());
        }
    }

    #[test]
    fn reserved_terminal_modes_and_zero_geometry_are_rejected() {
        let root = std::env::current_dir().unwrap();

        let mut attached = request(&root);
        attached.terminal.input_transport = InputTransport::AttachedStream;
        assert!(resolve_claude_launch(&attached).is_err());

        let mut rmux_identity = request(&root);
        rmux_identity.terminal.profile = TerminalProfile::RmuxStandard;
        assert!(resolve_claude_launch(&rmux_identity).is_err());

        for (rows, cols) in [(0, 120), (24, 0), (0, 0)] {
            let mut geometry = request(&root);
            geometry.terminal.rows = rows;
            geometry.terminal.cols = cols;
            assert!(resolve_claude_launch(&geometry).is_err());
        }
    }

    /// Every variant of a plain enum, as an array a test can iterate, with the
    /// list forced to stay complete by a wildcard-free `match`.
    ///
    /// This exists because a HAND-WRITTEN variant list is not a guard, it is a
    /// guard-shaped hole: the assertion runs over the array, so a variant that
    /// was never added to the array is never asserted about, and the test keeps
    /// passing while claiming coverage it does not have. The `exhaustive`
    /// function below is the whole mechanism -- adding a variant to the enum
    /// stops this compiling until the variant is added here, and adding it here
    /// puts it through every assertion in the caller.
    ///
    /// Same shape and same reason as `wire_values!` at
    /// `crates/protocol/tests/v1_conformance_vectors.rs:387`, which is what
    /// forces the shared manifest and the client lists to move together.
    macro_rules! every_variant {
        ($ty:ty, [$($variant:path),+ $(,)?]) => {{
            fn exhaustive(value: $ty) {
                match value {
                    $($variant => ()),+
                }
            }
            [$({ exhaustive($variant); $variant }),+]
        }};
    }

    /// EVERY SPELLING pmux CAN EMIT, DERIVED FROM THE ENUM -- and, stated
    /// plainly below, the two things that still are not covered.
    ///
    /// WHAT WAS WRONG WITH THE PREVIOUS VERSION OF THIS TEST. Its variant list
    /// was written out by hand, so it asserted over exactly the variants
    /// someone had remembered to type into it. MEASURED: `EffortLevel::Ultracode`
    /// was re-added across every surface -- the enum, `effort_name`, the client
    /// lists, the shared manifest -- and the whole suite passed, including this
    /// test, whose own doc comment claimed that "adding a variant whose spelling
    /// the child does not take as a depth setting is now a failing test". The
    /// new variant was simply absent from the array, so the membership
    /// assertion never saw it. That is this tree's recurring defect in its
    /// purest form: a check that passes by having no cases.
    ///
    /// WHAT THE GUARD NOW COVERS. Both arrays come from [`every_variant!`],
    /// whose inner wildcard-free `match` will not compile while a variant is
    /// missing from the list, and the pinned literal arrays below have a fixed
    /// length, so a new variant cannot be smuggled past either. Once in the
    /// list, an effort variant must pass membership in
    /// [`CLAUDE_ACCEPTED_EFFORT_SPELLINGS`], which is a RECORDED MEASUREMENT of
    /// the child rather than another view of pmux -- the one assertion here that
    /// is not a tautology about a single crate. `ultracode` fails it because of
    /// what it MEANS: the 2.1.220 bundle calls it "xhigh + dynamic workflow
    /// orchestration", an orchestration mode rather than a depth setting.
    ///
    /// WHAT THE GUARD DOES NOT COVER, so that the next reader does not overclaim
    /// it the way the last doc comment did:
    ///
    /// * IT CANNOT ASK THE CHILD. [`CLAUDE_ACCEPTED_EFFORT_SPELLINGS`] is a
    ///   table checked in by hand. It goes stale the day Claude changes its
    ///   flag, and nothing in the deterministic suite can notice; only the live
    ///   lane can put the question to a real child.
    /// * AN UNKNOWN SPELLING NEVER FAILS AT RUNTIME, AND THE DIAGNOSTIC LANDS
    ///   WHERE pmux DOES NOT LOOK. The child does not exit and does not refuse
    ///   the flag -- it warns on stderr and silently runs the turn at the
    ///   DEFAULT effort. pmux gives that stderr nowhere to go that it reads:
    ///   `pmux-launcher` `exec`s the child inside the pane's pty
    ///   (`bin/pmux-launcher/src/main.rs:100-111`), `crates/rmux` contains no
    ///   `stderr` handling at all, and the only reader of that pty is the screen
    ///   snapshot, which no predicate searches for a flag warning. So a wrong
    ///   spelling buys a different amount of thinking than the caller paid for,
    ///   with no error on any pmux surface, at any layer, ever. THIS
    ///   COMPILE-TIME LIST IS THE ONLY THING BETWEEN A NEW VARIANT AND THAT
    ///   OUTCOME.
    #[test]
    fn every_typed_effort_and_permission_value_has_an_exact_spelling() {
        let spellings = every_variant!(
            EffortLevel,
            [
                EffortLevel::Low,
                EffortLevel::Medium,
                EffortLevel::High,
                EffortLevel::XHigh,
                EffortLevel::Max,
            ]
        )
        .map(effort_name);
        assert_eq!(spellings, ["low", "medium", "high", "xhigh", "max"]);
        for spelling in spellings {
            assert!(
                CLAUDE_ACCEPTED_EFFORT_SPELLINGS.contains(&spelling),
                "pmux emits `--effort {spelling}`, which Claude Code does not take as a depth setting; an unrecognized value is warned about on stderr and silently replaced by the default, and pmux never reads the child's stderr"
            );
        }
        assert_eq!(
            every_variant!(
                PermissionMode,
                [
                    PermissionMode::Default,
                    PermissionMode::AcceptEdits,
                    PermissionMode::Plan,
                    PermissionMode::Auto,
                    PermissionMode::BypassPermissions,
                    PermissionMode::DontAsk,
                    PermissionMode::DangerouslySkipPermissions,
                ]
            )
            .map(permission_mode_argv),
            [
                PermissionModeArgv::Pair("default"),
                PermissionModeArgv::Pair("acceptEdits"),
                PermissionModeArgv::Pair("plan"),
                PermissionModeArgv::Pair("auto"),
                PermissionModeArgv::Pair("bypassPermissions"),
                PermissionModeArgv::Pair("dontAsk"),
                PermissionModeArgv::Single("--dangerously-skip-permissions"),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn every_typed_launch_option_has_one_unambiguous_argv_mapping() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("work");
        let plugin = root.path().join("plugin");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&plugin).unwrap();
        let executable = root.path().join("claude");
        let settings = root.path().join("settings.json");
        let mcp = root.path().join("mcp.json");
        write_executable(&executable);
        fs::write(&settings, "{}").unwrap();
        fs::write(&mcp, "{}").unwrap();

        let mut request = request(&cwd);
        request.claude = Some(ClaudeLaunchConfig {
            executable: executable.to_string_lossy().into_owned(),
            model: Some("claude-sonnet".into()),
            effort: Some(EffortLevel::XHigh),
            permission_mode: Some(PermissionMode::DontAsk),
            allowed_tools: vec!["Read".into(), "Bash(git:*)".into()],
            denied_tools: vec!["Write".into()],
            settings: vec![ConfigSource::File {
                path: settings.to_string_lossy().into_owned(),
            }],
            mcp_configs: vec![ConfigSource::File {
                path: mcp.to_string_lossy().into_owned(),
            }],
            plugin_dirs: vec![plugin.to_string_lossy().into_owned()],
            system_prompt: SystemPromptPolicy::Default,
            extra_args: vec!["--verbose".into()],
        });

        let launch = resolve_claude_launch(&request).unwrap();
        let expected = vec![
            "--session-id".to_owned(),
            SessionId::nil().to_string(),
            "--model".to_owned(),
            "claude-sonnet".to_owned(),
            "--effort".to_owned(),
            "xhigh".to_owned(),
            "--permission-mode".to_owned(),
            "dontAsk".to_owned(),
            "--allowedTools".to_owned(),
            "Read".to_owned(),
            "--allowedTools".to_owned(),
            "Bash(git:*)".to_owned(),
            "--disallowedTools".to_owned(),
            "Write".to_owned(),
            "--settings".to_owned(),
            settings
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            "--mcp-config".to_owned(),
            mcp.canonicalize().unwrap().to_string_lossy().into_owned(),
            "--plugin-dir".to_owned(),
            plugin
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            "--verbose".to_owned(),
        ];
        assert_eq!(launch.process.args, expected);
        assert!(!launch.dangerous_permission_bypass);

        // The same request under the one mode that is a self-contained flag:
        // the two-argument `--permission-mode dontAsk` pair collapses to one
        // argument and nothing else about the command line moves.
        request
            .claude
            .as_mut()
            .expect("inline launch")
            .permission_mode = Some(PermissionMode::DangerouslySkipPermissions);
        let bypass = resolve_claude_launch(&request).unwrap();
        let mut bypass_expected = expected;
        bypass_expected.splice(6..8, ["--dangerously-skip-permissions".to_owned()]);
        assert_eq!(bypass.process.args, bypass_expected);
        assert!(bypass.dangerous_permission_bypass);
    }

    #[test]
    fn dangerous_permission_bypass_has_a_stable_snake_case_wire_value() {
        assert_eq!(
            serde_json::to_string(&PermissionMode::DangerouslySkipPermissions).unwrap(),
            "\"dangerously_skip_permissions\""
        );
        assert_eq!(
            serde_json::from_str::<PermissionMode>("\"dangerously_skip_permissions\"").unwrap(),
            PermissionMode::DangerouslySkipPermissions
        );
    }

    #[test]
    fn dangerously_skip_permissions_is_one_flag_and_no_other_mode_emits_it() {
        let root = std::env::current_dir().unwrap();
        let only_permission_mode = |mode: PermissionMode| {
            let mut request = request(&root);
            request.claude.as_mut().expect("inline launch").model = None;
            request.claude.as_mut().expect("inline launch").effort = None;
            request
                .claude
                .as_mut()
                .expect("inline launch")
                .allowed_tools = Vec::new();
            request
                .claude
                .as_mut()
                .expect("inline launch")
                .permission_mode = Some(mode);
            request
        };

        let launch = resolve_claude_launch(&only_permission_mode(
            PermissionMode::DangerouslySkipPermissions,
        ))
        .unwrap();
        assert_eq!(
            launch.process.args,
            [
                "--session-id".to_owned(),
                SessionId::nil().to_string(),
                "--dangerously-skip-permissions".to_owned(),
            ],
            "the bypass must be exactly one argument with no value following it"
        );
        assert!(launch.dangerous_permission_bypass);

        for mode in [
            PermissionMode::Default,
            PermissionMode::AcceptEdits,
            PermissionMode::Plan,
            PermissionMode::Auto,
            PermissionMode::BypassPermissions,
            PermissionMode::DontAsk,
        ] {
            let launch = resolve_claude_launch(&only_permission_mode(mode)).unwrap();
            assert!(
                !launch
                    .process
                    .args
                    .iter()
                    .any(|argument| argument == "--dangerously-skip-permissions"),
                "{mode:?} emitted the permission bypass flag"
            );
            assert_eq!(
                launch.process.args.len(),
                4,
                "{mode:?} must stay a --permission-mode flag/value pair"
            );
            assert_eq!(launch.process.args[2], "--permission-mode");
            assert!(!launch.dangerous_permission_bypass);
        }

        // The bypass is reachable only through the typed variant: the bounded
        // raw-argument allowlist still refuses to forward the flag itself.
        let mut smuggled = only_permission_mode(PermissionMode::Default);
        smuggled.claude.as_mut().expect("inline launch").extra_args =
            vec!["--dangerously-skip-permissions".to_owned()];
        assert!(
            resolve_claude_launch(&smuggled)
                .unwrap_err()
                .to_string()
                .contains("allowlist")
        );
    }

    #[cfg(unix)]
    #[test]
    fn direct_native_paths_are_canonicalized_with_required_types() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("work");
        let plugin = root.path().join("plugin");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&plugin).unwrap();
        let executable = root.path().join("claude");
        let settings = root.path().join("settings.json");
        let mcp = root.path().join("mcp.json");
        write_executable(&executable);
        fs::write(&settings, "{}").unwrap();
        fs::write(&mcp, "{}").unwrap();

        let mut request = request(&cwd);
        request.cwd = cwd.join("..").join("work").to_string_lossy().into_owned();
        request.claude.as_mut().expect("inline launch").executable =
            executable.to_string_lossy().into_owned();
        request.claude.as_mut().expect("inline launch").settings = vec![ConfigSource::File {
            path: settings.to_string_lossy().into_owned(),
        }];
        request.claude.as_mut().expect("inline launch").mcp_configs = vec![ConfigSource::File {
            path: mcp.to_string_lossy().into_owned(),
        }];
        request.claude.as_mut().expect("inline launch").plugin_dirs = vec![
            plugin
                .join("..")
                .join("plugin")
                .to_string_lossy()
                .into_owned(),
        ];

        let launch = resolve_claude_launch(&request).unwrap();
        assert_eq!(launch.process.cwd, cwd.canonicalize().unwrap());
        assert_eq!(
            launch.process.executable,
            executable.canonicalize().unwrap()
        );
        assert_eq!(
            argument_value(&launch.process.args, "--settings"),
            settings.canonicalize().unwrap().to_str().unwrap()
        );
        assert_eq!(
            argument_value(&launch.process.args, "--mcp-config"),
            mcp.canonicalize().unwrap().to_str().unwrap()
        );
        assert_eq!(
            argument_value(&launch.process.args, "--plugin-dir"),
            plugin.canonicalize().unwrap().to_str().unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn direct_native_paths_reject_missing_or_wrong_file_types() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("claude");
        let regular_file = root.path().join("config.json");
        let plugin = root.path().join("plugin");
        write_executable(&executable);
        fs::write(&regular_file, "{}").unwrap();
        fs::create_dir(&plugin).unwrap();

        let mut wrong_cwd = request(root.path());
        wrong_cwd.cwd = regular_file.to_string_lossy().into_owned();
        assert!(
            resolve_claude_launch(&wrong_cwd)
                .unwrap_err()
                .to_string()
                .contains("cwd must be a directory")
        );

        let mut wrong_executable = request(root.path());
        wrong_executable
            .claude
            .as_mut()
            .expect("inline launch")
            .executable = plugin.to_string_lossy().into_owned();
        assert!(
            resolve_claude_launch(&wrong_executable)
                .unwrap_err()
                .to_string()
                .contains("Claude executable must be a regular file")
        );

        let non_executable = root.path().join("not-executable");
        fs::write(&non_executable, "not executable").unwrap();
        let mut missing_execute_bit = request(root.path());
        missing_execute_bit
            .claude
            .as_mut()
            .expect("inline launch")
            .executable = non_executable.to_string_lossy().into_owned();
        assert!(
            resolve_claude_launch(&missing_execute_bit)
                .unwrap_err()
                .to_string()
                .contains("Claude executable is not executable")
        );

        let mut wrong_settings = request(root.path());
        wrong_settings
            .claude
            .as_mut()
            .expect("inline launch")
            .executable = executable.to_string_lossy().into_owned();
        wrong_settings
            .claude
            .as_mut()
            .expect("inline launch")
            .settings = vec![ConfigSource::File {
            path: plugin.to_string_lossy().into_owned(),
        }];
        assert!(
            resolve_claude_launch(&wrong_settings)
                .unwrap_err()
                .to_string()
                .contains("--settings must be a regular file")
        );

        let mut wrong_mcp = request(root.path());
        wrong_mcp.claude.as_mut().expect("inline launch").executable =
            executable.to_string_lossy().into_owned();
        wrong_mcp
            .claude
            .as_mut()
            .expect("inline launch")
            .mcp_configs = vec![ConfigSource::File {
            path: plugin.to_string_lossy().into_owned(),
        }];
        assert!(
            resolve_claude_launch(&wrong_mcp)
                .unwrap_err()
                .to_string()
                .contains("--mcp-config must be a regular file")
        );

        let mut wrong_plugin = request(root.path());
        wrong_plugin
            .claude
            .as_mut()
            .expect("inline launch")
            .executable = executable.to_string_lossy().into_owned();
        wrong_plugin
            .claude
            .as_mut()
            .expect("inline launch")
            .plugin_dirs = vec![regular_file.to_string_lossy().into_owned()];
        assert!(
            resolve_claude_launch(&wrong_plugin)
                .unwrap_err()
                .to_string()
                .contains("plugin directory must be a directory")
        );

        let mut missing_plugin = request(root.path());
        missing_plugin
            .claude
            .as_mut()
            .expect("inline launch")
            .executable = executable.to_string_lossy().into_owned();
        missing_plugin
            .claude
            .as_mut()
            .expect("inline launch")
            .plugin_dirs = vec![
            root.path()
                .join("missing-plugin")
                .to_string_lossy()
                .into_owned(),
        ];
        assert!(
            resolve_claude_launch(&missing_plugin)
                .unwrap_err()
                .to_string()
                .contains("plugin directory is unavailable")
        );
    }

    // ---- Config isolation -------------------------------------------------

    #[cfg(unix)]
    fn owner_only_directory() -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        directory
    }

    #[cfg(unix)]
    fn isolated(root: &Path, private: &Path) -> StartSessionRequest {
        let mut request = request(root);
        request.config_isolation = Some(ConfigIsolation {
            root: private.to_string_lossy().into_owned(),
        });
        request
    }

    #[cfg(unix)]
    #[test]
    fn config_isolation_overrides_a_snapshot_config_dir_and_pins_the_original_store() {
        let cwd = owner_only_directory();
        let private = owner_only_directory();
        let mut request = isolated(cwd.path(), private.path());
        request
            .environment
            .snapshot
            .insert("CLAUDE_CONFIG_DIR".into(), "/operator/root".into());

        let launch = resolve_claude_launch(&request).unwrap();
        let variables = &launch.process.environment.variables;
        assert_eq!(
            variables.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            private.path().canonicalize().unwrap().to_str(),
            "the private root must replace the operator's, canonicalized"
        );
        assert_eq!(
            variables.get("CLAUDE_SECURESTORAGE_CONFIG_DIR"),
            Some(&"/operator/root".to_owned()),
            "the credential store must stay pinned to the root this request would have used"
        );
    }

    #[cfg(unix)]
    #[test]
    fn config_isolation_pins_the_default_store_when_the_caller_had_no_config_dir() {
        let cwd = owner_only_directory();
        let private = owner_only_directory();
        // No CLAUDE_CONFIG_DIR and no HOME anywhere: the un-isolated child would
        // have used the DEFAULT, unsuffixed keychain service, which Claude
        // selects on an EMPTY securestorage dir.
        let request = isolated(cwd.path(), private.path());
        let launch = resolve_claude_launch(&request).unwrap();
        assert_eq!(
            launch
                .process
                .environment
                .variables
                .get("CLAUDE_SECURESTORAGE_CONFIG_DIR"),
            Some(&String::new()),
            "an empty pin is the mechanism, not a leftover: it must be PRESENT and EMPTY"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_injected_names_are_delivered_and_are_denylisted_by_nothing_today() {
        // Step 6 runs after the profile denylist so that a future `CLAUDE_`
        // entry cannot strip the pin and turn every isolated session into a
        // login screen. Stated honestly: THAT ORDERING IS UNOBSERVABLE TODAY,
        // because neither injected name is matched by any removal table, so no
        // assertion here can distinguish step 6 from step 4. This test pins the
        // other half of the pair -- both names reach the child, and both are
        // outside every removal table right now -- so the day one of them is
        // added, this test fails and points at the ordering rather than the pin
        // silently disappearing from a live run.
        assert!(!transparent_profile_removes("CLAUDE_CONFIG_DIR"));
        assert!(!transparent_profile_removes(
            "CLAUDE_SECURESTORAGE_CONFIG_DIR"
        ));
        assert!(!SUBSCRIPTION_AUTH_KEYS.contains(&"CLAUDE_CONFIG_DIR"));
        assert!(!SUBSCRIPTION_AUTH_KEYS.contains(&"CLAUDE_SECURESTORAGE_CONFIG_DIR"));

        let cwd = owner_only_directory();
        let private = owner_only_directory();
        let mut request = isolated(cwd.path(), private.path());
        request
            .environment
            .snapshot
            .insert("CLAUDECODE".into(), "1".into());
        let launch = resolve_claude_launch(&request).unwrap();
        let variables = &launch.process.environment.variables;
        assert!(
            !variables.contains_key("CLAUDECODE"),
            "the profile denylist still runs"
        );
        assert!(variables.contains_key("CLAUDE_CONFIG_DIR"));
        assert!(variables.contains_key("CLAUDE_SECURESTORAGE_CONFIG_DIR"));
        assert!(
            !launch
                .removed_environment_keys
                .iter()
                .any(|key| key.starts_with("CLAUDE_CONFIG_DIR")
                    || key == "CLAUDE_SECURESTORAGE_CONFIG_DIR"),
            "an injected name must never be reported as removed"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_pin_is_byte_exact_while_the_root_is_canonical() {
        // Claude hashes the pin (`sha256(NFC(value))[0..8]`) to name a keychain
        // item, so canonicalizing it would silently select a DIFFERENT store
        // and produce "Not logged in". The root has the opposite requirement:
        // it must name the same directory pmux seeds and the locator walks.
        let cwd = owner_only_directory();
        let private = owner_only_directory();
        let uncanonical = private.path().join(".").join(".");
        let mut request = request(cwd.path());
        request.config_isolation = Some(ConfigIsolation {
            root: uncanonical.to_string_lossy().into_owned(),
        });
        request.environment.snapshot.insert(
            "CLAUDE_CONFIG_DIR".into(),
            "/operator/root/../root/.".into(),
        );

        let launch = resolve_claude_launch(&request).unwrap();
        let variables = &launch.process.environment.variables;
        assert_eq!(
            variables.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            private.path().canonicalize().unwrap().to_str()
        );
        assert_eq!(
            variables.get("CLAUDE_SECURESTORAGE_CONFIG_DIR"),
            Some(&"/operator/root/../root/.".to_owned())
        );
    }

    #[cfg(unix)]
    #[test]
    fn config_isolation_refuses_a_caller_supplied_config_dir_or_pin() {
        let cwd = owner_only_directory();
        let private = owner_only_directory();
        for key in ["CLAUDE_CONFIG_DIR", "CLAUDE_SECURESTORAGE_CONFIG_DIR"] {
            let mut request = isolated(cwd.path(), private.path());
            request
                .environment
                .set
                .insert(key.to_owned(), "/caller/choice".to_owned());
            let error = resolve_claude_launch(&request).unwrap_err().to_string();
            assert!(
                error.contains("mutually exclusive") && error.contains(key),
                "{key}: unexpected refusal: {error}"
            );
        }

        // The ambient snapshot and an explicit `unset` are NOT conflicts: the
        // caller asked for nothing, and the snapshot value is the input to the
        // pin. Refusing here would lock out every operator who already runs
        // under a custom config root.
        let mut ambient = isolated(cwd.path(), private.path());
        ambient
            .environment
            .snapshot
            .insert("CLAUDE_CONFIG_DIR".into(), "/operator/root".into());
        ambient
            .environment
            .unset
            .insert("CLAUDE_CONFIG_DIR".to_owned());
        let launch = resolve_claude_launch(&ambient).unwrap();
        assert_eq!(
            launch
                .process
                .environment
                .variables
                .get("CLAUDE_SECURESTORAGE_CONFIG_DIR"),
            Some(&String::new()),
            "`unset` removes the value the pin is computed from, so the pin is empty"
        );
    }

    #[cfg(unix)]
    #[test]
    fn config_isolation_refuses_the_root_the_request_would_have_used_anyway() {
        let cwd = owner_only_directory();
        let private = owner_only_directory();

        let mut same_config_dir = isolated(cwd.path(), private.path());
        same_config_dir.environment.snapshot.insert(
            "CLAUDE_CONFIG_DIR".into(),
            private.path().to_string_lossy().into_owned(),
        );
        let error = resolve_claude_launch(&same_config_dir)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("would have used anyway"),
            "unexpected refusal: {error}"
        );

        // The HOME fallback is the same rule: `<HOME>/.claude` is what
        // `effective_config_root` resolves when CLAUDE_CONFIG_DIR is absent.
        let home = owner_only_directory();
        let dot_claude = home.path().join(".claude");
        fs::create_dir(&dot_claude).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dot_claude, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut same_home = request(cwd.path());
        same_home.config_isolation = Some(ConfigIsolation {
            root: dot_claude.to_string_lossy().into_owned(),
        });
        same_home
            .environment
            .snapshot
            .insert("HOME".into(), home.path().to_string_lossy().into_owned());
        let error = resolve_claude_launch(&same_home).unwrap_err().to_string();
        assert!(
            error.contains("would have used anyway"),
            "unexpected refusal: {error}"
        );
    }

    /// The same rule, under the alias that defeated the string comparison.
    ///
    /// A `config_isolation.root` of `/System/Volumes/Data<the caller's own root>`
    /// is the caller's own root -- same device, same inode -- and canonicalizing
    /// both sides says it is not, so pmux would have put its trust-table writer
    /// on the very file this feature exists to keep it off. The private root a
    /// `config_isolation` request is entitled to is a DIFFERENT DIRECTORY, and
    /// "different" is the kernel's word, not the caller's spelling.
    #[cfg(unix)]
    #[test]
    fn config_isolation_refuses_the_inherited_root_under_an_alias_of_the_same_inode() {
        use std::os::unix::fs::PermissionsExt;

        // Both fixtures live under one temporary parent so that the sibling
        // symlink `aliases_of` creates is removed with them.
        let parent = tempfile::tempdir().unwrap();
        let owner_only = |name: &str| {
            let path = parent.path().join(name);
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            path
        };
        let cwd = owner_only("cwd");
        let inherited = owner_only("inherited-config-root");

        for (label, alias) in aliases_of(&inherited) {
            assert_eq!(
                inode_of(&alias),
                inode_of(&inherited),
                "{label}: the fixture must alias the inherited root"
            );
            let mut request = isolated(&cwd, &alias);
            request.environment.snapshot.insert(
                "CLAUDE_CONFIG_DIR".into(),
                inherited.to_string_lossy().into_owned(),
            );
            let error = resolve_claude_launch(&request).unwrap_err().to_string();
            assert!(
                error.contains("would have used anyway"),
                "{label}: unexpected refusal: {error}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn config_isolation_refuses_a_root_that_overlaps_cwd() {
        let outer = owner_only_directory();
        let inner = outer.path().join("inner");
        fs::create_dir(&inner).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&inner, fs::Permissions::from_mode(0o700)).unwrap();
        }

        let mut root_inside_cwd = request(outer.path());
        root_inside_cwd.config_isolation = Some(ConfigIsolation {
            root: inner.to_string_lossy().into_owned(),
        });
        assert!(
            resolve_claude_launch(&root_inside_cwd)
                .unwrap_err()
                .to_string()
                .contains("may not contain one another")
        );

        let mut cwd_inside_root = request(&inner);
        cwd_inside_root.config_isolation = Some(ConfigIsolation {
            root: outer.path().to_string_lossy().into_owned(),
        });
        assert!(
            resolve_claude_launch(&cwd_inside_root)
                .unwrap_err()
                .to_string()
                .contains("may not contain one another")
        );

        let mut same = request(outer.path());
        same.config_isolation = Some(ConfigIsolation {
            root: outer.path().to_string_lossy().into_owned(),
        });
        assert!(
            resolve_claude_launch(&same)
                .unwrap_err()
                .to_string()
                .contains("may not contain one another")
        );
    }

    /// The containment rule is about DIRECTORIES, so no spelling of one may
    /// escape it.
    ///
    /// This is the check that `root.starts_with(cwd) || cwd.starts_with(root)`
    /// could not make. Every row is proven to be the same `(device, inode)` as
    /// the directory it aliases before the rule is asked about it, so an alias
    /// that stopped aliasing fails here as a broken fixture.
    #[cfg(unix)]
    #[test]
    fn containment_is_decided_on_the_directory_and_not_on_a_path_prefix() {
        let parent = tempfile::tempdir().unwrap();
        let workspace = parent.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let inside = workspace.join("nested").join("root");
        fs::create_dir_all(&inside).unwrap();
        let elsewhere = parent.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();

        let truth = inode_of(&workspace);
        for (label, alias) in aliases_of(&workspace) {
            assert_eq!(
                inode_of(&alias),
                truth,
                "{label}: the fixture must alias the workspace"
            );
            assert!(
                one_directory_contains_the_other(&alias, &inside),
                "{label}: {} is beneath the workspace however the workspace is spelled",
                inside.display()
            );
            assert!(
                one_directory_contains_the_other(&inside, &alias),
                "{label}: the relation must not depend on argument order"
            );
            assert!(
                one_directory_contains_the_other(&alias, &workspace),
                "{label}: a directory contains itself under every spelling"
            );
            assert!(
                !one_directory_contains_the_other(&alias, &elsewhere),
                "{label}: two unrelated directories must stay unrelated, or this \
                 test passes by refusing everything"
            );
        }
        // A sibling whose NAME is a prefix of the other's is not containment.
        // `Path::starts_with` already compared whole components rather than
        // bytes, and the resource rule must not lose that.
        let sibling = parent.path().join("workspace-two");
        fs::create_dir(&sibling).unwrap();
        assert!(!one_directory_contains_the_other(&workspace, &sibling));
    }

    /// LEAK 8, at the predicate.
    ///
    /// The test above proves the relation survives every SPELLING of one
    /// directory. This one proves it survives a spelling of a DIFFERENT
    /// directory that reaches inside the first: a symlink to a strict
    /// descendant. Six leaks were aliases and were closed by asking the kernel
    /// for identity; this is the walk asking the kernel for identity of the
    /// right resource at every step and still walking the wrong CHAIN, because
    /// the chain came from the spelling.
    ///
    /// A link to the claimed directory ITSELF was always caught -- the walk's
    /// first element is the path, and `stat` resolves it -- so every row here
    /// points at a strict descendant, which is the direction that writes
    /// inside.
    #[cfg(unix)]
    #[test]
    fn containment_walks_what_the_child_reaches_and_not_the_spelling_it_was_sent() {
        let parent = tempfile::tempdir().unwrap();
        let workspace = parent.path().join("workspace");
        let nested = workspace.join("nested");
        fs::create_dir_all(&nested).unwrap();
        let elsewhere = parent.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();

        // Lexically disjoint from the workspace, and inside it on the resource.
        let link = parent.path().join("link-to-nested");
        std::os::unix::fs::symlink(&nested, &link).unwrap();
        assert!(
            !link.starts_with(&workspace),
            "the fixture must share no component with the workspace"
        );

        for (label, candidate) in [
            ("the link itself", link.clone()),
            ("a directory under the link", link.join("deeper")),
            ("an ABSENT directory under the link", link.join("not-yet")),
        ] {
            if label == "a directory under the link" {
                fs::create_dir(nested.join("deeper")).unwrap();
            }
            assert!(
                one_directory_contains_the_other(&workspace, &candidate),
                "{label}: {} really lies under {} and the rule must say so",
                candidate.display(),
                workspace.display()
            );
            assert!(
                one_directory_contains_the_other(&candidate, &workspace),
                "{label}: the relation must not depend on argument order"
            );
        }

        // A symlink is not itself containment, or the rule would refuse every
        // caller who spells a directory through one.
        let unrelated = parent.path().join("link-to-elsewhere");
        std::os::unix::fs::symlink(&elsewhere, &unrelated).unwrap();
        assert!(
            !one_directory_contains_the_other(&workspace, &unrelated),
            "a link to an unrelated directory must stay unrelated, or this test passes by \
             refusing everything"
        );
        assert!(!one_directory_contains_the_other(
            &workspace,
            &unrelated.join("under")
        ));
    }

    /// What the resolution actually returns, and that a cycle cannot make it
    /// run forever.
    ///
    /// The termination argument the walk used to rest on was "the walk is
    /// lexical". It still is -- but it now runs over a CANONICAL path, so the
    /// argument has to cover the canonicalization too. It does, and for a
    /// reason that is the operating system's rather than this code's: a cycle
    /// costs one bounded `ELOOP` per prefix, and the number of prefixes is the
    /// number of components in the spelling the caller sent.
    #[cfg(unix)]
    #[test]
    fn the_resolved_path_is_the_canonical_prefix_plus_what_does_not_exist_yet() {
        let parent = tempfile::tempdir().unwrap();
        let real = parent.path().join("real");
        fs::create_dir(&real).unwrap();
        let canonical_parent = parent.path().canonicalize().unwrap();

        assert_eq!(
            path_the_child_will_reach(&real),
            canonical_parent.join("real"),
            "an existing path resolves to its canonical self"
        );
        assert_eq!(
            path_the_child_will_reach(&real.join("a").join("b")),
            canonical_parent.join("real").join("a").join("b"),
            "components that do not exist yet are kept, in order, under the resolved prefix"
        );

        let link = parent.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(
            path_the_child_will_reach(&link.join("under")),
            canonical_parent.join("real").join("under"),
            "a link anywhere in the path is resolved before the walk sees it"
        );

        // A cycle. `canonicalize` answers ELOOP at every prefix that goes
        // through it, so the resolution walks out to the first prefix that does
        // not -- in bounded time, which is the only claim being made.
        let looped = parent.path().join("loop");
        std::os::unix::fs::symlink(&looped, &looped).unwrap();
        let reached = path_the_child_will_reach(&looped.join("x"));
        assert_eq!(reached, canonical_parent.join("loop").join("x"));
        assert!(
            DirectoryIdentity::of(&reached) == DirectoryIdentity::Unresolved,
            "and the walk's first question about it is one the kernel refuses, which is \
             reported as containment"
        );
        assert!(
            one_directory_contains_the_other(&real, &looped.join("x")),
            "a path pmux cannot resolve is CONTAINED by anything it is asked about, because a \
             wrong `disjoint` is the answer that leaks"
        );

        // Nothing resolvable at all: returned unchanged, and refused by the
        // same fail-closed arm.
        let relative = Path::new("no/such/relative/path");
        assert_eq!(path_the_child_will_reach(relative), relative.to_path_buf());
    }

    /// The production-reachable half of the same defect, on macOS.
    ///
    /// Both sides of the rule are `Path::canonicalize`d, which is exactly why
    /// the string comparison looked safe. MEASURED: `canonicalize` does not
    /// collapse the APFS firmlink namespace, so a cwd spelled through it and a
    /// root genuinely inside that cwd share no component prefix at all, and the
    /// rule that exists to keep a cell's transcripts out of its own file tools'
    /// reach was admitting them.
    #[cfg(target_os = "macos")]
    #[test]
    fn config_isolation_refuses_a_root_inside_a_cwd_spelled_through_the_firmlink_alias() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = owner_only_directory();
        let canonical = workspace.path().canonicalize().unwrap();
        let inside = canonical.join("private-root");
        fs::create_dir(&inside).unwrap();
        fs::set_permissions(&inside, fs::Permissions::from_mode(0o700)).unwrap();

        let firmlink = Path::new("/System/Volumes/Data").join(canonical.strip_prefix("/").unwrap());
        assert!(
            firmlink.is_dir(),
            "the firmlink alias must exist for this case to mean anything: {}",
            firmlink.display()
        );
        assert_eq!(
            inode_of(&firmlink),
            inode_of(&canonical),
            "the firmlink alias must be the same directory"
        );
        assert!(
            !inside.starts_with(&firmlink) && !firmlink.starts_with(&inside),
            "the premise: neither canonical spelling is a component prefix of the other, \
             which is why the string rule admitted this"
        );

        let mut request = request(&firmlink);
        request.config_isolation = Some(ConfigIsolation {
            root: inside.to_string_lossy().into_owned(),
        });
        let error = resolve_claude_launch(&request).unwrap_err().to_string();
        assert!(
            error.contains("may not contain one another"),
            "unexpected refusal: {error}"
        );
    }

    /// THE SECOND DOOR, for `cell: minified`.
    ///
    /// Refused on the CELL rather than on the isolation-conflict rule that
    /// makes it redundant today, and asserted with a `config_isolation` block
    /// PRESENT -- which is the shape that reaches it, and the shape in which
    /// the older rule would produce a different message. A minified cell with
    /// no isolation block at all is refused one gate earlier, by
    /// `a_minified_cell_is_refused_without_a_private_configuration_root`.
    #[cfg(unix)]
    #[test]
    fn a_minified_cell_may_not_reach_its_configuration_root_through_the_environment() {
        let cwd = owner_only_directory();
        let private = owner_only_directory();

        assert_eq!(
            CONFIG_ROOT_ENV_DOORS,
            [
                "CLAUDE_CONFIG_DIR",
                "CLAUDE_SECURESTORAGE_CONFIG_DIR",
                "HOME",
                "USERPROFILE",
                "XDG_CONFIG_HOME",
            ],
            "the door list is the rule; a name silently leaving it reopens one"
        );

        for key in CONFIG_ROOT_ENV_DOORS {
            let mut request = isolated(cwd.path(), private.path());
            request.cell = SessionCell::Minified;
            request
                .environment
                .set
                .insert((*key).to_owned(), "/caller/choice".to_owned());
            let error = resolve_claude_launch(&request).unwrap_err().to_string();
            assert!(
                error.contains("a minified cell may not set")
                    && error.contains(key)
                    && error.contains("config_isolation"),
                "{key}: the refusal must name the supported way: {error}"
            );
        }

        // The door is shut for a spelling nothing canonicalizes as well as for
        // a plausible one: the rule is about the NAME being present, so the
        // `..`-through-missing value that reached leak 5b never gets as far as
        // a directory question for a minified cell.
        let mut denormalized = isolated(cwd.path(), private.path());
        denormalized.cell = SessionCell::Minified;
        denormalized.environment.set.insert(
            "CLAUDE_CONFIG_DIR".to_owned(),
            format!("{}/NOPE/../victim", cwd.path().display()),
        );
        assert!(
            resolve_claude_launch(&denormalized)
                .unwrap_err()
                .to_string()
                .contains("a minified cell may not set")
        );

        // PATH A KEEPS THE DOOR. An ordinary cell setting the same name is not
        // refused by this rule; if it ever is, every un-isolated caller that
        // points one session at its own root stops working.
        let mut path_a = request(cwd.path());
        path_a
            .environment
            .set
            .insert("CLAUDE_CONFIG_DIR".to_owned(), "/caller/choice".to_owned());
        assert_eq!(path_a.cell, SessionCell::Full);
        let resolved = resolve_claude_launch(&path_a).expect("Path A keeps the environment door");
        assert_eq!(
            resolved
                .process
                .environment
                .variables
                .get("CLAUDE_CONFIG_DIR")
                .map(String::as_str),
            Some("/caller/choice")
        );

        // PATH A KEEPS THE THREE NEW NAMES TOO. Stated separately from
        // `CLAUDE_CONFIG_DIR` because they were added for leak 9 and a
        // too-broad fix would have taken the ordinary door with them -- `HOME`
        // in particular is in `INHERITED_EXACT_KEYS` and is how every un-isolated
        // caller reaches `~/.claude` at all.
        for key in ["HOME", "USERPROFILE", "XDG_CONFIG_HOME"] {
            let mut ordinary = request(cwd.path());
            ordinary
                .environment
                .set
                .insert(key.to_owned(), "/caller/home".to_owned());
            let resolved = resolve_claude_launch(&ordinary)
                .unwrap_or_else(|error| panic!("Path A must keep the {key} door: {error}"));
            assert_eq!(
                resolved
                    .process
                    .environment
                    .variables
                    .get(key)
                    .map(String::as_str),
                Some("/caller/home"),
                "{key} must reach an ordinary child unchanged"
            );
        }
    }

    /// LEAK 9: the door `effective_config_root`'s `else` branch could not see.
    ///
    /// `native::effective_config_root` reads `CLAUDE_CONFIG_DIR` **else**
    /// `HOME/.claude`. A minified cell always carries `config_isolation`, and
    /// `build_environment`'s step 6 always writes `CLAUDE_CONFIG_DIR` from it --
    /// so for a minified cell the `else` branch is unreachable and admission
    /// never examines `HOME` at all, whatever it points at.
    ///
    /// This test pins the MECHANISM rather than the symptom, because the symptom
    /// (a live victim's root) needs a daemon and the mechanism does not. Two
    /// facts together are the whole leak, and the second is what makes the first
    /// dangerous:
    ///
    /// 1. The delivered root comes from isolation and is NOT the `HOME`-derived
    ///    one, so no config-root rule anywhere downstream will look at `HOME`.
    /// 2. Without the request-key rule, `HOME` reaches the child verbatim.
    ///
    /// Asserted by removing the guard's reach rather than by trusting it: the
    /// same request is resolved as `cell: full`, which proves fact 1 and fact 2
    /// on real output, and then as `cell: minified`, which must refuse.
    #[cfg(unix)]
    #[test]
    fn a_minified_cell_may_not_redirect_home_past_the_configuration_root_rule() {
        let cwd = owner_only_directory();
        let private = owner_only_directory();
        let victim = owner_only_directory();
        let victim_home = victim.path().to_string_lossy().into_owned();

        // Path A, same request shape, admitted -- and this is the measurement
        // that shows why a rule keyed on the delivered root could never have
        // caught it.
        let mut ordinary = isolated(cwd.path(), private.path());
        ordinary
            .environment
            .set
            .insert("HOME".to_owned(), victim_home.clone());
        let resolved = resolve_claude_launch(&ordinary).expect("Path A keeps the door");
        let delivered = &resolved.process.environment.variables;
        assert_eq!(
            delivered.get("HOME").map(String::as_str),
            Some(victim_home.as_str()),
            "the child is handed the caller's HOME verbatim"
        );
        assert_ne!(
            delivered.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some(format!("{victim_home}/.claude").as_str()),
            "and the delivered configuration root is the ISOLATION root, so the \
             `HOME`-derived one admission would have judged is never computed"
        );
        assert_eq!(
            delivered.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some(
                private
                    .path()
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .as_ref()
            ),
            "the premise: step 6 supplies the root, so `effective_config_root` \
             takes its first branch and its `HOME` branch is dead"
        );

        // The same request as a minified cell must not get that far.
        let mut minified = isolated(cwd.path(), private.path());
        minified.cell = SessionCell::Minified;
        minified
            .environment
            .set
            .insert("HOME".to_owned(), victim_home);
        let error = resolve_claude_launch(&minified).unwrap_err().to_string();
        assert!(
            error.contains("a minified cell may not set")
                && error.contains("HOME")
                && error.contains("config_isolation"),
            "unexpected refusal: {error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn config_isolation_refuses_a_root_anyone_else_can_read() {
        use std::os::unix::fs::PermissionsExt;

        let cwd = owner_only_directory();
        let private = owner_only_directory();
        fs::set_permissions(private.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let error = resolve_claude_launch(&isolated(cwd.path(), private.path()))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("must have mode 0700") && error.contains("0755"),
            "unexpected refusal: {error}"
        );
        // pmux VERIFIES and refuses; it does not relabel a directory it did not
        // create.
        assert_eq!(
            fs::metadata(private.path()).unwrap().permissions().mode() & 0o777,
            0o755,
            "a refusal must not chmod the caller's directory"
        );
    }

    #[cfg(unix)]
    #[test]
    fn config_isolation_refuses_a_missing_or_non_directory_root() {
        let cwd = owner_only_directory();
        let private = owner_only_directory();

        let mut missing = request(cwd.path());
        missing.config_isolation = Some(ConfigIsolation {
            root: private
                .path()
                .join("never-created")
                .to_string_lossy()
                .into_owned(),
        });
        let error = resolve_claude_launch(&missing).unwrap_err().to_string();
        assert!(
            error.contains("config isolation root is unavailable"),
            "pmux must never create the root: {error}"
        );

        let file = private.path().join("not-a-directory");
        fs::write(&file, b"").unwrap();
        let mut wrong_kind = request(cwd.path());
        wrong_kind.config_isolation = Some(ConfigIsolation {
            root: file.to_string_lossy().into_owned(),
        });
        assert!(
            resolve_claude_launch(&wrong_kind)
                .unwrap_err()
                .to_string()
                .contains("config isolation root must be a directory")
        );

        let mut relative = request(cwd.path());
        relative.config_isolation = Some(ConfigIsolation {
            root: "relative/root".into(),
        });
        assert!(
            resolve_claude_launch(&relative)
                .unwrap_err()
                .to_string()
                .contains("config isolation root must be absolute")
        );

        let mut empty = request(cwd.path());
        empty.config_isolation = Some(ConfigIsolation {
            root: String::new(),
        });
        assert!(
            resolve_claude_launch(&empty)
                .unwrap_err()
                .to_string()
                .contains("must be non-empty")
        );
    }

    #[cfg(unix)]
    #[test]
    fn config_isolation_refuses_a_root_whose_config_json_would_shadow_the_seed() {
        // `lE()` prefers `<config dir>/.config.json` over `.claude.as_ref().expect("inline launch").json`, so a
        // root carrying one would accept the seed and then ignore it. The
        // failure would present as a turn-1 hang on the onboarding screen.
        let cwd = owner_only_directory();
        let private = owner_only_directory();
        fs::write(private.path().join(".config.json"), b"{}").unwrap();
        let error = resolve_claude_launch(&isolated(cwd.path(), private.path()))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(".config.json"),
            "unexpected refusal: {error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn without_isolation_nothing_about_the_launch_environment_changes() {
        let cwd = owner_only_directory();
        let mut request = request(cwd.path());
        request
            .environment
            .snapshot
            .insert("CLAUDE_CONFIG_DIR".into(), "/operator/root".into());
        // A caller-supplied securestorage pin stays legal and unfiltered
        // without isolation: it is a name a caller can deliver today, and
        // refusing it would be an unrelated behaviour change.
        request.environment.set.insert(
            "CLAUDE_SECURESTORAGE_CONFIG_DIR".into(),
            "/elsewhere".into(),
        );

        let launch = resolve_claude_launch(&request).unwrap();
        let variables = &launch.process.environment.variables;
        assert_eq!(
            variables.get("CLAUDE_CONFIG_DIR"),
            Some(&"/operator/root".to_owned())
        );
        assert_eq!(
            variables.get("CLAUDE_SECURESTORAGE_CONFIG_DIR"),
            Some(&"/elsewhere".to_owned())
        );
    }

    /// What pmux delivers to a minified cell that it delivers to nobody else.
    ///
    /// The table under test is [`MINIFIED_CELL_ENVIRONMENT`]; the reasons each
    /// name is in it, and the four measured-breaking names that are not, live
    /// on the constant. This asserts the three properties a caller can actually
    /// depend on:
    ///
    /// 1. A minified cell receives every name in the table.
    /// 2. An ordinary cell receives none of them. A cell-scoped delivery that
    ///    silently widened to every session would change what Path A callers
    ///    launch with, which is not this table's decision to make.
    /// 3. The table SURVIVES the transparent profile's denylist. Asserted with
    ///    the caller having offered the same name in `set`, because that is the
    ///    ordering that would break if step 7 were ever moved before step 5:
    ///    every name here is `CLAUDE_CODE_*`, and four `CLAUDE*` markers have
    ///    already been added to `TRANSPARENT_EXACT_KEYS` after live failures.
    #[cfg(unix)]
    #[test]
    fn a_minified_cell_is_launched_with_the_marketplace_autoinstall_suppressed() {
        let cwd = owner_only_directory();
        let private = owner_only_directory();

        assert_eq!(
            MINIFIED_CELL_ENVIRONMENT,
            [("CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL", "1")],
            "every name here was measured, one at a time, against a live cell"
        );

        let mut minified = isolated(cwd.path(), private.path());
        minified.cell = SessionCell::Minified;
        // Property 3: the caller's own value for the same name is overwritten
        // rather than merely surviving, so what the cell launches with does not
        // depend on what the caller happened to ask for.
        minified.environment.set.insert(
            "CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL".to_owned(),
            "0".to_owned(),
        );
        let delivered = resolve_claude_launch(&minified)
            .expect("a minified cell resolves")
            .process
            .environment
            .variables;
        for (key, value) in MINIFIED_CELL_ENVIRONMENT {
            assert_eq!(
                delivered.get(*key).map(String::as_str),
                Some(*value),
                "{key} must reach a minified cell's child with pmux's value"
            );
        }

        let ordinary = isolated(cwd.path(), private.path());
        assert_eq!(ordinary.cell, SessionCell::Full);
        let delivered = resolve_claude_launch(&ordinary)
            .expect("an ordinary isolated cell resolves")
            .process
            .environment
            .variables;
        for (key, _) in MINIFIED_CELL_ENVIRONMENT {
            assert_eq!(
                delivered.get(*key),
                None,
                "{key} is the minified cell's delivery, not every session's"
            );
        }
    }

    /// What pmux puts in a minified cell's ARGV that it puts in nobody else's.
    ///
    /// The sibling of the environment test above, and the same three
    /// properties, because the same two mistakes are available: a cell-scoped
    /// flag that silently widened to every session changes what Path A callers
    /// launch with, and a flag that is declared and never appended is the
    /// defect this constant exists to close -- three documents said pmux passed
    /// `--strict-mcp-config` while no launch path did.
    ///
    /// The spelling is asserted LITERALLY here, once. `MINIFIED_CELL_FLAGS`
    /// carries the measurement that chose it; every other assertion in the tree
    /// derives from the constant, and
    /// `stateless::tests::the_documented_minified_launch_bundle_is_the_argv_a_mint_emits`
    /// refuses any other code-tree file the right to restate it.
    #[cfg(unix)]
    #[test]
    fn a_minified_cell_is_launched_with_out_of_root_mcp_configuration_suppressed() {
        let cwd = owner_only_directory();
        let private = owner_only_directory();

        assert_eq!(
            MINIFIED_CELL_FLAGS,
            ["--strict-mcp-config"],
            "MEASURED at 2.1.226: without it a pristine minified cell fetches \
             the operator's account connector list over HTTP"
        );

        let mut minified = isolated(cwd.path(), private.path());
        minified.cell = SessionCell::Minified;
        // Property 3 in this test's terms: the caller's own extra arguments do
        // not decide what the cell is. Both of `SAFE_EXTRA_FLAGS` are offered,
        // so the cell's flags have to survive a non-empty `extra_args` rather
        // than only the empty case a pool mint happens to present.
        minified.claude.as_mut().expect("inline launch").extra_args =
            SAFE_EXTRA_FLAGS.iter().map(|f| (*f).to_owned()).collect();
        let args = resolve_claude_launch(&minified)
            .expect("a minified cell resolves")
            .process
            .args;
        for flag in MINIFIED_CELL_FLAGS {
            assert_eq!(
                args.iter().filter(|arg| arg.as_str() == *flag).count(),
                1,
                "{flag} must reach a minified cell's argv exactly once: {args:?}"
            );
        }

        let ordinary = isolated(cwd.path(), private.path());
        assert_eq!(ordinary.cell, SessionCell::Full);
        let args = resolve_claude_launch(&ordinary)
            .expect("an ordinary isolated cell resolves")
            .process
            .args;
        for flag in MINIFIED_CELL_FLAGS {
            assert!(
                !args.iter().any(|arg| arg == flag),
                "{flag} is the minified cell's argv, not every session's: {args:?}"
            );
        }

        // And a caller cannot ask for it: the extra-argument allowlist is the
        // only channel that could carry a bare flag, and it is closed. Stated
        // as a refusal rather than as a property of the allowlist's contents,
        // because the contents are what would change.
        for flag in MINIFIED_CELL_FLAGS {
            let mut asked = isolated(cwd.path(), private.path());
            asked.claude.as_mut().expect("inline launch").extra_args = vec![(*flag).to_owned()];
            let error = resolve_claude_launch(&asked)
                .expect_err("a driver-owned flag is not a caller's to name")
                .to_string();
            assert!(
                error.contains(flag) && error.contains("allowlist"),
                "the refusal must name the flag it refused: {error}"
            );
        }
    }

    /// The rule that keeps a minified cell out of the caller's own root, which
    /// until now only an `#[ignore]`d end-to-end test could reach -- so `cargo
    /// test` stayed green with it deleted.
    #[cfg(unix)]
    #[test]
    fn a_minified_cell_is_refused_without_a_private_configuration_root() {
        let cwd = owner_only_directory();
        let private = owner_only_directory();
        let expected = "a minified cell requires config_isolation";

        let mut bare = request(cwd.path());
        bare.cell = SessionCell::Minified;
        let error = resolve_claude_launch(&bare).unwrap_err().to_string();
        assert!(error.contains(expected), "unexpected refusal: {error}");

        // Naming a private-looking root through the environment is not asking
        // for isolation. It is the shape that reached the config-root leak, and
        // it buys nothing here: without a `config_isolation` block there is no
        // securestorage pin, no seed, and no admission rule that treats the
        // directory as pmux's to manage.
        let mut by_environment = request(cwd.path());
        by_environment.cell = SessionCell::Minified;
        by_environment.environment.set.insert(
            "CLAUDE_CONFIG_DIR".into(),
            private.path().to_string_lossy().into_owned(),
        );
        let error = resolve_claude_launch(&by_environment)
            .unwrap_err()
            .to_string();
        assert!(error.contains(expected), "unexpected refusal: {error}");

        // The same cell with a private root resolves, so the refusals above are
        // about the missing root and not about the cell being unlaunchable.
        let mut isolated_cell = isolated(cwd.path(), private.path());
        isolated_cell.cell = SessionCell::Minified;
        resolve_claude_launch(&isolated_cell).unwrap();
    }

    // -----------------------------------------------------------------------
    // Guards found by cargo-mutants, not by reading
    // -----------------------------------------------------------------------

    /// The team-marker refusal is ARMED on the map handed to the child.
    ///
    /// SURVIVING MUTANT CLOSED: `reject_team_markers_reaching_child -> Ok(())`
    /// -- the whole guard deleted, and the suite green. Its doc says "a future
    /// policy change that stops stripping one re-arms the refusal
    /// automatically", which is a claim about THIS function; every test that
    /// existed exercised its sibling, the one that refuses a marker in
    /// `environment.set` before resolution, so nothing ever established that
    /// the second gate does anything at all.
    ///
    /// Called directly, because reaching it through `resolve_claude_launch`
    /// requires a marker name that survives the allowlist AND the transparent
    /// profile's removals -- which is exactly the future policy change the
    /// guard exists to survive, and which cannot be constructed today.
    #[test]
    fn a_team_marker_on_the_resolved_map_is_refused_however_it_got_there() {
        // Every spelling `is_team_marker` claims, derived from the two
        // substrings it names rather than restated: the check is
        // case-insensitive and matches anywhere in the name.
        for marker in [
            "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS",
            "AGENT_TEAM",
            "agent_team_id",
            "X_TEAMMATE",
            "teammate",
            "PREFIX_AGENT_TEAM_SUFFIX",
        ] {
            let variables = BTreeMap::from([
                (marker.to_owned(), "1".to_owned()),
                ("PATH".into(), "/bin".into()),
            ]);
            let error = reject_team_markers_reaching_child(&variables)
                .expect_err("a team marker reaching the child is refused")
                .to_string();
            assert!(
                error.contains("agent-team and teammate environment variables are forbidden"),
                "{marker}: {error}"
            );
            assert!(
                is_team_marker(marker),
                "the fixture must be a marker: {marker}"
            );
        }
        // ...and an ordinary map passes, so the guard is a filter and not a wall.
        reject_team_markers_reaching_child(&BTreeMap::from([
            ("PATH".to_owned(), "/bin".to_owned()),
            ("HOME".to_owned(), "/home/x".to_owned()),
            ("TEAM_SIZE".to_owned(), "4".to_owned()),
        ]))
        .expect("a map with no marker is admitted");
    }

    /// Each forbidden shape of an argv VALUE is refused on its own.
    ///
    /// SURVIVING MUTANT CLOSED: `push_value:740 || -> &&`. The three clauses
    /// were only ever exercised together, and `||` and `&&` agree on an input
    /// that satisfies all three.
    #[test]
    fn an_argv_value_is_refused_for_each_forbidden_shape_separately() {
        for value in ["", "has\0nul", "-looks-like-an-option"] {
            let mut args = Vec::new();
            let error = push_value(&mut args, "--flag", value)
                .expect_err("a forbidden argv value is refused")
                .to_string();
            assert!(error.contains("--flag value must be"), "{value:?}: {error}");
            assert!(args.is_empty(), "a refused value must push nothing");
        }
        let mut args = Vec::new();
        push_value(&mut args, "--flag", "/ordinary/value").expect("an ordinary value is pushed");
        assert_eq!(
            args,
            vec!["--flag".to_owned(), "/ordinary/value".to_owned()]
        );
    }

    /// An executable bit in ANY of the three positions is enough, and none is
    /// not.
    ///
    /// **CLOSES NO SURVIVING MUTANT, and says so rather than claiming one.**
    /// This doc read `SURVIVING MUTANT CLOSED: is_executable -> false` until
    /// that was checked against the run instead of remembered from it. The
    /// `#[cfg(unix)]` `is_executable` is one of the best-covered predicates in
    /// the file: all five of its mutants were CAUGHT by the suite that already
    /// existed -- `-> false` reddened 24 tests and `-> true` reddened two, both
    /// through `resolve_claude_launch`. The only `is_executable` mutant that
    /// SURVIVED is at `claude_launch.rs:1216`, which is the `#[cfg(not(unix))]`
    /// twin: not compiled on this host, so its mutant is byte-identical to the
    /// baseline and no test on any macOS or Linux run can ever kill it. It is
    /// triaged as unreachable-by-design in `docs/current-state.md` §9.23, not
    /// closed here.
    ///
    /// It is kept because it is worth keeping on its own terms: it asks the
    /// predicate DIRECTLY about each of the three bit positions and about their
    /// absence, where every pre-existing case reached it through a launch of
    /// `/bin/sh` at `0o755`. That is a statement no mutant was going to make,
    /// which is the honest reason for a test that closes nothing.
    #[cfg(unix)]
    #[test]
    fn the_executable_bit_is_read_in_every_position_and_its_absence_refuses() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("candidate");
        fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();

        for mode in [0o100, 0o010, 0o001, 0o755] {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600 | mode)).unwrap();
            assert!(
                is_executable(&fs::metadata(&path).unwrap()),
                "mode {mode:o} is executable to somebody"
            );
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            !is_executable(&fs::metadata(&path).unwrap()),
            "a file with no executable bit anywhere is not executable"
        );
        // And the refusal reaches a caller, spelled the way the launch spells
        // it, rather than staying a private predicate nobody consults.
        let error =
            canonical_absolute(&path, "Claude executable", RequiredPathKind::ExecutableFile)
                .expect_err("a non-executable file is not a Claude executable")
                .to_string();
        assert!(error.contains("is not executable"), "{error}");
    }
}
