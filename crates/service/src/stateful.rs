//! Full-cell one-shot: default Claude Code tools in a caller-named cwd.
//!
//! Not the minified pool. Isolation root and Claude binary stay daemon
//! configuration. `cwd` is the only resource the request may name.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use pseudomux_protocol::v1::{
    AuthPolicy, ClaudeLaunchConfig, ClosePolicy, CloseSessionRequest, CompatibilityPolicy,
    ConfigIsolation, EnvironmentSpec, ErrorBody, ErrorCode, LifecycleMode, PermissionMode,
    RetentionPolicy, RunStatefulRequest, SessionCell, SessionIdentity, StartSessionRequest,
    StatelessResult, SystemPromptPolicy, TurnLeasePolicy, TurnOutcome, TurnRequest,
};
use serde_json::json;
use uuid::Uuid;

use crate::claude_launch::directory_lies_within;
use crate::driver_io::validate_prompt;
use crate::native::{NativeService, unix_now_ms};
use crate::pool::{AccountName, PoolConfig, path_b_not_enabled, resolve_pool_class};
use crate::private_dir::create_private_dir_all;
use crate::stateless::{POOL_TERMINAL, daemon_environment_snapshot};
use crate::v1::{DriverFailure, SessionOwner};

const STATEFUL_IDLE_TTL_MS: u64 = 600_000;

pub async fn run_stateful(
    service: &Arc<NativeService>,
    request: RunStatefulRequest,
) -> Result<StatelessResult, ErrorBody> {
    if !service.stateful_enabled() {
        return Err(stateful_not_enabled());
    }
    let Some(pool_config) = service.pool_config() else {
        return Err(path_b_not_enabled());
    };
    let prompt = validate_prompt(&request.prompt).map_err(DriverFailure::into_protocol)?;
    require_skip_permissions(request.permission_mode)?;
    let cwd = validate_stateful_cwd(&request.cwd, &pool_config.parent_dir)?;
    let account = account_from_name(request.account.as_deref(), pool_config)?;
    let pin = pool_config
        .pin_for(account)
        .expect("has_account already checked")
        .to_owned();
    let (class, resolved) = resolve_pool_class(&request.model, request.effort)
        .map_err(crate::pool::ModelEffortRefusal::into_error_body)?;
    let now_ms = unix_now_ms()?;
    let deadline = pool_config.effective_deadline_ms(now_ms, request.deadline_unix_ms);
    let claude = pool_config.claude_executable.clone();

    let session_uuid = Uuid::new_v4();
    let session_dir = pool_config
        .parent_dir
        .join("stateful")
        .join(session_uuid.to_string());
    let isolation_root = session_dir.join("root");
    // Guard before mkdir so a partial tree is still erased. Drop runs after
    // Force-close below, never while a mint is still in the registry.
    let _isolation = IsolationTree(session_dir);
    create_private_dir_all(&isolation_root).map_err(|error| {
        ErrorBody::new(
            ErrorCode::InvalidConfig,
            format!("could not create stateful isolation root: {error:#}"),
        )
    })?;

    let environment = daemon_environment_snapshot()?;
    let start = stateful_launch_request(
        &cwd,
        &isolation_root,
        &claude,
        class.canonical_model,
        resolved.effort_level,
        &pin,
        &environment,
    );
    let handle = service
        .start_session_owned(start, SessionOwner::Caller)
        .await?;
    // Full chrome (default prompt, tools, first-launch marketplace) can still
    // be painting when start returns ready. The 15s input gate then refuses
    // PromptNotAcknowledged. Measured macos 2.1.272: first mint missed the
    // composer proof; a subsequent mint on the same daemon completed.
    tokio::time::sleep(Duration::from_millis(2_000)).await;
    let turn_id = Uuid::new_v4();
    let turn = TurnRequest {
        turn_id,
        prompt,
        deadline_unix_ms: Some(deadline),
        lease: TurnLeasePolicy::default(),
    };
    let turn_result = async {
        let actor = service
            .registry()
            .actor(handle.session_id, handle.generation_id)
            .await?;
        let transcript_drain_ms = actor.snapshot().await?.compatibility.transcript_drain_ms;
        match actor.submit_turn(turn).await {
            Ok(_) => {
                service
                    .wait_for_turn(&actor, turn_id, Some(deadline), transcript_drain_ms)
                    .await
            }
            Err(error) => Err(error),
        }
    }
    .await;
    if let Err(error) = service
        .close_session_owned(
            SessionOwner::Caller,
            CloseSessionRequest {
                session_id: handle.session_id,
                generation_id: handle.generation_id,
                policy: ClosePolicy::Force,
            },
        )
        .await
    {
        tracing::warn!(
            session_id = %handle.session_id,
            error = %error.message,
            "could not force-close stateful session"
        );
    }
    // IsolationTree Drop erases the uuid dir. Copy JSONL out first so a
    // Harbor wrap can collect it after Force-close.
    harvest_stateful_transcripts(&isolation_root, &pool_config.parent_dir.join("transcripts"));
    let result = turn_result?;
    if result.outcome != TurnOutcome::Completed {
        return Err(ErrorBody::new(
            ErrorCode::ClaudeExited,
            format!(
                "the stateful turn reached a terminal whose outcome was {:?} rather than completed",
                result.outcome
            ),
        ));
    }
    Ok(StatelessResult {
        model: class.canonical_model.to_owned(),
        reported_model: result.model,
        effort: resolved.effort_level,
        text: result.text,
        stop_reason: result.stop_reason,
        usage: result.usage,
        claude_version: result.claude_version,
    })
}

#[must_use]
pub fn stateful_not_enabled() -> ErrorBody {
    ErrorBody::new(
        ErrorCode::UnsupportedFeature,
        "stateful Full cells are not enabled: start pmuxd with --stateful and --pool-parent",
    )
    .with_details(json!({"violation": "stateful_not_enabled"}))
}

pub(crate) fn require_skip_permissions(mode: Option<PermissionMode>) -> Result<(), ErrorBody> {
    match mode {
        Some(PermissionMode::DangerouslySkipPermissions) => Ok(()),
        other => Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            "unattended Full cells require permission_mode=dangerously_skip_permissions \
             (otherwise the TUI blocks on a permission prompt)",
        )
        .with_details(json!({
            "violation": "permission_mode_required",
            "got": other.map(|mode| format!("{mode:?}")),
        }))),
    }
}

pub(crate) fn validate_stateful_cwd(cwd: &str, pool_parent: &Path) -> Result<PathBuf, ErrorBody> {
    if cwd.is_empty() || !cwd.starts_with('/') || cwd.contains('\0') {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            format!("stateful cwd must be an absolute path; got {cwd:?}"),
        )
        .with_details(json!({"violation": "cwd_not_absolute"})));
    }
    let path = PathBuf::from(cwd);
    let canonical = path.canonicalize().map_err(|error| {
        ErrorBody::new(
            ErrorCode::InvalidConfig,
            format!("stateful cwd {cwd:?} is not an existing directory: {error}"),
        )
        .with_details(json!({"violation": "cwd_missing"}))
    })?;
    if !canonical.is_dir() {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            format!("stateful cwd {cwd:?} is not a directory"),
        )
        .with_details(json!({"violation": "cwd_not_directory"})));
    }
    if directory_lies_within(pool_parent, &canonical) || directory_lies_within(pool_parent, &path) {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            "stateful cwd must not be under the pool parent (minified slot trees)",
        )
        .with_details(json!({"violation": "cwd_inside_pool_parent"})));
    }
    Ok(canonical)
}

/// The size budget for the whole harvest directory.
///
/// Unlike the Path B evidence mirror next door, what lands here is the FULL
/// transcript -- the caller's prompts and the model's completions, not a
/// mirror pruned to eight content-free fields -- so an unbounded directory
/// under the pool parent is a growing pile of conversation content that
/// nothing ever deletes. It is bounded the way
/// [`crate::pool::evidence::prune`] bounds that mirror, with the same
/// oldest-first rule, and for the same reason.
///
/// Four times [`crate::pool::evidence::MAX_EVIDENCE_BYTES`], because these
/// files carry content and that one's do not. A Harbor A/B wrap copies this
/// directory out per trial, which is why the harvest is on by default and
/// why the budget is generous rather than tight: the ceiling is there so an
/// unattended daemon cannot fill a disk, not to keep the directory small.
pub const MAX_HARVEST_BYTES: u64 = 256 * 1024 * 1024;

/// Copy each `root/projects/<slug>/*.jsonl` to `{dest}/{slug}__{file}` so
/// Force-close erase does not take the only copy. Best-effort: a harvest
/// miss must not change the turn's error.
///
/// After each harvest the directory is pruned to [`MAX_HARVEST_BYTES`],
/// oldest-first. This turn's own files are the newest in it, so a wrap
/// collecting them after the turn still finds them; what ages out is an
/// earlier trial's.
fn harvest_stateful_transcripts(isolation_root: &Path, dest: &Path) {
    harvest_stateful_transcripts_to_budget(isolation_root, dest, MAX_HARVEST_BYTES);
}

/// [`harvest_stateful_transcripts`] with the budget named, so a test can state
/// the oldest-first rule without writing 256 MiB to prove it.
fn harvest_stateful_transcripts_to_budget(isolation_root: &Path, dest: &Path, budget: u64) {
    let projects = isolation_root.join("projects");
    let Ok(entries) = fs::read_dir(&projects) else {
        return;
    };
    if let Err(error) = create_private_dir_all(dest) {
        tracing::warn!(
            path = %dest.display(),
            error = %error,
            "could not create stateful transcript harvest directory"
        );
        return;
    }
    for entry in entries.flatten() {
        let project = entry.path();
        if !project.is_dir() {
            continue;
        }
        let Ok(files) = fs::read_dir(&project) else {
            continue;
        };
        let slug = entry.file_name();
        for file in files.flatten() {
            let path = file.path();
            if !path.is_file()
                || !path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"))
            {
                continue;
            }
            let mut name = slug.clone();
            name.push("__");
            name.push(file.file_name());
            if let Err(error) = fs::copy(&path, dest.join(&name)) {
                tracing::warn!(
                    from = %path.display(),
                    error = %error,
                    "could not harvest stateful transcript"
                );
            }
        }
    }
    let pruned = crate::pool::evidence::prune(dest, budget);
    if pruned > 0 {
        tracing::info!(
            path = %dest.display(),
            pruned,
            budget_bytes = budget,
            "pruned harvested Full-cell transcripts to stay under the budget"
        );
    }
}

struct IsolationTree(PathBuf);

impl Drop for IsolationTree {
    fn drop(&mut self) {
        match fs::remove_dir_all(&self.0) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(
                    path = %self.0.display(),
                    error = %error,
                    "could not erase stateful isolation tree"
                );
            }
        }
    }
}

fn account_from_name(raw: Option<&str>, config: &PoolConfig) -> Result<AccountName, ErrorBody> {
    let name = match raw {
        None | Some("") => AccountName::DEFAULT,
        Some(raw) => AccountName::parse(raw).map_err(|error| {
            ErrorBody::new(ErrorCode::InvalidConfig, error.to_string())
                .with_details(json!({"violation": "invalid_account_name"}))
        })?,
    };
    if config.has_account(name) {
        Ok(name)
    } else {
        let known = config
            .accounts
            .iter()
            .map(|account| account.name.to_string())
            .collect::<Vec<_>>();
        Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            format!(
                "account {:?} is not configured on this daemon; configured accounts: {}",
                name.as_str(),
                known.join(", ")
            ),
        )
        .with_details(json!({
            "violation": "unknown_account",
            "account": name.as_str(),
            "configured": known,
        })))
    }
}

fn stateful_launch_request(
    cwd: &Path,
    isolation_root: &Path,
    claude: &Path,
    model: &str,
    effort: Option<pseudomux_protocol::v1::EffortLevel>,
    pin: &str,
    environment: &EnvironmentSpec,
) -> StartSessionRequest {
    StartSessionRequest {
        identity: SessionIdentity::New { session_id: None },
        cwd: cwd.to_string_lossy().into_owned(),
        agent: None,
        claude: Some(ClaudeLaunchConfig {
            executable: claude.to_string_lossy().into_owned(),
            model: Some(model.to_owned()),
            effort,
            permission_mode: Some(PermissionMode::DangerouslySkipPermissions),
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            settings: Vec::new(),
            mcp_configs: Vec::new(),
            plugin_dirs: Vec::new(),
            system_prompt: SystemPromptPolicy::Default,
            extra_args: Vec::new(),
        }),
        environment: environment.clone(),
        auth_policy: AuthPolicy::Subscription,
        config_isolation: Some(ConfigIsolation {
            root: isolation_root.to_string_lossy().into_owned(),
            securestorage_dir: pin.to_owned(),
        }),
        terminal: POOL_TERMINAL,
        lifecycle: LifecycleMode::Transcript,
        retention: RetentionPolicy::Persistent {
            idle_ttl_ms: STATEFUL_IDLE_TTL_MS,
        },
        compatibility: CompatibilityPolicy::RequireTested,
        cell: SessionCell::Full,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn relative_cwd_is_refused() {
        let error = validate_stateful_cwd("relative", Path::new("/tmp/pool")).unwrap_err();
        assert_eq!(error.details["violation"], "cwd_not_absolute");
    }

    #[test]
    fn cwd_under_pool_parent_is_refused() {
        let parent = tempfile::tempdir().unwrap();
        let nested = parent.path().join("slot");
        fs::create_dir(&nested).unwrap();
        let error = validate_stateful_cwd(nested.to_str().unwrap(), parent.path()).unwrap_err();
        assert_eq!(error.details["violation"], "cwd_inside_pool_parent");
    }

    #[test]
    fn cwd_that_is_the_pool_parent_is_refused() {
        let parent = tempfile::tempdir().unwrap();
        let error =
            validate_stateful_cwd(parent.path().to_str().unwrap(), parent.path()).unwrap_err();
        assert_eq!(error.details["violation"], "cwd_inside_pool_parent");
    }

    #[test]
    fn cwd_reached_through_a_symlink_into_the_pool_parent_is_refused() {
        let parent = tempfile::tempdir().unwrap();
        let nested = parent.path().join("slot");
        fs::create_dir(&nested).unwrap();
        let alias_root = tempfile::tempdir().unwrap();
        let alias = alias_root.path().join("via-link");
        std::os::unix::fs::symlink(&nested, &alias).unwrap();
        let error = validate_stateful_cwd(alias.to_str().unwrap(), parent.path()).unwrap_err();
        assert_eq!(error.details["violation"], "cwd_inside_pool_parent");
    }

    #[test]
    fn missing_and_non_directory_cwd_are_refused() {
        let parent = tempfile::tempdir().unwrap();
        let missing = parent.path().join("no-such-dir");
        let error =
            validate_stateful_cwd(missing.to_str().unwrap(), Path::new("/tmp/pool")).unwrap_err();
        assert_eq!(error.details["violation"], "cwd_missing");

        let file = parent.path().join("not-a-dir");
        fs::write(&file, b"x").unwrap();
        let error =
            validate_stateful_cwd(file.to_str().unwrap(), Path::new("/tmp/pool")).unwrap_err();
        assert_eq!(error.details["violation"], "cwd_not_directory");
    }

    #[test]
    fn skip_permissions_is_required() {
        assert!(require_skip_permissions(None).is_err());
        assert!(require_skip_permissions(Some(PermissionMode::DontAsk)).is_err());
        assert!(require_skip_permissions(Some(PermissionMode::DangerouslySkipPermissions)).is_ok());
    }

    #[test]
    fn full_launch_is_not_a_minified_pool_mint() {
        let environment = EnvironmentSpec {
            snapshot: Default::default(),
            set: Default::default(),
            unset: Default::default(),
        };
        let isolation = Path::new("/tmp/pool/stateful/deadbeef/root");
        let request = stateful_launch_request(
            Path::new("/tmp/task"),
            isolation,
            Path::new("/usr/local/bin/claude"),
            "claude-sonnet-5",
            Some(pseudomux_protocol::v1::EffortLevel::Low),
            "/Users/me/.claude-1",
            &environment,
        );
        assert_eq!(request.cell, SessionCell::Full);
        assert_eq!(request.cwd, "/tmp/task");
        assert_eq!(request.agent, None);
        assert_eq!(
            request.config_isolation.as_ref().map(|c| c.root.as_str()),
            Some("/tmp/pool/stateful/deadbeef/root")
        );
        assert_eq!(
            request
                .config_isolation
                .as_ref()
                .map(|c| c.securestorage_dir.as_str()),
            Some("/Users/me/.claude-1")
        );
        let claude = request.claude.expect("inline launch");
        assert!(claude.denied_tools.is_empty());
        assert!(claude.allowed_tools.is_empty());
        assert!(claude.mcp_configs.is_empty());
        assert_eq!(claude.system_prompt, SystemPromptPolicy::Default);
        assert_eq!(
            claude.permission_mode,
            Some(PermissionMode::DangerouslySkipPermissions)
        );
        assert_eq!(request.auth_policy, AuthPolicy::Subscription);
        assert_eq!(request.lifecycle, LifecycleMode::Transcript);
    }

    #[test]
    fn harvest_copies_project_jsonl_out_of_isolation() {
        let parent = tempfile::tempdir().unwrap();
        let isolation = parent.path().join("stateful").join("deadbeef").join("root");
        let project = isolation.join("projects").join("-app-repo");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("session.jsonl"), b"{\"type\":\"assistant\"}\n").unwrap();
        let dest = parent.path().join("transcripts");
        harvest_stateful_transcripts(&isolation, &dest);
        let harvested = dest.join("-app-repo__session.jsonl");
        assert_eq!(
            fs::read_to_string(&harvested).unwrap(),
            "{\"type\":\"assistant\"}\n"
        );
        {
            let _guard = IsolationTree(isolation.parent().unwrap().to_path_buf());
        }
        assert!(!isolation.exists());
        assert!(harvested.exists());
    }

    #[test]
    fn isolation_tree_erase_removes_the_uuid_dir() {
        let parent = tempfile::tempdir().unwrap();
        let session_dir = parent.path().join("stateful").join("deadbeef");
        let root = session_dir.join("root");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("marker"), b"x").unwrap();
        {
            let _guard = IsolationTree(session_dir.clone());
            assert!(root.exists());
        }
        assert!(!session_dir.exists());
        assert!(parent.path().join("stateful").exists());
    }

    /// The harvest is bounded, and the bound takes the OLDEST file.
    ///
    /// The directory holds full transcripts and a Harbor wrap collects it per
    /// trial, so the property that matters is not "it is small" but "the turn
    /// that just ran still finds its own files". Written against a tiny
    /// budget rather than [`MAX_HARVEST_BYTES`] so the test states the rule
    /// instead of the constant.
    /// Set one file's mtime to an exact second, so the test can state which
    /// file is older instead of hoping. Same idiom as `pool::evidence`'s own
    /// prune tests.
    fn set_mtime_seconds(path: &Path, seconds: i64) {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let raw = CString::new(path.as_os_str().as_bytes()).unwrap();
        let times = [
            libc::timespec {
                tv_sec: seconds as libc::time_t,
                tv_nsec: 0,
            },
            libc::timespec {
                tv_sec: seconds as libc::time_t,
                tv_nsec: 0,
            },
        ];
        // SAFETY: `raw` is a NUL-terminated path this test just created and
        // `times` is a two-element array of the shape utimensat expects.
        #[allow(unsafe_code)]
        let result = unsafe { libc::utimensat(libc::AT_FDCWD, raw.as_ptr(), times.as_ptr(), 0) };
        assert_eq!(result, 0, "could not set the mtime of {}", path.display());
    }

    #[test]
    fn the_harvest_prunes_oldest_first_and_keeps_this_turn() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("transcripts");
        fs::create_dir_all(&dest).unwrap();

        let old = dest.join("old__a.jsonl");
        fs::write(&old, vec![b'x'; 4096]).unwrap();
        // An mtime far enough back that "oldest" is a fact and not a race.
        set_mtime_seconds(&old, 1_000);

        let isolation = root.path().join("isolation");
        let project = isolation.join("projects").join("slug");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("turn.jsonl"), vec![b'y'; 1024]).unwrap();

        harvest_stateful_transcripts_to_budget(&isolation, &dest, 2048);

        assert!(
            dest.join("slug__turn.jsonl").exists(),
            "this turn's transcript was harvested"
        );
        assert!(
            !old.exists(),
            "the older file was pruned, because the two together are over a \
             budget this small"
        );
    }
}
