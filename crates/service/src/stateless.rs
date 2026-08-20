//! The integration: the stateless pool over a real `NativeService`.
//!
//! [`crate::pool`] owns admission, the class key, slots, epochs, the filesystem
//! roots and the state machine, and owns no process. This module is the other
//! half -- the one that touches a child, a TUI, a transcript and the session
//! registry -- and it is the whole of what `pool::host`'s
//! "what the integration step must provide" asked for.
//!
//! # The one property everything here exists to preserve
//!
//! **The caller names no resource.** Every field of [`MintSpec`] is daemon
//! configuration plus a slot identity; not one byte of a `RunStatelessRequest`
//! reaches a path, an environment variable or a system prompt. That is not an
//! aesthetic: nine leaks in this codebase were each reachable only because a
//! caller could name a resource pmux also used, and a caller who cannot name a
//! resource cannot alias one.
//!
//! Read [`NativeInstanceHost::mint`] with that in mind -- the request is not in
//! scope there, and cannot be.

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use pseudomux_protocol::v1::{
    AuthPolicy, ClaudeLaunchConfig, ClosePolicy, CloseSessionRequest, CompatibilityPolicy,
    ConfigIsolation, EnvironmentSpec, ErrorBody, ErrorCode, InputTransport, LifecycleMode,
    PermissionMode, RetentionPolicy, RunStatelessRequest, SessionCell, SessionIdentity,
    StartSessionRequest, StatelessResult, SystemPromptPolicy, TerminalProfile, TerminalSpec,
    TurnLeasePolicy, TurnOutcome, TurnRequest,
};

use crate::native::NativeService;
use crate::pool::{
    ClearFailure, Destroyed, HostFailure, HostTurn, InstanceHandle, InstanceHost, MintSpec, Pool,
};
use crate::v1::SessionOwner;

/// The pane geometry every pool instance is launched at.
///
/// CHOSEN, not measured, and it is the same 24x120 `TerminalSpec::default()`
/// already gives every other pmux launch. It is stated here as a constant
/// rather than taken from the default so that a change to the default cannot
/// silently move the geometry the composer gate was calibrated against -- the
/// gate measures from the last rendered row, and its dual growth law was fitted
/// on screens this wide.
pub const POOL_TERMINAL: TerminalSpec = TerminalSpec {
    rows: 24,
    cols: 120,
    profile: TerminalProfile::Transparent,
    input_transport: InputTransport::Sdk,
};

/// The compatibility policy every pool mint is admitted under.
///
/// A minified cell is admitted on a tested profile only, and `register`
/// enforces that before an actor exists. `AllowUntested` would let a pool
/// instance run on a Claude whose composer geometry pmux has never measured,
/// which is the one input the fast path trusts.
///
/// A CONSTANT, and public, because [`launch_request_for`] is no longer its only
/// reader: the daemon's health tree asks the compatibility registry whether the
/// Claude this pool would launch is admissible, and it has to ask under the
/// policy a mint asks under. Two copies of that policy is a health report free
/// to answer `exercised` while every mint is refused -- which is what shipped.
pub const POOL_COMPATIBILITY: CompatibilityPolicy = CompatibilityPolicy::RequireTested;

/// The cell every pool instance is launched as. Read by the same two callers
/// [`POOL_COMPATIBILITY`] is, and for the same reason: `SessionCell::Minified`
/// is what turns an untested profile into `require_tested_for_minified_cell`'s
/// refusal.
pub const POOL_CELL: SessionCell = SessionCell::Minified;

/// The tool-surface denial that makes a Path B cell isolated.
///
/// `"*"` removes tools, subagents and bundled skills -- MEASURED at ~29,000
/// tokens of context in the foundation work. It is also what makes the pool's
/// sidechain guard meaningful: with no tool surface a `Task` subagent is
/// structurally unreachable, so a sidechain row is evidence that this denial
/// did not take effect.
const DENY_EVERY_TOOL: &str = "*";

/// A `NativeService`-backed [`InstanceHost`].
///
/// Holds the service WEAKLY. `NativeService` owns the [`Pool`], the pool owns
/// its `Arc<dyn InstanceHost>`, and this is that host -- a strong reference back
/// would close the cycle and leak the whole service, its registry, every live
/// pane and the private runtime, on every daemon that ever built one.
///
/// A dead weak pointer is reported as [`ErrorCode::DaemonLost`] and never as a
/// success: the service being gone means the child this call would have talked
/// to is gone with it.
pub struct NativeInstanceHost {
    service: Weak<NativeService>,
    /// The DAEMON's own process environment, captured once when the pool was
    /// built.
    ///
    /// This is the one thing a mint takes that is not in [`MintSpec`], and it is
    /// still not a caller input: it is the environment the operator started
    /// `pmuxd` under, which is daemon configuration in the same sense
    /// `--pool-claude` is. Nothing on the wire can put a byte in it.
    ///
    /// It is captured ONCE, at pool construction, rather than read per mint.
    /// `std::env::set_var` is process-global and unsafe, and a pool whose
    /// instances were minted under different environments would have instances
    /// of one class that are not fungible -- which is the one thing the class
    /// key exists to guarantee.
    ///
    /// It is needed, and the live probe is what established that: with an empty
    /// snapshot the child gets no `HOME` and no `PATH`, and the first turn
    /// returned `needs_login`. The environment still goes through
    /// `build_environment`'s allowlist, `AuthPolicy::Subscription`'s removals
    /// and the transparent profile's denylist exactly as a Path A start's does,
    /// and step 6 still overwrites `CLAUDE_CONFIG_DIR` with the pool's own root.
    daemon_environment: EnvironmentSpec,
}

impl NativeInstanceHost {
    /// # Errors
    ///
    /// Returns `invalid_config` when this process's environment is not UTF-8.
    /// Refused rather than lossily converted: a lossy conversion changes the
    /// launch, and a silently dropped entry changes it invisibly.
    pub fn new(service: &Arc<NativeService>) -> Result<Self, ErrorBody> {
        Ok(Self {
            service: Arc::downgrade(service),
            daemon_environment: daemon_environment_snapshot()?,
        })
    }

    fn service(&self) -> Result<Arc<NativeService>, ErrorBody> {
        self.service.upgrade().ok_or_else(|| {
            ErrorBody::new(
                ErrorCode::DaemonLost,
                "the native service that owns this stateless pool has been dropped",
            )
        })
    }
}

/// This process's environment, or a refusal.
///
/// The daemon's own `std::env::vars_os`, converted exactly. Every name in it is
/// still filtered by `build_environment`'s allowlist before it reaches a child.
fn daemon_environment_snapshot() -> Result<EnvironmentSpec, ErrorBody> {
    let mut snapshot = std::collections::BTreeMap::new();
    for (key, value) in std::env::vars_os() {
        let key = key.into_string().map_err(|_| {
            ErrorBody::new(
                ErrorCode::InvalidConfig,
                "the daemon's environment carries a non-UTF-8 variable name, so the exact launch                  snapshot the stateless pool needs cannot be taken",
            )
        })?;
        let value = value.into_string().map_err(|_| {
            ErrorBody::new(
                ErrorCode::InvalidConfig,
                format!(
                    "the daemon's environment variable {key} has a non-UTF-8 value, so the exact                      launch snapshot the stateless pool needs cannot be taken"
                ),
            )
        })?;
        snapshot.insert(key, value);
    }
    Ok(EnvironmentSpec {
        snapshot,
        // EMPTY, and they stay empty. `set` bypasses `build_environment`'s
        // allowlist entirely -- it is the explicit channel -- and `unset` is a
        // caller patch. The pool has no caller to take either from, and adding
        // one here would be pmux inventing a request byte on a caller's behalf.
        set: std::collections::BTreeMap::new(),
        unset: std::collections::BTreeSet::new(),
    })
}

/// Build the launch request for one mint, from the spec and the daemon's own
/// environment, and nothing else.
///
/// A free function taking a [`MintSpec`] and an [`EnvironmentSpec`] on purpose.
/// It is the whole of "pmux mints every resource", and a free function is the
/// form in which that claim is CHECKABLE: there is no `self` here holding a
/// request, no captured caller string, nothing in scope that a caller supplied.
/// A test can assert most of the property by reading the signature.
///
/// The environment is the daemon's own, captured once at pool construction --
/// see `NativeInstanceHost::daemon_environment` -- private, so deliberately not
/// an intra-doc link from public documentation -- for why it is needed and why
/// it is not a hole in the claim.
#[must_use]
pub fn launch_request_for(spec: &MintSpec, environment: &EnvironmentSpec) -> StartSessionRequest {
    StartSessionRequest {
        // `None`: pmux picks the session id. A caller-chosen id is one of the
        // two ways a transcript that already served work gets admitted as a
        // fresh cell, and the pool has no caller to take one from.
        identity: SessionIdentity::New { session_id: None },
        cwd: spec.cwd.to_string_lossy().into_owned(),
        agent: None,
        claude: Some(ClaudeLaunchConfig {
            executable: spec.claude_executable.to_string_lossy().into_owned(),
            // Both halves of the class key, rendered from the SAME table entry
            // that produced the key. `InstanceClass` is only constructible from
            // a `ResolvedModelEffort`, so the argv and the pool's model of the
            // process cannot disagree.
            model: Some(spec.class.canonical_model.to_owned()),
            effort: spec.class.effort_level(),
            // No modal may ever appear: nothing is attached to answer one, and
            // a pool instance blocked on a permission prompt is a slot lost
            // until the TTL sweep.
            permission_mode: Some(PermissionMode::DontAsk),
            allowed_tools: Vec::new(),
            denied_tools: vec![DENY_EVERY_TOOL.to_owned()],
            settings: Vec::new(),
            mcp_configs: Vec::new(),
            plugin_dirs: Vec::new(),
            // REPLACE, not append, for the agent-prompt *file*. An append would
            // leave CLAUDE.md / settings in front of the daemon prompt; that
            // content is not daemon configuration. Claude Code still prepends
            // its own identity line ahead of REPLACE (see the 2.1.236 body
            // dump). Replace-mode also survives `/clear`, which is what makes
            // one instance serve turn 2 under the same displacer it served
            // turn 1 under.
            system_prompt: SystemPromptPolicy::Replace {
                prompt: spec.system_prompt.clone(),
            },
            extra_args: Vec::new(),
        }),
        // The daemon's own environment, and never a caller's. `set` and `unset`
        // are empty in it: `set` bypasses the allowlist entirely, and the pool
        // has no caller to take a patch from. Step 6 of `build_environment`
        // overwrites `CLAUDE_CONFIG_DIR` with the isolation root below and runs
        // AFTER every removal, so an inherited `HOME` cannot move the config
        // root the child lands on.
        environment: environment.clone(),
        auth_policy: AuthPolicy::Subscription,
        config_isolation: Some(ConfigIsolation {
            root: spec.root.to_string_lossy().into_owned(),
        }),
        terminal: POOL_TERMINAL,
        lifecycle: LifecycleMode::Transcript,
        // Persistent with the pool's own TTL. The registry-level reaper is
        // excluded from pool sessions positively (`SessionOwner::Pool`), so
        // this bound is carried for the actor's own idle accounting and the
        // pool's sweep is the one that fires.
        retention: RetentionPolicy::Persistent {
            idle_ttl_ms: spec.instance_idle_ttl_ms,
        },
        // Both from constants this module also hands the health probe, so the
        // question a mint asks the compatibility registry and the question the
        // daemon's health tree asks it are one question.
        compatibility: POOL_COMPATIBILITY,
        cell: POOL_CELL,
    }
}

#[async_trait]
impl InstanceHost for NativeInstanceHost {
    async fn mint(&self, spec: MintSpec) -> Result<InstanceHandle, HostFailure> {
        let service = self.service().map_err(HostFailure::reaped)?;
        let request = launch_request_for(&spec, &self.daemon_environment);
        // `start_session_owned` runs the containment walk, the pristine-root
        // scan and `assert_empty_at_launch` before any actor exists, so a mint
        // that returns `Ok` has already carried the launch proof.
        //
        // A failed start is `reaped`: `start_session_owned`'s own failure path
        // (`finish_failed_start`) either tears the terminal down or queues it
        // for the startup-cleanup reaper, and in both cases it is pmux, not the
        // pool, that owns the process. The pool's `process_may_survive` bit
        // exists for a host that CANNOT make that claim, and this one can.
        let handle = service
            .start_session_pool(request)
            .await
            .map_err(HostFailure::reaped)?;
        Ok(InstanceHandle {
            session_id: handle.session_id,
            generation_id: handle.generation_id,
            // The pid is not observable from a `SessionHandle`, and it is not
            // invented. `MintSpec`'s pid file stays unwritten rather than
            // holding a number pmux guessed; a boot scan reading a fabricated
            // pid would kill whatever now holds it.
            pid: None,
            claude_version: handle.compatibility.claude_version,
        })
    }

    async fn run_turn(
        &self,
        handle: &InstanceHandle,
        prompt: String,
        deadline_unix_ms: u64,
    ) -> Result<HostTurn, HostFailure> {
        let service = self.service().map_err(HostFailure::possibly_live)?;
        let result = service
            .run_pool_turn(handle, prompt, deadline_unix_ms)
            .await
            // `possibly_live`, and this is the one place the distinction is
            // load-bearing on the turn path: a turn that did not reach a
            // transcript-proven terminal may still be generating. The pool
            // destroys the instance either way; what this bit decides is
            // whether the tree may be erased, and erasing a config root out
            // from under a live Claude races its own writer.
            .map_err(HostFailure::possibly_live)?;
        if result.outcome != TurnOutcome::Completed {
            // Anything other than a delivered terminal is an `Err`, per the
            // seam's contract. A `Failed` outcome carries text and usage, and
            // returning it as `Ok` would hand a caller an answer the transcript
            // said was an API error.
            return Err(HostFailure::possibly_live(ErrorBody::new(
                ErrorCode::ClaudeExited,
                format!(
                    "the stateless turn reached a terminal whose outcome was {:?} rather than completed",
                    result.outcome
                ),
            )));
        }
        Ok(HostTurn {
            text: result.text,
            reported_model: result.model,
            stop_reason: result.stop_reason,
            usage: result.usage,
            // COUNTED, from the same analysis that produced the tokens beside
            // it. This used to be `None` because `TurnResult` published the
            // sidechain's tokens and never its row count, and re-reading the
            // transcript here would steal the actor's cursor -- so the count
            // was added where the rows are already walked, in the transcript
            // engine, and carried out on `TurnResult`. `usize` conversion is
            // saturating rather than fallible because the value is only ever
            // compared against zero.
            sidechain_rows: Some(usize::try_from(result.sidechain_rows).unwrap_or(usize::MAX)),
        })
    }

    async fn clear(&self, handle: &InstanceHandle) -> Result<(), ClearFailure> {
        let service = self.service().map_err(|error| ClearFailure {
            error,
            // The clear was NOT proven un-submitted, so the default -- destroy
            // the instance -- applies. Every field here is a positive claim and
            // the absence of one is never read as the claim.
            clear_not_submitted: false,
            preamble_mismatch: None,
        })?;
        service.clear_pool_instance(handle).await
    }

    async fn destroy(&self, handle: &InstanceHandle) -> Result<Destroyed, HostFailure> {
        let service = self.service().map_err(HostFailure::possibly_live)?;
        service.destroy_pool_instance(handle).await
    }
}

impl NativeService {
    /// Start one pool instance. Not on any wire path.
    pub(crate) async fn start_session_pool(
        self: &Arc<Self>,
        request: StartSessionRequest,
    ) -> Result<pseudomux_protocol::v1::SessionHandle, ErrorBody> {
        self.start_session_owned(request, SessionOwner::Pool).await
    }

    /// Run one turn on a pool instance to a transcript-proven terminal.
    pub(crate) async fn run_pool_turn(
        self: &Arc<Self>,
        handle: &InstanceHandle,
        prompt: String,
        deadline_unix_ms: u64,
    ) -> Result<pseudomux_protocol::v1::TurnResult, ErrorBody> {
        let turn_id = uuid::Uuid::new_v4();
        let turn = TurnRequest {
            turn_id,
            prompt,
            deadline_unix_ms: Some(deadline_unix_ms),
            lease: TurnLeasePolicy::default(),
        };
        let actor = self
            .registry()
            .pool_actor(handle.session_id, handle.generation_id)
            .await?;
        // Read from the actor rather than cached on the handle. The drain is a
        // property of the compatibility cell the session was ADMITTED under,
        // and the actor is the only thing that holds it; a copy on
        // `InstanceHandle` would be a second fact about one session that a
        // profile change could not reach.
        let transcript_drain_ms = actor.snapshot().await?.compatibility.transcript_drain_ms;
        actor.submit_turn(turn).await?;
        // The SAME handle the submit went through. `wait_for_turn` takes an
        // actor precisely so this cannot re-resolve the session through a
        // resolver that refuses pool instances -- which is the defect this
        // signature was changed for.
        self.wait_for_turn(&actor, turn_id, Some(deadline_unix_ms), transcript_drain_ms)
            .await
    }

    /// Type `/clear`, resolve the rotation, prove the successor empty, bind it.
    pub(crate) async fn clear_pool_instance(
        self: &Arc<Self>,
        handle: &InstanceHandle,
    ) -> Result<(), ClearFailure> {
        let quarantine = |error: ErrorBody| ClearFailure {
            clear_not_submitted: crate::driver_io::clear_was_not_submitted(&error.details),
            // A preamble mismatch halts the WHOLE pool, so it is read from
            // the one detail that means it and never inferred from a message.
            preamble_mismatch: crate::driver_io::clear_refusal_repromotion_trigger(&error.details),
            error,
        };
        let actor = self
            .registry()
            .pool_actor(handle.session_id, handle.generation_id)
            .await
            .map_err(quarantine)?;
        let boundary = self
            .clear_boundary(handle.session_id, handle.generation_id)
            .await
            .map_err(quarantine)?;
        let deadline_unix_ms = crate::native::unix_now_ms()
            .map(|now| now.saturating_add(self.clear_timeout_ms()))
            .map_err(quarantine)?;
        actor
            .clear_and_rebind(boundary, deadline_unix_ms)
            .await
            .map(|_| ())
            .map_err(quarantine)
    }

    /// Force-close one pool instance and prove its process boundary empty.
    ///
    /// **Touches no file.** The pool owns the filesystem and erases the roots
    /// itself, after this returns `process_reaped: true`.
    pub(crate) async fn destroy_pool_instance(
        self: &Arc<Self>,
        handle: &InstanceHandle,
    ) -> Result<Destroyed, HostFailure> {
        let request = CloseSessionRequest {
            session_id: handle.session_id,
            generation_id: handle.generation_id,
            policy: ClosePolicy::Force,
        };
        match self.close_session_owned(SessionOwner::Pool, request).await {
            Ok(result) => Ok(Destroyed {
                process_reaped: result.process_reaped,
            }),
            // A pool instance leaves the registry ONLY inside
            // `close_session_with_state`, and only after that close returned
            // `process_reaped: true`. So a not-found here is not an assumption
            // that the process is gone -- it is the record of an earlier close
            // that positively proved it. Every other refusal is reported, and
            // `possibly_live` is the honest bit for all of them: a close that
            // did not complete has proven nothing about the child.
            Err(error) if error.code == ErrorCode::SessionNotFound => Ok(Destroyed {
                process_reaped: true,
            }),
            Err(error) => Err(HostFailure::possibly_live(error)),
        }
    }
}

/// One stateless call, from the wire to the pool and back.
///
/// Lives beside the host rather than in `native.rs` so the whole Path B surface
/// -- the refusal when no pool is configured, and the call when one is -- reads
/// as one thing.
///
/// # Errors
///
/// `unsupported_feature` when no pool is configured; otherwise whatever
/// [`Pool::run`] refused with. Nothing here adds a code.
pub async fn run_stateless(
    pool: Option<&Arc<Pool>>,
    request: RunStatelessRequest,
) -> Result<StatelessResult, ErrorBody> {
    match pool {
        Some(pool) => pool.run(request).await,
        None => Err(crate::pool::path_b_not_enabled()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::{Epoch, SlotPaths, resolve_pool_class};
    use pseudomux_protocol::v1::EffortLevel;
    use std::path::{Path, PathBuf};

    fn spec() -> MintSpec {
        let paths = SlotPaths::new(&PathBuf::from("/pool"), 3, Epoch::from(7u64));
        let (class, _) = resolve_pool_class("opus", Some(EffortLevel::High)).expect("admitted");
        MintSpec {
            slot: 3,
            epoch: Epoch::from(7u64),
            class,
            root: paths.root,
            cwd: paths.cwd,
            claude_executable: PathBuf::from("/usr/local/bin/claude"),
            system_prompt: "Answer directly.".to_owned(),
            instance_idle_ttl_ms: 300_000,
        }
    }

    /// The same [`MintSpec`] the pool builds, but rooted in a real 0700 tree so
    /// the launch pipeline can canonicalize and owner-check it.
    ///
    /// `sonnet/low`, not `opus/high`: nothing here launches a process, and the
    /// class is still resolved through `resolve_pool_class` rather than
    /// constructed, so the argv under test is the argv a live mint of the class
    /// the pool actually warms would carry.
    #[cfg(unix)]
    fn on_disk_spec(parent: &Path) -> MintSpec {
        use std::os::unix::fs::PermissionsExt;

        let paths = SlotPaths::new(parent, 3, Epoch::from(7u64));
        let (class, _) = resolve_pool_class("sonnet", Some(EffortLevel::Low)).expect("admitted");
        for directory in [&paths.root, &paths.cwd] {
            std::fs::create_dir_all(directory).expect("the pool creates these itself");
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
                .expect("0700 from birth, exactly as the pool creates them");
        }
        MintSpec {
            slot: 3,
            epoch: Epoch::from(7u64),
            class,
            root: paths.root,
            cwd: paths.cwd,
            claude_executable: PathBuf::from("/bin/sh"),
            system_prompt: "Answer directly.".to_owned(),
            instance_idle_ttl_ms: 300_000,
        }
    }

    /// A daemon environment carrying the two names that matter and one that
    /// must not survive the allowlist.
    fn daemon_environment() -> EnvironmentSpec {
        EnvironmentSpec {
            snapshot: std::collections::BTreeMap::from([
                ("HOME".to_owned(), "/Users/operator".to_owned()),
                ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
            ]),
            set: std::collections::BTreeMap::new(),
            unset: std::collections::BTreeSet::new(),
        }
    }

    /// Every field of the launch request comes from the spec or the daemon's
    /// own environment, and the isolation clauses are the ones a minified cell
    /// is admitted under.
    ///
    /// This is the integration's half of "the caller names no resource". The
    /// other half is structural and is stated in `launch_request_for`'s doc: the
    /// function takes a `MintSpec` and the daemon's own environment, so there is
    /// no caller string in scope to leak. What this test adds is that the fields
    /// it DOES fill are filled correctly -- a `launch_request_for` that ignored
    /// the spec entirely would also have no caller string in it.
    #[test]
    fn a_mint_names_only_what_the_pool_minted() {
        let spec = spec();
        let request = launch_request_for(&spec, &daemon_environment());

        assert_eq!(request.cwd, "/pool/3/7/cwd");
        assert_eq!(
            request.config_isolation.as_ref().map(|c| c.root.as_str()),
            Some("/pool/3/7/root")
        );
        assert_eq!(
            request.claude.as_ref().expect("inline launch").executable,
            "/usr/local/bin/claude"
        );
        assert_eq!(
            request
                .claude
                .as_ref()
                .expect("inline launch")
                .model
                .as_deref(),
            Some("claude-opus-5")
        );
        assert_eq!(
            request.claude.as_ref().expect("inline launch").effort,
            Some(EffortLevel::High)
        );
        assert_eq!(
            request
                .claude
                .as_ref()
                .expect("inline launch")
                .system_prompt,
            SystemPromptPolicy::Replace {
                prompt: "Answer directly.".to_owned()
            }
        );

        // The isolation clauses, each one separately.
        assert_eq!(request.cell, SessionCell::Minified);
        assert_eq!(request.compatibility, CompatibilityPolicy::RequireTested);
        assert_eq!(
            request
                .claude
                .as_ref()
                .expect("inline launch")
                .permission_mode,
            Some(PermissionMode::DontAsk)
        );
        assert_eq!(
            request.claude.as_ref().expect("inline launch").denied_tools,
            vec!["*".to_owned()]
        );
        assert!(
            request
                .claude
                .as_ref()
                .expect("inline launch")
                .allowed_tools
                .is_empty()
        );
        assert!(
            request
                .claude
                .as_ref()
                .expect("inline launch")
                .extra_args
                .is_empty()
        );
        assert!(
            request
                .claude
                .as_ref()
                .expect("inline launch")
                .settings
                .is_empty()
        );
        assert!(
            request
                .claude
                .as_ref()
                .expect("inline launch")
                .mcp_configs
                .is_empty()
        );
        assert!(
            request
                .claude
                .as_ref()
                .expect("inline launch")
                .plugin_dirs
                .is_empty()
        );
        assert_eq!(
            request.identity,
            SessionIdentity::New { session_id: None },
            "a pool instance never names its own transcript id"
        );
        assert_eq!(
            request.retention,
            RetentionPolicy::Persistent {
                idle_ttl_ms: 300_000
            }
        );

        // The environment is the DAEMON's, verbatim, and `set`/`unset` are
        // empty. `set` bypasses `build_environment`'s allowlist entirely, so a
        // non-empty one here would be pmux inventing an unfiltered request byte
        // on a caller's behalf.
        assert_eq!(request.environment.snapshot, daemon_environment().snapshot);
        assert!(
            request.environment.set.is_empty(),
            "the explicit, allowlist-bypassing channel must stay empty"
        );
        assert!(request.environment.unset.is_empty());

        // `config_isolation` outranks the inherited `HOME`: step 6 of
        // `build_environment` overwrites `CLAUDE_CONFIG_DIR` after every
        // removal, so the delivered root is the slot's and not `HOME`'s.
        assert_eq!(
            request.environment.snapshot.get("HOME").map(String::as_str),
            Some("/Users/operator"),
            "the inherited HOME is present, which is exactly why the isolation root has to win"
        );
        assert_eq!(
            request.config_isolation.as_ref().map(|c| c.root.as_str()),
            Some("/pool/3/7/root")
        );
    }

    /// The mint carries no request byte, checked against the resources a caller
    /// would try to name.
    ///
    /// The signature already makes this true -- `launch_request_for` has no
    /// request in scope -- and the test is here because the signature is what a
    /// future change would relax first. It walks the delivered environment for
    /// every door `CONFIG_ROOT_ENV_DOORS` names, which is the set a Path A
    /// caller is refused for a minified cell.
    #[test]
    fn a_mint_sets_no_configuration_door_of_its_own() {
        let request = launch_request_for(&spec(), &daemon_environment());
        for door in [
            "CLAUDE_CONFIG_DIR",
            "CLAUDE_SECURESTORAGE_CONFIG_DIR",
            "HOME",
            "USERPROFILE",
            "XDG_CONFIG_HOME",
        ] {
            assert!(
                !request.environment.set.contains_key(door),
                "the mint set {door} through the channel that bypasses the allowlist"
            );
        }
    }

    /// A model whose admitted effort set is empty renders no `--effort`, and the
    /// launch request must carry that absence rather than a default.
    #[test]
    fn a_model_that_takes_no_effort_renders_no_effort_field() {
        let mut spec = spec();
        let (class, _) = resolve_pool_class("haiku", None).expect("admitted");
        spec.class = class;
        let request = launch_request_for(&spec, &daemon_environment());
        assert_eq!(
            request
                .claude
                .as_ref()
                .expect("inline launch")
                .model
                .as_deref(),
            Some("claude-haiku-4-5")
        );
        assert_eq!(
            request.claude.as_ref().expect("inline launch").effort,
            None,
            "a model with an empty admitted set must not be sent a tier"
        );
    }

    #[tokio::test]
    async fn a_daemon_without_a_pool_refuses_rather_than_pretending() {
        let refusal = run_stateless(
            None,
            RunStatelessRequest {
                model: "opus".to_owned(),
                effort: None,
                prompt: "hello".to_owned(),
                deadline_unix_ms: None,
            },
        )
        .await
        .expect_err("a daemon with no pool cannot serve a stateless call");
        assert_eq!(refusal.code, ErrorCode::UnsupportedFeature);
        assert_eq!(
            refusal.details.get("violation").and_then(|v| v.as_str()),
            Some("path_b_not_enabled")
        );
    }

    // -----------------------------------------------------------------------
    // THE LAUNCH BUNDLE, AND THE THREE DOCUMENTS THAT EACH DESCRIBED A
    // DIFFERENT ONE.
    //
    // `crates/service/src/v1/minified.rs` and `tools/promotion/
    // measure_transcript_drain.py` each stated the
    // flags a Path B cell launches with. Both named the same two flags
    // that no launch path emitted -- one of them the MCP suppression
    // `claude_launch::MINIFIED_CELL_FLAGS` now carries, the other `--safe-mode`,
    // which pmux still does not pass. A doc comment and an argv builder in
    // different files can disagree in silence forever, so the spellings are now
    // published as data at each site and compared, element for element and in
    // argv order, against the argv a real mint produces.
    // -----------------------------------------------------------------------

    /// The only files a code tree may name a [`MINIFIED_CELL_FLAGS`] spelling
    /// in, with the reason each has the right.
    ///
    /// [`MINIFIED_CELL_FLAGS`]: crate::claude_launch::MINIFIED_CELL_FLAGS
    const BUNDLE_SPELLING_HOMES: [&str; 4] = [
        // The definition, and the measurement that put it there.
        "crates/service/src/claude_launch.rs",
        // The published list, checked below.
        "crates/service/src/v1/minified.rs",
        // The measurement tool's copy of the published list, checked below.
        "tools/promotion/measure_transcript_drain.py",
        // A startup-screen fixture: the stderr a Claude that REJECTED this flag
        // would render. Sanctioned rather than rewritten because pmux now emits
        // the flag, which is what makes that screen reachable at all.
        "crates/service/src/native.rs",
    ];

    /// Build output and tool caches, skipped by the scan.
    const SCAN_SKIPPED_DIRECTORIES: [&str; 8] = [
        ".git",
        ".context",
        ".pseudomux",
        ".ruff_cache",
        "__pycache__",
        "dist",
        "node_modules",
        "target",
    ];

    /// The trees the scan walks.
    ///
    /// `docs/` is deliberately absent and it is the one exclusion worth
    /// stating: those files are DATED RECEIPTS, and `docs/path-b.md` §2.2 has
    /// to stay free to record the retraction that caused this defect. What
    /// binds the claims that matter is the equality check below, not the scan;
    /// the scan exists so a FIFTH source file cannot quietly acquire an opinion
    /// about the bundle.
    const SCANNED_TREES: [&str; 6] = ["bin", "clients", "crates", "fuzz", "tests", "tools"];

    fn workspace_root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("the workspace root must resolve")
    }

    fn scannable_files(directory: &Path, files: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(directory)
            .unwrap_or_else(|error| panic!("could not read {}: {error}", directory.display()))
        {
            let path = entry
                .expect("workspace directory entry must be readable")
                .path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if SCAN_SKIPPED_DIRECTORIES.contains(&name) {
                continue;
            }
            let metadata = std::fs::symlink_metadata(&path)
                .unwrap_or_else(|error| panic!("could not stat {}: {error}", path.display()));
            if metadata.is_dir() {
                scannable_files(&path, files);
            } else if metadata.is_file() {
                files.push(path);
            }
        }
    }

    /// Every `--option` token in `text`, in the order it appears.
    ///
    /// Deliberately a scanner rather than a split: the Rust site publishes its
    /// list as a doc-comment sentence with backticks and commas and the Python
    /// site publishes a quoted tuple, and a rule about punctuation is a rule
    /// that gets a site wrong. The one thing both spell identically is the flag
    /// itself.
    fn option_tokens(text: &str) -> Vec<String> {
        let characters = text.chars().collect::<Vec<_>>();
        let mut tokens = Vec::new();
        let mut index = 0;
        while index + 2 < characters.len() {
            if characters[index] == '-' && characters[index + 1] == '-' {
                let mut end = index + 2;
                while end < characters.len()
                    && (characters[end].is_ascii_alphanumeric() || characters[end] == '-')
                {
                    end += 1;
                }
                if end > index + 2 {
                    tokens.push(characters[index..end].iter().collect::<String>());
                }
                index = end;
            } else {
                index += 1;
            }
        }
        tokens
    }

    /// The text between `opening` and the first occurrence of `closing` after
    /// it, or a panic naming the file that stopped publishing its list.
    fn published_span<'a>(source: &'a str, path: &str, opening: &str, closing: &str) -> &'a str {
        let (_, after) = source
            .split_once(opening)
            .unwrap_or_else(|| panic!("{path} no longer publishes its launch bundle: {opening:?}"));
        let (span, _) = after
            .split_once(closing)
            .unwrap_or_else(|| panic!("{path}'s launch bundle is not terminated by {closing:?}"));
        span
    }

    /// The bundle three documents describe is the argv one mint emits.
    ///
    /// The emitted side is not a fixture. It is
    /// [`launch_request_for`] -- the function a live mint calls -- driven
    /// through the same three steps `NativeService::start_session_owned_with_retention`
    /// drives it through, on a real 0700 slot tree. So this fails if `build_args`
    /// changes, if `launch_request_for` changes, if `SensitiveLaunchFiles`
    /// stops appending the system-prompt file, or if any of the three
    /// documents is edited without the launch.
    ///
    /// The scan is the second half. Equality binds the sites that publish a
    /// list; the scan refuses any other file in a code tree the right to name
    /// a spelling at all, so the list of sites cannot silently grow.
    #[cfg(unix)]
    #[test]
    fn the_documented_minified_launch_bundle_is_the_argv_a_mint_emits() {
        let parent = tempfile::tempdir().expect("a pool parent");
        let runtime = tempfile::tempdir().expect("a private runtime directory");
        let request = launch_request_for(&on_disk_spec(parent.path()), &daemon_environment());
        assert_eq!(request.cell, SessionCell::Minified, "the premise");
        let emitted = crate::claude_launch::minified_launch_flags(runtime.path(), &request)
            .expect("a pool mint's launch resolves");

        // The cell-owned flags reach argv. A constant that is declared and
        // never appended is exactly the defect being closed, so this is
        // asserted on the emitted tokens and not on the constant. Which
        // spellings those are is pinned by the two published lists below and,
        // literally, by `claude_launch`'s own test -- deliberately not restated
        // here, which is what the scan at the end enforces.
        let cell_flags = crate::claude_launch::MINIFIED_CELL_FLAGS;
        for flag in cell_flags {
            assert!(
                emitted.iter().any(|emitted| emitted == flag),
                "{flag} is in MINIFIED_CELL_FLAGS and not in argv: {emitted:?}"
            );
        }
        // Asserted on the emitted tokens rather than on the constant's length,
        // so an emptied `MINIFIED_CELL_FLAGS` cannot make the loop above pass
        // by having nothing to iterate.
        assert!(
            emitted
                .iter()
                .any(|flag| cell_flags.contains(&flag.as_str())),
            "no cell-owned flag reached argv, so every check here is vacuous: {emitted:?}"
        );
        let distinct = emitted
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        assert_eq!(
            distinct,
            emitted.len(),
            "a repeated option in the bundle would make the published lists \
             ambiguous: {emitted:?}"
        );

        let root = workspace_root();
        let read = |relative: &str| {
            std::fs::read_to_string(root.join(relative))
                .unwrap_or_else(|error| panic!("could not read {relative}: {error}"))
        };

        let rust_site = "crates/service/src/v1/minified.rs";
        let rust_source = read(rust_site);
        assert_eq!(
            option_tokens(published_span(
                &rust_source,
                rust_site,
                "BUNDLE:",
                "\n//!\n"
            )),
            emitted,
            "{rust_site}'s published bundle is not the argv a mint emits"
        );

        let python_site = "tools/promotion/measure_transcript_drain.py";
        let python_source = read(python_site);
        assert_eq!(
            option_tokens(published_span(
                &python_source,
                python_site,
                "MINIFIED_LAUNCH_FLAGS: tuple[str, ...] = (",
                ")",
            )),
            emitted,
            "{python_site}'s MINIFIED_LAUNCH_FLAGS is not the argv a mint emits"
        );

        let mut files = Vec::new();
        for tree in SCANNED_TREES {
            scannable_files(&root.join(tree), &mut files);
        }
        assert!(
            files.len() > 100,
            "the tree scan found only {}",
            files.len()
        );
        let homes = BUNDLE_SPELLING_HOMES
            .iter()
            .map(|home| root.join(home))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            homes.len(),
            BUNDLE_SPELLING_HOMES.len(),
            "BUNDLE_SPELLING_HOMES names a file twice"
        );
        for home in &homes {
            assert!(home.is_file(), "{} does not exist", home.display());
        }
        for path in files {
            if homes.contains(&path) {
                continue;
            }
            let text = String::from_utf8_lossy(
                &std::fs::read(&path)
                    .unwrap_or_else(|error| panic!("could not read {}: {error}", path.display())),
            )
            .into_owned();
            for flag in crate::claude_launch::MINIFIED_CELL_FLAGS {
                assert!(
                    !text.contains(flag),
                    "{} names {flag}; the minified launch bundle belongs to \
                     {BUNDLE_SPELLING_HOMES:?} and nowhere else in a code tree",
                    path.display()
                );
            }
        }
    }
}
