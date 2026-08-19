//! Canonical protocol-v1 service facade.
//!
//! `NativeService` is the single high-level entry point used by the UDS daemon,
//! CLI adapters, MCP, and language clients. It owns one private rmux runtime and
//! delegates per-session semantics to [`crate::v1::SessionRegistry`].

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pseudomux_claude::{TranscriptLocationError, TranscriptLocator};
use pseudomux_protocol::v1::{
    AttachSessionRequest, ClearSessionRequest, ClearSessionResult, CloseSessionRequest,
    CloseSessionResult, DaemonDiagnosis, DisconnectAction, ErrorBody, ErrorCode, HealthLayer,
    HealthLayerName, LayerFinding, ListAgentsRequest, MAX_SAFE_JSON_INTEGER, PROTOCOL_VERSION,
    Pong, Request, ResponseResult, RetentionPolicy, RunOnceRequest, RunTurnRequest, RuntimeFinding,
    RuntimeProbe, SessionAgentPin, SessionCell, SessionFinding, SessionGenerationId, SessionHandle,
    SessionId, SessionIdentity, SessionProbe, SessionState, StartSessionRequest, TurnAccepted,
    TurnResult, validate_v1_serializable,
};
use pseudomux_rmux::{
    ControlPlaneFault, LaunchSpec, TerminalBackendError, TerminalSession, TerminalSnapshot,
};
use serde::Serialize;
use serde_json::json;
use tokio::process::Command;
use tokio::sync::{Mutex, RwLock, oneshot};

use crate::claude_launch::{
    DirectoryIdentity, ResolvedClaudeLaunch, must_treat_as_same_directory,
    one_directory_contains_the_other, resolve_claude_launch, select_session_id,
    traverses_a_parent_component,
};
use crate::compatibility::{
    CompatibilityProfileRegistry, DEFAULT_UNTESTED_TRANSCRIPT_DRAIN_MS,
    validate_transcript_drain_ms, validate_v1_terminal_support,
};
use crate::config_isolation::{ConfigRootSeed, SeedDisposition, seed_private_config_root};
use crate::driver_io::{
    FileTranscriptSource, RmuxTerminalControl, TerminalScreenState, classify_terminal_snapshot,
    validate_prompt,
};
use crate::launch_broker::BrokerProbe;
use crate::pool::{Pool, PoolConfig, TrackedSpawner};
use crate::runtime::{CreateTerminalError, PrivateRuntime, PrivateRuntimeConfig, SessionRuntime};
use crate::sensitive_launch::SensitiveLaunchFiles;
use crate::tasks::TrackedTasks;
use crate::tombstones::ClosedSessionTombstones;
use crate::v1::{
    ClearRebind, DriverResult, SessionActorConfig, SessionOwner, SessionRegistration,
    SessionRegistry, StoredTurnTerminal, TerminalControl, TranscriptSource,
    WritableAttachCompletion, require_tested_for_minified_cell,
};

const CLOSED_SESSION_TOMBSTONE_CAPACITY: usize = 4_096;
/// Default absolute-deadline window for submitting `/clear` to the TUI, in
/// milliseconds from now. Chosen to equal the driver's `INPUT_GATE_MAX_DURATION`
/// so the server default never binds before the input gate's own bound does.
const DEFAULT_CLEAR_TIMEOUT_MS: u64 = 15_000;
const STARTUP_READY_STABLE_FOR: Duration = Duration::from_millis(250);
const STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// How long [`NativeService::diagnose`] waits for one actor to report its state.
///
/// An actor serves its mailbox in order, so a session that is mid-submission
/// answers only after the input gate releases the terminal mutex. Exceeding
/// this bound is classified [`SessionFinding::SessionActorUnresponsive`], which
/// is `unproven` and never a fault -- so the exact value here can only cost the
/// report precision, never correctness. It is generous for that reason.
const DIAGNOSE_ACTOR_DEADLINE: Duration = Duration::from_secs(2);

#[derive(Clone, Debug)]
pub struct NativeServiceConfig {
    pub actor: SessionActorConfig,
    pub readiness_timeout: Duration,
    pub version_timeout: Duration,
    pub attach_ttl: Duration,
    /// Frequency for atomically enforcing persistent-session idle deadlines.
    pub idle_reaper_interval: Duration,
    /// Absolute companion binary used only when Hybrid lifecycle is requested.
    pub hybrid_hook_client: Option<PathBuf>,
    /// Exact evidence cells admitted by `RequireTested`. This is intentionally
    /// empty until Phase 0 evidence promotes a version/platform/profile cell.
    pub tested_claude_profiles: CompatibilityProfileRegistry,
    /// Conservative drain for explicit `AllowUntested` cells that do not match
    /// an admitted profile. It never promotes the cell to tested status.
    pub untested_transcript_drain_ms: u64,
    /// Deadline granted to a `clear_session` request that supplies none.
    ///
    /// Equal to the driver's own input-gate bound, so the default deadline is
    /// never the binding constraint and the fixed rebind refusal deadline stays
    /// the meaningful one. A caller may shorten this; it cannot lengthen the
    /// rebind wait, which is a correctness deadline and not negotiable.
    pub default_clear_timeout_ms: u64,
    /// Where stored agents live, or `None` for a daemon that serves none.
    ///
    /// The `Option` is the enable switch and nothing else. A daemon without one
    /// refuses every agent method and every start naming an agent, by name and
    /// with the flag to add, rather than growing a directory on a caller's
    /// request path.
    pub agent_store: Option<PathBuf>,
    /// The stateless token engine's configuration, or `None` for a daemon that
    /// does not serve Path B.
    ///
    /// Already validated -- [`PoolConfig`] is only constructible through
    /// `PoolSettings::validate` -- so a daemon that reaches this point with
    /// `Some` has an admissible pool by construction and no runtime path has to
    /// re-check a bound. The `Option` is the enable switch and nothing else; a
    /// present-but-invalid configuration is not representable.
    pub pool: Option<PoolConfig>,
}

impl Default for NativeServiceConfig {
    fn default() -> Self {
        Self {
            actor: SessionActorConfig::default(),
            readiness_timeout: Duration::from_secs(90),
            version_timeout: Duration::from_secs(10),
            attach_ttl: Duration::from_secs(30),
            idle_reaper_interval: Duration::from_secs(1),
            hybrid_hook_client: None,
            tested_claude_profiles: CompatibilityProfileRegistry::default(),
            untested_transcript_drain_ms: DEFAULT_UNTESTED_TRANSCRIPT_DRAIN_MS,
            default_clear_timeout_ms: DEFAULT_CLEAR_TIMEOUT_MS,
            // OFF by default, for the same reason `pool` is: a store that
            // appeared merely because a daemon was built would create a
            // directory holding environment values on every embedder that
            // never asked for one.
            agent_store: None,
            // OFF by default. A pool that appeared merely because a daemon was
            // built would mint instances -- real Claude processes, real
            // directories -- on every embedder that never asked for one.
            pool: None,
        }
    }
}

struct SessionMetadata {
    generation_id: SessionGenerationId,
    terminal: Arc<RmuxTerminalControl>,
    /// The same transcript source the actor drives, retained in its concrete
    /// type.
    ///
    /// The actor holds it as `dyn TranscriptSource`, which is everything a turn
    /// needs and not enough to clear: `clear_and_rebind` requires the concrete
    /// terminal and the concrete transcript *of one instance*, because typing
    /// `/clear` and identifying the file it opens are one operation. Neither
    /// type carries a session id, so pairing them is a construction-site
    /// invariant -- and this is the construction site, the one place that
    /// already knows both halves belong to the same Claude process.
    transcript: Arc<FileTranscriptSource>,
    private_session_name: String,
    /// The cell this live session is driven as.
    ///
    /// Retained because admission is a statement about the INCUMBENT: the next
    /// start has to know what already holds this session's config root and cwd
    /// before it can be told whether it may have them too. Read only by
    /// [`NativeService::live_resource_claims`].
    cell: SessionCell,
    /// Who may address this session; the same value the registry was told.
    ///
    /// Duplicated here deliberately, and the duplication is bounded: the
    /// registry's copy fences every session-addressed method, this copy fences
    /// the generic idle reaper, and the reaper walks THIS map. A reaper that
    /// asked the registry instead would be relying on `expire_idle` refusing --
    /// which it does, but "the call I made happened to be rejected" is a
    /// different statement from "I declined to make it", and only the second
    /// survives someone adding a reaper path that does not go through the
    /// registry.
    owner: SessionOwner,
    /// Holds owner-only config and prompt files until the Claude process is reaped.
    _sensitive_launch: SensitiveLaunchFiles,
    #[cfg(unix)]
    _lifecycle: SessionLifecycle,
}

/// The concrete clear-and-rebind pair for one live session.
///
/// Exists so the privileged operation travels as a capability for exactly one
/// call instead of as a boundary every session is built with. It is constructed
/// only from a [`SessionMetadata`] entry, which is what makes "these two halves
/// are the same Claude process" true by construction rather than by convention.
struct RmuxClearRebind {
    terminal: Arc<RmuxTerminalControl>,
    transcript: Arc<FileTranscriptSource>,
}

#[async_trait::async_trait]
impl ClearRebind for RmuxClearRebind {
    async fn clear_and_rebind(
        &self,
        session_id: SessionId,
        deadline_unix_ms: u64,
    ) -> DriverResult<SessionId> {
        crate::driver_io::clear_and_rebind(
            self.terminal.as_ref(),
            self.transcript.as_ref(),
            session_id,
            deadline_unix_ms,
        )
        .await
    }
}

impl SessionMetadata {
    async fn shutdown(self) {
        #[cfg(unix)]
        {
            let mut metadata = self;
            metadata._lifecycle.shutdown().await;
        }
        #[cfg(not(unix))]
        drop(self);
    }
}

/// An unpublished terminal whose first cleanup attempt was inconclusive.
///
/// A caller cannot retry an admission failure because no public generation was
/// issued, so the service retains every resource that must outlive Claude and
/// retries cleanup autonomously. PrivateRuntime shutdown remains the final
/// process-wide cleanup authority.
struct PendingStartupCleanup {
    session_id: SessionId,
    terminal: PendingStartupTerminal,
    _sensitive_launch: Option<SensitiveLaunchFiles>,
    #[cfg(unix)]
    _lifecycle: Option<SessionLifecycle>,
}

impl PendingStartupCleanup {
    fn terminal_only(session_id: SessionId, terminal: Box<dyn TerminalSession>) -> Self {
        Self {
            session_id,
            terminal: PendingStartupTerminal::Raw(terminal),
            _sensitive_launch: None,
            #[cfg(unix)]
            _lifecycle: None,
        }
    }

    /// True when no future cleanup attempt on this owner can do anything.
    ///
    /// Only [`PendingStartupTerminal::Lost`] qualifies: the terminal went into
    /// a detached close task that ended without giving it back, so
    /// [`Self::close_terminal`] has nothing left to call and returns the same
    /// non-retryable `RecoveryFailed` every time it is asked.
    fn is_permanently_failed(&self) -> bool {
        matches!(self.terminal, PendingStartupTerminal::Lost)
    }

    fn raw_terminal_mut(&mut self) -> &mut dyn TerminalSession {
        match &mut self.terminal {
            PendingStartupTerminal::Raw(terminal) => terminal.as_mut(),
            PendingStartupTerminal::Controlled(_) => {
                panic!("startup terminal was promoted before readiness")
            }
            PendingStartupTerminal::Closing(_) | PendingStartupTerminal::Lost => {
                panic!("startup terminal was closed before readiness")
            }
            PendingStartupTerminal::Promoting => {
                unreachable!("startup terminal promotion cannot cross an await")
            }
        }
    }

    fn promote_terminal(
        &mut self,
        #[cfg(unix)] lifecycle_expected: bool,
        #[cfg(unix)] lifecycle_stop_sequence: Arc<AtomicU64>,
        #[cfg(unix)] lifecycle_stop_at_ms: Arc<AtomicU64>,
    ) -> Arc<RmuxTerminalControl> {
        let PendingStartupTerminal::Raw(terminal) =
            std::mem::replace(&mut self.terminal, PendingStartupTerminal::Promoting)
        else {
            panic!("startup terminal can only be promoted once")
        };
        let terminal = RmuxTerminalControl::new(terminal);
        #[cfg(unix)]
        let terminal = if lifecycle_expected {
            terminal.with_lifecycle_observation(lifecycle_stop_sequence, lifecycle_stop_at_ms)
        } else {
            terminal
        };
        let terminal = Arc::new(terminal);
        self.terminal = PendingStartupTerminal::Controlled(Arc::clone(&terminal));
        terminal
    }

    /// Requests teardown of an unpublished terminal, without ever dropping the
    /// close in flight.
    ///
    /// Close is the one terminal call that is deliberately *not* made
    /// cancellation-safe beneath the `TerminalSession` trait.
    /// `RmuxTerminal::close` is a compound `&mut self` state machine -- observe
    /// the process boundary, request rmux cleanup, wait for the reap,
    /// force-reap the exact surviving POSIX session members -- and dropping it
    /// midway leaves that machine half-run: a kill requested, nothing
    /// confirmed, the interactive process still alive, and (because rmux-sdk
    /// treats a dropped in-flight request as a permanent transport failure) the
    /// connection that would have confirmed it destroyed. `v1::actor::force_reap_terminal`
    /// answers that by detaching the whole close onto its own task. This is the
    /// same hazard on the unpublished-startup path, which `force_reap_terminal`
    /// cannot reach: `finish_failed_start` runs inside `start_session`, whose
    /// future is dropped whenever the requesting client goes away, and at that
    /// point the terminal has no actor and no `TerminalControl` yet.
    ///
    /// Both arms therefore run their close on a spawned task and join it.
    /// Dropping a `JoinHandle` detaches rather than aborts, so a cancelled
    /// caller still leaves the close running to completion. Only the
    /// `Raw`/`Closing` arm goes further and *adopts* that task on retry
    /// ([`Self::close_raw_terminal`]); the `Controlled` arm
    /// ([`Self::close_controlled_terminal`]) parks nothing and spawns a fresh
    /// `close(Force)` per attempt. That is sound rather than merely tolerable:
    /// `RmuxTerminalControl` holds the terminal under its own mutex for the
    /// whole close, so attempts serialise instead of racing, and
    /// `RmuxTerminal::close` skips the rmux cleanup request once
    /// `cleanup_requested` is set, so a retry re-observes the process boundary
    /// rather than re-issuing a kill the first attempt already delivered.
    async fn close_terminal(&mut self) -> Result<(), ErrorBody> {
        // Matching by place with no bindings; nothing is moved out of `self`.
        let process_reaped = match self.terminal {
            PendingStartupTerminal::Raw(_)
            | PendingStartupTerminal::Closing(_)
            | PendingStartupTerminal::Lost => self.close_raw_terminal().await?,
            PendingStartupTerminal::Controlled(_) => self.close_controlled_terminal().await?,
            PendingStartupTerminal::Promoting => {
                unreachable!("startup terminal promotion cannot cross an await")
            }
        };
        if process_reaped {
            Ok(())
        } else {
            Err(ErrorBody::new(
                ErrorCode::RecoveryFailed,
                "startup cleanup did not confirm that the unpublished process was reaped",
            )
            .retryable(true)
            .with_details(json!({ "session_id": self.session_id })))
        }
    }

    /// Closes a not-yet-promoted terminal on a task the caller cannot drop.
    ///
    /// Detaching here has to survive one invariant `force_reap_terminal` does
    /// not carry: `StartupCleanupGuard::drop` requeues this exact owner for a
    /// later retry, so the terminal cannot simply be moved into a task and
    /// forgotten -- a requeued owner with no terminal could never retry
    /// anything. The terminal is moved into the task *and* the join handle is
    /// parked in [`PendingStartupTerminal::Closing`], so a cancelled caller
    /// requeues an owner that still knows where its terminal went; the retry
    /// adopts the same task rather than issuing a second kill for a request the
    /// first attempt already delivered, and the terminal is handed back when it
    /// finishes. This adoption is what
    /// [`PendingStartupCleanup::close_terminal`] describes as belonging to this
    /// arm alone — the promoted arm re-enters `close` instead.
    ///
    /// That parking is also why the task needs no `TrackedTasks` permit to be
    /// accounted for at shutdown. `NativeService::shutdown` drains the pending
    /// queue through `close_terminal` *before* it tears the private runtime
    /// down, so the close is always joined rather than merely outlived -- a
    /// stronger property than a permit, which only proves something finished.
    async fn close_raw_terminal(&mut self) -> Result<bool, ErrorBody> {
        if matches!(self.terminal, PendingStartupTerminal::Raw(_)) {
            let PendingStartupTerminal::Raw(mut terminal) =
                std::mem::replace(&mut self.terminal, PendingStartupTerminal::Lost)
            else {
                unreachable!("the variant was just observed to be Raw")
            };
            self.terminal = PendingStartupTerminal::Closing(tokio::spawn(async move {
                let closed = terminal.close().await;
                (terminal, closed)
            }));
        }
        let PendingStartupTerminal::Closing(close) = &mut self.terminal else {
            // `Lost`: an earlier attempt's task ended without returning the
            // terminal, so there is nothing here left to close or to retry.
            // PrivateRuntime shutdown remains the final cleanup authority.
            return Err(ErrorBody::new(
                ErrorCode::RecoveryFailed,
                "startup terminal close task did not return its terminal",
            )
            .retryable(false)
            .with_details(json!({ "session_id": self.session_id })));
        };
        // Awaiting `&mut JoinHandle` keeps the handle parked in `self` for the
        // whole round trip, so cancelling here requeues an owner that can still
        // adopt this task. The handle is replaced before any later await, so it
        // can never be polled twice.
        match close.await {
            Ok((terminal, closed)) => {
                self.terminal = PendingStartupTerminal::Raw(terminal);
                closed.map_err(map_startup_terminal_error)
            }
            Err(_) => {
                self.terminal = PendingStartupTerminal::Lost;
                Err(ErrorBody::new(
                    ErrorCode::RecoveryFailed,
                    "startup terminal close task did not complete",
                )
                .retryable(false)
                .with_details(json!({ "session_id": self.session_id })))
            }
        }
    }

    /// Closes an already-promoted terminal on a task the caller cannot drop.
    ///
    /// Simpler than [`Self::close_raw_terminal`] only because
    /// `RmuxTerminalControl` is already shared behind an `Arc` and holds the
    /// terminal under its own mutex for the whole close, so a clone can be
    /// moved into the task without taking anything out of `self`.
    async fn close_controlled_terminal(&mut self) -> Result<bool, ErrorBody> {
        let PendingStartupTerminal::Controlled(terminal) = &self.terminal else {
            unreachable!("only a promoted startup terminal is closed through its control handle")
        };
        let terminal = Arc::clone(terminal);
        let session_id = self.session_id;
        let close = tokio::spawn(async move {
            terminal
                .close(session_id, pseudomux_protocol::v1::ClosePolicy::Force)
                .await
        });
        match close.await {
            Ok(closed) => closed.map_err(DriverFailureExt::protocol),
            Err(_) => Err(ErrorBody::new(
                ErrorCode::RecoveryFailed,
                "startup terminal close task did not complete",
            )
            .retryable(false)
            .with_details(json!({ "session_id": self.session_id }))),
        }
    }

    async fn shutdown(self) {
        #[cfg(unix)]
        {
            let mut cleanup = self;
            if let Some(lifecycle) = cleanup._lifecycle.as_mut() {
                lifecycle.shutdown().await;
            }
        }
        #[cfg(not(unix))]
        drop(self);
    }

    fn into_metadata(
        mut self,
        generation_id: SessionGenerationId,
        transcript: Arc<FileTranscriptSource>,
        private_session_name: String,
        cell: SessionCell,
        owner: SessionOwner,
    ) -> SessionMetadata {
        let PendingStartupTerminal::Controlled(terminal) = self.terminal else {
            panic!("only a ready controlled terminal can be published")
        };
        SessionMetadata {
            generation_id,
            terminal,
            transcript,
            private_session_name,
            cell,
            owner,
            _sensitive_launch: self
                ._sensitive_launch
                .take()
                .expect("published startup retains sensitive launch ownership"),
            #[cfg(unix)]
            _lifecycle: self
                ._lifecycle
                .take()
                .expect("published startup retains lifecycle ownership"),
        }
    }
}

enum PendingStartupTerminal {
    Raw(Box<dyn TerminalSession>),
    Controlled(Arc<RmuxTerminalControl>),
    /// Temporary sentinel used only during the synchronous Raw -> Controlled move.
    Promoting,
    /// A not-yet-promoted terminal handed to a detached close task, with the
    /// handle that gives it back. Parking the handle here is what lets a
    /// cancelled `close_terminal` requeue an owner that can still adopt the
    /// close it started. See [`PendingStartupCleanup::close_raw_terminal`].
    Closing(
        tokio::task::JoinHandle<(
            Box<dyn TerminalSession>,
            Result<bool, pseudomux_rmux::TerminalBackendError>,
        )>,
    ),
    /// A detached close task ended without returning the terminal, which only
    /// happens if it panicked or the runtime dropped it. Nothing local is left
    /// to close; PrivateRuntime shutdown is the remaining cleanup authority.
    Lost,
}

/// Cancellation guard for every resource acquired after terminal creation.
/// Dropping the start future moves the exact owner into the service retry queue
/// instead of relying on an asynchronous terminal `Drop` side effect.
struct StartupCleanupGuard {
    cleanup: Option<PendingStartupCleanup>,
    pending: Arc<StdMutex<Vec<PendingStartupCleanup>>>,
}

impl StartupCleanupGuard {
    fn new(
        session_id: SessionId,
        terminal: Box<dyn TerminalSession>,
        sensitive_launch: SensitiveLaunchFiles,
        #[cfg(unix)] lifecycle: SessionLifecycle,
        pending: Arc<StdMutex<Vec<PendingStartupCleanup>>>,
    ) -> Self {
        Self {
            cleanup: Some(PendingStartupCleanup {
                session_id,
                terminal: PendingStartupTerminal::Raw(terminal),
                _sensitive_launch: Some(sensitive_launch),
                #[cfg(unix)]
                _lifecycle: Some(lifecycle),
            }),
            pending,
        }
    }

    fn cleanup_mut(&mut self) -> &mut PendingStartupCleanup {
        self.cleanup
            .as_mut()
            .expect("startup cleanup guard was already disarmed")
    }

    fn from_cleanup(
        cleanup: PendingStartupCleanup,
        pending: Arc<StdMutex<Vec<PendingStartupCleanup>>>,
    ) -> Self {
        Self {
            cleanup: Some(cleanup),
            pending,
        }
    }

    fn into_cleanup(mut self) -> PendingStartupCleanup {
        self.cleanup
            .take()
            .expect("startup cleanup guard was already disarmed")
    }

    fn into_metadata(
        mut self,
        generation_id: SessionGenerationId,
        transcript: Arc<FileTranscriptSource>,
        private_session_name: String,
        cell: SessionCell,
        owner: SessionOwner,
    ) -> SessionMetadata {
        self.cleanup
            .take()
            .expect("startup cleanup guard was already disarmed")
            .into_metadata(generation_id, transcript, private_session_name, cell, owner)
    }
}

impl Drop for StartupCleanupGuard {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            self.pending
                .lock()
                .expect("pending startup cleanup lock poisoned")
                .push(cleanup);
        }
    }
}

/// Value delivered from the independently completing terminal-creation task.
/// If the request future is cancelled after the task sends but before it takes
/// the value, dropping the channel payload retains the terminal for cleanup.
struct TerminalCreationDelivery {
    session_id: SessionId,
    terminal: Option<Box<dyn TerminalSession>>,
    pending: Arc<StdMutex<Vec<PendingStartupCleanup>>>,
}

impl TerminalCreationDelivery {
    fn take(mut self) -> Box<dyn TerminalSession> {
        self.terminal
            .take()
            .expect("terminal creation delivery can only be consumed once")
    }
}

impl Drop for TerminalCreationDelivery {
    fn drop(&mut self) {
        if let Some(terminal) = self.terminal.take() {
            self.pending
                .lock()
                .expect("pending startup cleanup lock poisoned")
                .push(PendingStartupCleanup::terminal_only(
                    self.session_id,
                    terminal,
                ));
        }
    }
}

#[cfg(unix)]
struct SessionLifecycle {
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

#[cfg(unix)]
impl SessionLifecycle {
    fn start(
        prepared: crate::hybrid_hooks::PreparedLifecycle,
        stop_sequence: Arc<AtomicU64>,
        stop_at_ms: Arc<AtomicU64>,
        tasks: &Arc<TrackedTasks>,
    ) -> Self {
        let Some(mut hybrid) = prepared.into_hybrid() else {
            return Self {
                shutdown: None,
                task: None,
            };
        };
        let (shutdown, mut shutdown_requested) = oneshot::channel();
        let task_permit = tasks.track();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = &mut shutdown_requested => break,
                    observation = hybrid.recv() => {
                        let Some(observation) = observation else {
                            break;
                        };
                        if matches!(
                            observation.event(),
                            crate::hybrid_hooks::LifecycleEventKind::Stop
                                | crate::hybrid_hooks::LifecycleEventKind::StopFailure
                        ) {
                            // Stamp the instant *before* publishing the
                            // sequence bump. Readers acquire the sequence first
                            // and only then read the stamp, so this ordering
                            // guarantees they never pair a fresh sequence with
                            // a stale instant.
                            record_lifecycle_stop_instant(stop_at_ms.as_ref());
                            let _ = increment_lifecycle_stop_sequence(stop_sequence.as_ref());
                        }
                    }
                }
            }
            // Cooperative Hybrid shutdown owns every accepted connection and
            // returns only after its relay task is quiescent. HybridLifecycle's
            // final Drop then unlinks the owner-only artifacts.
            hybrid.shutdown().await;
            drop(task_permit);
        });
        Self {
            shutdown: Some(shutdown),
            task: Some(task),
        }
    }

    async fn shutdown(&mut self) {
        self.request_shutdown();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }

    fn request_shutdown(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

/// Records the wall-clock instant of a Stop/StopFailure hook, in the same
/// UNIX-millisecond domain as every protocol `*_at_ms` field.
///
/// `0` is the "never observed" sentinel, so an instant that is unrepresentable
/// in protocol-v1's safe integer domain — or that lands exactly on the epoch —
/// is dropped rather than stored: an absent measurement is honest, a clamped
/// one is not. The hook's own arrival is never gated on this succeeding.
///
/// `driver_io::now_unix_ms` is the equivalent conversion on the driver side. It
/// stays private there because it returns a `DriverFailure`, which has no
/// meaning in this lifecycle task; this is the local equivalent rather than a
/// widened visibility.
#[cfg(unix)]
fn record_lifecycle_stop_instant(stop_at_ms: &AtomicU64) {
    let Ok(elapsed) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return;
    };
    if let Some(milliseconds) = representable_stop_instant(elapsed) {
        stop_at_ms.store(milliseconds, Ordering::Release);
    }
}

/// The stored form of one measured instant, or `None` when there is no honest
/// one to store.
///
/// Separated from the clock read because the clock read is not a decision and
/// this is: every branch here refuses a value, and a caller holding a real
/// `SystemTime` can reach none of them. A full-scope mutation run made both of
/// these comparisons unable to refuse anything and no test in the workspace
/// could tell, for exactly that reason -- the only instants the suite ever put
/// through this function were the ones the host's clock happened to produce.
#[cfg(unix)]
fn representable_stop_instant(elapsed: Duration) -> Option<u64> {
    u64::try_from(elapsed.as_millis())
        .ok()
        .filter(|milliseconds| *milliseconds != 0 && *milliseconds <= MAX_SAFE_JSON_INTEGER)
}

#[cfg(unix)]
fn increment_lifecycle_stop_sequence(sequence: &AtomicU64) -> Option<u64> {
    sequence
        .fetch_update(Ordering::Release, Ordering::Relaxed, |current| {
            current
                .checked_add(1)
                .filter(|next| *next <= MAX_SAFE_JSON_INTEGER)
        })
        .ok()
        .and_then(|previous| previous.checked_add(1))
}

#[cfg(unix)]
impl Drop for SessionLifecycle {
    fn drop(&mut self) {
        // Do not abort the owner task: a dropped close/start future must still
        // drive HybridLifecycle through its synchronous artifact cleanup. The
        // service-level tracker awaits detached owners during daemon shutdown.
        self.request_shutdown();
        drop(self.task.take());
    }
}

struct IdleReaper {
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl IdleReaper {
    async fn shutdown(&mut self) {
        self.request_shutdown();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }

    fn request_shutdown(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

impl Drop for IdleReaper {
    fn drop(&mut self) {
        // The in-progress cleanup pass owns metadata. Request a cooperative
        // stop and let it finish; aborting it could detach a Hybrid owner after
        // the metadata had already been removed from the public session map.
        self.request_shutdown();
        drop(self.task.take());
    }
}

/// Who decides one start's retention.
///
/// A DECISION RATHER THAN A VALUE, and that distinction is the whole point.
/// `run_once` used to express "this session is one-shot" by writing
/// `RetentionPolicy::OneShot` into the request before starting it -- and agent
/// resolution, which replaces the entire launch policy with the stored version,
/// then replaced that too. A value written into the request is a value the
/// resolver owns; a decision carried beside it is one the method owns and can
/// apply after resolution has run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Retention {
    /// Whatever the resolved request carries: the caller's own `retention` for
    /// an inline start, or the stored agent's for one that named a version.
    AsResolved,
    /// One-shot, whatever the request or the agent said. Reserved for methods
    /// that close the session themselves.
    ForcedOneShot,
}

/// The retention one session is registered with, once every source has spoken.
///
/// PURE, and lifted out of `start_session_owned_with_retention` so the one
/// interaction that used to be wrong -- a forced one-shot meeting an agent's
/// stored `Persistent` -- is assertable without a Claude process, a PTY or a
/// daemon. See `a_forced_one_shot_survives_an_agents_stored_retention`.
fn decide_retention(decision: Retention, resolved: RetentionPolicy) -> RetentionPolicy {
    match decision {
        Retention::AsResolved => resolved,
        Retention::ForcedOneShot => RetentionPolicy::OneShot,
    }
}

/// The idle TTL a session is registered with, which is the OBSERVABLE half of
/// [`decide_retention`]: a one-shot session has no TTL, because the method that
/// asked for it closes it.
fn idle_ttl_ms_for(retention: &RetentionPolicy) -> Option<u64> {
    match retention {
        RetentionPolicy::OneShot => None,
        RetentionPolicy::Persistent { idle_ttl_ms } => Some(*idle_ttl_ms),
    }
}

/// Process-wide owner of the private terminal runtime and v1 session registry.
pub struct NativeService {
    /// The private runtime, held as [`SessionRuntime`] rather than as the
    /// concrete `PrivateRuntime` so a test can build this service without a
    /// live rmux sidecar. See that trait for what the seam is worth.
    runtime: Arc<dyn SessionRuntime>,
    registry: Arc<SessionRegistry>,
    config: NativeServiceConfig,
    sessions: RwLock<HashMap<SessionId, SessionMetadata>>,
    pending_startup_cleanup: Arc<StdMutex<Vec<PendingStartupCleanup>>>,
    closed_sessions: RwLock<ClosedSessionTombstones>,
    /// Serializes the complete start/publication and explicit-close transactions.
    start_guard: Mutex<()>,
    idle_reaper: StdMutex<Option<IdleReaper>>,
    maintenance_tasks: Arc<TrackedTasks>,
    lifecycle_tasks: Arc<TrackedTasks>,
    shutdown_started: AtomicBool,
    /// The stateless pool, once `start_pool` has built it.
    ///
    /// Behind a lock and not a constructor argument because the pool's host
    /// holds a `Weak<NativeService>`: the service must exist before the host
    /// can point at it. `OnceLock` rather than `Mutex<Option<_>>` so the pool
    /// is write-once -- a daemon that could swap its pool could strand live
    /// instances in a pool nothing tears down.
    pool: std::sync::OnceLock<Arc<Pool>>,
    /// The agent store, opened once at construction and REFUSED at boot if its
    /// directory is not owner-only and owned by this user.
    agent_store: Option<crate::agent::AgentStore>,
}

impl NativeService {
    pub async fn start(
        runtime_config: PrivateRuntimeConfig,
        service_config: NativeServiceConfig,
    ) -> Result<Arc<Self>, ErrorBody> {
        validate_transcript_drain_ms(service_config.untested_transcript_drain_ms)
            .map_err(|error| ErrorBody::new(ErrorCode::InvalidConfig, error.to_string()))?;
        let pool_config = service_config.pool.clone();
        // OPENED BEFORE THE RUNTIME EXISTS. A store whose directory is not
        // owner-only fails startup, and it fails it before an rmux sidecar or
        // a runtime directory has been created, so a refused configuration
        // leaves nothing behind.
        let agent_store = service_config
            .agent_store
            .as_deref()
            .map(crate::agent::AgentStore::open)
            .transpose()?;
        let runtime = PrivateRuntime::start(runtime_config)
            .await
            .map_err(|error| ErrorBody::new(ErrorCode::RmuxUnavailable, error.to_string()))?;
        let service = Arc::new(Self::from_runtime_with_agent_store(
            Arc::new(runtime),
            service_config,
            agent_store,
        ));
        service.start_idle_reaper();
        if let Some(pool_config) = pool_config {
            if let Err(error) = service.start_pool(pool_config).await {
                // A FAILED START OWNS WHAT IT MINTED, and this is the arm that
                // establishes it. `start_pool` publishes the pool before the
                // first instance is minted precisely so this handle exists; up
                // to here nothing used it, and a warm mint that failed on its
                // third instance simply abandoned the first two -- their
                // children unreaped by pmux and their epoch trees on disk.
                //
                // MEASURED, because the cost is not linear. A failed start
                // erases the ONE tree it collided with (`mint_roots` refuses,
                // `abandon_mint` destroys) and abandoned every tree it had
                // minted before it, so the leftover set went
                // `L -> (L \ {min L}) union {0..min L - 1}`. Starting from the
                // three trees a SIGTERM'd `--path-b-warm ...=3` leaves, that
                // recurrence took **7 consecutive refusing restarts** before
                // one served -- 2^3 - 1, exactly, and 2^15 - 1 = 32,767 at the
                // owner's 15-instance cap. Draining here makes the leftover set
                // strictly shrink, so the chain is bounded by the number of
                // trees left rather than by an exponential in the highest slot
                // index among them.
                //
                // Best effort and never masking: the startup error the operator
                // asked about is the one returned, and a drain failure is
                // recorded beside it rather than replacing it.
                if let Err(drain) = service.shutdown().await {
                    tracing::error!(
                        operation = "path_b_startup_drain",
                        code = ?drain.code,
                        "a failed pool startup could not drain what it had already minted"
                    );
                }
                return Err(error);
            }
        }
        Ok(service)
    }

    /// Build the stateless pool and mint its operator-declared warm set.
    ///
    /// Failure here fails daemon startup, and deliberately: a warm set that
    /// cannot be minted is an operator error worth refusing to boot over, not a
    /// degraded mode nobody reads the log line for.
    async fn start_pool(self: &Arc<Self>, config: PoolConfig) -> Result<(), ErrorBody> {
        let pool = Pool::new(
            config,
            Arc::new(crate::stateless::NativeInstanceHost::new(self)?),
            Arc::new(crate::v1::SystemClock),
            Arc::new(TrackedSpawner::new(Arc::clone(&self.maintenance_tasks))),
        );
        // Published BEFORE the warm set is minted. `Pool::start` mints real
        // Claude processes; if startup is abandoned midway, a pool nothing has
        // a handle to is a pool whose instances `shutdown` cannot reach.
        if self.pool.set(Arc::clone(&pool)).is_err() {
            return Err(ErrorBody::new(
                ErrorCode::Internal,
                "the stateless pool was already built for this service",
            ));
        }
        pool.start().await
    }

    /// The stateless pool, when this daemon serves Path B.
    #[must_use]
    pub fn pool(&self) -> Option<&Arc<Pool>> {
        self.pool.get()
    }

    fn from_runtime_with_agent_store(
        runtime: Arc<dyn SessionRuntime>,
        config: NativeServiceConfig,
        agent_store: Option<crate::agent::AgentStore>,
    ) -> Self {
        // One counter covers every detached task this service is responsible
        // for, whichever layer spawns it: terminal creation that outlived its
        // request (`create_terminal_for_start`), the idle reaper, and the
        // actors' detached `close(Force)`. Sharing it is what lets `shutdown`
        // fence all three with one await instead of guessing at each.
        let maintenance_tasks = Arc::new(TrackedTasks::default());
        Self {
            runtime,
            registry: Arc::new(
                SessionRegistry::new(config.actor.clone())
                    .with_detached_tasks(Arc::clone(&maintenance_tasks)),
            ),
            config,
            sessions: RwLock::new(HashMap::new()),
            pending_startup_cleanup: Arc::new(StdMutex::new(Vec::new())),
            closed_sessions: RwLock::new(ClosedSessionTombstones::new(
                CLOSED_SESSION_TOMBSTONE_CAPACITY,
            )),
            start_guard: Mutex::new(()),
            idle_reaper: StdMutex::new(None),
            maintenance_tasks,
            lifecycle_tasks: Arc::new(TrackedTasks::default()),
            shutdown_started: AtomicBool::new(false),
            pool: std::sync::OnceLock::new(),
            agent_store,
        }
    }

    fn start_idle_reaper(self: &Arc<Self>) {
        let interval = self
            .config
            .idle_reaper_interval
            .max(Duration::from_millis(1));
        let service = Arc::downgrade(self);
        let (shutdown, mut shutdown_requested) = oneshot::channel();
        let task_permit = self.maintenance_tasks.track();
        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    _ = &mut shutdown_requested => break,
                    _ = ticker.tick() => {}
                }
                let Some(service) = service.upgrade() else {
                    break;
                };
                // Deliberately outside the select: once a pass owns cleanup
                // resources, shutdown waits for the whole pass instead of
                // cancelling it between map removal and lifecycle teardown.
                service.reap_idle_sessions().await;
                // The pool's sweep, on the same tick and NOT in the same pass.
                // `reap_idle_sessions` skips pool-owned sessions positively, so
                // without this line a pool instance past its TTL would be swept
                // by nobody at all -- which is how "excluded from the generic
                // reaper" quietly becomes "never reaped".
                if let Some(pool) = service.pool() {
                    pool.sweep_idle().await;
                }
            }
            drop(task_permit);
        });
        *self.idle_reaper.lock().expect("idle reaper lock poisoned") = Some(IdleReaper {
            shutdown: Some(shutdown),
            task: Some(task),
        });
    }

    async fn reap_idle_sessions(&self) {
        self.reap_pending_startup_cleanup().await;
        let sessions: Vec<_> = self
            .sessions
            .read()
            .await
            .iter()
            // The pool's instances are skipped HERE, at the enumeration, and
            // not by relying on `expire_idle` refusing them further down. The
            // registry does refuse them -- see `SessionRegistry::expire_idle` --
            // but a reaper that only ever declines because its callee said no
            // is a reaper that starts closing pool instances the day someone
            // adds a second path to the same close.
            .filter(|(_, metadata)| metadata.owner == SessionOwner::Caller)
            .map(|(session_id, metadata)| (*session_id, metadata.generation_id))
            .collect();
        let Ok(now_ms) = unix_now_ms() else {
            // Wall-clock conversion failure is fail-closed: never expire a
            // session using an unrepresentable synthetic timestamp.
            return;
        };
        for (session_id, generation_id) in sessions {
            let Ok(Some(result)) = self
                .registry
                .expire_idle(session_id, generation_id, now_ms)
                .await
            else {
                continue;
            };
            if !result.process_reaped {
                continue;
            }
            let metadata = {
                let mut sessions = self.sessions.write().await;
                if self
                    .registry
                    .unregister(session_id, generation_id)
                    .await
                    .is_err()
                {
                    continue;
                }
                if sessions
                    .get(&session_id)
                    .is_some_and(|metadata| metadata.generation_id == generation_id)
                {
                    sessions.remove(&session_id)
                } else {
                    None
                }
            };
            if let Some(metadata) = metadata {
                metadata.shutdown().await;
            }
            self.closed_sessions
                .write()
                .await
                .insert(session_id, generation_id);
        }
    }

    async fn reap_pending_startup_cleanup(&self) {
        let pending = take_retryable_startup_cleanup(self.pending_startup_cleanup.as_ref());
        if pending.is_empty() {
            return;
        }

        for cleanup in pending {
            let mut cleanup = StartupCleanupGuard::from_cleanup(
                cleanup,
                Arc::clone(&self.pending_startup_cleanup),
            );
            if cleanup.cleanup_mut().close_terminal().await.is_err() {
                // Dropping the attempt atomically returns the still-owned
                // resources to the retry queue, including when this future is
                // itself cancelled during the close operation.
                drop(cleanup);
            } else {
                cleanup.into_cleanup().shutdown().await;
            }
        }
    }

    async fn finish_failed_start(
        &self,
        startup_error: ErrorBody,
        mut cleanup: StartupCleanupGuard,
    ) -> ErrorBody {
        match cleanup.cleanup_mut().close_terminal().await {
            Ok(()) => {
                cleanup.into_cleanup().shutdown().await;
                startup_error
            }
            Err(cleanup_error) =>
            // The guard requeues the exact resources after the combined public
            // error has been constructed; cancellation does the same.
            {
                combine_startup_and_cleanup_errors(startup_error, cleanup_error)
            }
        }
    }

    async fn create_terminal_for_start(
        &self,
        session_id: SessionId,
        rows: u16,
        cols: u16,
        process: LaunchSpec,
    ) -> Result<Box<dyn TerminalSession>, ErrorBody> {
        let runtime = Arc::clone(&self.runtime);
        let pending = Arc::clone(&self.pending_startup_cleanup);
        let task_permit = self.maintenance_tasks.track();
        let (delivery, created) = oneshot::channel();
        tokio::spawn(async move {
            let result = runtime
                .create_terminal(session_id, rows, cols, process)
                .await;
            let result = result.map(|terminal| TerminalCreationDelivery {
                session_id,
                terminal: Some(terminal),
                pending,
            });
            // A dropped receiver drops TerminalCreationDelivery here. Its Drop
            // moves a successfully created terminal into the service retry
            // queue before this task releases its completion permit.
            let _ = delivery.send(result);
            drop(task_permit);
        });

        // These two failures were previously one arm, which was wrong twice
        // over: it discarded the typed cause the backend had just produced, and
        // it reported "rmux is unavailable" for a case in which rmux was never
        // even asked. They are split here, and each carries a content-free
        // classification on the wire and in exactly one log line.
        match created.await {
            Ok(Ok(terminal)) => Ok(terminal.take()),
            Ok(Err(error)) => {
                let cause = error.cause();
                tracing::warn!(
                    operation = "create_terminal",
                    session_id = %session_id,
                    cause,
                    "private terminal startup failed"
                );
                Err(map_startup_create_terminal_error(error)
                    .with_details(json!({ "cause": cause })))
            }
            // The delivery oneshot was dropped without a value. That only
            // happens if the creation task was cancelled or panicked, which is
            // a pmux-internal fault and says nothing about the health of rmux.
            // `TerminalCreationDelivery::drop` has already returned any
            // successfully created terminal to the retry queue.
            Err(_) => {
                let cause = "startup_delivery_dropped";
                tracing::warn!(
                    operation = "create_terminal",
                    session_id = %session_id,
                    cause,
                    "private terminal startup did not deliver a result"
                );
                Err(ErrorBody::new(
                    ErrorCode::RmuxUnavailable,
                    "private terminal startup did not complete",
                )
                .retryable(true)
                .with_details(json!({ "cause": cause })))
            }
        }
    }

    pub async fn dispatch(self: &Arc<Self>, request: Request) -> Result<ResponseResult, ErrorBody> {
        validate_native_request(&request)?;
        match request {
            // This arm never dereferences `self`, and that is not an oversight:
            // a liveness check for the accept loop must not be able to fail for
            // a reason the accept loop is not responsible for. It is also
            // exactly why `Ping` is not, and can never be, a health check --
            // the private runtime, the session registry, the launch broker and
            // the rmux sidecar all hang off `self` and none of them is touched
            // here. `Request::Diagnose` is the request that touches them.
            Request::Ping => Ok(ResponseResult::Pong(Pong {
                server_version: env!("CARGO_PKG_VERSION").to_owned(),
                protocol_version: PROTOCOL_VERSION,
            })),
            Request::Diagnose => Ok(ResponseResult::Diagnosis(Box::new(self.diagnose().await))),
            Request::StartSession(request) => self
                .start_session(request)
                .await
                .map(ResponseResult::SessionStarted),
            Request::RunTurn(request) => self
                .run_turn(request)
                .await
                .map(ResponseResult::TurnAccepted),
            Request::CancelTurn(request) => self
                .registry
                .cancel_turn(request)
                .await
                .map(ResponseResult::TurnCancelled),
            Request::InspectSession(request) => self
                .registry
                .inspect(request)
                .await
                .map(|snapshot| ResponseResult::SessionSnapshot(Box::new(snapshot))),
            Request::AttachSession(request) => self.attach(request).await,
            Request::CloseSession(request) => {
                dispatch_close_session(
                    self.registry.as_ref(),
                    &self.sessions,
                    &self.closed_sessions,
                    &self.start_guard,
                    request,
                )
                .await
            }
            Request::SubscribeEvents(request) => {
                validate_subscribe_events(&request)?;
                self.registry
                    .events(request)
                    .await
                    .map(ResponseResult::Events)
            }
            Request::RunOnce(request) => self
                .run_once(request)
                .await
                .map(|result| ResponseResult::TurnResult(Box::new(result))),
            Request::ClearSession(request) => self
                .clear_session(request)
                .await
                .map(ResponseResult::SessionCleared),
            // The stateless pool lives in `crate::pool` and is not wired into
            // this service yet. This arm is the single line the integration
            // step replaces with a call into `Pool::run`; until then the daemon
            // refuses honestly rather than pretending the capability exists.
            Request::RunStateless(request) => crate::stateless::run_stateless(self.pool(), request)
                .await
                .map(|result| ResponseResult::StatelessResult(Box::new(result))),
            Request::CreateAgent(request) => self
                .agent_store()?
                .create(request.spec, now_ms())
                .map(|descriptor| ResponseResult::AgentCreated(Box::new(descriptor))),
            Request::GetAgent(request) => self
                .agent_store()?
                .get(request.agent_id, request.version)
                .map(|descriptor| ResponseResult::Agent(Box::new(descriptor))),
            Request::ListAgents(ListAgentsRequest {}) => self
                .agent_store()?
                .list()
                .map(|list| ResponseResult::AgentList(Box::new(list))),
            Request::UpdateAgent(request) => self
                .agent_store()?
                .update(
                    request.agent_id,
                    request.expected_version,
                    request.spec,
                    now_ms(),
                )
                .map(|descriptor| ResponseResult::AgentUpdated(Box::new(descriptor))),
        }
    }

    /// The agent store, or the refusal a daemon that was started without one
    /// owes a caller who asks for an agent.
    ///
    /// Named rather than defaulted: a daemon with no store does not silently
    /// grow one on first use, because that would be a directory created on a
    /// caller's request path, and every other directory in this product is
    /// created at boot or refused.
    fn agent_store(&self) -> Result<&crate::agent::AgentStore, ErrorBody> {
        self.agent_store.as_ref().ok_or_else(missing_agent_store)
    }

    /// Replaces an [`AgentRef`] with the launch configuration it names, and
    /// applies that agent's containment rules.
    ///
    /// The only impure step in the whole agent path: one read of one immutable
    /// file, keyed by a version the request itself named. Everything after it
    /// is [`crate::agent::resolve_agent_start`], which is pure.
    ///
    /// Containment runs HERE, before `admit_bound_resources`, and it can only
    /// ADD a refusal: no value of `workspace_root` or
    /// `require_config_isolation` makes an otherwise-refused start admissible,
    /// because both are asked before the existing rules and neither writes
    /// anything into the request.
    fn resolve_agent_reference(
        &self,
        request: &mut StartSessionRequest,
        retention: Retention,
    ) -> Result<Option<SessionAgentPin>, ErrorBody> {
        resolve_agent_and_retention(self.agent_store.as_ref(), request, retention)
    }
}

/// Replaces an [`AgentRef`] with the launch configuration it names, and THEN
/// applies the calling method's retention decision.
///
/// **THE ORDER IS THE FIX.** `run_once` used to express "this session is
/// one-shot" by writing `RetentionPolicy::OneShot` into the request before the
/// start ran; resolution replaced the whole launch policy with the stored
/// agent's, including its `Persistent { idle_ttl_ms }`, and a `pmux run
/// --agent` registered the agent's idle TTL instead of `None`. The two steps
/// live in one function so the order is a property of one place, and
/// `a_forced_one_shot_survives_the_agent_it_resolves` runs this exact function
/// against a real store with no daemon, no PTY and no Claude.
///
/// It is a free function taking the store rather than a method for that reason:
/// the impure step is one read of one immutable file, and nothing else about a
/// `NativeService` is involved.
fn resolve_agent_and_retention(
    store: Option<&crate::agent::AgentStore>,
    request: &mut StartSessionRequest,
    retention: Retention,
) -> Result<Option<SessionAgentPin>, ErrorBody> {
    let pin = resolve_agent_reference_into(store, request)?;
    // AFTER resolution, because it overrides what resolution produced. This is
    // the ONE field a method is allowed to decide over an agent's stored value,
    // and only because the method closes the session itself: a one-shot
    // session's retention is a property of `run_once`, not of the configuration
    // it launches.
    request.retention = decide_retention(retention, request.retention.clone());
    Ok(pin)
}

fn missing_agent_store() -> ErrorBody {
    ErrorBody::new(
        ErrorCode::InvalidConfig,
        "this daemon serves no agent store, so no agent can be stored, read or referenced",
    )
    .with_details(json!({
        "recommendation": "restart pmuxd with --agent-store DIR, or send the inline launch \
                           fields on each start_session"
    }))
}

fn resolve_agent_reference_into(
    store: Option<&crate::agent::AgentStore>,
    request: &mut StartSessionRequest,
) -> Result<Option<SessionAgentPin>, ErrorBody> {
    let Some(reference) = request.agent else {
        return Ok(None);
    };
    let (spec, config_digest) = store
        .ok_or_else(missing_agent_store)?
        .load_for_launch(reference)?;
    crate::agent::admit_agent_containment(
        &spec.containment,
        reference.agent_id,
        Path::new(&request.cwd),
        request.config_isolation.as_ref(),
    )?;
    let (resolved, pin) = crate::agent::resolve_agent_start(
        &spec,
        &config_digest,
        reference,
        std::mem::replace(request, placeholder_start_request()),
    );
    *request = resolved;
    // The resolved DTO goes through the SAME public preflight an inline request
    // went through on the way in. A stored spec that was admissible when it was
    // written and is not admissible now -- because a validator moved -- is
    // refused at the start it would have launched, rather than silently
    // launched under the older rule.
    validate_native_request(request)?;
    validate_public_start_retention(&request.retention)?;
    Ok(Some(pin))
}

impl NativeService {
    pub async fn start_session(
        self: &Arc<Self>,
        request: StartSessionRequest,
    ) -> Result<SessionHandle, ErrorBody> {
        validate_native_request(&request)?;
        // An agent-named start has no `retention` of its own yet: it is refused
        // above if it carried one, and the stored value has not been read.
        // `resolve_agent_reference` re-applies both preflights to the RESOLVED
        // request, which is the one that describes what will launch.
        if request.agent.is_none() {
            validate_public_start_retention(&request.retention)?;
        }
        self.start_session_internal(request).await
    }

    /// Brings a private configuration root to the state this session needs.
    ///
    /// The write is conditional on no live session being bound to the root.
    /// Claude writes `.claude.json` itself, under its own lock and with its own
    /// stale-write repair path; pmux does not implement that protocol, so the
    /// only two honest positions are "I am the sole writer" and "I will not
    /// write". A root already hosting a session gets a read-only check and,
    /// when the required state is absent, a refusal.
    ///
    /// No per-root mutex is introduced. `start_session_internal` holds
    /// `start_guard` across its whole body, so every seed in one daemon is
    /// already serialized against every other; a second lock would only be able
    /// to disagree with the first.
    fn seed_config_isolation_root(
        config_root: &Path,
        resolved: &ResolvedClaudeLaunch,
        disposition: SeedDisposition,
    ) -> Result<(), ErrorBody> {
        seed_private_config_root(
            &ConfigRootSeed {
                root: config_root,
                trusted_cwd: &resolved.process.cwd,
                dangerous_permission_bypass: resolved.dangerous_permission_bypass,
            },
            disposition,
        )
        .map(|_| ())
        .map_err(|error| ErrorBody::new(ErrorCode::InvalidConfig, format!("{error:#}")))
    }

    async fn start_session_internal(
        self: &Arc<Self>,
        request: StartSessionRequest,
    ) -> Result<SessionHandle, ErrorBody> {
        self.start_session_owned_with_retention(
            request,
            SessionOwner::Caller,
            Retention::AsResolved,
        )
        .await
    }

    /// The one start path, with the owner named at the call site.
    ///
    /// Every resource rule this function already enforces -- the containment
    /// walk over live claims, the pristine-root scan, the transcript-identity
    /// check -- runs for a pool mint exactly as it runs for a caller start, and
    /// that is why the pool's roots are published into `self.sessions` at all
    /// rather than held somewhere the admission rules cannot see them. A pool
    /// instance that were invisible to `live_resource_claims` would be a
    /// directory a Path A caller could name and be admitted to.
    pub(crate) async fn start_session_owned(
        self: &Arc<Self>,
        request: StartSessionRequest,
        owner: SessionOwner,
    ) -> Result<SessionHandle, ErrorBody> {
        self.start_session_owned_with_retention(request, owner, Retention::AsResolved)
            .await
    }

    /// The one start path, with the owner AND the retention decision named at
    /// the call site.
    ///
    /// [`Retention::ForcedOneShot`] exists because `run_once` decides its
    /// session's retention itself and must decide it AFTER agent resolution.
    /// `run_once` used to set `retention = OneShot` on the request before this
    /// function ran; `resolve_agent_reference` then replaced the whole launch
    /// policy with the stored agent's, including its `Persistent`, so a
    /// `pmux run --agent` registered the agent's idle TTL instead of `None` --
    /// a value pmux itself wrote and pmux itself discarded.
    pub(crate) async fn start_session_owned_with_retention(
        self: &Arc<Self>,
        mut request: StartSessionRequest,
        owner: SessionOwner,
        retention: Retention,
    ) -> Result<SessionHandle, ErrorBody> {
        // AGENT RESOLUTION IS THE FIRST THING THAT HAPPENS, and it happens
        // exactly once, at the one door every start goes through.
        //
        // After this line the request is a `StartSessionRequest` that nothing
        // downstream can distinguish from one a caller typed inline: `agent` is
        // cleared and `claude` is present. That is what keeps `docs/spec.md`
        // Sec. 4.4 literally true -- argv is a pure function of the request and of
        // the immutable version the request names -- and it is why not one
        // admission rule below had to learn what an agent is.
        let agent_pin = self.resolve_agent_reference(&mut request, retention)?;
        validate_v1_terminal_support(request.terminal.profile, request.terminal.input_transport)?;
        let _guard = self.start_guard.lock().await;
        if self.shutdown_started.load(Ordering::Acquire) {
            return Err(ErrorBody::new(
                ErrorCode::DaemonLost,
                "native service shutdown has started",
            )
            .retryable(true));
        }
        let session_id = select_session_id(&request).map_err(map_launch_error)?;
        if matches!(request.identity, SessionIdentity::New { session_id: None }) {
            request.identity = SessionIdentity::New {
                session_id: Some(session_id),
            };
        }

        // Identity admission is intentionally earlier than Hybrid relay or
        // sensitive launch-file preparation. `resolve_claude_launch` normally
        // requires those inline inputs to have been materialized, so resolve a
        // side-effect-free identity view containing only the cwd, executable,
        // environment, session identity, and other non-materialized options.
        // The complete request is resolved again after successful admission.
        let mut identity_request = request.clone();
        {
            let claude = require_resolved_launch_mut(&mut identity_request)?;
            claude.settings.clear();
            claude.mcp_configs.clear();
            claude.system_prompt = pseudomux_protocol::v1::SystemPromptPolicy::Default;
        }
        let identity_resolved =
            resolve_claude_launch(&identity_request).map_err(map_launch_error)?;
        debug_assert_eq!(identity_resolved.session_id, session_id);
        let config_root = effective_config_root(&identity_resolved)?;
        // Every directory this start binds, enumerated rather than assumed.
        // `effective_config_root` above is the delivered root for all four
        // shapes that produce one -- an explicit `environment.set`, an
        // inherited snapshot value, the `config_isolation` root that step 6
        // writes over that name, and the `HOME`-derived default -- and
        // `identity_resolved.process.cwd` is the canonical directory the child
        // is launched in. The line below is what stops the third of those from
        // being an ASSUMPTION about step 6's ordering.
        require_isolation_root_is_the_effective_root(
            request.config_isolation.as_ref(),
            &config_root,
        )?;
        // The two resource rules run against the root and cwd this request
        // RESOLVES to, before the transcript scan, and unconditionally: an
        // applicant that named no isolation root still gets delivered one, and
        // it is the delivered directory -- not the request's shape -- that a
        // live cell is holding. Both consult one snapshot of the live claims,
        // which is sound because `start_guard` is held here and every start and
        // explicit close takes it, so nothing can acquire either resource
        // between the question and the answer.
        //
        // The claims are collected before the rules run so no filesystem work --
        // canonicalization, or the pristine-root scan -- happens while the
        // session map is locked.
        let claims = live_resource_claims(&*self.sessions.read().await);
        let seed_disposition = admit_bound_resources(
            &claims,
            &config_root,
            &identity_resolved.process.cwd,
            request.cell,
        )?;
        validate_transcript_identity(&request.identity, &identity_resolved, &config_root)?;
        // Seeding happens before a Claude process exists, and before
        // `detect_claude_version` runs the executable under this same
        // environment. `config_root` is already the private root: step 6 of
        // `build_environment` overwrote `CLAUDE_CONFIG_DIR` with its canonical
        // form, and `effective_config_root` reads the delivered map -- which is
        // what makes "the seeded file and the located transcript are under one
        // root" true by construction rather than by a second computation.
        //
        // Seeding, unlike admission, stays conditional on the caller having
        // ASKED for a private root: these two files are pmux's to write only in
        // a directory the caller offered for that purpose. An un-isolated start
        // is launched under the operator's own root, and pmux does not write
        // there.
        if request.config_isolation.is_some() {
            Self::seed_config_isolation_root(&config_root, &identity_resolved, seed_disposition)?;
        }

        #[cfg(unix)]
        let lifecycle = {
            let hook_client = self
                .config
                .hybrid_hook_client
                .as_deref()
                .unwrap_or_else(|| Path::new("/pmux-hook-unconfigured"));
            let lifecycle = crate::hybrid_hooks::prepare_lifecycle(
                &request.lifecycle,
                self.runtime.runtime_dir(),
                session_id,
                hook_client,
                &require_resolved_launch(&request)?.settings,
            )
            .await
            .map_err(|error| ErrorBody::new(ErrorCode::InvalidConfig, error.to_string()))?;
            {
                let claude = require_resolved_launch_mut(&mut request)?;
                claude.settings = lifecycle.launch_settings(&claude.settings);
            }
            lifecycle
        };
        #[cfg(not(unix))]
        if matches!(
            request.lifecycle,
            pseudomux_protocol::v1::LifecycleMode::Hybrid { .. }
        ) {
            return Err(ErrorBody::new(
                ErrorCode::UnsupportedFeature,
                "hybrid lifecycle is not implemented on this platform",
            ));
        }
        let sensitive_launch = SensitiveLaunchFiles::prepare(
            self.runtime.runtime_dir(),
            session_id,
            require_resolved_launch_mut(&mut request)?,
        )
        .map_err(map_launch_error)?;
        let mut resolved = resolve_claude_launch(&request).map_err(map_launch_error)?;
        debug_assert_eq!(resolved.session_id, session_id);
        sensitive_launch.apply_to(&mut resolved.process);
        if self
            .sessions
            .read()
            .await
            .contains_key(&resolved.session_id)
        {
            return Err(ErrorBody::new(
                ErrorCode::IdCollision,
                format!("session {} is already active", resolved.session_id),
            ));
        }

        let claude_version = detect_claude_version(&resolved, self.config.version_timeout).await?;
        let compatibility = self.config.tested_claude_profiles.resolve(
            request.compatibility,
            &claude_version,
            request.terminal.profile,
            request.terminal.input_transport,
            self.config.untested_transcript_drain_ms,
        )?;
        // The cell's one real guard, applied before a TUI exists. Deciding the
        // cell at start rather than by a later request is what makes this
        // possible: an inadmissible cell now refuses without ever spawning a
        // child, where a mid-session selection could only refuse after one was
        // already running and had to be torn down.
        if request.cell == SessionCell::Minified {
            require_tested_for_minified_cell(&compatibility)?;
        }

        let transcript = Arc::new(
            FileTranscriptSource::new(&config_root, &resolved.process.cwd, resolved.session_id)
                .map_err(map_location_error)?,
        );
        #[cfg(unix)]
        let lifecycle_expected = lifecycle.hybrid().is_some();
        #[cfg(unix)]
        let lifecycle_stop_sequence = Arc::new(AtomicU64::new(0));
        // `0` means "no Stop hook has ever been observed for this session". The
        // stamp is only ever overwritten with a representable instant, so the
        // sentinel stays unambiguous for the lifetime of the session.
        #[cfg(unix)]
        let lifecycle_stop_at_ms = Arc::new(AtomicU64::new(0));
        #[cfg(unix)]
        let mut lifecycle_guard = SessionLifecycle::start(
            lifecycle,
            Arc::clone(&lifecycle_stop_sequence),
            Arc::clone(&lifecycle_stop_at_ms),
            &self.lifecycle_tasks,
        );
        let terminal_result = self
            .create_terminal_for_start(
                resolved.session_id,
                request.terminal.rows,
                request.terminal.cols,
                resolved.process.clone(),
            )
            .await;
        #[cfg(unix)]
        let terminal = require_created_terminal(terminal_result, &mut lifecycle_guard).await?;
        #[cfg(not(unix))]
        let terminal = terminal_result?;
        let mut startup = StartupCleanupGuard::new(
            resolved.session_id,
            terminal,
            sensitive_launch,
            #[cfg(unix)]
            lifecycle_guard,
            Arc::clone(&self.pending_startup_cleanup),
        );

        let startup_screen = match wait_until_ready(
            startup.cleanup_mut().raw_terminal_mut(),
            self.config.readiness_timeout,
        )
        .await
        {
            Ok(startup_screen) => startup_screen,
            Err(error) => {
                return Err(self.finish_failed_start(error, startup).await);
            }
        };
        // The launch half of assert-empty is NOT applied here. It is a property
        // of admitting a session, not of this code path, so it lives in
        // `SessionRegistry::register` below, which every route to a minified
        // cell -- wire or embedder -- passes through and which a test can reach
        // without a Claude process. A refusal arrives as the `register` error
        // already handled at the end of this function.
        let private_session_name = startup
            .cleanup_mut()
            .raw_terminal_mut()
            .backend_ref()
            .rmux_session_name
            .clone();
        let terminal = startup.cleanup_mut().promote_terminal(
            #[cfg(unix)]
            lifecycle_expected,
            #[cfg(unix)]
            Arc::clone(&lifecycle_stop_sequence),
            #[cfg(unix)]
            Arc::clone(&lifecycle_stop_at_ms),
        );
        let idle_ttl_ms = idle_ttl_ms_for(&request.retention);
        let registration = SessionRegistration {
            session_id: resolved.session_id,
            generation_id: SessionGenerationId::new(),
            owner,
            cwd: resolved.process.cwd.to_string_lossy().into_owned(),
            compatibility,
            dangerous_permission_bypass: resolved.dangerous_permission_bypass,
            resumable: true,
            cell: request.cell,
            agent: agent_pin,
            idle_ttl_ms,
            initial_needs_input: match startup_screen {
                TerminalScreenState::NeedsInput(needs_input) => Some(needs_input),
                TerminalScreenState::Ready => None,
                TerminalScreenState::Recognised(_) | TerminalScreenState::Unrecognised(_) => {
                    unreachable!(
                        "startup wait only returns ready or a recognized interactive screen"
                    )
                }
            },
            terminal: terminal.clone(),
            transcript: Arc::clone(&transcript) as Arc<dyn TranscriptSource>,
        };
        // Acquire the metadata publication lock before actor registration. Once
        // register obtains its own lock it has no further await, so actor and
        // metadata publication become one cancellation-free poll segment.
        let mut sessions = self.sessions.write().await;
        let handle = match self.registry.register(registration).await {
            Ok(handle) => handle,
            Err(error) => {
                drop(sessions);
                return Err(self.finish_failed_start(error, startup).await);
            }
        };
        sessions.insert(
            resolved.session_id,
            startup.into_metadata(
                handle.generation_id,
                transcript,
                private_session_name,
                request.cell,
                owner,
            ),
        );
        Ok(handle)
    }

    pub async fn run_turn(
        self: &Arc<Self>,
        request: RunTurnRequest,
    ) -> Result<TurnAccepted, ErrorBody> {
        validate_native_request(&request)?;
        validate_turn_lease(&request.turn)?;
        validate_prompt(&request.turn.prompt).map_err(DriverFailureExt::protocol)?;
        self.registry.run_turn(request).await
    }

    pub async fn run_once(
        self: &Arc<Self>,
        request: RunOnceRequest,
    ) -> Result<TurnResult, ErrorBody> {
        validate_native_request(&request)?;
        validate_turn_lease(&request.turn)?;
        // Reject caller-controlled input before constructing a PTY. Besides
        // avoiding needless work, this prevents an invalid one-shot request
        // from becoming an unattended startup modal.
        validate_prompt(&request.turn.prompt).map_err(DriverFailureExt::protocol)?;
        // The retention travels as a DECISION rather than as a value written
        // into the request, because a value written here is one an agent's
        // stored policy then replaced: see
        // `start_session_owned_with_retention`.
        let handle = self
            .start_session_owned_with_retention(
                request.session,
                SessionOwner::Caller,
                Retention::ForcedOneShot,
            )
            .await?;
        let turn_id = request.turn.turn_id;
        let deadline = request.turn.deadline_unix_ms;
        if let Err(error) = self
            .registry
            .run_turn(RunTurnRequest {
                session_id: handle.session_id,
                generation_id: handle.generation_id,
                turn: request.turn,
            })
            .await
        {
            let cleanup = self
                .close_session(CloseSessionRequest {
                    session_id: handle.session_id,
                    generation_id: handle.generation_id,
                    policy: pseudomux_protocol::v1::ClosePolicy::Force,
                })
                .await
                .and_then(require_process_reaped);
            return Err(match cleanup {
                Ok(_) => error,
                Err(cleanup_error) => combine_turn_and_cleanup_errors(error, cleanup_error),
            });
        }
        // Resolved once, here, under the caller resolver: `run_once` is a wire
        // method, so its session is a caller's.
        let result = match self
            .registry
            .actor(handle.session_id, handle.generation_id)
            .await
        {
            Ok(actor) => {
                self.wait_for_turn(
                    &actor,
                    turn_id,
                    deadline,
                    handle.compatibility.transcript_drain_ms,
                )
                .await
            }
            Err(error) => Err(error),
        };
        let cleanup = self
            .close_session(CloseSessionRequest {
                session_id: handle.session_id,
                generation_id: handle.generation_id,
                policy: if result.is_ok() {
                    pseudomux_protocol::v1::ClosePolicy::Graceful
                } else {
                    pseudomux_protocol::v1::ClosePolicy::Force
                },
            })
            .await
            .and_then(require_process_reaped);
        match (result, cleanup) {
            (Ok(result), Ok(_)) => Ok(result),
            (Err(error), Ok(_)) | (Ok(_), Err(error)) => Err(error),
            (Err(turn_error), Err(cleanup_error)) => {
                Err(combine_turn_and_cleanup_errors(turn_error, cleanup_error))
            }
        }
    }

    /// Polls one already-resolved actor until it publishes a terminal outcome.
    ///
    /// Takes the ACTOR, not a session id, and that is the fix for a live defect
    /// rather than a style choice. It used to take `(session_id, generation_id)`
    /// and re-resolve them through `SessionRegistry::stored_turn`, which goes
    /// through the caller-only resolver. Once `SessionOwner` split that
    /// resolver, every pool turn resolved its actor correctly, submitted
    /// correctly, and then asked a resolver that refuses pool sessions for the
    /// answer -- so every stateless call returned `session_not_found` AFTER the
    /// prompt had been typed into a real Claude. MEASURED live, not reasoned:
    /// `pmux ask --model sonnet --effort low` returned `code=SessionNotFound
    /// message="session 662eb2d7-... is not registered"` while the pool census
    /// reported that instance live and idle.
    ///
    /// A handle cannot be obtained without having already decided the owner, so
    /// there is now exactly one owner decision per call path and a second one
    /// is not expressible here.
    pub(crate) async fn wait_for_turn(
        &self,
        actor: &crate::v1::SessionActorHandle,
        turn_id: pseudomux_protocol::v1::TurnId,
        deadline_unix_ms: Option<u64>,
        transcript_drain_ms: u64,
    ) -> Result<TurnResult, ErrorBody> {
        let session_id = actor.session_id();
        let generation_id = actor.generation_id();
        // This is an infrastructure guard, not a competing turn deadline. The
        // actor gets its full deadline plus bounded recovery time, and its
        // stored terminal is checked before the guard on every iteration.
        let now_ms = unix_now_ms()?;
        let actor_deadline_ms = match deadline_unix_ms {
            Some(deadline_unix_ms) => deadline_unix_ms,
            None => checked_default_deadline_ms(now_ms, self.config.actor.default_turn_timeout_ms)?,
        };
        let remaining_ms = actor_deadline_ms.saturating_sub(now_ms);
        let safety_delay = turn_wait_safety_delay(
            remaining_ms,
            self.config.actor.cancel_recovery_timeout,
            transcript_drain_ms,
        )?;
        let safety_deadline = tokio::time::Instant::now()
            .checked_add(safety_delay)
            .ok_or_else(|| {
                ErrorBody::new(
                    ErrorCode::InvalidConfig,
                    "turn safety guard exceeds the monotonic clock domain",
                )
            })?;
        loop {
            match actor.stored_turn(turn_id).await? {
                Some(StoredTurnTerminal::Result(result)) => return Ok(*result),
                Some(StoredTurnTerminal::Failed(error)) => return Err(error),
                None => {}
            }
            if tokio::time::Instant::now() >= safety_deadline {
                return Err(ErrorBody::new(
                    ErrorCode::DaemonLost,
                    "session actor did not publish its terminal turn outcome within the safety guard",
                )
                .retryable(true)
                .with_details(json!({
                    "session_id": session_id,
                    "generation_id": generation_id,
                    "turn_id": turn_id,
                })));
            }
            let sleep_for = safety_deadline
                .saturating_duration_since(tokio::time::Instant::now())
                .min(Duration::from_millis(25));
            tokio::time::sleep(sleep_for).await;
        }
    }

    /// Submit one Path A turn and wait for its transcript-proven result.
    ///
    /// The Messages lease book uses this instead of `RunStateless` so a pinned
    /// cell can take a delta without `/clear`.
    pub async fn run_turn_to_completion(
        self: &Arc<Self>,
        request: RunTurnRequest,
        transcript_drain_ms: u64,
    ) -> Result<TurnResult, ErrorBody> {
        let turn_id = request.turn.turn_id;
        let deadline = request.turn.deadline_unix_ms;
        let session_id = request.session_id;
        let generation_id = request.generation_id;
        self.run_turn(request).await?;
        let actor = self.registry.actor(session_id, generation_id).await?;
        self.wait_for_turn(&actor, turn_id, deadline, transcript_drain_ms)
            .await
    }

    /// Clears a minified-cell session's context between turns and returns the
    /// session id Claude rotated to.
    ///
    /// `/clear` costs ~30ms where relaunching the TUI costs ~4.4s, which is the
    /// entire reason a stateless cell is affordable. The caller's session id and
    /// generation are unchanged by it: what rotates is Claude's own id, and the
    /// only thing that follows the rotation is the transcript tail.
    pub async fn clear_session(
        &self,
        request: ClearSessionRequest,
    ) -> Result<ClearSessionResult, ErrorBody> {
        validate_native_request(&request)?;
        let deadline_unix_ms = match request.deadline_unix_ms {
            Some(deadline) => deadline,
            None => unix_now_ms()?.saturating_add(self.config.default_clear_timeout_ms),
        };
        if deadline_unix_ms > MAX_SAFE_JSON_INTEGER {
            return Err(ErrorBody::new(
                ErrorCode::InvalidConfig,
                "clear deadline is outside protocol-v1's safe-integer domain",
            ));
        }
        let actor = self
            .registry
            .actor(request.session_id, request.generation_id)
            .await?;
        let boundary = self
            .clear_boundary(request.session_id, request.generation_id)
            .await?;
        actor
            .clear_session(
                boundary,
                request.expected_transcript_session_id,
                deadline_unix_ms,
            )
            .await
    }

    /// The concrete terminal/transcript pair for one live session, as a
    /// one-call capability.
    ///
    /// The pair is read out under the same generation fence the actor was
    /// resolved under, so the terminal and the transcript handed to the rebind
    /// are the ones belonging to this process incarnation.
    ///
    /// Extracted from [`Self::clear_session`] rather than copied, so the wire
    /// path and the pool path take the SAME pair by the SAME fence. Two copies
    /// of this read is exactly how one of them comes to omit the generation
    /// filter.
    pub(crate) async fn clear_boundary(
        &self,
        session_id: SessionId,
        generation_id: SessionGenerationId,
    ) -> Result<Arc<dyn ClearRebind>, ErrorBody> {
        self.sessions
            .read()
            .await
            .get(&session_id)
            .filter(|metadata| metadata.generation_id == generation_id)
            .map(|metadata| {
                Arc::new(RmuxClearRebind {
                    terminal: Arc::clone(&metadata.terminal),
                    transcript: Arc::clone(&metadata.transcript),
                }) as Arc<dyn ClearRebind>
            })
            .ok_or_else(|| {
                ErrorBody::new(
                    ErrorCode::StaleSessionGeneration,
                    "session backend no longer matches the requested process generation",
                )
            })
    }

    /// The deadline a clear gets when its caller supplies none.
    #[must_use]
    pub(crate) const fn clear_timeout_ms(&self) -> u64 {
        self.config.default_clear_timeout_ms
    }

    pub async fn close_session(
        &self,
        request: CloseSessionRequest,
    ) -> Result<CloseSessionResult, ErrorBody> {
        self.close_session_owned(SessionOwner::Caller, request)
            .await
    }

    pub(crate) async fn close_session_owned(
        &self,
        owner: SessionOwner,
        request: CloseSessionRequest,
    ) -> Result<CloseSessionResult, ErrorBody> {
        validate_native_request(&request)?;
        close_session_with_state(
            self.registry.as_ref(),
            &self.sessions,
            &self.closed_sessions,
            &self.start_guard,
            owner,
            request,
        )
        .await
    }

    async fn attach(&self, request: AttachSessionRequest) -> Result<ResponseResult, ErrorBody> {
        // Fence even unsupported attach variants before inspecting backend
        // metadata so a delayed request cannot be mistaken for the current
        // process incarnation.
        self.registry
            .actor(request.session_id, request.generation_id)
            .await?;
        if request.read_only {
            return Err(ErrorBody::new(
                ErrorCode::UnsupportedFeature,
                "read-only attach is not implemented by the pinned rmux stream protocol",
            ));
        }
        let (terminal, private_session_name) = self
            .sessions
            .read()
            .await
            .get(&request.session_id)
            .filter(|metadata| metadata.generation_id == request.generation_id)
            .map(|metadata| {
                (
                    Arc::clone(&metadata.terminal),
                    metadata.private_session_name.clone(),
                )
            })
            .ok_or_else(|| {
                ErrorBody::new(
                    ErrorCode::StaleSessionGeneration,
                    "session backend no longer matches the requested process generation",
                )
            })?;
        #[cfg(unix)]
        {
            let attach_id = uuid::Uuid::new_v4();
            self.registry
                .reserve_writable_attach(request.session_id, request.generation_id, attach_id)
                .await?;
            if let Some(size) = request.size
                && let Err(error) = terminal.resize(size.rows, size.cols).await
            {
                let _ = self
                    .registry
                    .release_writable_attach(
                        request.session_id,
                        request.generation_id,
                        attach_id,
                        WritableAttachCompletion::Unused,
                    )
                    .await;
                return Err(error.into_protocol());
            }
            let (grant, completion) = match crate::attach::grant_attach(
                self.runtime.runtime_dir(),
                self.runtime.rmux_socket(),
                private_session_name,
                self.config.attach_ttl,
            )
            .await
            {
                Ok(grant) => grant,
                Err(error) => {
                    let _ = self
                        .registry
                        .release_writable_attach(
                            request.session_id,
                            request.generation_id,
                            attach_id,
                            WritableAttachCompletion::Unused,
                        )
                        .await;
                    return Err(map_attach_grant_error(error));
                }
            };
            let registry = Arc::clone(&self.registry);
            let session_id = request.session_id;
            let generation_id = request.generation_id;
            tokio::spawn(async move {
                let completion = match completion.wait().await {
                    crate::attach::AttachCompletionOutcome::Unused => {
                        WritableAttachCompletion::Unused
                    }
                    crate::attach::AttachCompletionOutcome::PotentiallyMutated => {
                        WritableAttachCompletion::PotentiallyMutated
                    }
                };
                let _ = registry
                    .release_writable_attach(session_id, generation_id, attach_id, completion)
                    .await;
            });
            Ok(ResponseResult::AttachCapability(
                pseudomux_protocol::v1::AttachCapability {
                    session_id: request.session_id,
                    generation_id: request.generation_id,
                    token: grant.token,
                    endpoint: grant.endpoint.to_string_lossy().into_owned(),
                    expires_at_ms: grant.expires_at_ms,
                    read_only: false,
                },
            ))
        }
        #[cfg(not(unix))]
        {
            let _ = (terminal, private_session_name);
            Err(ErrorBody::new(
                ErrorCode::UnsupportedFeature,
                "attach capabilities are not implemented on this platform",
            ))
        }
    }

    #[must_use]
    pub fn registry(&self) -> &Arc<SessionRegistry> {
        &self.registry
    }

    /// Completes one real operation against the private runtime and reports
    /// what it found, per session. Never starts a Claude turn.
    ///
    /// ## Cost
    ///
    /// Exactly one rmux round trip for the whole daemon, plus one in-process
    /// actor mailbox round trip per session. Nothing here acquires a terminal
    /// mutex, so a probe cannot contend with a turn that is mid-submission, and
    /// nothing here scales the *network* cost with the pool.
    ///
    /// ## The ordering is load-bearing
    ///
    /// The three reads happen in exactly this order, and no other order is
    /// free of false alarms:
    ///
    /// 1. the registry's session set, with each session's private rmux name;
    /// 2. the sidecar's own list of live private terminals;
    /// 3. each session's state, from its actor.
    ///
    /// Reading the registry first bounds the set: a session published *after*
    /// step 1 is simply not in this report, whereas listing first and reading
    /// the registry second would have reported every start that completed in
    /// between as a session whose terminal is missing. Reading each state last
    /// closes the other direction: a terminal legitimately torn down between
    /// steps 1 and 2 belongs to a session whose actor is already `closing`,
    /// `closed` or `failed` by step 3, and those states make no terminal claim
    /// at all. That covers both teardown paths, including the idle reaper's,
    /// which takes no `start_guard` and so could not have been fenced by one.
    ///
    /// ## What it deliberately does not do
    ///
    /// It does not report an rmux session with no registry entry as a leak.
    /// pmux publishes a session only *after* its terminal exists, so "in the
    /// sidecar, not in the registry" is the normal shape of every in-flight
    /// start. The count is reported as a fact and folded into nothing.
    pub async fn diagnose(&self) -> DaemonDiagnosis {
        // Step 1. See the ordering note above.
        //
        // CALLER SESSIONS ONLY, and the pool's private terminal names taken
        // separately. `DaemonDiagnosis::sessions` publishes a `session_id` per
        // entry, so including a pool instance here would put the one name
        // `SessionOwner` exists to hide onto the wire, in a report any client
        // may ask for -- and it did, MEASURED live: the first health tree this
        // daemon produced listed a pool instance's session id and generation
        // id, and reported it as "left the registry while the probe was
        // running" because the caller-only resolver had refused it.
        //
        // The pool's instances are still probed. They are counted into the pool
        // layer, which reports numbers and no identifiers.
        let (registered, pool_terminals): (
            Vec<(SessionId, SessionGenerationId, String)>,
            Vec<String>,
        ) = {
            let sessions = self.sessions.read().await;
            let mut registered: Vec<_> = sessions
                .iter()
                .filter(|(_, metadata)| metadata.owner == SessionOwner::Caller)
                .map(|(session_id, metadata)| {
                    (
                        *session_id,
                        metadata.generation_id,
                        metadata.private_session_name.clone(),
                    )
                })
                .collect();
            registered.sort_by_key(|(session_id, _, _)| *session_id);
            let mut pool_terminals: Vec<String> = sessions
                .values()
                .filter(|metadata| metadata.owner == SessionOwner::Pool)
                .map(|metadata| metadata.private_session_name.clone())
                .collect();
            pool_terminals.sort();
            (registered, pool_terminals)
        };

        // Step 2.
        let started = std::time::Instant::now();
        let probed = self.runtime.probe_request_path().await;
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

        // Step 3, concurrently: a single wedged actor must cost one deadline
        // for the report, not one deadline per session behind it.
        let mut states = tokio::task::JoinSet::new();
        for (session_id, generation_id, _) in &registered {
            let registry = Arc::clone(&self.registry);
            let (session_id, generation_id) = (*session_id, *generation_id);
            states.spawn(async move {
                let read = tokio::time::timeout(DIAGNOSE_ACTOR_DEADLINE, async move {
                    registry
                        .actor(session_id, generation_id)
                        .await?
                        .snapshot()
                        .await
                })
                .await;
                (session_id, read)
            });
        }
        let mut observed: HashMap<SessionId, ActorStateObservation> = HashMap::new();
        while let Some(joined) = states.join_next().await {
            // A panicked or cancelled read is an unanswered read. It is never
            // silently dropped: an absent entry below is `Unanswered` too.
            if let Ok((session_id, read)) = joined {
                observed.insert(session_id, ActorStateObservation::from_read(read));
            }
        }

        let mut diagnosis = build_diagnosis(
            &registered,
            probed.as_ref(),
            elapsed_ms,
            self.runtime.launch_broker_is_accepting(),
            &observed,
        );
        diagnosis.layers = self
            .health_layers(probed.as_ref(), elapsed_ms, &pool_terminals, &diagnosis)
            .await;
        // The evidence blobs are opaque JSON, and protocol v1 refuses an opaque
        // integer outside the signed safe-integer range. A layer that put one
        // there made the WHOLE report unserializable -- not that layer, the
        // report -- so `diagnose` answered `internal` and every claim in it was
        // lost. That is exactly the shape of failure this surface exists to
        // prevent, arriving through the surface itself.
        //
        // Dropping the offending layer would be worse than reporting it: the
        // layer is REPLACED by one that says its evidence could not be
        // represented, which keeps the entry present and honest. A dropped
        // layer would be silently `not_established` with no reason attached.
        for layer in &mut diagnosis.layers {
            if validate_v1_serializable(layer).is_err() {
                *layer = HealthLayer::new(
                    layer.layer,
                    LayerFinding::NotEstablished,
                    format!(
                        "{} (this layer's evidence could not be represented within protocol v1,                          so it was withheld)",
                        layer.detail
                    ),
                    serde_json::Value::Null,
                );
            }
        }
        diagnosis
    }

    /// The health tree, one entry per [`HealthLayerName`], built from the reads
    /// [`Self::diagnose`] already performed.
    ///
    /// The layers are constructed from an exhaustive `match` over the name
    /// enum, not from a list of pushes. A `push` per layer is how a layer comes
    /// to be omitted, and an omitted layer is what
    /// [`DaemonDiagnosis::missing_layers`] exists to catch -- but catching it at
    /// runtime is a worse outcome than not being able to write it.
    ///
    /// No layer here performs a NEW operation against the sidecar. Every one is
    /// derived from the single control-plane exchange `diagnose` already made,
    /// the broker liveness read, the pool's own census and this daemon's
    /// configuration. A health surface whose cost scales with its number of
    /// layers is one an operator learns not to call.
    async fn health_layers(
        &self,
        probed: Result<&BTreeSet<String>, &ControlPlaneFault>,
        elapsed_ms: u64,
        pool_terminals: &[String],
        diagnosis: &DaemonDiagnosis,
    ) -> Vec<HealthLayer> {
        let broker_accepting = self.runtime.launch_broker_is_accepting();
        // The one NEW operation this method performs, and it is against the
        // daemon's own 0600 endpoint in its own 0700 runtime directory rather
        // than against the sidecar. It is a single local frame exchange bounded
        // by the runtime's operation timeout; the alternative was reporting
        // `exercised` over a task-liveness read.
        let broker_probe = self.runtime.probe_launch_broker().await;
        // The envelope is READ from the runtime, not written beside it. The
        // number a report measures against and the number the runtime enforces
        // have to be the same number, or the report is measuring against a
        // bound nothing has.
        let envelope_ms =
            u64::try_from(self.runtime.operation_timeout().as_millis()).unwrap_or(u64::MAX);
        let pool_subject = match self.pool() {
            Some(pool) => Some(PoolSubject {
                pool_size: pool.config().pool_size,
                declared_warm: pool.config().declared_warm_total(),
                census: pool.census().await,
                conversation_leases: pool.conversation_leases().await,
            }),
            None => None,
        };
        // The SECOND new operation, and it is the one that decides whether a
        // `pmux ask` can be served at all. It runs only on a daemon that has a
        // pool -- see `admit_pool_claude` -- so a Path A daemon spawns nothing.
        let pool_claude = self.admit_pool_claude().await;
        let mut layers = Vec::with_capacity(HealthLayerName::ALL.len());
        for name in HealthLayerName::ALL.iter().copied() {
            layers.push(match name {
                HealthLayerName::Configuration => self.configuration_layer(),
                HealthLayerName::ControlPlane => control_plane_layer(probed, elapsed_ms),
                HealthLayerName::PrivateRuntime => private_runtime_layer(probed, elapsed_ms),
                HealthLayerName::LaunchBroker => {
                    launch_broker_layer(broker_accepting, &broker_probe)
                }
                HealthLayerName::CompatibilityProfile => compatibility_layer(
                    self.config.tested_claude_profiles.admissible_here(),
                    pool_claude.as_ref(),
                ),
                HealthLayerName::Pool => {
                    pool_layer(pool_subject.as_ref(), pool_terminals, probed.ok())
                }
                HealthLayerName::Sessions => sessions_layer(&diagnosis.sessions),
                HealthLayerName::Performance => {
                    performance_layer(probed, elapsed_ms, envelope_ms, &diagnosis.sessions)
                }
            });
        }
        layers
    }

    /// Ask, of the Claude this daemon's pool would launch, the question every
    /// mint asks: is this version admissible?
    ///
    /// # The report this exists to stop
    ///
    /// `pmux doctor` exited 0 `healthy` on a host running Claude Code 2.1.223
    /// against a sole promoted 2.1.220, and the very next `pmux ask` was
    /// refused with `unsupported_claude_version`. Both operands were already in
    /// the daemon -- `pool.config().claude_executable` names the binary, and
    /// `self.config.tested_claude_profiles` is the set it must be in -- and the
    /// compatibility layer's own doc defended not comparing them with "nothing
    /// here knows which Claude the pool will launch", which is not true of a
    /// daemon that holds the pool's configuration. That sentence is why this
    /// method exists: a doc that states a reason its own module refutes.
    ///
    /// # What is derived rather than restated
    ///
    /// The policy, the terminal identity and the cell all come from the
    /// constants [`crate::stateless::launch_request_for`] writes into a real
    /// mint request, and the refusal comes from the same
    /// [`CompatibilityProfileRegistry::resolve`] plus
    /// `require_tested_for_minified_cell` pair `start_session` runs. A second
    /// copy of the admission rule here is a health report free to keep
    /// answering `exercised` after the mint's copy has changed.
    ///
    /// `None` on a daemon with no pool: nothing it runs needs a tested cell,
    /// and nothing is spawned to find out.
    async fn admit_pool_claude(&self) -> Option<PoolClaudeAdmission> {
        let pool = self.pool()?;
        let executable = pool.config().claude_executable.clone();
        // The pool parent: owner-only, created at boot, and the one directory
        // this daemon is certain exists and is its own.
        let cwd = pool.config().parent_dir.clone();
        // The daemon's OWN environment, which is the snapshot
        // `NativeInstanceHost` captures for every mint. A mint's copy is then
        // filtered by `build_environment`'s allowlist, and this one is not --
        // which cannot move the answer, because the executable is named by
        // absolute path, so no `PATH` entry chooses a different binary and no
        // variable changes the version the chosen one prints.
        let environment = pseudomux_rmux::EnvironmentSnapshot::capture();
        let version =
            match claude_version_of(&executable, &cwd, &environment, self.config.version_timeout)
                .await
            {
                Ok(version) => version,
                Err(error) => {
                    return Some(PoolClaudeAdmission::Unreadable {
                        executable,
                        error: error.message,
                    });
                }
            };
        Some(admit_claude_version(
            &self.config.tested_claude_profiles,
            version,
            self.config.untested_transcript_drain_ms,
        ))
    }

    /// Configuration is EXERCISED, not merely present: the daemon is running on
    /// it, and a pool configuration that was inadmissible refused at boot rather
    /// than reaching here.
    fn configuration_layer(&self) -> HealthLayer {
        HealthLayer::new(
            HealthLayerName::Configuration,
            LayerFinding::Exercised,
            "the daemon is running on a configuration that passed every boot bound;              an inadmissible one refuses to boot rather than degrading",
            json!({
                // Three numbers, because one cannot tell them apart: what the
                // operator admitted, what pmux promoted for this platform, and
                // what a mint would actually find. A daemon that works because
                // pmux shipped a cell and a daemon that works because its
                // operator measured one are different deployments, and an
                // operator debugging a refusal needs to know which they have.
                "tested_claude_profiles": self.config.tested_claude_profiles.len(),
                "promoted_cells_for_this_platform":
                    crate::compatibility::CompatibilityProfileRegistry::promoted_here(),
                "compatibility_cells_matching_this_platform":
                    self.config.tested_claude_profiles.admissible_here(),
                "promoted_profiles": crate::compatibility::PROMOTED_PROFILES
                    .iter()
                    .map(|promoted| json!({
                        "claude_version_floor": promoted.claude_version_floor,
                        "claude_version_tested_through": promoted.claude_version_tested_through,
                        "claude_versions": promoted.version_range().to_string(),
                        "os": promoted.os,
                        "arch": promoted.arch,
                        "transcript_drain_ms": promoted.transcript_drain_ms,
                        "drain_provenance": promoted.drain_provenance,
                        "range_provenance": promoted.range_provenance,
                    }))
                    .collect::<Vec<_>>(),
                // What retracts a promoted range, published beside the range
                // itself. An operator holding a daemon that has stopped
                // admitting their Claude reads this to find out which of the
                // five conditions to check, and each entry names the file that
                // detects it rather than describing one.
                "repromotion_triggers": crate::compatibility::RepromotionTrigger::ALL
                    .iter()
                    .map(|trigger| {
                        let detector = trigger.detector();
                        json!({
                            "trigger": detector.id,
                            "detected_by": detector.file,
                            "detector": detector.symbol,
                            "what_to_do": detector.how,
                        })
                    })
                    .collect::<Vec<_>>(),
                "untested_transcript_drain_ms": self.config.untested_transcript_drain_ms,
                "default_clear_timeout_ms": self.config.default_clear_timeout_ms,
                "path_b_enabled": self.pool().is_some(),
                "path_b": self.pool().map(|pool| {
                    let config = pool.config();
                    json!({
                        "pool_size": config.pool_size,
                        "recycle_turns": config.recycle_turns,
                        "instance_idle_ttl_ms": config.instance_idle_ttl_ms,
                        "turn_timeout_ms": config.turn_timeout_ms,
                        "system_prompt_bytes": config.system_prompt.len(),
                        // HEX, not a JSON number. It is a u64 FNV-1a value, and
                        // protocol v1 refuses an opaque JSON integer outside the
                        // signed safe-integer range -- so encoding it as a
                        // number made the WHOLE diagnosis unserializable, which
                        // is how the first live probe of this surface found it.
                        "system_prompt_fingerprint": format!(
                            "{:016x}",
                            config.system_prompt_fingerprint
                        ),
                        "warm_classes": config
                            .warm_set
                            .iter()
                            .map(|warm| json!({
                                "class": warm.class.to_string(),
                                "count": warm.count,
                            }))
                            .collect::<Vec<_>>(),
                        // Where the version-drift corpus is accumulating, so an
                        // operator can go and read it -- and `null` when they
                        // turned it off, so "on by default" is a claim the
                        // running daemon answers rather than one a document
                        // makes.
                        "evidence_dir": config
                            .evidence_dir
                            .as_ref()
                            .map(|path| path.display().to_string()),
                        "evidence_budget_bytes": crate::pool::evidence::MAX_EVIDENCE_BYTES,
                        "evidence_retained_row_fields":
                            crate::pool::evidence::RETAINED_ROW_FIELDS,
                    })
                }),
            }),
        )
    }

    /// Closes every owned pane before awaiting private sidecar termination.
    pub async fn shutdown(&self) -> Result<(), ErrorBody> {
        // Fence publication first. A start that already owns the guard either
        // publishes atomically or drops its cancellation guard into the pending
        // queue before shutdown can proceed; later starts fail before side effects.
        {
            let _guard = self.start_guard.lock().await;
            self.shutdown_started.store(true, Ordering::Release);
        }

        let mut reaper = self
            .idle_reaper
            .lock()
            .expect("idle reaper lock poisoned")
            .take();
        if let Some(reaper) = reaper.as_mut() {
            reaper.shutdown().await;
        }
        // The pool drains FIRST, and before the generic per-session close loop
        // below. That loop closes by session id and knows nothing about slots,
        // so a pool instance it closed would leave the pool holding a slot
        // whose process is gone and whose root is still on disk. `Pool::shutdown`
        // destroys each instance through the same host path a TTL expiry uses
        // and erases every root it can prove reaped.
        if let Some(pool) = self.pool() {
            pool.shutdown().await;
        }
        // Also fences a terminal creation that outlived an aborted request.
        // Its delivery Drop has queued every successfully created terminal.
        self.maintenance_tasks.wait_idle().await;
        self.reap_pending_startup_cleanup().await;
        let sessions: Vec<_> = self
            .sessions
            .read()
            .await
            .iter()
            .map(|(session_id, metadata)| (*session_id, metadata.generation_id))
            .collect();
        let mut first_error = None;
        for (session_id, generation_id) in sessions {
            if let Err(error) = self
                .close_session(CloseSessionRequest {
                    session_id,
                    generation_id,
                    policy: pseudomux_protocol::v1::ClosePolicy::Force,
                })
                .await
                .and_then(require_process_reaped)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        // A second pass over the same counter, because the first one ran before
        // any session was closed. Closing a session can leave a detached
        // `close(Force)` in flight (`v1::actor::force_reap_terminal`), and
        // tearing the private runtime down underneath one would turn a kill
        // this service issued into a kill nothing ever confirmed. No request is
        // accepted past the shutdown fence above, so nothing can re-arm the
        // counter after this returns.
        self.maintenance_tasks.wait_idle().await;
        match self.runtime.shutdown().await {
            Ok(()) => {
                // A successful private-runtime shutdown positively reaped all
                // pane boundaries. Drain both unpublished cleanup and surviving
                // published metadata so lifecycle/settings owners cannot outlive
                // this call merely because an earlier per-session close failed.
                drain_after_runtime_shutdown(
                    self.registry.as_ref(),
                    &self.sessions,
                    self.pending_startup_cleanup.as_ref(),
                    &self.closed_sessions,
                    self.lifecycle_tasks.as_ref(),
                    &mut first_error,
                )
                .await;
            }
            Err(error) if first_error.is_none() => {
                first_error = Some(ErrorBody::new(
                    ErrorCode::RmuxUnavailable,
                    format!("failed to stop private rmux runtime: {error}"),
                ));
            }
            Err(_) => {}
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

#[cfg(unix)]
async fn require_created_terminal(
    result: Result<Box<dyn TerminalSession>, ErrorBody>,
    lifecycle: &mut SessionLifecycle,
) -> Result<Box<dyn TerminalSession>, ErrorBody> {
    match result {
        Ok(terminal) => Ok(terminal),
        Err(error) => {
            lifecycle.shutdown().await;
            Err(error)
        }
    }
}

/// Takes the startup-cleanup owners a retry can still change, leaving the
/// permanently-failed ones parked in the queue.
///
/// The reaper requeues on *any* `close_terminal` error and runs on every idle
/// tick, so without this filter a single [`PendingStartupTerminal::Lost`]
/// owner — a close task that panicked, leaving nothing to close and a
/// permanently non-retryable `RecoveryFailed` behind — would be dequeued,
/// re-failed, and requeued once a second for the rest of the daemon's life.
/// Nothing about that entry can change: no terminal, no handle, no operation
/// left to issue.
///
/// It is parked rather than dropped, deliberately. The owner still holds this
/// session's launch material and its lifecycle owner, both of which must
/// outlive the interactive process, and nothing ever confirmed that process was
/// reaped — a panicked close is exactly the case where pmux does *not* know. So
/// the queue keeps owning it, `PrivateRuntime::shutdown` stays the cleanup
/// authority for the process, and [`drain_after_runtime_shutdown`] releases
/// what the entry holds once that shutdown has positively reaped every pane
/// boundary. Retrying once a second in between buys neither of those.
fn take_retryable_startup_cleanup(
    pending: &StdMutex<Vec<PendingStartupCleanup>>,
) -> Vec<PendingStartupCleanup> {
    let mut retained = pending
        .lock()
        .expect("pending startup cleanup lock poisoned");
    let (retryable, permanent): (Vec<_>, Vec<_>) = std::mem::take(&mut *retained)
        .into_iter()
        .partition(|cleanup| !cleanup.is_permanently_failed());
    *retained = permanent;
    retryable
}

async fn drain_after_runtime_shutdown(
    registry: &SessionRegistry,
    sessions: &RwLock<HashMap<SessionId, SessionMetadata>>,
    pending_startup_cleanup: &StdMutex<Vec<PendingStartupCleanup>>,
    closed_sessions: &RwLock<ClosedSessionTombstones>,
    lifecycle_tasks: &TrackedTasks,
    first_error: &mut Option<ErrorBody>,
) {
    let pending = {
        let mut cleanup = pending_startup_cleanup
            .lock()
            .expect("pending startup cleanup lock poisoned");
        std::mem::take(&mut *cleanup)
    };
    for cleanup in pending {
        cleanup.shutdown().await;
    }

    let survivors: Vec<_> = {
        let mut sessions = sessions.write().await;
        sessions.drain().collect()
    };
    for (session_id, metadata) in survivors {
        let generation_id = metadata.generation_id;
        let unregistered = match registry.unregister(session_id, generation_id).await {
            Ok(()) => true,
            Err(error) => {
                if first_error.is_none() {
                    *first_error = Some(error);
                }
                false
            }
        };
        metadata.shutdown().await;
        if unregistered {
            closed_sessions
                .write()
                .await
                .insert(session_id, generation_id);
        }
    }

    // This also catches owners detached by cancellation while their explicit
    // shutdown future was awaiting the cooperative task join.
    lifecycle_tasks.wait_idle().await;
}

async fn dispatch_close_session(
    registry: &SessionRegistry,
    sessions: &RwLock<HashMap<SessionId, SessionMetadata>>,
    closed_sessions: &RwLock<ClosedSessionTombstones>,
    start_guard: &Mutex<()>,
    request: CloseSessionRequest,
) -> Result<ResponseResult, ErrorBody> {
    close_session_with_state(
        registry,
        sessions,
        closed_sessions,
        start_guard,
        SessionOwner::Caller,
        request,
    )
    .await
    .and_then(require_process_reaped)
    .map(ResponseResult::SessionClosed)
}

async fn close_session_with_state(
    registry: &SessionRegistry,
    sessions: &RwLock<HashMap<SessionId, SessionMetadata>>,
    closed_sessions: &RwLock<ClosedSessionTombstones>,
    start_guard: &Mutex<()>,
    owner: SessionOwner,
    request: CloseSessionRequest,
) -> Result<CloseSessionResult, ErrorBody> {
    // Serialize close with the full start transaction. Without this guard,
    // close can observe neither the actor nor metadata while start is still
    // creating the terminal and then return before that session is published.
    let _guard = start_guard.lock().await;
    // The tombstone answers `already_closed` without consulting the registry,
    // so it is read only for the owner that could have produced it. A caller
    // asking about a pool session id must fall through to `close_as`, whose
    // owner check refuses it as not-found; answering `already_closed: true`
    // here would confirm the id existed, which is the oracle `SessionOwner`
    // exists to remove.
    if owner == SessionOwner::Caller
        && closed_sessions
            .read()
            .await
            .contains(request.session_id, request.generation_id)
    {
        return Ok(CloseSessionResult {
            session_id: request.session_id,
            generation_id: request.generation_id,
            already_closed: true,
            process_reaped: true,
        });
    }
    let result = registry.close_as(owner, request.clone()).await?;
    if result.process_reaped {
        let metadata = {
            let mut sessions = sessions.write().await;
            registry
                .unregister(request.session_id, request.generation_id)
                .await?;
            if sessions
                .get(&request.session_id)
                .is_some_and(|metadata| metadata.generation_id == request.generation_id)
            {
                sessions.remove(&request.session_id)
            } else {
                None
            }
        };
        if let Some(metadata) = metadata {
            metadata.shutdown().await;
        }
        closed_sessions
            .write()
            .await
            .insert(request.session_id, request.generation_id);
    }
    Ok(result)
}

fn validate_turn_lease(turn: &pseudomux_protocol::v1::TurnRequest) -> Result<(), ErrorBody> {
    if turn.lease.on_disconnect != DisconnectAction::Continue
        || turn.lease.heartbeat_timeout_ms.is_some()
    {
        return Err(ErrorBody::new(
            ErrorCode::UnsupportedFeature,
            "disconnect actions and heartbeat leases require a future leased connection API",
        ));
    }
    Ok(())
}

/// What one session's actor said about its own state while the probe ran.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActorStateObservation {
    Reported(SessionState),
    /// The registry no longer holds this session, or holds a later generation.
    Gone,
    /// The actor did not answer inside [`DIAGNOSE_ACTOR_DEADLINE`], or the read
    /// itself did not come back.
    Unanswered,
}

impl ActorStateObservation {
    fn from_read(
        read: Result<
            Result<pseudomux_protocol::v1::SessionSnapshot, ErrorBody>,
            tokio::time::error::Elapsed,
        >,
    ) -> Self {
        match read {
            Ok(Ok(snapshot)) => Self::Reported(snapshot.state),
            // Every failure mode of the read collapses here on purpose. The
            // registry answers `session_not_found` and `stale_session_generation`
            // for a session that left during the probe, and `daemon_lost` for an
            // actor whose task ended; none of the three is evidence about the
            // rmux sidecar, which is the only thing this report claims to have
            // tested.
            Ok(Err(_)) => Self::Gone,
            Err(_) => Self::Unanswered,
        }
    }

    const fn state(self) -> Option<SessionState> {
        match self {
            Self::Reported(state) => Some(state),
            Self::Gone | Self::Unanswered => None,
        }
    }
}

/// Whether pmux would still let a caller do work on a session in this state.
///
/// Derived from the refusal `SessionActor::submit_turn` already returns
/// (`crate::v1::actor`, the `self.state != Ready` arm): the states whose
/// refusal is NON-retryable are the states in which pmux has already declared
/// the session unusable. Everything else is pmux telling a caller to come back.
///
/// This is deliberately not a hand-listed set of "bad states". A hand-listed
/// set is a rule written against today's state machine; this one is written
/// against the answer callers actually get.
const fn session_is_still_offered(state: SessionState) -> bool {
    !matches!(
        state,
        SessionState::Tainted | SessionState::Closing | SessionState::Closed | SessionState::Failed
    )
}

const fn runtime_finding(
    probed: Result<&BTreeSet<String>, &ControlPlaneFault>,
    launch_broker_is_accepting: bool,
) -> RuntimeFinding {
    match probed {
        Err(ControlPlaneFault::Unreachable) => RuntimeFinding::ControlPlaneUnreachable,
        Err(ControlPlaneFault::Unresponsive) => RuntimeFinding::ControlPlaneUnresponsive,
        Err(ControlPlaneFault::Refused) => RuntimeFinding::ControlPlaneRefused,
        Ok(_) if !launch_broker_is_accepting => RuntimeFinding::LaunchBrokerStopped,
        Ok(_) => RuntimeFinding::PrivateRuntimeResponsive,
    }
}

/// The whole judgement of [`NativeService::diagnose`], separated from the three
/// reads that feed it.
///
/// Deliberately pure and deliberately not a method: every classification this
/// report makes is decided here, so every classification can be exercised
/// without a sidecar, a Claude process or a clock. The reads above are what the
/// live reproductions prove; this is what the default suite proves.
fn build_diagnosis(
    registered: &[(SessionId, SessionGenerationId, String)],
    probed: Result<&BTreeSet<String>, &ControlPlaneFault>,
    elapsed_ms: u64,
    launch_broker_is_accepting: bool,
    observed: &HashMap<SessionId, ActorStateObservation>,
) -> DaemonDiagnosis {
    let live = probed.ok();
    let runtime = RuntimeProbe::new(
        runtime_finding(probed, launch_broker_is_accepting),
        elapsed_ms,
        live.map(|live| u32::try_from(live.len()).unwrap_or(u32::MAX)),
    );
    let sessions = registered
        .iter()
        .map(|(session_id, generation_id, private_session_name)| {
            let observation = observed
                .get(session_id)
                .copied()
                // A session whose read never came back is unanswered, not
                // absent: dropping it would delete a session from the report
                // for the one reason that most deserves an entry.
                .unwrap_or(ActorStateObservation::Unanswered);
            let present = live.map(|live| live.contains(private_session_name));
            SessionProbe::new(
                *session_id,
                *generation_id,
                session_finding(observation, present),
                observation.state(),
                present,
            )
        })
        .collect();
    // EMPTY, and filled by `NativeService::diagnose` afterwards. This function
    // is the pure classifier -- it is handed three reads and no service -- and
    // the layers need the configuration, the pool census and the broker. An
    // empty list here is not a silent pass: `DaemonDiagnosis::outcome` folds
    // every ABSENT layer as `Unproven`, so a diagnosis that never reached the
    // layer builder reports `unproven` and not `pass`.
    DaemonDiagnosis {
        layers: Vec::new(),
        runtime,
        sessions,
    }
}

/// The control plane: could a connection be made to the private rmux socket.
///
/// Separated from [`private_runtime_layer`] because they fail differently and
/// an operator does different things about them. `Unreachable` means no
/// connection -- the sidecar is gone, or its socket is. Anything past that
/// means a connection WAS made, so the control plane itself is proven, and the
/// fault belongs to the layer above.
fn control_plane_layer(
    probed: Result<&BTreeSet<String>, &ControlPlaneFault>,
    elapsed_ms: u64,
) -> HealthLayer {
    let (finding, detail) = match probed {
        Err(ControlPlaneFault::Unreachable) => (
            LayerFinding::Faulted,
            "no connection could be established to the private rmux socket; nothing behind it \
             was reached"
                .to_owned(),
        ),
        // Both of these are the control plane WORKING. A deadline expiry means
        // the connection was open, and a refusal means the sidecar answered.
        Err(ControlPlaneFault::Unresponsive | ControlPlaneFault::Refused) | Ok(_) => (
            LayerFinding::Exercised,
            format!("a connection to the private rmux socket was established in {elapsed_ms} ms"),
        ),
    };
    HealthLayer::new(
        HealthLayerName::ControlPlane,
        finding,
        detail,
        json!({ "elapsed_ms": elapsed_ms }),
    )
}

/// The private runtime: did the rmux sidecar COMPLETE a dispatch-path exchange.
///
/// This is the layer all four false-healthy reproductions failed at, and the
/// reason it is a distinct layer from the control plane: a sidecar that has been
/// stopped, killed or wedged still owns a socket that accepts, so "the endpoint
/// is there" and "the endpoint serves" are two facts and only the second is
/// worth reporting as health.
///
/// It reuses the probe the foundation built -- `probe_request_path`, which takes
/// the sidecar's dispatch state lock -- rather than inventing a second one. Two
/// probes of one subject is two answers that can disagree.
fn private_runtime_layer(
    probed: Result<&BTreeSet<String>, &ControlPlaneFault>,
    elapsed_ms: u64,
) -> HealthLayer {
    let (finding, detail, terminals) = match probed {
        Ok(live) => (
            LayerFinding::Exercised,
            format!(
                "the private rmux sidecar completed a `list-sessions` exchange, which takes its \
                 dispatch state lock, in {elapsed_ms} ms"
            ),
            Some(live.len()),
        ),
        Err(ControlPlaneFault::Unreachable) => (
            LayerFinding::NotEstablished,
            "the control plane could not be reached, so no exchange was attempted and nothing \
             is claimed about the sidecar"
                .to_owned(),
            None,
        ),
        Err(ControlPlaneFault::Unresponsive) => (
            LayerFinding::Faulted,
            format!(
                "the private rmux sidecar accepted a connection and did not complete the \
                 exchange within the same deadline every session operation is held to \
                 ({elapsed_ms} ms elapsed)"
            ),
            None,
        ),
        Err(ControlPlaneFault::Refused) => (
            LayerFinding::Faulted,
            "the private rmux sidecar answered the exchange with an error".to_owned(),
            None,
        ),
    };
    HealthLayer::new(
        HealthLayerName::PrivateRuntime,
        finding,
        detail,
        json!({ "elapsed_ms": elapsed_ms, "live_private_terminals": terminals }),
    )
}

/// The launch broker's accept loop, which every session start goes through.
///
/// `Exercised` here now means what the word means: a connection was made to the
/// broker's own endpoint, a launcher frame was written, and the broker's answer
/// was read back. Accept, framing, length prefix and dispatch all ran.
///
/// It used to be a task-liveness read -- `!task.is_finished()` -- reported as
/// `exercised`, which is this codebase's bug class exactly: the finding
/// promised a completed exchange and the predicate tested whether a future had
/// resolved. A loop that accepts and wedges before `serve_connection` reads its
/// first byte passes a liveness read and hangs `pmux-launcher` inside a real
/// session start.
///
/// The one step deliberately NOT exercised is the pending-token lookup, and the
/// detail string says so rather than leaving the reader to assume otherwise:
/// that lookup consumes a one-use capability, and a diagnostic that spends
/// capabilities is one an operator may not call twice. See
/// [`crate::launch_broker::LaunchBroker::probe`].
///
/// Both inputs are reported. They answer different questions -- "did the loop
/// end?" and "did an exchange complete?" -- and a broker that is accepting
/// while its exchanges fail is a state neither one alone can name.
fn launch_broker_layer(accepting: bool, probe: &BrokerProbe) -> HealthLayer {
    let (finding, detail) = match (probe.exchanged(), accepting) {
        (true, true) => (
            LayerFinding::Exercised,
            format!(
                "{}; the token lookup is NOT on this path, because redeeming a token consumes a \
                 one-use launch capability",
                probe.describe()
            ),
        ),
        // Exchanged but the accept loop reports finished: the exchange is the
        // stronger evidence and the disagreement is the finding. Reporting the
        // pass alone would hide a broker that is about to stop serving.
        (true, false) => (
            LayerFinding::Faulted,
            format!(
                "{}, but the accept loop's task has finished, so this was the last connection it \
                 will ever serve and every later session start meets ConnectionRefused",
                probe.describe()
            ),
        ),
        (false, _) => (
            LayerFinding::Faulted,
            format!(
                "{} (accept loop task still running: {accepting}); every session start goes \
                 through this endpoint",
                probe.describe()
            ),
        ),
    };
    HealthLayer::new(
        HealthLayerName::LaunchBroker,
        finding,
        detail,
        json!({ "accepting": accepting, "exchanged": probe.exchanged() }),
    )
}

/// The compatibility profile decides whether a minified cell can start at all,
/// and therefore whether Path B can serve anything.
///
/// `admitted` is [`CompatibilityProfileRegistry::admissible_here`] -- operator
/// cells PLUS pmux's own promoted ones, filtered to this platform. Counting the
/// operator's set alone was correct only while the promoted set was empty; a
/// daemon serving Path B off a promoted cell would have reported `Faulted` over
/// a pool that was minting perfectly well. Counting every cell regardless of
/// platform is the opposite error: a macos cell admits nothing on Linux, and a
/// count that includes it reports an admission path that does not exist.
///
/// # The pool is the subject, not the registry
///
/// Whether the layer has a subject is decided by the POOL and by nothing else.
/// Nothing on a pool-less daemon needs a promoted cell: full-cell sessions do
/// not, and a caller who explicitly demands a tested one is refused at that
/// request, which is a per-request answer and not a property of this daemon's
/// health. So `path_b == false` is `NothingToExercise` at any count.
///
/// That arm used to read `(0, false)`, and its companion `(count, _)` reported
/// `Exercised` for a pool-less daemon that merely HELD a profile -- a finding
/// whose word says the layer was exercised over a predicate that only asked
/// whether a list was non-empty. Promotion makes that list non-empty on every
/// supported host, so the old shape would have flipped every Path A daemon on
/// macos/aarch64 from `nothing to exercise` to `exercised` without one more
/// thing being exercised.
///
/// # The version IS in the predicate now
///
/// It used to not be, under the reason "nothing here knows which Claude the
/// pool will launch". That was false of the daemon: `pool.config()` names the
/// executable, and [`NativeService::admit_pool_claude`] runs it and asks the
/// registry. The old `Exercised` arm said "a minified cell has an admission
/// path" while every mint on this host was refused -- and because `pmux doctor`
/// exits on the fold over these layers, it exited 0 `healthy` one command
/// before `pmux ask` returned `unsupported_claude_version`.
///
/// The count alone stays in the detail because it answers the other half of an
/// operator's question -- whether the platform has any cell at all, as opposed
/// to whether this VERSION is in one -- and the two failures want different
/// fixes.
fn compatibility_layer(admitted: usize, pool_claude: Option<&PoolClaudeAdmission>) -> HealthLayer {
    // Exhaustive on the admission, with no wildcard: a state added to
    // `PoolClaudeAdmission` is a compile error here rather than a state the
    // layer silently reports as healthy.
    let (finding, detail) = match pool_claude {
        None => (
            LayerFinding::NothingToExercise,
            format!(
                "no stateless pool is configured on this daemon, so nothing it runs needs a \
                 promoted Claude compatibility cell and there was nothing to exercise \
                 ({admitted} cell(s) would match this platform); a request that explicitly \
                 demands a tested cell is refused at that request, and full-cell sessions need \
                 no profile"
            ),
        ),
        Some(PoolClaudeAdmission::Admitted { version }) => (
            LayerFinding::Exercised,
            format!(
                "the stateless engine's Claude Code {version} is admitted by one of the \
                 {admitted} Claude compatibility cell(s) matching this platform, so a minified \
                 cell can be minted"
            ),
        ),
        Some(PoolClaudeAdmission::Refused { version, refusal }) => (
            LayerFinding::Faulted,
            format!(
                "the stateless engine would launch Claude Code {version}, which none of the \
                 {admitted} Claude compatibility cell(s) matching this platform admits, so every \
                 `pmux run` is refused with unsupported_claude_version ({refusal}); measure this \
                 version and admit it with `pmuxd --tested-claude-profile`, or run a version pmux \
                 has already promoted"
            ),
        ),
        Some(PoolClaudeAdmission::Unreadable { executable, error }) => (
            LayerFinding::NotEstablished,
            format!(
                "the stateless engine's Claude executable {} could not be asked its version, so \
                 whether any of the {admitted} compatibility cell(s) matching this platform \
                 admits it is unknown: {error}",
                executable.display()
            ),
        ),
    };
    HealthLayer::new(
        HealthLayerName::CompatibilityProfile,
        finding,
        detail,
        json!({
            "admitted_cells": admitted,
            "path_b_enabled": pool_claude.is_some(),
            "pool_claude_version": match pool_claude {
                Some(
                    PoolClaudeAdmission::Admitted { version }
                    | PoolClaudeAdmission::Refused { version, .. },
                ) => Some(version.clone()),
                Some(PoolClaudeAdmission::Unreadable { .. }) | None => None,
            },
        }),
    )
}

/// What the daemon established about the Claude its stateless pool would
/// launch, by running that executable and asking its own registry.
///
/// Three states and not a `bool`, for the reason `LayerFinding` has four: "this
/// version is refused" and "the version could not be read" are different
/// operator problems, and folding the second into the first reports a fault
/// nobody can act on while folding it into the third reports health nobody
/// measured.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PoolClaudeAdmission {
    /// The registry admits this version under the policy a mint uses.
    Admitted { version: String },
    /// The registry refuses it, and the refusal is the one a mint would get.
    Refused { version: String, refusal: String },
    /// The executable could not be asked. Nothing is claimed either way.
    Unreadable { executable: PathBuf, error: String },
}

/// What a pool mint would get for one already-read Claude version.
///
/// Split from [`NativeService::admit_pool_claude`] so the DECISION is testable
/// without a process: reading a version needs an executable on disk, and
/// deciding what the registry does with it needs nothing. The two halves failed
/// differently in the report that produced this function -- the version was
/// read perfectly on every start, and nothing ever compared it.
///
/// Every input is taken from the same place a mint takes it:
/// [`crate::stateless::POOL_COMPATIBILITY`], [`crate::stateless::POOL_TERMINAL`]
/// and [`crate::stateless::POOL_CELL`] are the constants
/// `crate::stateless::launch_request_for` writes into a mint request, and the
/// two calls below are the pair `NativeService::start_session` runs on that
/// request. Naming `RequireTested`, `Transparent` or `Sdk` here instead would
/// be a second copy of the admission rule, free to keep reporting `exercised`
/// after the mint's copy has moved.
fn admit_claude_version(
    registry: &CompatibilityProfileRegistry,
    version: String,
    untested_transcript_drain_ms: u64,
) -> PoolClaudeAdmission {
    let refusal = registry
        .resolve(
            crate::stateless::POOL_COMPATIBILITY,
            &version,
            crate::stateless::POOL_TERMINAL.profile,
            crate::stateless::POOL_TERMINAL.input_transport,
            untested_transcript_drain_ms,
        )
        .and_then(|report| {
            if crate::stateless::POOL_CELL == SessionCell::Minified {
                require_tested_for_minified_cell(&report)?;
            }
            Ok(())
        })
        .err();
    match refusal {
        Some(refusal) => PoolClaudeAdmission::Refused {
            version,
            refusal: refusal.message,
        },
        None => PoolClaudeAdmission::Admitted { version },
    }
}

/// What the pool layer is asked about: the shape the operator DECLARED, and
/// the census of what the pool is holding right now.
///
/// `declared_warm` is in here because the empty-pool question cannot be
/// answered without it, and it was the field whose absence made this layer
/// wrong twice in opposite directions. With only `(pool_size, census)` the
/// layer can ask "is the pool empty?" and nothing else, so it cannot tell a
/// cold pool that was never asked to hold anything -- vacuous, and a pass --
/// from a pool that was told to hold N and is holding none, which is a fault
/// no other surface records. The first encoding answered `unproven` to both
/// and made every correct Path B daemon permanently unprovable; the second
/// answered `pass` to both and made a pool that could serve nothing report
/// healthy. Both are the same missing input.
struct PoolSubject {
    pool_size: u32,
    /// [`crate::pool::PoolConfig::declared_warm_total`], read from the live
    /// config rather than restated, so the number this layer judges against is
    /// the number `Pool::start` refused to boot without.
    declared_warm: u32,
    census: crate::pool::PoolCensus,
    /// Conversation → `s{slot}e{epoch}` map. Empty in fixture-built
    /// subjects; the live diagnose path fills it from the pool.
    conversation_leases: Vec<crate::pool::ConversationLease>,
}

/// The stateless pool, per class where it has one.
///
/// A pool holding no warm instances is not a fault BY ITSELF: absence of an
/// undeclared instance is a capacity fact, and a pool nobody asked to hold
/// anything is idle rather than broken. It is a fault when the operator
/// declared a warm floor and the pool is holding none of it, because a
/// declared floor is capacity that exists precisely so a caller arriving cold
/// finds it -- the same reading the TTL sweep already gives it, and the same
/// condition `Pool::start` refuses to boot on. A HALTED pool is a fault, and a
/// pool that has leaked a slot is, because both are permanent losses that no
/// retry recovers.
fn pool_layer(
    subject: Option<&PoolSubject>,
    pool_terminals: &[String],
    live: Option<&BTreeSet<String>>,
) -> HealthLayer {
    let Some(PoolSubject {
        pool_size,
        declared_warm,
        census,
        conversation_leases,
    }) = subject
    else {
        // `NothingToExercise`, not `NotEstablished`: a daemon booted without
        // `--path-b-parent` has no pool and never will have one, so there is
        // nothing here whose health could be in question. Reporting it as
        // unproven made every Path A daemon fail `pmux doctor` forever, for
        // having declined a feature.
        return HealthLayer::new(
            HealthLayerName::Pool,
            LayerFinding::NothingToExercise,
            "no stateless pool is configured on this daemon, so there was nothing to exercise; \
             --path-b-parent is what enables one and this daemon was not given it",
            json!({ "configured": false }),
        );
    };
    // The same terminal-presence question `SessionProbe` asks per caller
    // session, asked of the pool's instances and answered as a COUNT. A pool
    // instance's session id is the one name no client may learn, so this layer
    // reports how many of them the sidecar reports and never which.
    let terminals_present = live.map(|live| {
        pool_terminals
            .iter()
            .filter(|name| live.contains(*name))
            .count()
    });
    let evidence = json!({
        "configured": true,
        "pool_size": pool_size,
        "declared_warm": declared_warm,
        "capacity": census.capacity,
        "live": census.live,
        "idle": census.idle,
        // `in_flight` is `CheckedOut | Delivering` and NOT `Clearing`: a
        // clearing instance has already answered its caller, and an operator
        // reading this to decide whether the pool is saturated by real work
        // needs the two apart. See `PoolCensus::clearing`.
        "in_flight": census.in_flight,
        "clearing": census.clearing,
        "leased": census.leased,
        "conversation_leases": conversation_leases.iter().map(|lease| {
            json!({
                "conversation": lease.conversation_id,
                "cell": lease.cell,
                "state": lease.state,
            })
        }).collect::<Vec<_>>(),
        "reserved": census.reserved,
        "tearing_down": census.tearing_down,
        "leaked": census.leaked,
        "halted": census.halted,
        "registered_instances": pool_terminals.len(),
        "instance_terminals_present": terminals_present,
    });
    let (finding, detail) = if let Some(reason) = census.halted {
        (
            LayerFinding::Faulted,
            format!(
                "the stateless pool has HALTED ({reason}): pmux's model of the local command \
                 menu no longer matches the installed Claude, so every checkout is refused"
            ),
        )
    } else if census.leaked > 0 {
        (
            LayerFinding::Faulted,
            format!(
                "the stateless pool has leaked {} of its {pool_size} slots: a teardown could not \
                 prove its process reaped, so the tree was retained as evidence and the capacity \
                 is permanently subtracted",
                census.leaked
            ),
        )
    } else if census.live == 0 && *declared_warm == 0 {
        // Vacuous, and only because nothing was declared. The question this
        // arm answers is not "is the pool empty?" -- that one has no health
        // content -- but "is the pool empty when nothing said it should not
        // be?", and the answer is derived from `declared_warm` rather than
        // from the emptiness alone.
        //
        // The detail promises exactly what the predicate tested and stops.
        // It used to close with "and the next call of any class mints one",
        // which this layer never tests and which was FALSE in the state that
        // produced this finding: measured against a drained pool whose Claude
        // executable no longer starts, six consecutive `ask` calls were
        // refused while this branch reported `pass`.
        (
            LayerFinding::NothingToExercise,
            format!(
                "no warm floor is declared and the stateless pool holds none of its {pool_size} \
                 slot(s) live, so there was nothing to exercise; with nothing declared, holding \
                 none is a capacity fact rather than a fault. An undeclared class is minted when \
                 a caller asks for it, and this layer does not test whether that mint would \
                 succeed"
            ),
        )
    } else if census.live == 0 {
        // DECLARED and absent. `Pool::start` refuses this daemon's boot when it
        // cannot mint the warm set -- "operator errors worth failing startup
        // over, not degraded modes" -- and the TTL sweep will not evict into
        // the floor, because "declared capacity exists precisely so it is there
        // when a caller arrives cold". The identical condition thirty seconds
        // after boot is the same fault, and nothing else in the daemon records
        // it: `spawn_rewarm` drops a failed mint on the floor with no log and
        // no counter, so this layer is the only surface that can say it.
        //
        // Only the whole floor being absent is claimed, not a partial deficit.
        // A cold swap may take a declared-but-idle instance when an undeclared
        // class has an actual caller and nothing else is idle, and a recycle
        // holds a slot between destroy and mint, so `live < declared_warm`
        // alone is a state a correct pool passes through under load. `live == 0`
        // is not: it means not one declared instance survives.
        (
            LayerFinding::Faulted,
            format!(
                "a warm floor of {declared_warm} instance(s) is declared and the stateless pool \
                 holds none of its {pool_size} slot(s) live: not one declared instance is \
                 present. A declared floor is capacity that exists so a caller arriving cold \
                 finds it, and a pool that cannot mint the same set refuses this daemon's boot"
            ),
        )
    } else if terminals_present.is_some_and(|present| present < pool_terminals.len()) {
        (
            LayerFinding::Faulted,
            format!(
                "the stateless pool holds {} registered instance(s) and the private rmux sidecar \
                 reports a terminal for only {}; the instances are named nowhere in this report \
                 because a pool instance's session id is the one name no client may learn",
                pool_terminals.len(),
                terminals_present.unwrap_or(0)
            ),
        )
    } else if terminals_present.is_none() {
        // The control-plane probe did not complete, so the one question this
        // layer asks that is not the pool talking to itself -- does the sidecar
        // still hold a terminal for every instance the pool believes in? -- was
        // not asked. That is UNPROVEN, and it used to fall through to
        // `Exercised`: measured after the private sidecar was SIGKILLed under
        // fifteen concurrent callers, the layer reported `exercised` (pass)
        // over `instance_terminals_present: null` with one instance
        // registered, while its own detail string said "no instance terminal
        // was looked for". `private_runtime` and `performance` depend on the
        // same probe and both already report `not_established` here; this layer
        // was the one that did not.
        (
            LayerFinding::NotEstablished,
            format!(
                "the stateless pool holds {} live instance(s) against a capacity of {} and \
                 believes {} of them are registered, but the control-plane probe did not \
                 complete, so whether the private rmux sidecar still holds a terminal for any of \
                 them was not established",
                census.live,
                census.capacity,
                pool_terminals.len()
            ),
        )
    } else {
        (
            LayerFinding::Exercised,
            format!(
                "the stateless pool holds {} live instance(s) against a capacity of {}: {} idle, \
                 {} serving a turn, {} holding a conversation lease, {} clearing between turns, \
                 {} reserved, {} tearing down; the sidecar reports a \
                 terminal for all {} registered instance(s)",
                census.live,
                census.capacity,
                census.idle,
                census.in_flight,
                census.leased,
                census.clearing,
                census.reserved,
                census.tearing_down,
                terminals_present.unwrap_or_default(),
            ),
        )
    };
    HealthLayer::new(HealthLayerName::Pool, finding, detail, evidence)
}

/// The registered sessions, folded from the per-session probes.
///
/// One line, and deliberately: the fold lives on `HealthLayer` in the protocol
/// crate so that the daemon and every test that needs a realistic tree call the
/// SAME producer. A fixture that assembles this layer by hand can state a
/// combination the daemon cannot emit -- `sessions: []` beside a `sessions`
/// layer reading `exercised` is exactly the one that let the encoding defect
/// below ship past a green suite.
///
/// A daemon holding no sessions reports `NothingToExercise`, which is `pass`.
/// It used to report `NotEstablished`, which is `unproven`, and that made every
/// correct Path B daemon permanently unprovable -- see [`LayerFinding`].
fn sessions_layer(sessions: &[SessionProbe]) -> HealthLayer {
    HealthLayer::for_sessions(sessions)
}

/// The one control-plane exchange this daemon just performed, measured against
/// the envelope it was sized on.
///
/// The envelope is the runtime's OWN `operation_timeout`, read from it rather
/// than restated here: it is the deadline every session operation against the
/// sidecar is already held to, so exceeding it here is exceeding the bound the
/// rest of the daemon depends on. A constant beside the runtime would be a
/// second copy of that bound, free to drift from the one that is enforced. The
/// layer
/// reports what it timed and nothing else: it does not claim anything about
/// turn latency, because it started no turn.
fn performance_layer(
    probed: Result<&BTreeSet<String>, &ControlPlaneFault>,
    elapsed_ms: u64,
    envelope_ms: u64,
    sessions: &[SessionProbe],
) -> HealthLayer {
    let evidence = json!({
        "control_plane_elapsed_ms": elapsed_ms,
        "control_plane_envelope_ms": envelope_ms,
        "actor_deadline_ms": DIAGNOSE_ACTOR_DEADLINE.as_millis(),
        "actors_unanswered": sessions
            .iter()
            .filter(|session| session.finding == SessionFinding::SessionActorUnresponsive)
            .count(),
    });
    if probed.is_err() {
        return HealthLayer::new(
            HealthLayerName::Performance,
            LayerFinding::NotEstablished,
            "the control-plane exchange did not complete, so its duration measures a failure \
             rather than a latency and nothing is claimed about performance",
            evidence,
        );
    }
    let unanswered = sessions
        .iter()
        .filter(|session| session.finding == SessionFinding::SessionActorUnresponsive)
        .count();
    let (finding, detail) = if elapsed_ms > envelope_ms {
        (
            LayerFinding::Faulted,
            format!(
                "the control-plane exchange completed in {elapsed_ms} ms, over the \
                 {envelope_ms} ms envelope every session operation against the sidecar is held \
                 to"
            ),
        )
    } else if unanswered > 0 {
        (
            LayerFinding::Faulted,
            format!(
                "the control-plane exchange completed in {elapsed_ms} ms, but {unanswered} \
                 session actor(s) did not answer within {} ms",
                DIAGNOSE_ACTOR_DEADLINE.as_millis()
            ),
        )
    } else {
        (
            LayerFinding::Exercised,
            format!(
                "the control-plane exchange completed in {elapsed_ms} ms, inside the \
                 {envelope_ms} ms envelope, and every session actor answered inside {} ms",
                DIAGNOSE_ACTOR_DEADLINE.as_millis()
            ),
        )
    };
    HealthLayer::new(HealthLayerName::Performance, finding, detail, evidence)
}

/// `private_terminal_present` is `None` exactly when the control-plane probe
/// did not complete, which is the only state in which no terminal was looked
/// for.
const fn session_finding(
    observation: ActorStateObservation,
    private_terminal_present: Option<bool>,
) -> SessionFinding {
    let Some(present) = private_terminal_present else {
        return SessionFinding::NotProbed;
    };
    match observation {
        ActorStateObservation::Gone => SessionFinding::SessionClosedDuringProbe,
        ActorStateObservation::Unanswered => SessionFinding::SessionActorUnresponsive,
        ActorStateObservation::Reported(state) if !session_is_still_offered(state) => {
            SessionFinding::SessionDeclaredUnusable
        }
        ActorStateObservation::Reported(_) if present => SessionFinding::TerminalPresent,
        ActorStateObservation::Reported(_) => SessionFinding::TerminalMissing,
    }
}

fn validate_native_request<T>(request: &T) -> Result<(), ErrorBody>
where
    T: Serialize + ?Sized,
{
    validate_v1_serializable(request).map_err(|_| {
        ErrorBody::new(
            ErrorCode::InvalidConfig,
            "request cannot be represented within protocol v1",
        )
    })
}

/// Protocol timestamps for the agent store, from the same clock every other
/// published instant uses.
fn now_ms() -> pseudomux_protocol::v1::TimestampMs {
    use crate::v1::Clock;

    crate::v1::SystemClock.now_ms()
}

/// The launch configuration of a request that has already been resolved.
///
/// A start whose `claude` is absent has not been through
/// `resolve_agent_reference`, which is the first line of the one start path.
/// It is a refusal rather than an `expect` because `NativeService` is `pub` and
/// an embedder can construct any DTO the type system admits.
///
/// # Errors
///
/// [`ErrorCode::InvalidConfig`], naming what the request is missing.
fn require_resolved_launch(
    request: &StartSessionRequest,
) -> Result<&pseudomux_protocol::v1::ClaudeLaunchConfig, ErrorBody> {
    request.claude.as_ref().ok_or_else(unresolved_launch)
}

fn require_resolved_launch_mut(
    request: &mut StartSessionRequest,
) -> Result<&mut pseudomux_protocol::v1::ClaudeLaunchConfig, ErrorBody> {
    request.claude.as_mut().ok_or_else(unresolved_launch)
}

fn unresolved_launch() -> ErrorBody {
    ErrorBody::new(
        ErrorCode::InvalidConfig,
        "start request carries neither an inline `claude` launch configuration nor a resolvable \
         `agent` reference",
    )
}

/// A structurally valid start used only as the value `std::mem::replace` leaves
/// behind for the one statement between taking the caller's request and writing
/// the resolved one back.
///
/// It is never launched, never validated and never observed: the very next
/// statement overwrites it. It exists because resolution consumes the request
/// by value -- which is what makes it a pure function of its inputs rather than
/// a mutation with an implicit order.
fn placeholder_start_request() -> StartSessionRequest {
    StartSessionRequest {
        identity: SessionIdentity::New { session_id: None },
        cwd: String::new(),
        claude: None,
        agent: None,
        environment: pseudomux_protocol::v1::EnvironmentSpec::default(),
        auth_policy: pseudomux_protocol::v1::AuthPolicy::default(),
        config_isolation: None,
        terminal: pseudomux_protocol::v1::TerminalSpec::default(),
        lifecycle: pseudomux_protocol::v1::LifecycleMode::default(),
        retention: RetentionPolicy::default(),
        compatibility: pseudomux_protocol::v1::CompatibilityPolicy::default(),
        cell: SessionCell::default(),
    }
}

pub fn validate_public_start_retention(retention: &RetentionPolicy) -> Result<(), ErrorBody> {
    if matches!(retention, RetentionPolicy::OneShot) {
        return Err(ErrorBody::new(
            ErrorCode::UnsupportedFeature,
            "one_shot retention is reserved for run_once; start_session requires persistent retention",
        ));
    }
    Ok(())
}

fn validate_subscribe_events(
    request: &pseudomux_protocol::v1::SubscribeEventsRequest,
) -> Result<(), ErrorBody> {
    use pseudomux_protocol::v1::{MAX_SUBSCRIBE_EVENTS, MAX_SUBSCRIBE_WAIT_MS};

    if request.wait_ms > MAX_SUBSCRIBE_WAIT_MS || request.max_events > MAX_SUBSCRIBE_EVENTS {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            "event subscription exceeds the public wait or batch bound",
        )
        .with_details(json!({
            "wait_ms": diagnostic_u64(request.wait_ms),
            "max_events": request.max_events,
            "maximum_wait_ms": MAX_SUBSCRIBE_WAIT_MS,
            "maximum_events": MAX_SUBSCRIBE_EVENTS,
        })));
    }
    Ok(())
}

fn require_process_reaped(result: CloseSessionResult) -> Result<CloseSessionResult, ErrorBody> {
    if !result.process_reaped {
        return Err(ErrorBody::new(
            ErrorCode::RecoveryFailed,
            "session close completed without confirming that the owned process was reaped",
        )
        .retryable(true)
        .with_details(json!({ "session_id": result.session_id })));
    }
    Ok(result)
}

#[cfg(test)]
async fn close_unpublished_terminal(
    session_id: SessionId,
    terminal: &Arc<RmuxTerminalControl>,
) -> Result<(), ErrorBody> {
    let process_reaped = terminal
        .close(session_id, pseudomux_protocol::v1::ClosePolicy::Force)
        .await
        .map_err(DriverFailureExt::protocol)?;
    if process_reaped {
        Ok(())
    } else {
        Err(ErrorBody::new(
            ErrorCode::RecoveryFailed,
            "startup cleanup did not confirm that the unpublished process was reaped",
        )
        .retryable(true)
        .with_details(json!({ "session_id": session_id })))
    }
}

fn combine_startup_and_cleanup_errors(startup: ErrorBody, cleanup: ErrorBody) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::RecoveryFailed,
        "session startup failed and process cleanup could not yet be confirmed",
    )
    .retryable(cleanup.retryable)
    .with_details(json!({
        "startup_error": startup,
        "cleanup_error": cleanup,
    }))
}

fn combine_turn_and_cleanup_errors(turn: ErrorBody, cleanup: ErrorBody) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::RecoveryFailed,
        "the one-shot turn failed and process cleanup could not be confirmed",
    )
    .retryable(cleanup.retryable)
    .with_details(json!({
        "turn_error": turn,
        "cleanup_error": cleanup,
    }))
}

fn turn_wait_safety_delay(
    remaining_turn_ms: u64,
    cancel_recovery_timeout: Duration,
    transcript_drain_ms: u64,
) -> Result<Duration, ErrorBody> {
    // Cancellation can consume one terminal-recovery window, a second window
    // plus the compatibility drain while stabilizing JSONL, and a final
    // independent forced-cleanup window. Keep the infrastructure guard outside
    // all three; it is not allowed to publish a competing turn outcome.
    let recovery = cancel_recovery_timeout.checked_mul(3).ok_or_else(|| {
        ErrorBody::new(
            ErrorCode::InvalidConfig,
            "turn safety guard duration overflows the platform duration domain",
        )
    })?;
    Duration::from_millis(remaining_turn_ms)
        .checked_add(recovery)
        .and_then(|delay| delay.checked_add(Duration::from_millis(transcript_drain_ms)))
        .and_then(|delay| delay.checked_add(Duration::from_secs(1)))
        .ok_or_else(|| {
            ErrorBody::new(
                ErrorCode::InvalidConfig,
                "turn safety guard duration overflows the platform duration domain",
            )
        })
}

async fn wait_until_ready(
    terminal: &mut dyn TerminalSession,
    timeout: Duration,
) -> Result<TerminalScreenState, ErrorBody> {
    wait_until_ready_with_timings(
        terminal,
        timeout,
        STARTUP_READY_STABLE_FOR,
        STARTUP_POLL_INTERVAL,
    )
    .await
}

async fn wait_until_ready_with_timings(
    terminal: &mut dyn TerminalSession,
    timeout: Duration,
    stable_for: Duration,
    poll_interval: Duration,
) -> Result<TerminalScreenState, ErrorBody> {
    let deadline = tokio::time::Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| {
            ErrorBody::new(
                ErrorCode::InvalidConfig,
                "startup readiness timeout overflows the monotonic clock domain",
            )
        })?;
    let mut stable_candidate: Option<(TerminalSnapshot, tokio::time::Instant)> = None;
    // `null` until a frame is actually read, and not a hand-written skeleton of
    // zeros. The skeleton was a second, shorter statement of
    // `startup_screen_diagnostics`'s key set that reported a frame of 0 rows and
    // 0 lines -- a shape no capture can have -- for the one case that is
    // genuinely different: no capture happened at all.
    let mut screen_diagnostics = serde_json::Value::Null;
    loop {
        if terminal.lease_lost() {
            return Err(ErrorBody::new(
                ErrorCode::DaemonLost,
                "private rmux lease was lost during Claude startup",
            )
            .retryable(true));
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(startup_ready_timeout(screen_diagnostics));
        }
        let snapshot = tokio::time::timeout(deadline - now, terminal.snapshot())
            .await
            .map_err(|_| startup_ready_timeout(screen_diagnostics.clone()))?
            .map_err(map_startup_terminal_error)?;
        crate::screen_corpus::record_snapshot(crate::driver_io::STARTUP_READINESS_SITE, &snapshot);
        screen_diagnostics = startup_screen_diagnostics(&snapshot);
        match classify_terminal_snapshot(&snapshot) {
            TerminalScreenState::Ready => {
                let same_candidate = stable_candidate
                    .as_ref()
                    .is_some_and(|(observed, _)| observed == &snapshot);
                if !same_candidate {
                    stable_candidate = Some((snapshot, tokio::time::Instant::now()));
                }
                if stable_candidate
                    .as_ref()
                    .is_some_and(|(_, since)| since.elapsed() >= stable_for)
                {
                    return Ok(TerminalScreenState::Ready);
                }
            }
            TerminalScreenState::NeedsInput(needs_input) => {
                return Ok(TerminalScreenState::NeedsInput(needs_input));
            }
            // A booting pane is the ONE place an unrecognized screen is the
            // ordinary case: MEASURED at 2.1.227, a cold cell renders 81 blank
            // frames before its first composer. So this does not refuse on
            // sight; it refuses by reaching `deadline`, and
            // `startup_ready_timeout` then reports the shape of the last frame
            // it saw. `Recognised` sits in the same arm because a composer
            // holding text at startup is equally not readiness.
            TerminalScreenState::Recognised(_) | TerminalScreenState::Unrecognised(_) => {
                stable_candidate = None;
            }
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(startup_ready_timeout(screen_diagnostics));
        }
        tokio::time::sleep((deadline - now).min(poll_interval)).await;
    }
}

fn startup_ready_timeout(screen_diagnostics: serde_json::Value) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::NeedsInput,
        "Claude startup did not reach a ready or recognized interactive screen",
    )
    .with_details(json!({ "screen_shape": screen_diagnostics }))
}

/// What Claude Code's own commander prints when it is handed an option it does
/// not know, and the whole of re-promotion trigger 3's evidence.
///
/// MEASURED, on this host, at two versions and byte-identically:
///
/// ```text
/// $ claude --pmux-probe-sentinel doctor
/// error: unknown option '--pmux-probe-sentinel'
/// $ echo $?
/// 1
/// ```
///
/// Identical at 2.1.223 and 2.1.226, on stderr, with EMPTY stdout, and -- the
/// property that makes the probe free -- the commander names the FIRST unknown
/// option and exits before `doctor`, or any other subcommand, runs. Nothing is
/// executed, no model is reached, and no ledger ordinal is spent.
///
/// The substring rather than the whole line, because the quoted option is the
/// one thing that varies and the option pmux would be told about is whichever
/// of the 24 spellings in `claude_launch.rs` and `sensitive_launch.rs` was
/// removed.
///
/// This is a marker, not a proof: it says the child rejected *an* option, not
/// which, and a screen that never renders it (a child that dies before the PTY
/// is read) reports `false`. It is reported as one boolean beside the other
/// structural facts, so a startup refusal that is really a launch-bundle
/// rejection can be told apart from a Claude that is merely slow -- which today
/// look identical.
const LAUNCH_BUNDLE_REJECTED_MARKER: &str = "unknown option";

/// Whether a startup screen carries the child's own rejection of a launch flag.
///
/// [`crate::compatibility::RepromotionTrigger::LaunchBundleRejected`] names this
/// function's constant as its detector.
#[must_use]
fn launch_bundle_rejected(visible_text: &str) -> bool {
    visible_text.contains(LAUNCH_BUNDLE_REJECTED_MARKER)
}

/// Emits only structural facts needed to diagnose a changed Claude TUI. Raw
/// screen text, line lengths, paths, account data, and prompt content never
/// leave the terminal adapter.
///
/// The first eight keys are [`crate::driver_io::ScreenShape`], which is also
/// what an unrecognized screen is reported by everywhere else in the daemon, and
/// they are TAKEN from it rather than restated: two describers of one frame
/// would be free to disagree about the frame both were describing. What is added
/// here is only what a STARTUP refusal has a use for -- where the composer glyph
/// sits relative to the bottom, and whether the child rejected a launch flag.
fn startup_screen_diagnostics(snapshot: &TerminalSnapshot) -> serde_json::Value {
    let visible_text = &snapshot.visible_text;
    let lines: Vec<_> = visible_text.split('\n').collect();
    let non_empty: Vec<_> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| (!line.trim().is_empty()).then_some((index, line.trim())))
        .collect();
    let prompt_glyph_offsets_from_bottom: Vec<_> = non_empty
        .iter()
        .filter_map(|(index, line)| {
            line.starts_with('❯')
                .then(|| diagnostic_offset_from_bottom(lines.len(), *index))
                .flatten()
        })
        .take(4)
        .collect();
    let ascii_prompt_offsets_from_bottom: Vec<_> = non_empty
        .iter()
        .filter_map(|(index, line)| {
            matches!(*line, ">" | "> ")
                .then(|| diagnostic_offset_from_bottom(lines.len(), *index))
                .flatten()
        })
        .take(4)
        .collect();

    let mut diagnostics = crate::driver_io::ScreenShape::of(snapshot).to_json();
    let startup_only = json!({
        "exact_prompt_glyph_line": non_empty.iter().any(|(_, line)| *line == "❯"),
        "prompt_glyph_offsets_from_bottom": prompt_glyph_offsets_from_bottom,
        "ascii_prompt_offsets_from_bottom": ascii_prompt_offsets_from_bottom,
        "last_non_empty_starts_with_prompt_glyph": non_empty
            .last()
            .is_some_and(|(_, line)| line.starts_with('❯')),
        "contains_esc_hint": visible_text.to_ascii_lowercase().contains("esc"),
        "contains_enter_hint": visible_text.to_ascii_lowercase().contains("enter"),
        // Re-promotion trigger 3. A structural fact like every other key here:
        // it says the child named an option it does not know, and it does not
        // reproduce the option, the line, or anything else off the screen.
        "child_rejected_a_launch_flag": launch_bundle_rejected(visible_text),
        "repromotion_trigger": launch_bundle_rejected(visible_text).then(|| {
            crate::compatibility::RepromotionTrigger::LaunchBundleRejected.id()
        }),
    });
    // The merge refuses to overwrite: a key added to `ScreenShape` that this
    // function also names would otherwise be published as whichever of the two
    // values happened to be written second, silently.
    let (serde_json::Value::Object(diagnostics_map), serde_json::Value::Object(extra)) =
        (&mut diagnostics, startup_only)
    else {
        unreachable!("both halves of the startup diagnostics are JSON objects")
    };
    for (key, value) in extra {
        assert!(
            diagnostics_map.insert(key.clone(), value).is_none(),
            "startup_screen_diagnostics restates ScreenShape's {key:?}"
        );
    }
    diagnostics
}

/// Maps a private-terminal *creation* failure onto the wire.
///
/// The backend half deliberately routes through [`map_startup_terminal_error`]
/// rather than hand-rolling its own arms. This call and the startup readiness
/// snapshot two calls later (`wait_until_ready_with_timings`, which already
/// uses that mapper) sit on the same `start_session` path and can fail for the
/// identical reason; reporting one control-plane loss as `RmuxUnavailable` or
/// as `DaemonLost` depending on which of the two calls happened to hit it is
/// not a distinction any caller can act on, and it is not one anybody chose --
/// it was an artifact of two independently written match arms.
///
/// Converging on the existing mapper adds no `ErrorCode` that `start_session`
/// could not already return. It is recorded as a deliberate deviation from the
/// staging note in `.context/review/transport-fix-design.md`; see the
/// "Deviation: startup error codes converge on `map_startup_terminal_error`"
/// section there for why converging in the other direction was rejected.
fn map_startup_create_terminal_error(error: CreateTerminalError) -> ErrorBody {
    match error {
        CreateTerminalError::Backend(backend) => map_startup_terminal_error(backend),
        // The sensitive-launch broker refused the spec, so rmux was never asked
        // and its health says nothing about this failure. The code is left at
        // `RmuxUnavailable`: the registration failure is an untyped
        // `anyhow::Error` with no shape to map, and giving it one is a separate
        // change from making the two rmux-backed sites agree.
        CreateTerminalError::LaunchRegistration(_) => ErrorBody::new(
            ErrorCode::RmuxUnavailable,
            "private terminal launch registration failed",
        )
        .retryable(true),
    }
}

fn map_startup_terminal_error(error: TerminalBackendError) -> ErrorBody {
    let (code, message, retryable) = match error {
        TerminalBackendError::InvalidLaunch(_) => (
            ErrorCode::InvalidConfig,
            "terminal startup request was invalid",
            false,
        ),
        // Session-scoped, like every other `ControlPlaneLost` after per-session
        // transports: this start's own connection failed, which is not on its
        // own evidence that the sidecar is gone or that any other session is
        // affected. Kept retryable for the same reason
        // `driver_io::map_terminal_error` keeps it — see the derivation there.
        TerminalBackendError::ControlPlaneLost => (
            ErrorCode::DaemonLost,
            "private rmux session control plane was lost during terminal startup",
            true,
        ),
        TerminalBackendError::Rmux(_) => (
            ErrorCode::RmuxUnavailable,
            "private rmux startup operation failed",
            true,
        ),
        TerminalBackendError::ProcessBoundary(_) => (
            ErrorCode::RmuxUnavailable,
            "terminal startup process-boundary operation failed",
            true,
        ),
    };
    ErrorBody::new(code, message).retryable(retryable)
}

/// What one directory is to the session that binds it.
///
/// Both a human label for the refusal and the ROLE two ordinary sessions'
/// directories are compared under: "another session is already using this
/// configuration root" is a statement about config roots, and answering it with
/// somebody else's cwd would change the seed disposition for a shape that is
/// not the one being asked about. A minified cell is the case where role stops
/// mattering -- see [`claim_reaches`].
const CONFIG_ROOT_ROLE: &str = "configuration root";
const WORKING_DIRECTORY_ROLE: &str = "working directory";

/// The directories one live session holds, and the cell holding them.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LiveResourceClaim {
    config_root: PathBuf,
    cwd: PathBuf,
    cell: SessionCell,
}

impl LiveResourceClaim {
    /// Every directory this session binds, with the role it binds it in.
    ///
    /// Enumerated rather than reached through the two fields at each call site,
    /// because the rule that matters is a statement about ALL of them: leak 7's
    /// third shape was an intruder cwd standing on a live cell's CONFIGURATION
    /// ROOT, which the old per-field comparisons could not express. A session
    /// that acquires a third bound directory gets it into every rule by adding
    /// one row here.
    fn directories(&self) -> [(&'static str, &Path); 2] {
        [
            (CONFIG_ROOT_ROLE, self.config_root.as_path()),
            (WORKING_DIRECTORY_ROLE, self.cwd.as_path()),
        ]
    }
}

/// What every live session in this daemon currently holds.
///
/// Both paths come off the session's own transcript source, which is the
/// construction site that already had to know them: the root the seeded files
/// were written into and the cwd the transcript is located under are the two
/// values that same locator was built from, so there is no second computation to
/// drift from the first.
///
/// NEITHER IS PROMISED TO BE CANONICAL, and the rules that consult them must
/// not assume it. `TranscriptLocator::new` canonicalizes the cwd it is handed
/// and keeps the configuration root exactly as given, and the plain
/// `environment.set["CLAUDE_CONFIG_DIR"]` door hands over the caller's own
/// spelling -- so a live session's claimed root can be a symlink path pointing
/// anywhere. That is the mirror of LEAK 8, and it is closed in the predicate
/// rather than here: `claude_launch::one_directory_contains_the_other` resolves
/// whichever of its two paths it is about to walk, and asks both ways round, so
/// a claim gets resolved whether it is the container or the contained.
/// Canonicalizing here instead would have been a second answer to drift from
/// the first, and would still leave every other caller of these paths reading
/// the stored spelling.
fn live_resource_claims(sessions: &HashMap<SessionId, SessionMetadata>) -> Vec<LiveResourceClaim> {
    sessions
        .values()
        .map(|session| LiveResourceClaim {
            config_root: session.transcript.config_root().to_path_buf(),
            cwd: session.transcript.cwd().to_path_buf(),
            cell: session.cell,
        })
        .collect()
}

/// Admits one start against both resources a live session can already hold.
///
/// One function rather than two call sites, so that everything a `start` decides
/// about the incumbents is reachable from a test that has no Claude, no rmux
/// runtime and no `NativeService`: what is left unobserved at the call site is
/// which two paths are handed in, and those come from `effective_config_root`
/// and the resolved launch, both of which are pinned on their own.
fn admit_bound_resources(
    claims: &[LiveResourceClaim],
    config_root: &Path,
    cwd: &Path,
    applicant: SessionCell,
) -> Result<SeedDisposition, ErrorBody> {
    require_establishable_identity(config_root, CONFIG_ROOT_ROLE)?;
    require_establishable_identity(cwd, WORKING_DIRECTORY_ROLE)?;
    let disposition = admit_config_root(
        config_root,
        applicant,
        incumbent_cell_for_config_root(claims, config_root, applicant),
    )?;
    admit_cwd(
        cwd,
        applicant,
        incumbent_cell_for_cwd(claims, cwd, applicant),
    )?;
    Ok(disposition)
}

/// The configuration root the caller ASKED for and the one the child will
/// actually be launched with must be one directory.
///
/// This is the assumption `admit_bound_resources` stands on, stated as a check
/// instead. Admission is keyed on [`effective_config_root`], and the reason
/// that is enough for an isolated start is that `build_environment`'s step 6
/// overwrites `CLAUDE_CONFIG_DIR` with the canonicalized `config_isolation`
/// root, so the delivered root IS the named one. Nothing enforced that. A
/// future ordering change in step 6, a denylist entry that stripped the pin, or
/// any second writer of that name would silently give admission one directory
/// and the child another -- and admission would then be deciding about a
/// directory nobody was ever launched into.
///
/// Compared on the RESOURCE, so the caller may spell the isolation root however
/// they like (the delivered value is canonical and the named one need not be)
/// and an alias is still one directory. A start whose two roots differ is
/// refused rather than reconciled: pmux cannot tell which of them is the
/// mistake.
fn require_isolation_root_is_the_effective_root(
    isolation: Option<&pseudomux_protocol::v1::ConfigIsolation>,
    config_root: &Path,
) -> Result<(), ErrorBody> {
    let Some(isolation) = isolation else {
        return Ok(());
    };
    let named = Path::new(&isolation.root);
    if must_treat_as_same_directory(named, config_root) {
        return Ok(());
    }
    Err(ErrorBody::new(
        ErrorCode::InvalidConfig,
        format!(
            "the config isolation root this start named ({}) is not the configuration root it would be launched with ({}), so admission and the child would be deciding about different directories",
            named.display(),
            config_root.display()
        ),
    ))
}

/// Refuses an applicant whose directory the operating system will not identify.
///
/// The rule, stated exactly, because the two halves are easy to conflate and
/// the previous code conflated them into a byte comparison:
///
/// * `stat` says NO SUCH DIRECTORY -> admissible. That is an answer. It proves
///   the applicant is not the directory any live session is running in, because
///   a live session's directory exists; and it is the ordinary shape of a
///   configuration root that pmux, or the child, is about to create. Refusing
///   here would refuse a legitimate first start into a fresh root.
/// * `stat` FAILS any other way -> refused. Unreadable parent, symlink loop,
///   name too long: pmux does not know which directory the child will be given,
///   so it cannot know whether that directory is one a live cell holds. The
///   only honest answers are "refuse" and "guess", and the previous code's
///   guess -- fall back to comparing the two paths as bytes -- is precisely
///   what the firmlink alias walked through.
/// * `stat` says NO SUCH DIRECTORY about a path spelled with `..` -> ALSO
///   refused. LEAK 5b. The first bullet's claim is that an absence proves the
///   applicant is not a live session's directory, and for a `..` spelling that
///   claim is false: the kernel resolves left-to-right, so `/X/NOPE/../rootA`
///   is `NotFound` while `NOPE` is missing, and a recursive create -- which is
///   what Claude does to its own `CLAUDE_CONFIG_DIR`, and MEASURED, what
///   `mkdir -p` does -- creates the intermediate and then lands the path on the
///   live `/X/rootA`. This is the same "refuse rather than guess" as the bullet
///   above, applied to the one lexical construct whose meaning depends on what
///   exists; see `claude_launch::traverses_a_parent_component` for why pmux
///   does not instead collapse the `..` and trust the result.
///
/// Stated on the APPLICANT rather than folded into the identity comparison
/// because "cannot prove these differ, so treat them as one" says nothing when
/// there is no incumbent to be one WITH: with an empty session map an
/// unidentifiable root would otherwise fall out as `None` incumbent and be
/// handed `SeedDisposition::Write`. That is also why the rule is here rather
/// than inside `DirectoryIdentity::of`: `must_treat_as_same_directory`'s other
/// caller compares the securestorage PIN, which is a keychain-service input
/// rather than a directory pmux binds, and wants the permissive answer.
///
/// Applied to BOTH bound resources, so it holds for any future entry path that
/// computes a configuration root or a working directory some other way. Today
/// `effective_config_root` refuses the whole `..` family one step earlier with
/// a message that names the hazard, and `resolve_claude_launch` hands over a
/// canonicalized cwd; this gate is what makes those two facts belt and braces
/// rather than the only thing standing there.
fn require_establishable_identity(path: &Path, label: &str) -> Result<(), ErrorBody> {
    let identity = DirectoryIdentity::of(path);
    let refuse = match identity {
        DirectoryIdentity::Unresolved => true,
        DirectoryIdentity::Vacant => traverses_a_parent_component(path),
        DirectoryIdentity::Resource(_) => false,
    };
    if refuse {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            format!(
                "{label} {} cannot be inspected, so pmux cannot establish which directory it names; a start is refused rather than admitted on a weaker test",
                path.display()
            ),
        ));
    }
    Ok(())
}

/// The cell an already-live session binds `root` to, or `None` when nothing
/// does.
///
/// Deliberately not a bool. "Is this root in use?" cannot express the rule the
/// leak needed, which is about WHAT is using it: a root a minified cell holds
/// admits nothing at all, while a root two ordinary sessions share is normal.
///
/// The match is on the RESOURCE, not on the spelling. LEAK 5 was this filter
/// comparing canonicalized path strings: `/System/Volumes/Data/private/tmp/R`
/// is `/private/tmp/R` -- same device, same inode -- and `canonicalize` returns
/// it unchanged, so a live cell's root was invisible to five separate attacks
/// that spelled it that way.
fn incumbent_cell_for_config_root(
    claims: &[LiveResourceClaim],
    root: &Path,
    applicant: SessionCell,
) -> Option<SessionCell> {
    incumbent_cell_for(claims, CONFIG_ROOT_ROLE, root, applicant)
}

/// The cell an already-live session binds `cwd` to, or `None`.
fn incumbent_cell_for_cwd(
    claims: &[LiveResourceClaim],
    cwd: &Path,
    applicant: SessionCell,
) -> Option<SessionCell> {
    incumbent_cell_for(claims, WORKING_DIRECTORY_ROLE, cwd, applicant)
}

/// The strictest live claim standing in the way of one directory this start
/// would bind.
fn incumbent_cell_for(
    claims: &[LiveResourceClaim],
    role: &'static str,
    path: &Path,
    applicant: SessionCell,
) -> Option<SessionCell> {
    strictest_cell(
        claims
            .iter()
            .filter(|claim| claim_reaches(claim, role, path, applicant))
            .map(|claim| claim.cell),
    )
}

/// Whether one live session already reaches a directory this start would bind.
///
/// LEAK 7, AND THE ONLY LINE OF IT. Six leaks were spellings of one directory
/// and were closed by deciding on the RESOURCE instead of on the path. This one
/// is not a spelling: `R/sub` really is a different resource from `R`, so every
/// alias-proof identity test in the tree answered "no incumbent" correctly and
/// admitted the start anyway. MEASURED over the socket against a live minified
/// cell, EIGHT shapes got in on that answer -- a configuration root nested in
/// the cell's root (absent, and the cell's own `projects/`), a cwd standing ON
/// the cell's configuration root, a cwd inside it, a minified applicant's own
/// private root nested inside it, a cwd inside the cell's workspace, a
/// configuration root that was an ANCESTOR of the cell's, and `HOME` redirected
/// so the delivered root landed at `<cell root>/.claude`. The cell's own root
/// ended up holding the intruder's transcript, `.claude.json` and
/// `settings.json`.
///
/// The invariant is therefore not "no other session may bind the same
/// directory" but: **no directory a live minified cell binds may be reachable
/// by any other session, in any role, at any depth**. That is a CONTAINMENT
/// question, and it is asked with the containment predicate that already
/// existed -- [`one_directory_contains_the_other`], inode-keyed, walking
/// ancestors in both directions -- rather than a second one written here.
///
/// REACHABLE, AND THAT WORD IS LOAD-BEARING. LEAK 8 was this rule asking the
/// right relation about the wrong path: the walk was lexical over the spelling
/// each side was written with, so a symlink component was resolved for
/// IDENTITY and then walked for ANCESTRY as though it were a real directory,
/// and a spelling that reached a strict descendant of a live cell's root
/// overlapped nothing the walk could see. MEASURED over the socket: a plain
/// `environment.set["CLAUDE_CONFIG_DIR"]` naming a symlink to the cell's own
/// `projects/` was ADMITTED and the intruder's transcript landed inside the
/// cell. The predicate now walks what the child will REACH; nothing here had
/// to change, which is the return on there being one implementation.
///
/// The full cross-product, asked once: every directory the claim binds
/// ([`LiveResourceClaim::directories`]) against this one directory of the
/// applicant, in BOTH containment directions. `admit_bound_resources` asks it
/// for each of the applicant's own directories, so all four pairs and both
/// directions are covered.
///
/// SYMMETRIC IN THE CELL, and both halves are load-bearing:
///
/// * A live MINIFIED claim answers on containment whatever the applicant is,
///   because the cell's directories are the thing being protected.
/// * A MINIFIED APPLICANT gets containment against every live claim including
///   ordinary ones, because a private root nested inside a live ordinary
///   session's workspace is the same leak arriving a second later: the moment
///   it is admitted it is a live cell whose root that session's file tools sit
///   on top of.
///
/// ORDINARY-versus-ORDINARY STAYS IDENTITY, and that is not timidity. Two
/// ordinary sessions sharing a configuration root is a supported shape that
/// yields `SeedDisposition::VerifyOnly`, and NESTING is the ordinary shape of a
/// filesystem: one session working in `~/work` and another in `~/work/crate`,
/// or an isolated root at `~/.claude-alt` under a session whose cwd is `~`.
/// Widening this arm to containment would refuse the first and, through the
/// disposition, stop pmux seeding the second -- a large, silent regression for
/// Path A bought for no Path B benefit, since neither session is making the
/// claim the containment rule exists to protect. It is also why role is
/// compared here and nowhere else: "is this configuration root taken" must be
/// answered by other sessions' configuration roots.
fn claim_reaches(
    claim: &LiveResourceClaim,
    role: &'static str,
    path: &Path,
    applicant: SessionCell,
) -> bool {
    let cell_is_involved =
        claim.cell == SessionCell::Minified || applicant == SessionCell::Minified;
    claim
        .directories()
        .into_iter()
        .any(|(claimed_role, directory)| {
            if cell_is_involved {
                one_directory_contains_the_other(directory, path)
            } else {
                claimed_role == role && must_treat_as_same_directory(directory, path)
            }
        })
}

/// One incumbent answer for a resource several sessions may hold: the strictest
/// claim wins, because admitting against the most permissive holder of a shared
/// resource would let one ordinary session standing on a minified cell's root
/// vouch for the next applicant.
fn strictest_cell(cells: impl Iterator<Item = SessionCell>) -> Option<SessionCell> {
    cells.reduce(|left, right| {
        if left == SessionCell::Minified || right == SessionCell::Minified {
            SessionCell::Minified
        } else {
            SessionCell::Full
        }
    })
}

/// Admits one start against the configuration root it actually resolves to, and
/// reports how that root may be seeded.
///
/// Stated about the INCUMBENT, not about the applicant. Every earlier form of
/// this rule asked what the REQUEST looked like -- "does it say
/// `cell: minified`?", "does it carry a `config_isolation` block?" -- and each
/// one was open to the next entry path that reached the same directory in a
/// different shape. MEASURED over the real socket: a start carrying
/// `environment.set["CLAUDE_CONFIG_DIR"] = <a live minified cell's root>` and no
/// `config_isolation` at all was ADMITTED, its child wrote into the cell's own
/// `projects/`, and the cell's prompt was readable from inside that root; so was
/// an ordinary cell naming the same live root explicitly. Neither request looked
/// like the shape the old rule guarded, and both delivered the same directory.
///
/// `effective_config_root` computes the root the child is actually launched
/// with, for every shape, so keying on it is what makes the rule closed under
/// entry paths that do not exist yet.
///
/// A minified cell takes both extra rules. A root already bound to any live
/// session is, for this cell, the thing being forbidden rather than a race to
/// avoid; and the root must additionally be one nothing has ever run in, because
/// `history.jsonl`, `paste-cache/` and `projects/` are per-ROOT and the cell's
/// whole claim is that it carries nothing from the caller before it. Both refuse
/// before any write and before any child exists.
///
/// THE MESSAGES ARE THE RULE, WRITTEN OUT. The first of them promised
/// containment -- "no other session may be launched **under it**" -- for six
/// leaks while [`claim_reaches`] decided on identity, and leak 7 is exactly the
/// gap between the two. Under the governing rule it was the code that moved;
/// these now say "is, contains, or lies under", which is the relation
/// [`claim_reaches`] actually decides and no more.
///
/// They name the APPLICANT's directory and never the incumbent's. A refused
/// caller learns that something is in the way, not where a live cell's private
/// root is: the message goes to the party that just tried to reach it.
fn admit_config_root(
    root: &Path,
    applicant: SessionCell,
    incumbent: Option<SessionCell>,
) -> Result<SeedDisposition, ErrorBody> {
    if incumbent == Some(SessionCell::Minified) {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            format!(
                "configuration root {} is, contains, or lies under a directory bound to a live minified cell, so no other session may be launched at it or anywhere under it, whatever cell or isolation shape it asks for",
                root.display()
            ),
        ));
    }
    if applicant == SessionCell::Minified {
        if incumbent.is_some() {
            return Err(ErrorBody::new(
                ErrorCode::InvalidConfig,
                format!(
                    "a minified cell requires a configuration root of its own, and {} is, contains, or lies under a directory already bound to a live session",
                    root.display()
                ),
            ));
        }
        crate::config_isolation::require_pristine_root_for_minified_cell(root)
            .map_err(|error| ErrorBody::new(ErrorCode::InvalidConfig, format!("{error:#}")))?;
    }
    Ok(if incumbent.is_some() {
        SeedDisposition::VerifyOnly
    } else {
        SeedDisposition::Write
    })
}

/// Admits one start against the working directory it actually resolves to.
///
/// Sessions are keyed by `SessionId` and nothing else, so two live sessions
/// sharing a cwd is representable and was reachable. For a minified cell it is
/// not admissible: the cwd is the transcript project slug AND the default
/// history-recall scope -- the same per-project channel that made
/// `history.jsonl` dangerous -- and it is a directory both instances' tools read
/// and write. A cell whose claim is that nothing distinguishes it from any other
/// instance cannot share the one directory its work is done in.
///
/// Both directions, for the reason `admit_config_root` is written the way it is:
/// "refuse a minified applicant whose cwd is taken" would still admit an
/// ordinary session into a live minified cell's cwd, which is the same leak
/// arriving from the other side.
///
/// The incumbent this consults is the containment answer, not the identity one,
/// and the OTHER ROLE is what makes that necessary rather than merely tidy:
/// leak 7's third and fourth shapes were an intruder whose cwd stood on -- and
/// then inside -- a live cell's CONFIGURATION ROOT, which the old
/// cwd-against-cwd comparison could never have seen however many spellings it
/// was taught. A cwd is where the transcript slug comes from and where the file
/// tools work; a cell's configuration root is not a workspace for anyone.
fn admit_cwd(
    cwd: &Path,
    applicant: SessionCell,
    incumbent: Option<SessionCell>,
) -> Result<(), ErrorBody> {
    if incumbent == Some(SessionCell::Minified) {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            format!(
                "working directory {} is, contains, or lies under a directory bound to a live minified cell and may not be shared with another session",
                cwd.display()
            ),
        ));
    }
    if applicant == SessionCell::Minified && incumbent.is_some() {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            format!(
                "a minified cell requires a working directory of its own, and {} is, contains, or lies under a directory already bound to a live session",
                cwd.display()
            ),
        ));
    }
    Ok(())
}

fn effective_config_root(resolved: &ResolvedClaudeLaunch) -> Result<PathBuf, ErrorBody> {
    let environment = &resolved.process.environment.variables;
    let root = environment
        .get("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            environment
                .get("HOME")
                .map(|home| Path::new(home).join(".claude"))
        })
        .ok_or_else(|| {
            ErrorBody::new(
                ErrorCode::InvalidConfig,
                "effective environment has neither CLAUDE_CONFIG_DIR nor HOME",
            )
        })?;
    if !root.is_absolute() {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            "effective Claude configuration root must be absolute",
        ));
    }
    // LEAK 5b, refused as a REQUEST rule rather than repaired as a path.
    //
    // Every sibling vector reaches this same directory through
    // `Path::canonicalize`, which fails on a path that does not exist and
    // returns one carrying no `..` when it succeeds. The plain
    // `environment.set["CLAUDE_CONFIG_DIR"]` door is the one that does not, and
    // a `..` is the one lexical construct whose meaning depends on what exists
    // and on whether the component before it is a symlink. pmux therefore
    // requires the root to be SPELLED without one instead of deciding what one
    // would mean:
    //
    // * Collapsing `..` lexically is not the kernel's rule -- with `b` a
    //   symlink, `a/b/..` is `b`'s target's parent, not `a` -- so a fix that
    //   collapsed and trusted would be wrong in the direction that leaks.
    // * Resolving `..` against the filesystem is what `metadata` already does,
    //   and a `NotFound` from it is not evidence, because `mkdir -p` creates
    //   the missing intermediate and then `..` resolves -- which is what
    //   Claude's own bootstrap does, and how the intruder's transcript landed
    //   inside a live minified cell's root.
    // * Refusing costs one spelling of a directory the caller can also spell
    //   without a `..`. That is the cheap side of the trade.
    //
    // Stated on the EFFECTIVE root, so it covers every shape that produces one:
    // an explicit `set`, an inherited snapshot value, and a `HOME`-derived
    // default. `DirectoryIdentity::of` fails closed on the same family
    // independently, so deleting this rule does not reopen the leak -- it only
    // replaces a specific message with a general one.
    if traverses_a_parent_component(&root) {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            format!(
                "effective Claude configuration root must be spelled without a `..` component, because what one names depends on what exists and on whether the component before it is a symlink: {}",
                root.display()
            ),
        ));
    }
    Ok(root)
}

fn validate_transcript_identity(
    identity: &SessionIdentity,
    resolved: &ResolvedClaudeLaunch,
    config_root: &Path,
) -> Result<(), ErrorBody> {
    let locator = TranscriptLocator::new(
        config_root,
        &resolved.process.cwd,
        resolved.session_id.to_string(),
    )
    .map_err(map_location_error)?;
    match identity {
        SessionIdentity::New { .. } => {
            let collisions = locator
                .existing_session_files()
                .map_err(map_location_error)?;
            if collisions.is_empty() {
                Ok(())
            } else {
                Err(ErrorBody::new(
                    ErrorCode::IdCollision,
                    format!(
                        "Claude session {} already has {} transcript file(s) beneath the effective configuration root",
                        resolved.session_id,
                        collisions.len()
                    ),
                ))
            }
        }
        SessionIdentity::Resume { .. } => match locator.locate() {
            Ok(_) => Ok(()),
            Err(TranscriptLocationError::NotFound { .. }) => Err(ErrorBody::new(
                ErrorCode::TranscriptUnavailable,
                format!(
                    "no validated transcript exists for resumed session {}",
                    resolved.session_id
                ),
            )),
            Err(error) => Err(map_location_error(error)),
        },
    }
}

async fn detect_claude_version(
    resolved: &ResolvedClaudeLaunch,
    timeout: Duration,
) -> Result<String, ErrorBody> {
    claude_version_of(
        &resolved.process.executable,
        &resolved.process.cwd,
        &resolved.process.environment,
        timeout,
    )
    .await
}

/// Ask one Claude executable its version, exactly the way a launch asks it.
///
/// Split out of [`detect_claude_version`] rather than copied beside it: the
/// health tree asks the pool's Claude the same question a mint asks, and a
/// second spawn site is a second answer -- different argv, a different
/// environment shape, or a different notion of what "did not report a version"
/// means. `env_clear` plus an exact map is the whole of that shape, and it is
/// stated once.
async fn claude_version_of(
    executable: &Path,
    cwd: &Path,
    environment: &pseudomux_rmux::EnvironmentSnapshot,
    timeout: Duration,
) -> Result<String, ErrorBody> {
    let mut command = Command::new(executable);
    command
        .arg("--version")
        .current_dir(cwd)
        .env_clear()
        .envs(&environment.variables)
        .kill_on_drop(true);
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| {
            ErrorBody::new(
                ErrorCode::ClaudeNotFound,
                "timed out while querying Claude Code version",
            )
        })?
        .map_err(|error| ErrorBody::new(ErrorCode::ClaudeNotFound, error.to_string()))?;
    if !output.status.success() {
        return Err(ErrorBody::new(
            ErrorCode::ClaudeNotFound,
            format!("Claude version query exited with {}", output.status),
        ));
    }
    let stdout = String::from_utf8(output.stdout).map_err(|_| {
        ErrorBody::new(
            ErrorCode::UnsupportedClaudeVersion,
            "Claude version output is not UTF-8",
        )
    })?;
    normalize_claude_version(&stdout).ok_or_else(|| {
        ErrorBody::new(
            ErrorCode::UnsupportedClaudeVersion,
            "Claude version output did not contain a semantic version",
        )
        .with_details(json!({ "output_bytes": diagnostic_usize(stdout.len()) }))
    })
}

fn normalize_claude_version(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .map(|token| {
            token.trim_matches(|character: char| {
                !character.is_ascii_alphanumeric() && character != '.'
            })
        })
        .find(|token| {
            let mut parts = token.split('.');
            matches!(
                (parts.next(), parts.next(), parts.next(), parts.next()),
                (Some(major), Some(minor), Some(patch), None)
                    if !major.is_empty()
                        && major.chars().all(|c| c.is_ascii_digit())
                        && minor.chars().all(|c| c.is_ascii_digit())
                        && patch.chars().all(|c| c.is_ascii_digit())
            )
        })
        .map(ToOwned::to_owned)
}

fn map_launch_error(error: anyhow::Error) -> ErrorBody {
    let message = error.to_string();
    let code = if message.contains("Claude executable") && message.contains("unavailable") {
        ErrorCode::ClaudeNotFound
    } else {
        ErrorCode::InvalidConfig
    };
    ErrorBody::new(code, message)
}

fn map_location_error(error: TranscriptLocationError) -> ErrorBody {
    let code = match error {
        TranscriptLocationError::RelativeConfigRoot(_)
        | TranscriptLocationError::InvalidCwd { .. } => ErrorCode::InvalidConfig,
        TranscriptLocationError::NotFound { .. } | TranscriptLocationError::Io { .. } => {
            ErrorCode::TranscriptUnavailable
        }
        TranscriptLocationError::Ambiguous { .. } | TranscriptLocationError::ScanLimit => {
            ErrorCode::SchemaDrift
        }
    };
    ErrorBody::new(code, error.to_string())
}

fn map_attach_grant_error(error: anyhow::Error) -> ErrorBody {
    let code = error
        .downcast_ref::<crate::attach::AttachTimeError>()
        .map_or(ErrorCode::RmuxUnavailable, |error| error.protocol_code());
    ErrorBody::new(code, error.to_string())
}

pub(crate) fn unix_now_ms() -> Result<u64, ErrorBody> {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| {
        ErrorBody::new(
            ErrorCode::RecoveryFailed,
            "current time precedes the Unix epoch",
        )
    })?;
    checked_protocol_timestamp_ms(elapsed.as_millis())
}

fn checked_protocol_timestamp_ms(milliseconds: u128) -> Result<u64, ErrorBody> {
    u64::try_from(milliseconds)
        .ok()
        .filter(|value| *value <= MAX_SAFE_JSON_INTEGER)
        .ok_or_else(|| {
            ErrorBody::new(
                ErrorCode::RecoveryFailed,
                "current time is outside protocol-v1's safe timestamp domain",
            )
        })
}

fn checked_default_deadline_ms(now_ms: u64, timeout_ms: u64) -> Result<u64, ErrorBody> {
    now_ms
        .checked_add(timeout_ms)
        .filter(|deadline| *deadline <= MAX_SAFE_JSON_INTEGER)
        .ok_or_else(|| {
            ErrorBody::new(
                ErrorCode::InvalidConfig,
                "default turn deadline is outside protocol-v1's safe timestamp domain",
            )
        })
}

fn diagnostic_u64(value: u64) -> serde_json::Value {
    if value <= MAX_SAFE_JSON_INTEGER {
        value.into()
    } else {
        value.to_string().into()
    }
}

fn diagnostic_usize(value: usize) -> serde_json::Value {
    u64::try_from(value).map_or_else(|_| value.to_string().into(), diagnostic_u64)
}

fn diagnostic_offset_from_bottom(length: usize, index: usize) -> Option<serde_json::Value> {
    index
        .checked_add(1)
        .and_then(|consumed| length.checked_sub(consumed))
        .map(diagnostic_usize)
}

trait DriverFailureExt {
    fn protocol(self) -> ErrorBody;
}

impl DriverFailureExt for crate::v1::DriverFailure {
    fn protocol(self) -> ErrorBody {
        self.into_protocol()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use pseudomux_protocol::v1::{
        ClaudeLaunchConfig, ClosePolicy, CompatibilityPolicy, CompatibilityReport, InputTransport,
        InspectSessionRequest, NeedsInputKind, ProbeOutcome, SessionState, SystemPromptPolicy,
        TerminalProfile,
    };
    use pseudomux_rmux::{
        BackendSessionRef, EnvironmentSnapshot, LaunchSpec, TerminalBackendError, TerminalSnapshot,
    };
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use tokio::sync::Notify;

    use crate::v1::{
        DriverFailure, DriverResult, InterruptRecovery, TerminalEvidence,
        TerminalScreenObservation, TranscriptArm, TranscriptBatch, TranscriptPosition,
        TranscriptSource,
    };

    /// Every guard that needs a whole `NativeService` and no live rmux.
    ///
    /// A child module rather than more of this one, and in its own file
    /// (`native/tests/seam.rs`) rather than inline: it is one body of work with
    /// one precondition -- `crate::runtime::SessionRuntime`, the seam that made
    /// a service constructible without a sidecar -- and its tests are keyed to
    /// the mutation-survivor rows that seam exists to close. It sees everything
    /// here, including the doubles above, through `use super::*`.
    #[cfg(unix)]
    mod seam;

    struct DropProbe(Arc<AtomicBool>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// A version one patch above `version`, so a test can name "the Claude
    /// beside the promoted one" without writing a second literal that stops
    /// being adjacent the day the promoted set moves.
    fn one_patch_above(version: &str) -> String {
        shift_patch(version, 1)
    }

    /// The other side of the same idea. A range has two ends, and the end that
    /// must NOT open is the one below the floor: sec.3.1 measured 2.1.201 and
    /// earlier at zero reachable `cli` arrivals, so everything below the floor
    /// is unestablished rather than safe.
    fn one_patch_below(version: &str) -> String {
        shift_patch(version, -1)
    }

    fn shift_patch(version: &str, by: i64) -> String {
        let (prefix, patch) = version
            .rsplit_once('.')
            .unwrap_or_else(|| panic!("{version} is not a three-part version"));
        let patch: i64 = patch
            .parse()
            .unwrap_or_else(|_| panic!("{version} has a non-numeric patch"));
        format!("{prefix}.{}", patch + by)
    }

    /// The admission a first-time user gets: **no `--tested-claude-profile` on
    /// argv at all**, so the promoted set is the whole of it.
    ///
    /// This is the check `pmux doctor` did not make. On the promotion host it
    /// was Claude Code 2.1.223 against a sole promoted 2.1.220, and the daemon
    /// reported `healthy` while holding both numbers.
    ///
    /// The refused version is DERIVED from the promoted range rather than
    /// written down: `2.1.223` as a literal stopped being the
    /// adjacent-and-refused case the moment the range reached 2.1.226, and a
    /// test that then still passes is testing nothing. Every version INSIDE the
    /// range is asserted admitted too, because a range is the new thing here
    /// and a test that only checked its endpoints would not notice a
    /// containment predicate that admits nothing between them. The
    /// zero-promoted-cells case is asserted as well: on a platform pmux has
    /// promoted nothing for -- Linux today -- the loop below is empty and a
    /// vacuous pass is exactly what this file keeps finding.
    #[test]
    fn a_version_no_promoted_cell_names_is_refused_and_the_refusal_says_what_to_do() {
        let registry = CompatibilityProfileRegistry::default();
        let promoted_here: Vec<&crate::compatibility::PromotedProfile> =
            crate::compatibility::PROMOTED_PROFILES
                .iter()
                .filter(|promoted| {
                    promoted.os == std::env::consts::OS && promoted.arch == std::env::consts::ARCH
                })
                .collect();
        assert_eq!(
            promoted_here.len(),
            CompatibilityProfileRegistry::promoted_here(),
            "the filter here and `promoted_here` must be counting the same cells"
        );

        for promoted in &promoted_here {
            let range = promoted.version_range();
            assert!(
                range.floor < range.tested_through,
                "{range} admits one version, so the range key buys nothing and every patch \
                 release still costs a promotion"
            );
            for patch in range.floor.patch..=range.tested_through.patch {
                let inside = format!("{}.{}.{patch}", range.floor.major, range.floor.minor);
                assert_eq!(
                    admit_claude_version(&registry, inside.clone(), 2_000),
                    PoolClaudeAdmission::Admitted {
                        version: inside.clone(),
                    },
                    "a promoted range must admit {inside}, which is inside {range}"
                );
            }

            for outside in [
                one_patch_above(promoted.claude_version_tested_through),
                one_patch_below(promoted.claude_version_floor),
                format!("{}.{}.0", range.floor.major, range.floor.minor + 1),
            ] {
                let PoolClaudeAdmission::Refused { version, refusal } =
                    admit_claude_version(&registry, outside.clone(), 2_000)
                else {
                    panic!("Claude {outside} is outside {range} and must be refused");
                };
                assert_eq!(version, outside);
                assert!(
                    refusal.contains(&outside),
                    "the refusal must name the version it refused: {refusal}"
                );
            }
        }

        // A platform with no promoted cell refuses everything, which is the
        // other half of the same claim and the one that keeps the loop honest.
        if promoted_here.is_empty() {
            assert!(
                matches!(
                    admit_claude_version(&registry, "2.1.220".to_owned(), 2_000),
                    PoolClaudeAdmission::Refused { .. }
                ),
                "with nothing promoted for this platform, no version is admissible"
            );
        }

        // ...and an operator who measures their own host is admitted through
        // the same function, so the fault is never "you did not use pmux's
        // number".
        let mut operator = CompatibilityProfileRegistry::default();
        operator
            .insert(crate::compatibility::TestedCompatibilityProfile {
                claude_version: "9.9.9".to_owned(),
                claude_version_tested_through: None,
                os: std::env::consts::OS.to_owned(),
                arch: std::env::consts::ARCH.to_owned(),
                terminal_profile: crate::stateless::POOL_TERMINAL.profile,
                input_transport: crate::stateless::POOL_TERMINAL.input_transport,
                transcript_drain_ms: 50,
            })
            .expect("an operator cell for this platform is admissible");
        assert_eq!(
            admit_claude_version(&operator, "9.9.9".to_owned(), 2_000),
            PoolClaudeAdmission::Admitted {
                version: "9.9.9".to_owned(),
            },
            "`--tested-claude-profile` is the escape hatch the refusal names, so it has to work"
        );
    }

    /// The three answers `NativeService::admit_pool_claude` can return.
    ///
    /// Both the admitted and the refused one are DERIVED from the promoted
    /// range and then produced by `admit_claude_version` itself, not written
    /// out. The literals they replace were `2.1.220` admitted and `2.1.223`
    /// refused -- the exact pairing `pmux doctor` reported `healthy` for one
    /// command before `pmux ask` was refused -- and `2.1.223` stopped being a
    /// refusal the moment the range reached 2.1.226. A fixture that keeps
    /// naming the old answer is a health test asserting against a refusal the
    /// daemon no longer issues.
    ///
    /// On a platform with nothing promoted -- Linux today -- there is no
    /// admitted version to derive, so the fixture falls back to a version that
    /// is refused there too and the tests that use it still describe a real
    /// answer.
    fn promoted_here_for_fixtures() -> Option<&'static crate::compatibility::PromotedProfile> {
        crate::compatibility::PROMOTED_PROFILES
            .iter()
            .find(|promoted| {
                promoted.os == std::env::consts::OS && promoted.arch == std::env::consts::ARCH
            })
    }

    fn admitted_pool_claude() -> PoolClaudeAdmission {
        let version = promoted_here_for_fixtures().map_or_else(
            || "2.1.220".to_owned(),
            |promoted| promoted.claude_version_floor.to_owned(),
        );
        PoolClaudeAdmission::Admitted { version }
    }

    fn refused_pool_claude() -> PoolClaudeAdmission {
        let version = promoted_here_for_fixtures().map_or_else(
            || "2.1.220".to_owned(),
            |promoted| one_patch_above(promoted.claude_version_tested_through),
        );
        let PoolClaudeAdmission::Refused { version, refusal } = admit_claude_version(
            &CompatibilityProfileRegistry::default(),
            version.clone(),
            2_000,
        ) else {
            panic!("{version} is outside every promoted range and must be refused");
        };
        PoolClaudeAdmission::Refused { version, refusal }
    }

    fn unreadable_pool_claude() -> PoolClaudeAdmission {
        PoolClaudeAdmission::Unreadable {
            executable: PathBuf::from("/usr/local/bin/claude"),
            error: "timed out while querying Claude Code version".to_owned(),
        }
    }

    #[cfg(unix)]
    fn lifecycle_with_probe(
        dropped: Arc<AtomicBool>,
        tasks: &Arc<TrackedTasks>,
    ) -> SessionLifecycle {
        let (shutdown, shutdown_requested) = oneshot::channel();
        let task_permit = tasks.track();
        let task = tokio::spawn(async move {
            let probe = DropProbe(dropped);
            let _ = shutdown_requested.await;
            drop(probe);
            drop(task_permit);
        });
        SessionLifecycle {
            shutdown: Some(shutdown),
            task: Some(task),
        }
    }

    fn empty_sensitive_launch(runtime_dir: &Path, session_id: SessionId) -> SensitiveLaunchFiles {
        let mut config = ClaudeLaunchConfig {
            executable: "/bin/false".to_owned(),
            model: None,
            effort: None,
            permission_mode: None,
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            settings: Vec::new(),
            mcp_configs: Vec::new(),
            plugin_dirs: Vec::new(),
            system_prompt: SystemPromptPolicy::Default,
            extra_args: Vec::new(),
        };
        SensitiveLaunchFiles::prepare(runtime_dir, session_id, &mut config).unwrap()
    }

    struct CloseTerminal {
        reaped: AtomicBool,
        closes: AtomicUsize,
    }

    impl CloseTerminal {
        fn new(reaped: bool) -> Self {
            Self {
                reaped: AtomicBool::new(reaped),
                closes: AtomicUsize::new(0),
            }
        }

        fn set_reaped(&self, reaped: bool) {
            self.reaped.store(reaped, Ordering::SeqCst);
        }

        fn unexpected() -> DriverFailure {
            DriverFailure::new(
                ErrorCode::Internal,
                "unexpected terminal operation in close composition test",
            )
        }
    }

    #[async_trait]
    impl TerminalControl for CloseTerminal {
        async fn submit_prompt(
            &self,
            _session_id: SessionId,
            _turn_id: pseudomux_protocol::v1::TurnId,
            _prompt: &str,
            _deadline_unix_ms: u64,
        ) -> DriverResult<()> {
            Err(Self::unexpected())
        }

        async fn completion_evidence(
            &self,
            _session_id: SessionId,
            _turn_id: pseudomux_protocol::v1::TurnId,
        ) -> DriverResult<TerminalEvidence> {
            Err(Self::unexpected())
        }

        /// Refuses, like every other method on this double.
        ///
        /// It used to answer `Other`, which was the one permissive answer in a
        /// double whose whole design is that an unscripted call is a test
        /// asking for something it did not set up. `CloseTerminal` is a close
        /// path; nothing on a close path reads the screen, and if something
        /// starts to, this refusal says so by name rather than handing it a
        /// screen nobody chose.
        async fn observe_screen(
            &self,
            _session_id: SessionId,
        ) -> DriverResult<TerminalScreenObservation> {
            Err(Self::unexpected())
        }

        async fn interrupt(
            &self,
            _session_id: SessionId,
            _turn_id: pseudomux_protocol::v1::TurnId,
        ) -> DriverResult<InterruptRecovery> {
            Err(Self::unexpected())
        }

        async fn close(&self, _session_id: SessionId, _policy: ClosePolicy) -> DriverResult<bool> {
            self.closes.fetch_add(1, Ordering::SeqCst);
            Ok(self.reaped.load(Ordering::SeqCst))
        }
    }

    struct CloseTranscript;

    #[async_trait]
    impl TranscriptSource for CloseTranscript {
        async fn arm_at_eof(&self, _session_id: SessionId) -> DriverResult<TranscriptArm> {
            Ok(TranscriptArm::default())
        }

        async fn poll(
            &self,
            _session_id: SessionId,
            position: &TranscriptPosition,
        ) -> DriverResult<TranscriptBatch> {
            Ok(TranscriptBatch {
                position: position.clone(),
                ..TranscriptBatch::default()
            })
        }
    }

    fn close_registration(
        session_id: SessionId,
        generation_id: SessionGenerationId,
        terminal: Arc<CloseTerminal>,
    ) -> SessionRegistration {
        SessionRegistration {
            agent: None,
            session_id,
            generation_id,
            owner: SessionOwner::Caller,
            cwd: "/close-composition".to_owned(),
            compatibility: CompatibilityReport {
                claude_version: "9.9.9".to_owned(),
                os: std::env::consts::OS.to_owned(),
                arch: std::env::consts::ARCH.to_owned(),
                terminal_profile: TerminalProfile::Transparent,
                input_transport: InputTransport::Sdk,
                tested: true,
                transcript_drain_ms: 1,
            },
            dangerous_permission_bypass: false,
            resumable: true,
            cell: SessionCell::Full,
            idle_ttl_ms: None,
            initial_needs_input: None,
            terminal,
            transcript: Arc::new(CloseTranscript),
        }
    }

    struct FakeStartupTerminal {
        reference: BackendSessionRef,
        screens: StdMutex<VecDeque<TerminalSnapshot>>,
        fallback: TerminalSnapshot,
        snapshot_delay: Duration,
        close_outcomes: StdMutex<VecDeque<FakeCloseOutcome>>,
        close_delay: Duration,
        /// Incremented only *after* the close has run to completion, so a test
        /// can tell "the close was driven to the end" apart from "the close was
        /// merely started".
        closes_completed: Arc<AtomicUsize>,
    }

    enum FakeCloseOutcome {
        Reaped(bool),
        Error,
        /// Aborts the close task the way a real panic inside
        /// `RmuxTerminal::close` would: the terminal is never returned, so its
        /// owner is left in [`PendingStartupTerminal::Lost`].
        Panic,
    }

    fn legacy_startup_snapshot(visible_text: &'static str) -> TerminalSnapshot {
        TerminalSnapshot {
            revision: 1,
            rows: visible_text
                .split('\n')
                .count()
                .try_into()
                .unwrap_or(u16::MAX),
            cols: visible_text
                .split('\n')
                .map(|line| line.chars().count())
                .max()
                .unwrap_or_default()
                .try_into()
                .unwrap_or(u16::MAX),
            cursor: None,
            visible_text: visible_text.to_owned(),
        }
    }

    fn structured_startup_snapshot(
        revision: u64,
        lines: impl IntoIterator<Item = &'static str>,
        cursor_row: u16,
        cursor_col: u16,
        cursor_visible: bool,
    ) -> TerminalSnapshot {
        let lines: Vec<_> = lines.into_iter().collect();
        TerminalSnapshot {
            revision,
            rows: lines.len().try_into().unwrap(),
            cols: 80,
            cursor: Some(pseudomux_rmux::TerminalCursor {
                row: cursor_row,
                col: cursor_col,
                visible: cursor_visible,
                style: 0,
            }),
            visible_text: lines.join("\n"),
        }
    }

    impl FakeStartupTerminal {
        fn new(screens: impl IntoIterator<Item = &'static str>) -> Self {
            Self::from_snapshots(screens.into_iter().map(legacy_startup_snapshot))
        }

        fn from_snapshots(screens: impl IntoIterator<Item = TerminalSnapshot>) -> Self {
            let screens: VecDeque<_> = screens.into_iter().collect();
            let fallback = screens.back().cloned().unwrap_or(TerminalSnapshot {
                revision: 1,
                rows: 1,
                cols: 1,
                cursor: None,
                visible_text: String::new(),
            });
            Self {
                reference: BackendSessionRef {
                    rmux_session_name: "fake-startup".to_owned(),
                    pane_id: 1,
                },
                screens: StdMutex::new(screens),
                fallback,
                snapshot_delay: Duration::ZERO,
                close_outcomes: StdMutex::new(VecDeque::from([FakeCloseOutcome::Reaped(true)])),
                close_delay: Duration::ZERO,
                closes_completed: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn with_snapshot_delay(mut self, delay: Duration) -> Self {
            self.snapshot_delay = delay;
            self
        }

        fn with_close_delay(mut self, delay: Duration) -> (Self, Arc<AtomicUsize>) {
            self.close_delay = delay;
            let completed = Arc::clone(&self.closes_completed);
            (self, completed)
        }

        fn with_close_outcomes(
            screens: impl IntoIterator<Item = &'static str>,
            outcomes: impl IntoIterator<Item = FakeCloseOutcome>,
        ) -> Self {
            let mut terminal = Self::new(screens);
            terminal.close_outcomes = StdMutex::new(outcomes.into_iter().collect());
            terminal
        }

        fn unsupported() -> TerminalBackendError {
            TerminalBackendError::Rmux("unexpected fake terminal operation".to_owned())
        }
    }

    #[async_trait]
    impl TerminalSession for FakeStartupTerminal {
        fn backend_ref(&self) -> &BackendSessionRef {
            &self.reference
        }

        fn lease_lost(&self) -> bool {
            false
        }

        async fn snapshot(&self) -> Result<TerminalSnapshot, TerminalBackendError> {
            tokio::time::sleep(self.snapshot_delay).await;
            Ok(self
                .screens
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| self.fallback.clone()))
        }

        /// This double scripts plain text and has no cell colours to report.
        /// It is a startup-readiness fake and never types a control command, so
        /// saying so is the honest answer; answering "no colours here" would let
        /// a selection proof mistake absent evidence for benign evidence.
        async fn styled_screen(
            &self,
        ) -> Result<pseudomux_rmux::StyledScreen, TerminalBackendError> {
            Err(Self::unsupported())
        }

        async fn wait_visible_text(
            &self,
            _needle: &str,
            _timeout: Duration,
        ) -> Result<TerminalSnapshot, TerminalBackendError> {
            Err(Self::unsupported())
        }

        async fn wait_quiet(
            &self,
            _stable_for: Duration,
            _timeout: Duration,
        ) -> Result<TerminalSnapshot, TerminalBackendError> {
            Err(Self::unsupported())
        }

        async fn paste(&mut self, _text: &str) -> Result<(), TerminalBackendError> {
            Err(Self::unsupported())
        }

        async fn enter(&mut self) -> Result<(), TerminalBackendError> {
            Err(Self::unsupported())
        }

        async fn interrupt(&mut self) -> Result<(), TerminalBackendError> {
            Err(Self::unsupported())
        }

        async fn resize(&mut self, _rows: u16, _cols: u16) -> Result<(), TerminalBackendError> {
            Err(Self::unsupported())
        }

        async fn close(&mut self) -> Result<bool, TerminalBackendError> {
            let outcome = self
                .close_outcomes
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(FakeCloseOutcome::Reaped(true));
            tokio::time::sleep(self.close_delay).await;
            self.closes_completed.fetch_add(1, Ordering::SeqCst);
            match outcome {
                FakeCloseOutcome::Reaped(reaped) => Ok(reaped),
                FakeCloseOutcome::Error => Err(TerminalBackendError::Rmux(
                    "synthetic startup cleanup failure".to_owned(),
                )),
                FakeCloseOutcome::Panic => panic!("synthetic startup close task panic"),
            }
        }
    }

    fn resolved_with_environment(environment: &[(&str, &str)]) -> ResolvedClaudeLaunch {
        ResolvedClaudeLaunch {
            session_id: SessionId::new_v4(),
            resume: false,
            process: LaunchSpec {
                executable: PathBuf::from("/bin/false"),
                args: Vec::new(),
                cwd: PathBuf::from("/tmp"),
                environment: EnvironmentSnapshot {
                    variables: environment
                        .iter()
                        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                        .collect(),
                },
            },
            removed_environment_keys: BTreeSet::new(),
            dangerous_permission_bypass: false,
        }
    }

    #[test]
    fn version_normalization_is_strict_and_stable() {
        assert_eq!(
            normalize_claude_version("2.1.207 (Claude Code)\n"),
            Some("2.1.207".to_owned())
        );
        assert_eq!(normalize_claude_version("Claude unknown"), None);
        assert_eq!(normalize_claude_version("2.1"), None);
    }

    /// One executable that prints a version, one that exits non-zero, and one
    /// that prints nothing recognizable -- the three answers the version query
    /// can get, through the real `Command`.
    ///
    /// This is the input to `RequireTested`. Everything the compatibility
    /// registry decides rests on the string this function returns, and until
    /// now nothing called it: replacing its whole body with `Ok("xyzzy")` --
    /// or with `Ok(String::new())`, which is a version no profile can match and
    /// every `AllowUntested` cell would then run under -- left the suite green,
    /// as did deleting the `!` from its exit-status check, which admits exactly
    /// the runs that FAILED and refuses the ones that succeeded.
    #[tokio::test]
    async fn the_version_query_reads_the_child_it_actually_ran() {
        let home = tempfile::tempdir().unwrap();
        let write_probe = |name: &str, body: &str| {
            let path = home.path().join(name);
            std::fs::write(&path, body).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            path
        };
        let speaking = write_probe(
            "claude-speaking",
            "#!/bin/sh\necho '2.1.226 (Claude Code)'\n",
        );
        let failing = write_probe("claude-failing", "#!/bin/sh\necho '2.1.226'\nexit 3\n");
        let mute = write_probe("claude-mute", "#!/bin/sh\necho 'Claude Code'\n");
        let environment = pseudomux_rmux::EnvironmentSnapshot {
            variables: BTreeMap::from([("PATH".to_owned(), "/usr/bin:/bin".to_owned())]),
        };
        let timeout = Duration::from_secs(10);

        assert_eq!(
            claude_version_of(&speaking, home.path(), &environment, timeout)
                .await
                .unwrap(),
            "2.1.226",
            "the version query must report the version the child printed"
        );

        let refused = claude_version_of(&failing, home.path(), &environment, timeout)
            .await
            .expect_err("a version query that exited non-zero proved nothing");
        assert_eq!(refused.code, ErrorCode::ClaudeNotFound);

        let unparsed = claude_version_of(&mute, home.path(), &environment, timeout)
            .await
            .expect_err("output with no semantic version in it is not a version");
        assert_eq!(unparsed.code, ErrorCode::UnsupportedClaudeVersion);

        // And through the one caller a start actually goes down: `detect_claude_version`
        // is what binds a launched process to the version its compatibility cell
        // was resolved against, so a body replaced by any constant string
        // promotes an arbitrary bundle.
        let mut resolved = resolved_with_environment(&[("PATH", "/usr/bin:/bin")]);
        resolved.process.executable = speaking;
        resolved.process.cwd = home.path().to_path_buf();
        assert_eq!(
            detect_claude_version(&resolved, timeout).await.unwrap(),
            "2.1.226"
        );
    }

    /// Both diagnostic encoders keep a `usize` inside protocol v1's safe
    /// integer domain, and the offset refuses rather than wraps.
    ///
    /// `diagnostic_usize` is what `startup_screen_diagnostics` publishes its
    /// counts through; replacing it with `Default::default()` publishes `null`
    /// for every count in the refusal an operator reads. The offset's two
    /// checked steps are each the difference between an offset and a wrapped
    /// one: an index at or past the end has no offset from the bottom, and
    /// `usize::MAX` has no successor.
    #[test]
    fn the_diagnostic_encoders_stay_inside_the_safe_integer_domain() {
        assert_eq!(diagnostic_usize(0), json!(0));
        assert_eq!(diagnostic_usize(7), json!(7));
        let past_the_domain = usize::try_from(MAX_SAFE_JSON_INTEGER).unwrap() + 1;
        assert_eq!(
            diagnostic_usize(past_the_domain),
            json!(past_the_domain.to_string()),
            "a count past the safe domain is published as a string, never as a lossy number"
        );

        assert_eq!(diagnostic_offset_from_bottom(4, 0), Some(json!(3)));
        assert_eq!(diagnostic_offset_from_bottom(4, 3), Some(json!(0)));
        assert_eq!(
            diagnostic_offset_from_bottom(4, 4),
            None,
            "an index at the end is not a row, so it has no offset from the bottom"
        );
        assert_eq!(diagnostic_offset_from_bottom(4, 9), None);
        assert_eq!(
            diagnostic_offset_from_bottom(usize::MAX, usize::MAX),
            None,
            "the last representable index has no successor to consume"
        );
    }

    #[test]
    fn require_tested_fails_closed() {
        let registry = CompatibilityProfileRegistry::default();
        let error = registry
            .resolve(
                CompatibilityPolicy::RequireTested,
                "2.1.207",
                TerminalProfile::Transparent,
                InputTransport::Sdk,
                DEFAULT_UNTESTED_TRANSCRIPT_DRAIN_MS,
            )
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::UnsupportedClaudeVersion);
        let report = registry
            .resolve(
                CompatibilityPolicy::AllowUntested,
                "2.1.207",
                TerminalProfile::Transparent,
                InputTransport::Sdk,
                DEFAULT_UNTESTED_TRANSCRIPT_DRAIN_MS,
            )
            .unwrap();
        assert!(!report.tested);
    }

    #[test]
    fn public_start_reserves_one_shot_retention_for_run_once() {
        let error = validate_public_start_retention(&RetentionPolicy::OneShot).unwrap_err();
        assert_eq!(error.code, ErrorCode::UnsupportedFeature);
        assert!(
            validate_public_start_retention(&RetentionPolicy::Persistent {
                idle_ttl_ms: 30_000,
            })
            .is_ok()
        );
    }

    #[test]
    fn successful_compositions_require_confirmed_process_reaping() {
        let session_id = SessionId::new_v4();
        let generation_id = SessionGenerationId::new();
        let error = require_process_reaped(CloseSessionResult {
            session_id,
            generation_id,
            already_closed: false,
            process_reaped: false,
        })
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::RecoveryFailed);
        assert!(error.retryable);
        assert!(
            require_process_reaped(CloseSessionResult {
                session_id,
                generation_id,
                already_closed: false,
                process_reaped: true,
            })
            .is_ok()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn lifecycle_shutdown_waits_for_task_owned_resources_to_drop() {
        let tasks = Arc::new(TrackedTasks::default());
        let dropped = Arc::new(AtomicBool::new(false));
        let mut lifecycle = lifecycle_with_probe(Arc::clone(&dropped), &tasks);

        lifecycle.shutdown().await;

        assert!(dropped.load(Ordering::SeqCst));
        assert!(lifecycle.task.is_none());
        tasks.wait_idle().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminal_creation_error_waits_for_task_owned_lifecycle_cleanup() {
        let tasks = Arc::new(TrackedTasks::default());
        let dropped = Arc::new(AtomicBool::new(false));
        let mut lifecycle = lifecycle_with_probe(Arc::clone(&dropped), &tasks);
        let original = ErrorBody::new(
            ErrorCode::RmuxUnavailable,
            "synthetic terminal creation failure",
        );

        let error = match require_created_terminal(Err(original), &mut lifecycle).await {
            Ok(_) => panic!("synthetic terminal creation failure unexpectedly succeeded"),
            Err(error) => error,
        };

        assert_eq!(error.code, ErrorCode::RmuxUnavailable);
        assert!(dropped.load(Ordering::SeqCst));
        assert!(lifecycle.task.is_none());
        tasks.wait_idle().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dropped_lifecycle_requests_cleanup_that_shutdown_can_fence() {
        let tasks = Arc::new(TrackedTasks::default());
        let dropped = Arc::new(AtomicBool::new(false));
        let lifecycle = lifecycle_with_probe(Arc::clone(&dropped), &tasks);

        drop(lifecycle);
        tasks.wait_idle().await;

        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn idle_reaper_shutdown_waits_for_an_in_progress_cleanup_pass() {
        let (shutdown, shutdown_requested) = oneshot::channel();
        let (started, started_rx) = oneshot::channel();
        let release = Arc::new(Notify::new());
        let task_release = Arc::clone(&release);
        let dropped = Arc::new(AtomicBool::new(false));
        let task_dropped = Arc::clone(&dropped);
        let task = tokio::spawn(async move {
            let probe = DropProbe(task_dropped);
            let _ = started.send(());
            task_release.notified().await;
            let _ = shutdown_requested.await;
            drop(probe);
        });
        let mut reaper = IdleReaper {
            shutdown: Some(shutdown),
            task: Some(task),
        };
        started_rx.await.unwrap();

        let shutdown = tokio::spawn(async move { reaper.shutdown().await });
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        assert!(!dropped.load(Ordering::SeqCst));

        release.notify_one();
        shutdown.await.unwrap();
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_start_retains_terminal_and_lifecycle_until_confirmed_cleanup() {
        let runtime = tempfile::tempdir().unwrap();
        let session_id = SessionId::new_v4();
        let tasks = Arc::new(TrackedTasks::default());
        let dropped = Arc::new(AtomicBool::new(false));
        let lifecycle = lifecycle_with_probe(Arc::clone(&dropped), &tasks);
        let pending = Arc::new(StdMutex::new(Vec::new()));
        let guard = StartupCleanupGuard::new(
            session_id,
            Box::new(FakeStartupTerminal::new(["unpublished"])),
            empty_sensitive_launch(runtime.path(), session_id),
            lifecycle,
            Arc::clone(&pending),
        );

        drop(guard);
        assert!(!dropped.load(Ordering::SeqCst));
        let mut cleanup = pending.lock().unwrap().pop().unwrap();
        assert!(pending.lock().unwrap().is_empty());

        cleanup.close_terminal().await.unwrap();
        cleanup.shutdown().await;
        tasks.wait_idle().await;
        assert!(dropped.load(Ordering::SeqCst));
    }

    /// Startup cleanup must never abandon a close it has already started.
    ///
    /// `finish_failed_start` runs inside `start_session`, whose future is
    /// dropped whenever the requesting client goes away, and it used to `await`
    /// `TerminalSession::close()` in place. `RmuxTerminal::close` is a compound
    /// state machine over a live rmux connection, so dropping it midway leaves a
    /// kill requested, nothing confirmed, the interactive process still alive,
    /// and — because rmux-sdk treats a dropped in-flight request as a permanent
    /// transport failure — the connection that would have confirmed it
    /// destroyed. Detaching the close is what removes all of that.
    ///
    /// Two properties, and the second is the one that made this awkward. The
    /// close must run to completion despite the caller's cancellation, *and* the
    /// requeued owner must still be able to retry — so the terminal cannot
    /// simply be moved into a task and forgotten. The retry therefore adopts the
    /// close already in flight rather than issuing a second one, which is what
    /// the completion count asserts: exactly one close for two attempts.
    #[tokio::test]
    async fn a_cancelled_startup_close_still_completes_and_its_retry_adopts_it() {
        let (terminal, closes_completed) =
            FakeStartupTerminal::new(["unpublished"]).with_close_delay(Duration::from_millis(75));
        let mut cleanup =
            PendingStartupCleanup::terminal_only(SessionId::new_v4(), Box::new(terminal));

        // One poll, then drop: enough to spawn the close task and reach the
        // first suspension inside it, never enough to finish the round trip.
        let abandoned = {
            let mut close = std::pin::pin!(cleanup.close_terminal());
            std::future::poll_fn(move |context| {
                std::task::Poll::Ready(close.as_mut().poll(context))
            })
            .await
        };
        assert!(
            abandoned.is_pending(),
            "the close must still be in flight when the caller is dropped"
        );
        assert_eq!(
            closes_completed.load(Ordering::SeqCst),
            0,
            "the close cannot have completed before it was abandoned"
        );

        // The abandoned close finishes on its own task, with no caller left.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            closes_completed.load(Ordering::SeqCst),
            1,
            "an abandoned startup close must still be driven to completion"
        );

        // The requeued owner still knows where its terminal went.
        cleanup
            .close_terminal()
            .await
            .expect("the retry must observe the close the abandoned attempt delivered");
        assert_eq!(
            closes_completed.load(Ordering::SeqCst),
            1,
            "the retry must adopt the in-flight close, not issue a second kill"
        );
    }

    /// An owner that can never make progress must stop being handed to retries.
    ///
    /// `PendingStartupTerminal::Lost` is what a panicked close task leaves
    /// behind: the terminal went into the task and never came back, so every
    /// later `close_terminal` returns the same `RecoveryFailed` with
    /// `retryable: false`. The idle reaper requeues on *any* error and runs
    /// once a second, so that dead owner used to be dequeued, re-failed, and
    /// requeued for the rest of the daemon's life.
    ///
    /// Both halves matter. It must stop being retried, and it must stay owned —
    /// it still holds this session's launch material, and a panicked close is
    /// precisely the case where nothing confirmed the process was reaped, so
    /// shutdown remains the authority that releases it.
    #[tokio::test]
    async fn a_permanently_failed_startup_cleanup_is_parked_instead_of_retried_forever() {
        let mut lost = PendingStartupCleanup::terminal_only(
            SessionId::new_v4(),
            Box::new(FakeStartupTerminal::with_close_outcomes(
                ["unpublished"],
                [FakeCloseOutcome::Panic],
            )),
        );
        let error = lost
            .close_terminal()
            .await
            .expect_err("a panicked close task cannot report a confirmed reap");
        assert_eq!(error.code, ErrorCode::RecoveryFailed);
        assert!(
            !error.retryable,
            "a close task that never returned its terminal is not retryable"
        );
        assert!(lost.is_permanently_failed());

        // A healthy sibling in the same queue must still be reaped, so the
        // filter cannot degenerate into "stop at the first bad entry".
        let healthy = PendingStartupCleanup::terminal_only(
            SessionId::new_v4(),
            Box::new(FakeStartupTerminal::new(["unpublished"])),
        );
        let healthy_id = healthy.session_id;
        let pending = StdMutex::new(vec![lost, healthy]);

        let taken = take_retryable_startup_cleanup(&pending);

        assert_eq!(
            taken.len(),
            1,
            "only the owner a retry can still change may be taken"
        );
        assert_eq!(taken[0].session_id, healthy_id);
        let parked = pending.lock().unwrap();
        assert_eq!(
            parked.len(),
            1,
            "the permanently failed owner must stay owned, not be dropped"
        );
        assert!(parked[0].is_permanently_failed());

        // Idempotent: a second pass takes nothing and still parks it.
        drop(parked);
        assert!(take_retryable_startup_cleanup(&pending).is_empty());
        assert_eq!(pending.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn cancelled_terminal_delivery_is_retained_for_confirmed_cleanup() {
        let session_id = SessionId::new_v4();
        let pending = Arc::new(StdMutex::new(Vec::new()));
        let delivery = TerminalCreationDelivery {
            session_id,
            terminal: Some(Box::new(FakeStartupTerminal::new(["unpublished"]))),
            pending: Arc::clone(&pending),
        };

        drop(delivery);
        let mut cleanup = pending.lock().unwrap().pop().unwrap();
        assert!(pending.lock().unwrap().is_empty());
        cleanup.close_terminal().await.unwrap();
        cleanup.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runtime_success_drains_surviving_metadata_and_preserves_first_error() {
        let runtime = tempfile::tempdir().unwrap();
        let registry = SessionRegistry::new(SessionActorConfig::default());
        let sessions = RwLock::new(HashMap::new());
        let pending = StdMutex::new(Vec::new());
        let closed_sessions = RwLock::new(ClosedSessionTombstones::new(8));
        let lifecycle_tasks = Arc::new(TrackedTasks::default());
        let session_id = SessionId::new_v4();
        let generation_id = SessionGenerationId::new();
        registry
            .register(close_registration(
                session_id,
                generation_id,
                Arc::new(CloseTerminal::new(false)),
            ))
            .await
            .unwrap();
        let lifecycle_dropped = Arc::new(AtomicBool::new(false));
        sessions.write().await.insert(
            session_id,
            SessionMetadata {
                generation_id,
                owner: SessionOwner::Caller,
                terminal: Arc::new(RmuxTerminalControl::new(Box::new(
                    FakeStartupTerminal::new(["published"]),
                ))),
                transcript: Arc::new(
                    FileTranscriptSource::new(runtime.path(), runtime.path(), session_id).unwrap(),
                ),
                private_session_name: "surviving-private-session".to_owned(),
                cell: SessionCell::Full,
                _sensitive_launch: empty_sensitive_launch(runtime.path(), session_id),
                _lifecycle: lifecycle_with_probe(Arc::clone(&lifecycle_dropped), &lifecycle_tasks),
            },
        );
        let mut first_error = Some(
            ErrorBody::new(ErrorCode::RecoveryFailed, "original close failure").retryable(true),
        );

        drain_after_runtime_shutdown(
            &registry,
            &sessions,
            &pending,
            &closed_sessions,
            lifecycle_tasks.as_ref(),
            &mut first_error,
        )
        .await;

        let error = first_error.unwrap();
        assert_eq!(error.code, ErrorCode::RecoveryFailed);
        assert_eq!(error.message, "original close failure");
        assert!(error.retryable);
        assert!(sessions.read().await.is_empty());
        assert!(lifecycle_dropped.load(Ordering::SeqCst));
        assert!(
            closed_sessions
                .read()
                .await
                .contains(session_id, generation_id)
        );
        assert_eq!(
            registry
                .inspect(InspectSessionRequest {
                    session_id,
                    generation_id,
                })
                .await
                .unwrap_err()
                .code,
            ErrorCode::SessionNotFound
        );
    }

    #[tokio::test]
    async fn dispatch_close_retries_unconfirmed_reap_then_tombstones_without_retargeting() {
        let registry = SessionRegistry::new(SessionActorConfig::default());
        let sessions = RwLock::new(HashMap::new());
        let closed_sessions = RwLock::new(ClosedSessionTombstones::new(8));
        let start_guard = Mutex::new(());
        let session_id = SessionId::new_v4();
        let first_generation = SessionGenerationId::new();
        let first_terminal = Arc::new(CloseTerminal::new(false));
        registry
            .register(close_registration(
                session_id,
                first_generation,
                Arc::clone(&first_terminal),
            ))
            .await
            .unwrap();

        let first_error = dispatch_close_session(
            &registry,
            &sessions,
            &closed_sessions,
            &start_guard,
            CloseSessionRequest {
                session_id,
                generation_id: first_generation,
                policy: ClosePolicy::Graceful,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(first_error.code, ErrorCode::RecoveryFailed);
        assert!(first_error.retryable);
        assert_eq!(first_terminal.closes.load(Ordering::SeqCst), 1);
        assert_eq!(
            registry
                .inspect(InspectSessionRequest {
                    session_id,
                    generation_id: first_generation,
                })
                .await
                .unwrap()
                .state,
            SessionState::Closing,
            "an unconfirmed close must retain the actor for an exact retry"
        );

        first_terminal.set_reaped(true);
        let second = dispatch_close_session(
            &registry,
            &sessions,
            &closed_sessions,
            &start_guard,
            CloseSessionRequest {
                session_id,
                generation_id: first_generation,
                policy: ClosePolicy::Force,
            },
        )
        .await
        .unwrap();
        let ResponseResult::SessionClosed(second) = second else {
            panic!("close dispatch returned the wrong result variant")
        };
        assert!(second.process_reaped);
        assert!(!second.already_closed);
        assert_eq!(first_terminal.closes.load(Ordering::SeqCst), 2);

        let replacement_generation = SessionGenerationId::new();
        let replacement_terminal = Arc::new(CloseTerminal::new(true));
        registry
            .register(close_registration(
                session_id,
                replacement_generation,
                Arc::clone(&replacement_terminal),
            ))
            .await
            .unwrap();

        let duplicate = dispatch_close_session(
            &registry,
            &sessions,
            &closed_sessions,
            &start_guard,
            CloseSessionRequest {
                session_id,
                generation_id: first_generation,
                policy: ClosePolicy::Force,
            },
        )
        .await
        .unwrap();
        let ResponseResult::SessionClosed(duplicate) = duplicate else {
            panic!("duplicate close dispatch returned the wrong result variant")
        };
        assert!(duplicate.process_reaped);
        assert!(duplicate.already_closed);
        assert_eq!(replacement_terminal.closes.load(Ordering::SeqCst), 0);
        assert_eq!(
            registry
                .inspect(InspectSessionRequest {
                    session_id,
                    generation_id: replacement_generation,
                })
                .await
                .unwrap()
                .state,
            SessionState::Ready,
            "a tombstoned close for generation A must not affect replacement B"
        );
    }

    #[test]
    fn one_shot_failure_never_hides_cleanup_failure() {
        let combined = combine_turn_and_cleanup_errors(
            ErrorBody::new(ErrorCode::SchemaDrift, "turn failed"),
            ErrorBody::new(ErrorCode::RecoveryFailed, "cleanup failed").retryable(true),
        );
        assert_eq!(combined.code, ErrorCode::RecoveryFailed);
        assert!(combined.retryable);
        assert_eq!(combined.details["turn_error"]["code"], "schema_drift");
        assert_eq!(combined.details["cleanup_error"]["code"], "recovery_failed");
    }

    #[test]
    fn startup_failure_never_hides_unpublished_cleanup_failure() {
        let combined = combine_startup_and_cleanup_errors(
            ErrorBody::new(ErrorCode::NeedsInput, "startup failed"),
            ErrorBody::new(ErrorCode::RecoveryFailed, "cleanup failed").retryable(true),
        );
        assert_eq!(combined.code, ErrorCode::RecoveryFailed);
        assert!(combined.retryable);
        assert_eq!(combined.details["startup_error"]["code"], "needs_input");
        assert_eq!(combined.details["cleanup_error"]["code"], "recovery_failed");
    }

    #[test]
    fn turn_wait_guard_contains_full_cancellation_and_drain_budget() {
        assert_eq!(
            turn_wait_safety_delay(1_000, Duration::from_secs(5), 2_500).unwrap(),
            Duration::from_millis(19_500)
        );
        assert_eq!(
            turn_wait_safety_delay(0, Duration::from_secs(5), 60_000).unwrap(),
            Duration::from_secs(76)
        );
        assert!(turn_wait_safety_delay(0, Duration::MAX, 0).is_err());
    }

    #[test]
    fn service_timestamps_and_default_deadlines_fail_closed_at_safe_max() {
        assert_eq!(
            checked_protocol_timestamp_ms(u128::from(MAX_SAFE_JSON_INTEGER)).unwrap(),
            MAX_SAFE_JSON_INTEGER
        );
        assert!(checked_protocol_timestamp_ms(u128::from(MAX_SAFE_JSON_INTEGER) + 1).is_err());
        assert_eq!(
            checked_default_deadline_ms(MAX_SAFE_JSON_INTEGER - 1, 1).unwrap(),
            MAX_SAFE_JSON_INTEGER
        );
        assert!(checked_default_deadline_ms(MAX_SAFE_JSON_INTEGER, 1).is_err());
    }

    #[test]
    fn attach_time_failures_keep_their_protocol_error_class() {
        for (failure, expected) in [
            (
                crate::attach::AttachTimeError::CurrentTimeUnavailable,
                ErrorCode::RecoveryFailed,
            ),
            (
                crate::attach::AttachTimeError::TtlOutOfRange,
                ErrorCode::InvalidConfig,
            ),
            (
                crate::attach::AttachTimeError::ExpiryOutOfRange,
                ErrorCode::InvalidConfig,
            ),
        ] {
            assert_eq!(
                map_attach_grant_error(anyhow::Error::new(failure)).code,
                expected
            );
        }
        assert_eq!(
            map_attach_grant_error(anyhow::anyhow!("backend unavailable")).code,
            ErrorCode::RmuxUnavailable
        );
    }

    #[cfg(unix)]
    #[test]
    fn lifecycle_stop_sequence_never_wraps_or_exceeds_safe_max() {
        let sequence = AtomicU64::new(MAX_SAFE_JSON_INTEGER - 1);
        assert_eq!(
            increment_lifecycle_stop_sequence(&sequence),
            Some(MAX_SAFE_JSON_INTEGER)
        );
        assert_eq!(increment_lifecycle_stop_sequence(&sequence), None);
        assert_eq!(sequence.load(Ordering::Relaxed), MAX_SAFE_JSON_INTEGER);
    }

    #[cfg(unix)]
    #[test]
    fn lifecycle_stop_instant_is_a_safe_wall_clock_unix_millisecond() {
        let before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let stop_at_ms = AtomicU64::new(0);
        record_lifecycle_stop_instant(&stop_at_ms);
        let after = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();

        let stamped = stop_at_ms.load(Ordering::Acquire);
        assert_ne!(stamped, 0, "0 is reserved for 'never observed'");
        assert!(stamped <= MAX_SAFE_JSON_INTEGER);
        // Same wall-clock domain as every protocol `*_at_ms` field, so the
        // signed difference against `last_transcript_activity_at_ms` is
        // meaningful.
        assert!(u128::from(stamped) >= before && u128::from(stamped) <= after);

        // Every later Stop overwrites the previous one: the field reports the
        // most recent hook, not the first.
        record_lifecycle_stop_instant(&stop_at_ms);
        assert!(stop_at_ms.load(Ordering::Acquire) >= stamped);
    }

    #[test]
    fn native_event_subscription_bounds_fail_closed_for_direct_callers() {
        use pseudomux_protocol::v1::{
            MAX_SUBSCRIBE_EVENTS, MAX_SUBSCRIBE_WAIT_MS, SubscribeEventsRequest,
        };

        let request = |wait_ms, max_events| SubscribeEventsRequest {
            session_id: SessionId::new_v4(),
            generation_id: SessionGenerationId::new(),
            after_sequence: 0,
            wait_ms,
            max_events,
        };
        assert!(
            validate_subscribe_events(&request(MAX_SUBSCRIBE_WAIT_MS, MAX_SUBSCRIBE_EVENTS))
                .is_ok()
        );
        assert_eq!(
            validate_subscribe_events(&request(MAX_SUBSCRIBE_WAIT_MS + 1, 1))
                .unwrap_err()
                .code,
            ErrorCode::InvalidConfig
        );
        assert_eq!(
            validate_subscribe_events(&request(1, MAX_SUBSCRIBE_EVENTS + 1))
                .unwrap_err()
                .code,
            ErrorCode::InvalidConfig
        );
        let unsafe_diagnostic =
            validate_subscribe_events(&request(MAX_SAFE_JSON_INTEGER + 1, 1)).unwrap_err();
        assert_eq!(
            unsafe_diagnostic.details["wait_ms"],
            json!((MAX_SAFE_JSON_INTEGER + 1).to_string())
        );
        assert!(serde_json::to_vec(&unsafe_diagnostic).is_ok());
    }

    #[test]
    fn config_root_comes_from_effective_process_environment() {
        let custom = resolved_with_environment(&[
            ("HOME", "/home/example"),
            ("CLAUDE_CONFIG_DIR", "/private/claude"),
        ]);
        assert_eq!(
            effective_config_root(&custom).unwrap(),
            PathBuf::from("/private/claude")
        );
        let ordinary = resolved_with_environment(&[("HOME", "/home/example")]);
        assert_eq!(
            effective_config_root(&ordinary).unwrap(),
            PathBuf::from("/home/example/.claude")
        );
    }

    /// LEAK 5b, refused as a request rule on the effective root.
    ///
    /// Stated on the root the child is actually launched with, so every shape
    /// that produces one is covered by the same sentence: an explicit `set`, an
    /// inherited snapshot value, and the `HOME`-derived default. That coverage
    /// is the point -- a rule written against `environment.set` alone would be
    /// the seventh spelling-shaped rule in this family.
    #[test]
    fn an_effective_config_root_spelled_with_a_parent_component_is_refused() {
        for (label, environment) in [
            (
                "explicit CLAUDE_CONFIG_DIR",
                vec![
                    ("HOME", "/home/example"),
                    ("CLAUDE_CONFIG_DIR", "/x/NOPE/../rootA"),
                ],
            ),
            (
                "a `..` the kernel could resolve is refused just the same, \
                 because what it resolves to depends on a symlink pmux does not control",
                vec![
                    ("HOME", "/home/example"),
                    ("CLAUDE_CONFIG_DIR", "/x/rootA/../rootA"),
                ],
            ),
            (
                "HOME-derived default",
                vec![("HOME", "/home/example/NOPE/..")],
            ),
        ] {
            let error = effective_config_root(&resolved_with_environment(&environment))
                .expect_err(label)
                .message;
            assert!(
                error.contains("must be spelled without a `..` component"),
                "{label}: unexpected refusal: {error}"
            );
        }

        // The rule cannot pass by refusing everything: the ordinary spellings
        // stay admissible, including the ones `Path::components` already elides.
        for spelling in ["/private/claude", "/private/./claude", "/private//claude"] {
            let root = effective_config_root(&resolved_with_environment(&[
                ("HOME", "/home/example"),
                ("CLAUDE_CONFIG_DIR", spelling),
            ]))
            .unwrap_or_else(|error| panic!("{spelling}: {}", error.message));
            assert_eq!(root, PathBuf::from(spelling));
        }
    }

    /// LEAK 5b at the admission gate, on BOTH bound resources.
    ///
    /// The gate's first bullet reads an absence as proof that no live session
    /// holds the applicant. This is the case where that reading is false, and
    /// it is asserted with an EMPTY claim list as well as against a live cell:
    /// with no incumbent the old rule fell through to `SeedDisposition::Write`,
    /// which is how the intruder's child came to create the missing
    /// intermediate and write its transcript inside a live cell's root.
    #[cfg(unix)]
    #[test]
    fn a_dot_dot_through_a_missing_directory_is_refused_as_either_bound_resource() {
        let parent = tempfile::tempdir().unwrap();
        let held_root = owner_only_child(parent.path(), "held-root");
        let held_cwd = owner_only_child(parent.path(), "held-cwd");
        let free_root = owner_only_child(parent.path(), "free-root");
        let free_cwd = owner_only_child(parent.path(), "free-cwd");
        let claims = vec![minified_claim(&held_root, &held_cwd)];

        let spelling = |directory: &Path| {
            parent
                .path()
                .join("NOPE")
                .join("..")
                .join(directory.file_name().unwrap())
        };
        let as_root = spelling(&held_root);
        let as_cwd = spelling(&held_cwd);
        for path in [&as_root, &as_cwd] {
            assert_eq!(
                std::fs::metadata(path).unwrap_err().kind(),
                std::io::ErrorKind::NotFound,
                "the premise: {} is NotFound today",
                path.display()
            );
        }

        for (label, root, cwd) in [
            ("config root", as_root.as_path(), free_cwd.as_path()),
            ("cwd", free_root.as_path(), as_cwd.as_path()),
        ] {
            for claims in [claims.as_slice(), &[]] {
                for applicant in [SessionCell::Full, SessionCell::Minified] {
                    let error = admit_bound_resources(claims, root, cwd, applicant)
                        .expect_err(label)
                        .message;
                    assert!(
                        error.contains("cannot establish which directory it names"),
                        "{label} as {applicant:?} against {} claims: {error}",
                        claims.len()
                    );
                }
            }
        }

        // Unchanged for the shape this gate exists to admit: a fresh root
        // nothing has created, spelled without a `..`.
        let not_yet = parent.path().join("root-that-does-not-exist-yet");
        assert_eq!(
            admit_bound_resources(&claims, &not_yet, &free_cwd, SessionCell::Full).unwrap(),
            SeedDisposition::Write
        );
    }

    /// A private root reaches the transcript authority with ZERO locator,
    /// transcript-source or rebind changes -- the whole §4 claim of the config
    /// isolation design, proven from the wire field rather than argued.
    #[cfg(unix)]
    #[test]
    fn config_isolation_carries_the_private_root_all_the_way_to_the_transcript_source() {
        use std::os::unix::fs::PermissionsExt;

        let private = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let operator = tempfile::tempdir().unwrap();
        for directory in [private.path(), cwd.path()] {
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }

        let session_id = SessionId::new_v4();
        let request = pseudomux_protocol::v1::StartSessionRequest {
            agent: None,
            identity: SessionIdentity::New {
                session_id: Some(session_id),
            },
            cwd: cwd.path().to_string_lossy().into_owned(),
            claude: Some(pseudomux_protocol::v1::ClaudeLaunchConfig {
                executable: "/bin/sh".into(),
                model: None,
                effort: None,
                permission_mode: None,
                allowed_tools: Vec::new(),
                denied_tools: Vec::new(),
                settings: Vec::new(),
                mcp_configs: Vec::new(),
                plugin_dirs: Vec::new(),
                system_prompt: pseudomux_protocol::v1::SystemPromptPolicy::Default,
                extra_args: Vec::new(),
            }),
            environment: pseudomux_protocol::v1::EnvironmentSpec {
                snapshot: std::collections::BTreeMap::from([
                    (
                        "CLAUDE_CONFIG_DIR".to_owned(),
                        operator.path().to_string_lossy().into_owned(),
                    ),
                    ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
                ]),
                ..Default::default()
            },
            auth_policy: pseudomux_protocol::v1::AuthPolicy::Subscription,
            config_isolation: Some(pseudomux_protocol::v1::ConfigIsolation {
                root: private.path().to_string_lossy().into_owned(),
            }),
            terminal: pseudomux_protocol::v1::TerminalSpec::default(),
            lifecycle: pseudomux_protocol::v1::LifecycleMode::Transcript,
            retention: RetentionPolicy::OneShot,
            compatibility: pseudomux_protocol::v1::CompatibilityPolicy::AllowUntested,
            cell: SessionCell::Full,
        };

        let resolved = resolve_claude_launch(&request).unwrap();
        let config_root = effective_config_root(&resolved).unwrap();
        let canonical_private = private.path().canonicalize().unwrap();
        assert_eq!(
            config_root, canonical_private,
            "effective_config_root must resolve the injected private root, not the operator's"
        );
        assert_ne!(config_root, operator.path().canonicalize().unwrap());

        // The one collision scan a start performs now walks only pmux's own
        // root, so an empty private root is an empty collision set.
        validate_transcript_identity(&request.identity, &resolved, &config_root).unwrap();

        let transcript =
            FileTranscriptSource::new(&config_root, &resolved.process.cwd, session_id).unwrap();
        assert_eq!(
            transcript.config_root(),
            canonical_private,
            "the transcript authority's namespace must be the private root"
        );

        // Seeding the same root twice is what a pool restart does; it must be a
        // no-op, and it must never create `projects/`.
        for _ in 0..2 {
            crate::config_isolation::seed_private_config_root(
                &crate::config_isolation::ConfigRootSeed {
                    root: &config_root,
                    trusted_cwd: &resolved.process.cwd,
                    dangerous_permission_bypass: resolved.dangerous_permission_bypass,
                },
                crate::config_isolation::SeedDisposition::Write,
            )
            .unwrap();
        }
        let seeded: serde_json::Value =
            serde_json::from_slice(&std::fs::read(config_root.join(".claude.json")).unwrap())
                .unwrap();
        assert_eq!(
            seeded
                .pointer("/hasCompletedOnboarding")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert!(
            seeded["projects"]
                .as_object()
                .unwrap()
                .contains_key(resolved.process.cwd.to_str().unwrap()),
            "the trust key is the canonical cwd the child is launched with: {seeded}"
        );
        assert!(!config_root.join("projects").exists());
        assert!(
            !operator.path().join(".claude.json").exists()
                && !operator.path().join("projects").exists(),
            "an isolated start must not touch the operator's root at all"
        );
    }

    #[cfg(unix)]
    fn owner_only_dir() -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        directory
    }

    /// The same directory, spelled the way a caller-set `CLAUDE_CONFIG_DIR`
    /// reaches `effective_config_root`: unresolved, and on this platform also
    /// through the `/var -> /private/var` symlink every temp directory sits
    /// behind.
    fn unresolved_spelling(path: &Path) -> PathBuf {
        path.join("..").join(path.file_name().unwrap())
    }

    fn minified_claim(config_root: &Path, cwd: &Path) -> LiveResourceClaim {
        LiveResourceClaim {
            config_root: config_root.to_path_buf(),
            cwd: cwd.to_path_buf(),
            cell: SessionCell::Minified,
        }
    }

    fn full_claim(config_root: &Path, cwd: &Path) -> LiveResourceClaim {
        LiveResourceClaim {
            config_root: config_root.to_path_buf(),
            cwd: cwd.to_path_buf(),
            cell: SessionCell::Full,
        }
    }

    /// The leak, stated about the incumbent.
    ///
    /// MEASURED over the real socket before this rule existed: only ONE request
    /// shape was refused from a live minified cell's root -- `cell: minified`
    /// plus a `config_isolation` block naming it. A start carrying
    /// `environment.set["CLAUDE_CONFIG_DIR"] = <that root>` and no isolation at
    /// all was admitted, and so was an ordinary cell naming the root
    /// explicitly; the foreign child then wrote into the cell's own `projects/`
    /// and the cell's prompt was readable from inside the root. Nothing about
    /// the applicant separates those three; the incumbent does.
    #[cfg(unix)]
    #[test]
    fn a_root_a_live_minified_cell_holds_admits_no_other_session_in_any_shape() {
        let held = owner_only_dir();
        let free = owner_only_dir();
        let cwd = owner_only_dir();
        let claims = vec![minified_claim(held.path(), cwd.path())];

        for applicant in [SessionCell::Full, SessionCell::Minified] {
            for spelling in [held.path().to_path_buf(), unresolved_spelling(held.path())] {
                let error = admit_config_root(
                    &spelling,
                    applicant,
                    incumbent_cell_for_config_root(&claims, &spelling, applicant),
                )
                .unwrap_err();
                assert_eq!(error.code, ErrorCode::InvalidConfig);
                assert!(
                    error.message.contains("live minified cell"),
                    "{applicant:?} at {}: {}",
                    spelling.display(),
                    error.message
                );
            }
        }

        // The rule is about THIS root, not about minified cells in general.
        assert_eq!(
            admit_config_root(
                free.path(),
                SessionCell::Full,
                incumbent_cell_for_config_root(&claims, free.path(), SessionCell::Full),
            )
            .unwrap(),
            SeedDisposition::Write
        );

        // An ordinary session sharing the root cannot vouch for the applicant:
        // the strictest claim on a resource is the one that answers.
        let shared = vec![
            full_claim(held.path(), cwd.path()),
            minified_claim(held.path(), cwd.path()),
        ];
        assert_eq!(
            incumbent_cell_for_config_root(&shared, held.path(), SessionCell::Full),
            Some(SessionCell::Minified)
        );
        assert!(
            admit_config_root(
                held.path(),
                SessionCell::Full,
                incumbent_cell_for_config_root(&shared, held.path(), SessionCell::Full),
            )
            .is_err()
        );
    }

    /// The refusal that was the only thing standing between the attack and the
    /// cell, and had zero tests anywhere.
    #[cfg(unix)]
    #[test]
    fn a_minified_cell_is_refused_a_root_any_live_session_is_already_using() {
        let occupied = owner_only_dir();
        let cwd = owner_only_dir();
        let claims = vec![full_claim(occupied.path(), cwd.path())];
        let incumbent =
            incumbent_cell_for_config_root(&claims, occupied.path(), SessionCell::Minified);
        assert_eq!(incumbent, Some(SessionCell::Full));

        let error =
            admit_config_root(occupied.path(), SessionCell::Minified, incumbent).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidConfig);
        assert!(
            error
                .message
                .contains("requires a configuration root of its own"),
            "{}",
            error.message
        );

        // The same incumbent is no obstacle to an ordinary session, which only
        // loses the right to WRITE the root while another process owns it. Its
        // own lookup is asked with its own cell, because the two applicants are
        // not entitled to the same answer.
        let ordinary_incumbent =
            incumbent_cell_for_config_root(&claims, occupied.path(), SessionCell::Full);
        assert_eq!(ordinary_incumbent, Some(SessionCell::Full));
        assert_eq!(
            admit_config_root(occupied.path(), SessionCell::Full, ordinary_incumbent).unwrap(),
            SeedDisposition::VerifyOnly
        );
        assert_eq!(
            admit_config_root(occupied.path(), SessionCell::Full, None).unwrap(),
            SeedDisposition::Write
        );
    }

    /// The pristine-root call site, previously reachable only from an
    /// `#[ignore]`d end-to-end test.
    #[cfg(unix)]
    #[test]
    fn a_minified_cell_is_refused_a_root_anything_has_ever_run_in() {
        let root = owner_only_dir();
        assert_eq!(
            admit_config_root(root.path(), SessionCell::Minified, None).unwrap(),
            SeedDisposition::Write,
            "an empty root is what a minified cell is for"
        );

        // pmux's own two seed files are not use.
        for seeded in [".claude.json", "settings.json"] {
            std::fs::write(root.path().join(seeded), "{}").unwrap();
        }
        assert_eq!(
            admit_config_root(root.path(), SessionCell::Minified, None).unwrap(),
            SeedDisposition::Write
        );

        // Anything else is. `backups/` is what one stray `claude auth status`
        // leaves behind, and it carries the whole projects map.
        std::fs::create_dir(root.path().join("backups")).unwrap();
        let error = admit_config_root(root.path(), SessionCell::Minified, None).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidConfig);
        assert!(error.message.contains("backups"), "{}", error.message);

        // An ordinary cell is not making the claim this rule enforces.
        assert_eq!(
            admit_config_root(root.path(), SessionCell::Full, None).unwrap(),
            SeedDisposition::Write
        );
    }

    /// Sessions are keyed by `SessionId` alone, so two live sessions sharing one
    /// cwd is representable -- and a shared cwd is one transcript project slug,
    /// one history-recall scope, and one directory both instances' tools read
    /// and write.
    #[cfg(unix)]
    #[test]
    fn a_cwd_is_not_shared_with_a_minified_cell_in_either_direction() {
        let root = owner_only_dir();
        let occupied = owner_only_dir();
        let free = owner_only_dir();

        let ordinary = vec![full_claim(root.path(), occupied.path())];
        let error = admit_cwd(
            occupied.path(),
            SessionCell::Minified,
            incumbent_cell_for_cwd(&ordinary, occupied.path(), SessionCell::Minified),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidConfig);
        assert!(
            error
                .message
                .contains("requires a working directory of its own"),
            "{}",
            error.message
        );
        // Two ordinary sessions may still share a workspace: they make no claim
        // this would protect.
        admit_cwd(
            occupied.path(),
            SessionCell::Full,
            incumbent_cell_for_cwd(&ordinary, occupied.path(), SessionCell::Full),
        )
        .unwrap();

        let cell = vec![minified_claim(root.path(), occupied.path())];
        for applicant in [SessionCell::Full, SessionCell::Minified] {
            for spelling in [
                occupied.path().to_path_buf(),
                unresolved_spelling(occupied.path()),
            ] {
                let error = admit_cwd(
                    &spelling,
                    applicant,
                    incumbent_cell_for_cwd(&cell, &spelling, applicant),
                )
                .unwrap_err();
                assert_eq!(error.code, ErrorCode::InvalidConfig);
            }
        }

        // An unbound cwd is admitted for either cell.
        for applicant in [SessionCell::Full, SessionCell::Minified] {
            admit_cwd(
                free.path(),
                applicant,
                incumbent_cell_for_cwd(&cell, free.path(), applicant),
            )
            .unwrap();
        }
    }

    /// An owner-only directory inside a caller-owned parent, so that the
    /// aliases below -- which are spelled relative to the parent -- exist.
    #[cfg(unix)]
    fn owner_only_child(parent: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = parent.join(name);
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    /// Every spelling of one directory a caller can put on the wire.
    ///
    /// Duplicated from `claude_launch::tests` on purpose: that copy proves the
    /// identity PREDICATE, this one proves the ADMISSION rules, and a shared
    /// fixture would let one deletion silently disarm both.
    #[cfg(unix)]
    fn aliases_of(directory: &Path) -> Vec<(&'static str, PathBuf)> {
        let name = directory.file_name().unwrap().to_str().unwrap();
        let mut aliases = vec![
            ("identity", directory.to_path_buf()),
            (
                "trailing slash",
                PathBuf::from(format!("{}/", directory.display())),
            ),
            ("dot-dot traversal", directory.join("..").join(name)),
        ];
        let link = directory.with_file_name(format!("symlink-to-{name}"));
        std::os::unix::fs::symlink(directory, &link).unwrap();
        aliases.push(("symlink", link));
        #[cfg(target_os = "macos")]
        {
            // APFS firmlink. Not a symlink, so `canonicalize` returns it
            // unchanged -- the alias that defeated the string comparison.
            let canonical = directory.canonicalize().unwrap();
            let firmlink =
                Path::new("/System/Volumes/Data").join(canonical.strip_prefix("/").unwrap());
            assert!(firmlink.is_dir(), "{}", firmlink.display());
            assert_ne!(
                firmlink.canonicalize().unwrap(),
                canonical,
                "canonicalize must not collapse the firmlink alias"
            );
            aliases.push(("firmlink", firmlink));
        }
        aliases
    }

    #[cfg(unix)]
    fn inode_of(path: &Path) -> (u64, u64) {
        use std::os::unix::fs::MetadataExt;

        let metadata = std::fs::metadata(path)
            .unwrap_or_else(|error| panic!("{} must be inspectable: {error}", path.display()));
        (metadata.dev(), metadata.ino())
    }

    /// LEAK 5, over the socket's own admission rules.
    ///
    /// MEASURED against a live minified cell before this: five starts spelled
    /// through the APFS firmlink namespace -- two `CLAUDE_CONFIG_DIR` shapes, a
    /// `config_isolation` root, and two `cwd`s, across both cells -- were all
    /// ADMITTED, and one of them made pmux write into the live cell's own
    /// `settings.json`, after which the cell's secret was readable from the
    /// intruder's root through `history.jsonl`, `projects/` and `backups/`.
    ///
    /// The premise of every row is asserted as an INODE fact before the rule is
    /// asked, so an alias that stopped aliasing fails here as a broken fixture
    /// rather than passing as a rule that held.
    #[cfg(unix)]
    #[test]
    fn a_live_minified_cells_resources_are_refused_under_every_alias_of_the_same_inode() {
        let parent = tempfile::tempdir().unwrap();
        let held_root = owner_only_child(parent.path(), "held-root");
        let held_cwd = owner_only_child(parent.path(), "held-cwd");
        let free_root = owner_only_child(parent.path(), "free-root");
        let free_cwd = owner_only_child(parent.path(), "free-cwd");
        let claims = vec![minified_claim(&held_root, &held_cwd)];

        let root_truth = inode_of(&held_root);
        for (label, alias) in aliases_of(&held_root) {
            assert_eq!(
                inode_of(&alias),
                root_truth,
                "{label}: the fixture must alias the held root"
            );
            for applicant in [SessionCell::Full, SessionCell::Minified] {
                let error =
                    admit_bound_resources(&claims, &alias, &free_cwd, applicant).unwrap_err();
                assert_eq!(error.code, ErrorCode::InvalidConfig);
                assert!(
                    error.message.contains("live minified cell"),
                    "{label} as {applicant:?}: {}",
                    error.message
                );
            }
        }

        let cwd_truth = inode_of(&held_cwd);
        for (label, alias) in aliases_of(&held_cwd) {
            assert_eq!(
                inode_of(&alias),
                cwd_truth,
                "{label}: the fixture must alias the held cwd"
            );
            for applicant in [SessionCell::Full, SessionCell::Minified] {
                let error =
                    admit_bound_resources(&claims, &free_root, &alias, applicant).unwrap_err();
                assert_eq!(error.code, ErrorCode::InvalidConfig);
                assert!(
                    error.message.contains("live minified cell"),
                    "{label} as {applicant:?}: {}",
                    error.message
                );
            }
        }

        // Two directories that really are different are still admitted, so this
        // test cannot pass by refusing everything.
        assert_ne!(inode_of(&free_root), root_truth);
        assert_ne!(inode_of(&free_cwd), cwd_truth);
        assert_eq!(
            admit_bound_resources(&claims, &free_root, &free_cwd, SessionCell::Minified).unwrap(),
            SeedDisposition::Write
        );
    }

    /// The subtle half: "there is no such directory" is an answer, "I could not
    /// look" is not.
    #[cfg(unix)]
    #[test]
    fn a_root_that_does_not_exist_yet_is_admitted_and_one_that_cannot_be_inspected_is_not() {
        use std::os::unix::fs::PermissionsExt;

        let parent = tempfile::tempdir().unwrap();
        let held_root = owner_only_child(parent.path(), "held-root");
        let held_cwd = owner_only_child(parent.path(), "held-cwd");
        let free_cwd = owner_only_child(parent.path(), "free-cwd");
        let free_root = owner_only_child(parent.path(), "free-root");
        let claims = vec![minified_claim(&held_root, &held_cwd)];

        // Vacant: the ordinary shape of a first start into a root nothing has
        // created yet. Admitted, and admitted to WRITE, because a path that
        // names no directory is not a directory a live cell is running in.
        let not_yet = parent.path().join("root-that-does-not-exist-yet");
        assert_eq!(
            DirectoryIdentity::of(&not_yet),
            DirectoryIdentity::Vacant,
            "the fixture must actually be absent"
        );
        assert_eq!(
            admit_bound_resources(&claims, &not_yet, &free_cwd, SessionCell::Full).unwrap(),
            SeedDisposition::Write
        );
        assert_eq!(
            admit_bound_resources(&[], &not_yet, &free_cwd, SessionCell::Full).unwrap(),
            SeedDisposition::Write
        );

        // Unresolved: refused on BOTH resources, and refused with no live
        // session anywhere -- which is the case a rule folded into the
        // incumbent comparison would have admitted with a `Write`.
        let closed = owner_only_child(parent.path(), "closed");
        let hidden = owner_only_child(&closed, "hidden");
        std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o000)).unwrap();
        let identity = DirectoryIdentity::of(&hidden);
        let as_root = admit_bound_resources(&[], &hidden, &free_cwd, SessionCell::Full);
        let as_cwd = admit_bound_resources(&[], &free_root, &hidden, SessionCell::Full);
        std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o700)).unwrap();

        if matches!(identity, DirectoryIdentity::Resource(_)) {
            // Running as a user the mode bits do not apply to; unreachable.
            return;
        }
        assert_eq!(identity, DirectoryIdentity::Unresolved);
        for (label, outcome) in [("config root", as_root), ("cwd", as_cwd)] {
            let error = outcome.unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidConfig);
            assert!(
                error.message.contains("cannot be inspected"),
                "{label}: {}",
                error.message
            );
        }
    }

    /// LEAK 7, as the eight RELATIONS it actually was.
    ///
    /// Not one row here is a SPELLING of a directory the live cell holds, which
    /// is why six rounds of alias-proofing left every one of them admitted:
    /// `R/sub` really is a different resource from `R`, so every inode
    /// comparison in the tree answered "no incumbent" correctly, and
    /// `admit_config_root` returned `SeedDisposition::Write` against a live
    /// minified cell's private root.
    ///
    /// MEASURED over the real socket before this rule existed: all eight got
    /// in, and the victim's own root ended up holding the intruder's
    /// transcript, `.claude.json` and `settings.json`
    /// (`crates/e2e/tests/cross_cell_contamination.rs::probe_the_containment_door`
    /// is the standing form of that measurement). This test is the RELATION;
    /// that probe is the ENTRY PATH, and the two rows below that name no door --
    /// the `HOME`-derived root and the intruder's own private root -- are
    /// exactly where the two split: `effective_config_root` turns
    /// `HOME=<cell root>` into `<cell root>/.claude` before admission ever runs,
    /// so at this level it is simply a nested configuration root.
    ///
    /// Each row is refused for BOTH applicant cells, because an ordinary
    /// session reaching into a cell is the same leak as a cell reaching out.
    #[cfg(unix)]
    #[test]
    fn no_directory_a_live_minified_cell_binds_is_reachable_at_any_depth_in_any_role() {
        let parent = tempfile::tempdir().unwrap();
        let held_root = owner_only_child(parent.path(), "held-root");
        let held_cwd = owner_only_child(parent.path(), "held-cwd");
        let free_root = owner_only_child(parent.path(), "free-root");
        let free_cwd = owner_only_child(parent.path(), "free-cwd");
        // The cell's own `projects/`: the one subdirectory of a live root an
        // intruder can name without guessing anything at all.
        let projects = owner_only_child(&held_root, "projects");
        let inside_held_cwd = owner_only_child(&held_cwd, "inside");
        // A configuration root pmux would CREATE, inside a root it must not.
        let absent_child = held_root.join("not-created-yet");
        assert_eq!(
            DirectoryIdentity::of(&absent_child),
            DirectoryIdentity::Vacant,
            "the fixture must actually be absent, or this row proves nothing"
        );
        // What `effective_config_root` computes from `HOME=<the cell's root>`.
        let home_derived = held_root.join(".claude");
        let ancestor = parent.path().to_path_buf();
        let claims = vec![minified_claim(&held_root, &held_cwd)];

        let rows: [(&str, &Path, &Path); 8] = [
            (
                "config root NESTED in the live cell root (absent subdir)",
                &absent_child,
                &free_cwd,
            ),
            (
                "config root NESTED in the live cell root (existing projects/)",
                &projects,
                &free_cwd,
            ),
            (
                "config root is an ANCESTOR of the live cell root",
                &ancestor,
                &free_cwd,
            ),
            (
                "HOME-derived root lands inside the live cell root",
                &home_derived,
                &free_cwd,
            ),
            (
                "cwd IS the live cell's configuration root",
                &free_root,
                &held_root,
            ),
            (
                "cwd INSIDE the live cell's configuration root (projects/)",
                &free_root,
                &projects,
            ),
            (
                "cwd INSIDE the live cell's workspace",
                &free_root,
                &inside_held_cwd,
            ),
            (
                "cwd is an ANCESTOR of the live cell's workspace",
                &free_root,
                &ancestor,
            ),
        ];
        for (label, root, cwd) in rows {
            for applicant in [SessionCell::Full, SessionCell::Minified] {
                let error = admit_bound_resources(&claims, root, cwd, applicant)
                    .err()
                    .unwrap_or_else(|| {
                        panic!(
                            "{label} as {applicant:?} was ADMITTED against the live cell holding \
                             {} and {}",
                            held_root.display(),
                            held_cwd.display()
                        )
                    });
                assert_eq!(error.code, ErrorCode::InvalidConfig, "{label}");
                assert!(
                    error.message.contains("is, contains, or lies under"),
                    "{label} as {applicant:?} was refused, but not by the containment rule: {}",
                    error.message
                );
            }
        }

        // The rule is about these directories, not about live cells in
        // general: two directories that overlap nothing are still admitted, so
        // this test cannot pass by refusing everything.
        assert_eq!(
            admit_bound_resources(&claims, &free_root, &free_cwd, SessionCell::Minified).unwrap(),
            SeedDisposition::Write
        );
    }

    /// LEAK 8, and the reason it is a second entry in this file rather than a
    /// row in the table above.
    ///
    /// Leak 7 was the RELATION being wrong: identity asked where containment
    /// was meant. This one is the relation being asked of the wrong PATH. The
    /// containment walk was lexical over the caller's spelling, and `stat`
    /// resolving each prefix hid it: a symlink in the MIDDLE of a path was seen
    /// as the directory it points at, and the walk then continued to that
    /// prefix's LEXICAL parents and never the target's real ones. So a spelling
    /// that reaches a strict DESCENDANT of a live cell's directory -- the
    /// direction that writes INSIDE the cell -- overlapped nothing the walk
    /// could see.
    ///
    /// The target is a strict descendant in every row on purpose. A symlink to
    /// the claimed directory ITSELF was always caught, because the walk's first
    /// element is the path and `stat` resolves it; that arm is leak 5's, and
    /// the last row here is a negative control that it still holds while
    /// nothing else has been widened into refusing everything.
    ///
    /// MEASURED over the real socket against a live minified cell before this
    /// was fixed: the first row below, delivered as a plain
    /// `environment.set["CLAUDE_CONFIG_DIR"]`, was ADMITTED and the intruder's
    /// transcript was found by an independent sweep inside the victim's own
    /// root
    /// (`crates/e2e/tests/cross_cell_contamination.rs::containment_relations`
    /// carries both symlink rows).
    #[cfg(unix)]
    #[test]
    fn a_spelling_that_reaches_inside_a_live_cell_through_a_symlink_is_refused() {
        let parent = tempfile::tempdir().unwrap();
        let held_root = owner_only_child(parent.path(), "held-root");
        let held_cwd = owner_only_child(parent.path(), "held-cwd");
        let free_root = owner_only_child(parent.path(), "free-root");
        let free_cwd = owner_only_child(parent.path(), "free-cwd");
        let projects = owner_only_child(&held_root, "projects");
        let inside_held_cwd = owner_only_child(&held_cwd, "inside");
        let elsewhere = owner_only_child(parent.path(), "elsewhere");
        let claims = vec![minified_claim(&held_root, &held_cwd)];

        // Every link lives OUTSIDE everything the cell binds, so no row here
        // shares a single lexical component with the victim.
        let link = |name: &str, target: &Path| {
            let path = parent.path().join(name);
            std::os::unix::fs::symlink(target, &path).unwrap();
            assert!(
                !path.starts_with(&held_root) && !path.starts_with(&held_cwd),
                "a leak-8 fixture must be lexically disjoint from the victim"
            );
            path
        };
        let to_projects = link("link-to-projects", &projects);
        let to_inside_cwd = link("link-to-inside-cwd", &inside_held_cwd);
        let to_held_root = link("link-to-held-root", &held_root);
        let to_elsewhere = link("link-to-elsewhere", &elsewhere);

        let rows: [(&str, &Path, &Path); 4] = [
            // The link as the FINAL component of the delivered root.
            (
                "config root is a symlink to the cell's projects/",
                &to_projects,
                &free_cwd,
            ),
            // The link in the MIDDLE: `effective_config_root` appends `.claude`
            // to whatever `HOME` says, so the delivered root is
            // `<link>/.claude` and nothing in the request names the victim.
            (
                "HOME-derived root under a symlink to the cell's projects/",
                &to_projects.join(".claude"),
                &free_cwd,
            ),
            // The same mechanism through the other bound resource.
            (
                "cwd is a symlink into the cell's workspace",
                &free_root,
                &to_inside_cwd,
            ),
            // A link straight AT a claimed directory. Caught before leak 8 --
            // the walk's first element is the path itself -- and it must stay
            // caught.
            (
                "config root is a symlink to the cell's root itself",
                &to_held_root,
                &free_cwd,
            ),
        ];
        for (label, root, cwd) in rows {
            for applicant in [SessionCell::Full, SessionCell::Minified] {
                let error = admit_bound_resources(&claims, root, cwd, applicant)
                    .err()
                    .unwrap_or_else(|| {
                        panic!(
                            "{label} as {applicant:?} was ADMITTED against the live cell holding \
                             {} and {}",
                            held_root.display(),
                            held_cwd.display()
                        )
                    });
                assert_eq!(error.code, ErrorCode::InvalidConfig, "{label}");
                assert!(
                    error.message.contains("is, contains, or lies under"),
                    "{label} as {applicant:?} was refused, but not by the containment rule: {}",
                    error.message
                );
            }
        }

        // The negative control, and it is the one that matters here: a symlink
        // is not itself suspicious. A start whose root and cwd are links to
        // directories the cell does not bind is admitted, and admitted to
        // WRITE.
        assert_eq!(
            admit_bound_resources(&claims, &to_elsewhere, &free_cwd, SessionCell::Minified)
                .unwrap(),
            SeedDisposition::Write
        );
    }

    /// LEAK 8's mirror: the symlink is in what the LIVE SESSION holds.
    ///
    /// Claims are stored as they were spelled, not canonicalized:
    /// `TranscriptLocator::new` canonicalizes the cwd it is handed and keeps the
    /// configuration root verbatim, and the plain
    /// `environment.set["CLAUDE_CONFIG_DIR"]` door hands over the caller's own
    /// spelling -- so a live session's claimed root really can be a symlink
    /// path. Canonicalizing only the applicant would have left this open.
    ///
    /// It is closed without a second rule because
    /// `one_directory_contains_the_other` asks `contains_or_is` BOTH ways
    /// round, and `contains_or_is` resolves whichever path it is about to walk.
    /// Each of the two paths is therefore resolved exactly once, whichever side
    /// of the question it is on.
    ///
    /// The applicant here binds the symlink target's real ANCESTOR, which is
    /// the direction the mirror opens: the incumbent's root is inside what the
    /// applicant is asking for, and no spelling in the request resembles it.
    #[cfg(unix)]
    #[test]
    fn a_live_claim_spelled_as_a_symlink_still_contains_what_it_really_holds() {
        let parent = tempfile::tempdir().unwrap();
        let applicant_root = owner_only_child(parent.path(), "applicant-root");
        let real_held_root = owner_only_child(&applicant_root, "really-here");
        let held_cwd = owner_only_child(parent.path(), "held-cwd");
        let free_cwd = owner_only_child(parent.path(), "free-cwd");
        let free_root = owner_only_child(parent.path(), "free-root");

        // What a live session that was handed this spelling stores.
        let claimed_spelling = parent.path().join("claim-link");
        std::os::unix::fs::symlink(&real_held_root, &claimed_spelling).unwrap();
        assert!(
            !claimed_spelling.starts_with(&applicant_root),
            "the claim's SPELLING must lie outside what the applicant asks for, or the walk \
             would find it lexically and this test would prove nothing"
        );
        let claims = vec![minified_claim(&claimed_spelling, &held_cwd)];

        for applicant in [SessionCell::Full, SessionCell::Minified] {
            let error = admit_bound_resources(&claims, &applicant_root, &free_cwd, applicant)
                .err()
                .unwrap_or_else(|| {
                    panic!(
                        "{applicant:?} was ADMITTED at {}, which really contains the root the \
                         live cell holds as {}",
                        applicant_root.display(),
                        claimed_spelling.display()
                    )
                });
            assert_eq!(error.code, ErrorCode::InvalidConfig);
            assert!(
                error.message.contains("is, contains, or lies under"),
                "{applicant:?}: {}",
                error.message
            );
        }

        // And the claim still admits what it does not reach.
        assert_eq!(
            admit_bound_resources(&claims, &free_root, &free_cwd, SessionCell::Minified).unwrap(),
            SeedDisposition::Write
        );
    }

    /// The other half of the same rule, and the reason it is not simply
    /// "containment always".
    ///
    /// A MINIFIED APPLICANT gets containment against every live claim including
    /// ordinary ones: a private root nested inside a live ordinary session's
    /// workspace is the same leak one second later, because the moment it is
    /// admitted it is a live cell whose root that session's file tools sit on
    /// top of.
    ///
    /// ORDINARY-versus-ORDINARY stays IDENTITY, and this is where that costs
    /// something if it is got wrong: nesting is the ordinary shape of a
    /// filesystem, and widening this arm would refuse a second ordinary session
    /// working in a subdirectory AND -- through the seed disposition -- stop
    /// pmux writing a private root that merely sits under a live session's cwd.
    #[cfg(unix)]
    #[test]
    fn containment_binds_a_minified_applicant_to_ordinary_sessions_and_leaves_them_to_each_other() {
        let parent = tempfile::tempdir().unwrap();
        let workspace = owner_only_child(parent.path(), "workspace");
        let operator_root = owner_only_child(parent.path(), "operator-root");
        let nested_root = owner_only_child(&workspace, "cell-root");
        let nested_cwd = owner_only_child(&workspace, "sub-workspace");
        let free_cwd = owner_only_child(parent.path(), "free-cwd");
        let claims = vec![full_claim(&operator_root, &workspace)];

        let error = admit_bound_resources(&claims, &nested_root, &free_cwd, SessionCell::Minified)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidConfig);
        assert!(
            error
                .message
                .contains("requires a configuration root of its own"),
            "{}",
            error.message
        );
        let error = admit_bound_resources(&claims, &free_cwd, &nested_cwd, SessionCell::Minified)
            .unwrap_err();
        assert!(
            error
                .message
                .contains("requires a working directory of its own"),
            "{}",
            error.message
        );

        // The same two directories, for an ordinary applicant: admitted, and
        // admitted to WRITE, because no cell is making the claim containment
        // exists to protect.
        assert_eq!(
            admit_bound_resources(&claims, &nested_root, &free_cwd, SessionCell::Full).unwrap(),
            SeedDisposition::Write
        );
        assert_eq!(
            admit_bound_resources(&claims, &operator_root, &nested_cwd, SessionCell::Full).unwrap(),
            SeedDisposition::VerifyOnly,
            "sharing the ROOT is what costs an ordinary session the right to write it; \
             nesting under another session's cwd is not"
        );
    }

    /// `run_once`'s one-shot survives THE AGENT IT RESOLVES.
    ///
    /// MEASURED before this: `run_once` set `retention = OneShot` on the
    /// request, `resolve_agent_reference` then replaced the whole launch policy
    /// with the stored version -- including its `Persistent { idle_ttl_ms }` --
    /// and the session was registered with the agent's idle TTL instead of
    /// `None`. A value pmux wrote and pmux discarded, inside pmux's own path,
    /// which is the accepted-and-ignored shape this codebase refuses in a
    /// caller's fields and had shipped in its own.
    ///
    /// It drives `resolve_agent_and_retention` -- the exact function the start
    /// path calls -- against a REAL store on disk, so the ORDER of the two
    /// steps is what is asserted and not merely the arithmetic of
    /// `decide_retention`. No daemon, no PTY and no Claude are involved: the
    /// whole impure step is one read of one immutable file.
    #[cfg(unix)]
    #[test]
    fn a_forced_one_shot_survives_the_agent_it_resolves() {
        use pseudomux_protocol::v1::{
            AgentContainment, AgentEnvironmentSpec, AgentRef, AgentSpec, AuthPolicy,
            EnvironmentSpec, LifecycleMode, RetentionPolicy, SessionCell, SessionIdentity,
            StartSessionRequest, TerminalSpec,
        };

        let stored_ttl = 900_000;
        let temp = tempfile::tempdir().unwrap();
        let cwd = owner_only_child(temp.path(), "work");
        let store = crate::agent::AgentStore::open(&temp.path().join("agents")).unwrap();
        let stored = store
            .create(
                AgentSpec {
                    name: "reviewer".into(),
                    description: None,
                    claude: ClaudeLaunchConfig {
                        executable: "/bin/sh".into(),
                        model: None,
                        effort: None,
                        permission_mode: None,
                        allowed_tools: Vec::new(),
                        denied_tools: Vec::new(),
                        settings: Vec::new(),
                        mcp_configs: Vec::new(),
                        plugin_dirs: Vec::new(),
                        system_prompt: SystemPromptPolicy::Default,
                        extra_args: Vec::new(),
                    },
                    environment: AgentEnvironmentSpec::default(),
                    auth_policy: AuthPolicy::default(),
                    terminal: TerminalSpec::default(),
                    lifecycle: LifecycleMode::default(),
                    retention: RetentionPolicy::Persistent {
                        idle_ttl_ms: stored_ttl,
                    },
                    compatibility: CompatibilityPolicy::default(),
                    cell: SessionCell::Full,
                    containment: AgentContainment::default(),
                },
                1_700_000_000_000,
            )
            .unwrap();

        let start = || StartSessionRequest {
            identity: SessionIdentity::New { session_id: None },
            cwd: cwd.to_string_lossy().into_owned(),
            claude: None,
            agent: Some(AgentRef {
                agent_id: stored.agent_id,
                version: stored.version,
            }),
            environment: EnvironmentSpec::default(),
            auth_policy: AuthPolicy::default(),
            config_isolation: None,
            terminal: TerminalSpec::default(),
            lifecycle: LifecycleMode::default(),
            retention: RetentionPolicy::default(),
            compatibility: CompatibilityPolicy::default(),
            cell: SessionCell::Full,
        };

        // An ordinary start takes the agent's retention, which is what makes
        // the assertion after it mean something.
        let mut ordinary = start();
        let pin = resolve_agent_and_retention(Some(&store), &mut ordinary, Retention::AsResolved)
            .expect("the stored agent resolves");
        assert_eq!(pin.expect("a pin").config_digest, stored.config_digest);
        assert_eq!(
            ordinary.retention,
            RetentionPolicy::Persistent {
                idle_ttl_ms: stored_ttl
            }
        );
        assert_eq!(idle_ttl_ms_for(&ordinary.retention), Some(stored_ttl));

        // `run_once` closes the session itself, so no stored retention may
        // outlive its decision -- and the decision is applied AFTER resolution,
        // which is the half that used to be wrong.
        let mut one_shot = start();
        resolve_agent_and_retention(Some(&store), &mut one_shot, Retention::ForcedOneShot)
            .expect("the stored agent resolves");
        assert_eq!(one_shot.retention, RetentionPolicy::OneShot);
        assert_eq!(idle_ttl_ms_for(&one_shot.retention), None);
        // Everything else still came from the agent, so the override is exactly
        // one field wide.
        assert_eq!(one_shot.claude, ordinary.claude);
        assert!(one_shot.agent.is_none());
    }

    /// `run_once` is the ONLY method that forces one-shot retention, and every
    /// other start takes what resolution produced.
    ///
    /// Counted from this module's own SOURCE, in the same idiom
    /// `pool::refusal`'s census and `pmux-mcp`'s `PROTOCOL_SOURCE` use, because
    /// the call sites themselves need a Claude process, a PTY and an rmux
    /// runtime to reach -- and a test that cannot reach a call site cannot
    /// notice it being changed. Without it, swapping `run_once`'s
    /// `Retention::ForcedOneShot` for `AsResolved` is green everywhere.
    #[test]
    fn run_once_is_the_only_start_that_forces_one_shot_retention() {
        const SOURCE: &str = include_str!("native.rs");

        let body = |signature: &str| -> String {
            let start = SOURCE
                .find(signature)
                .unwrap_or_else(|| panic!("{signature} is no longer in this module"));
            let tail = &SOURCE[start..];
            // Up to the function's own closing brace: the first line that is
            // exactly four spaces and `}`, which is impl-item indentation.
            let end = tail
                .find("\n    }\n")
                .unwrap_or_else(|| panic!("{signature} has no closing brace at impl indentation"));
            tail[..end].to_owned()
        };

        let run_once = body("pub async fn run_once(");
        assert!(
            run_once.contains("Retention::ForcedOneShot"),
            "run_once must decide its own retention; it closes the session itself"
        );
        for other in [
            "async fn start_session_internal(",
            "pub(crate) async fn start_session_owned(",
        ] {
            let source = body(other);
            assert!(
                source.contains("Retention::AsResolved"),
                "{other} must take the retention resolution produced"
            );
            assert!(
                !source.contains("Retention::ForcedOneShot"),
                "{other} must not force one-shot: only a method that closes the session itself may"
            );
        }
    }

    /// **THE COMPOSITION DIRECTION, WHICH IS THE WHOLE AGENT CONTAINMENT
    /// RULE.**
    ///
    /// `crate::agent::admit_agent_containment` runs BEFORE
    /// `admit_bound_resources` and writes nothing into the request, so the
    /// admission of an agent-named start is `containment AND existing rules`.
    /// This asserts the consequence: take a cwd the existing rules ALREADY
    /// refuse, and it stays refused under every value of `workspace_root` --
    /// including one that contains it, one that IS it, and none at all.
    ///
    /// The module doc has claimed this test by name since the resource shipped;
    /// it did not exist, and no test anywhere composed the two functions. The
    /// containment predicate was pinned in isolation
    /// (`containment_bounds_a_cwd_and_never_supplies_or_widens_one`), which
    /// proves the bound is a bound and says nothing about whether satisfying it
    /// can BUY anything. It lives here rather than beside that one because
    /// `admit_bound_resources` is private: the composition is only testable
    /// where both halves are visible.
    #[cfg(unix)]
    #[test]
    fn containment_can_only_refuse_more_never_admit_more() {
        let parent = tempfile::tempdir().unwrap();
        let workspace = owner_only_child(parent.path(), "workspace");
        let held_cwd = owner_only_child(&workspace, "held-cwd");
        let held_root = owner_only_child(parent.path(), "held-root");
        let free_root = owner_only_child(parent.path(), "free-root");
        let claims = vec![minified_claim(&held_root, &held_cwd)];
        let agent_id = uuid::Uuid::from_u128(4);

        // The existing rule, alone: a live minified cell holds `held_cwd`, so
        // an applicant naming it is refused with no agent involved at all.
        let baseline = admit_bound_resources(&claims, &free_root, &held_cwd, SessionCell::Full)
            .expect_err("a live minified cell's working directory is not available");
        assert_eq!(baseline.code, ErrorCode::InvalidConfig);

        // Every workspace_root an agent could name for that exact cwd: one that
        // CONTAINS it, one that IS it, its parent, and none. Not one of them
        // may turn the refusal above into an admission.
        for root in [
            Some(workspace.clone()),
            Some(held_cwd.clone()),
            Some(parent.path().to_path_buf()),
            None,
        ] {
            let containment = pseudomux_protocol::v1::AgentContainment {
                workspace_root: root
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned()),
                require_config_isolation: false,
            };
            // Whether containment admits this cwd is not the point; what
            // matters is that the composition refuses either way.
            let contained =
                crate::agent::admit_agent_containment(&containment, agent_id, &held_cwd, None);
            let composed = contained.and_then(|()| {
                admit_bound_resources(&claims, &free_root, &held_cwd, SessionCell::Full).map(|_| ())
            });
            let error = composed.expect_err(&format!(
                "workspace_root {root:?} made a cwd the existing rules refuse admissible"
            ));
            assert_eq!(error.code, ErrorCode::InvalidConfig);
        }

        // ...and the direction that IS allowed to change the answer: the same
        // start with a cwd nothing holds is admitted without an agent, and the
        // agent can only take that away.
        let free_cwd = owner_only_child(parent.path(), "free-cwd");
        admit_bound_resources(&claims, &free_root, &free_cwd, SessionCell::Full)
            .expect("an unheld pair is admissible");
        let narrowing = pseudomux_protocol::v1::AgentContainment {
            workspace_root: Some(workspace.to_string_lossy().into_owned()),
            require_config_isolation: false,
        };
        crate::agent::admit_agent_containment(&narrowing, agent_id, &free_cwd, None)
            .expect_err("an agent bounded to the workspace refuses a cwd outside it");
    }

    /// What an ancestry walk does with the three things that can make one not
    /// terminate, or terminate on a lie.
    ///
    /// The walk itself cannot loop: `Path::ancestors` is LEXICAL and strictly
    /// shortens the spelling by one component per step, so a symlink CYCLE on
    /// disk costs at most one step per component of the path the caller sent --
    /// asserted below as a bound, rather than asserted by the test merely
    /// finishing. What the kernel refuses to answer is answered by
    /// `DirectoryIdentity`, and every one of those answers is fail-CLOSED: the
    /// path is reported as contained and the start is refused.
    #[cfg(unix)]
    #[test]
    fn an_ancestry_walk_is_bounded_and_refuses_a_loop_an_unreadable_ancestor_and_an_overlong_name()
    {
        use std::os::unix::fs::PermissionsExt;

        let parent = tempfile::tempdir().unwrap();
        let held_root = owner_only_child(parent.path(), "held-root");
        let held_cwd = owner_only_child(parent.path(), "held-cwd");
        let free_cwd = owner_only_child(parent.path(), "free-cwd");
        let claims = vec![minified_claim(&held_root, &held_cwd)];

        // A symlink CYCLE. `metadata` answers ELOOP.
        let looped = parent.path().join("loop");
        std::os::unix::fs::symlink(&looped, &looped).unwrap();
        assert_eq!(
            DirectoryIdentity::of(&looped),
            DirectoryIdentity::Unresolved
        );

        // A name past PATH_MAX. `metadata` answers ENAMETOOLONG.
        let mut overlong = parent.path().to_path_buf();
        for _ in 0..300 {
            overlong.push("component");
        }
        assert!(
            overlong.as_os_str().len() > 1024,
            "the fixture must exceed PATH_MAX to be the case it claims"
        );
        assert_eq!(
            DirectoryIdentity::of(&overlong),
            DirectoryIdentity::Unresolved
        );
        // The bound, stated: one step per component of the SPELLING, and the
        // filesystem cannot add one.
        assert!(
            overlong.ancestors().count() <= overlong.as_os_str().len(),
            "the walk must be bounded by the path the caller sent"
        );

        // An unreadable ancestor. `metadata` answers EACCES.
        let closed = owner_only_child(parent.path(), "closed");
        let hidden = owner_only_child(&closed, "hidden");
        std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o000)).unwrap();
        let hidden_identity = DirectoryIdentity::of(&hidden);
        let hidden_contains = one_directory_contains_the_other(&held_root, &hidden);
        let hidden_admission =
            admit_bound_resources(&claims, &hidden, &free_cwd, SessionCell::Full);
        std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o700)).unwrap();

        for (label, path) in [("symlink loop", &looped), ("overlong name", &overlong)] {
            assert!(
                one_directory_contains_the_other(&held_root, path),
                "{label}: a path the kernel will not resolve must be reported as CONTAINED, \
                 because a wrong `disjoint` is the answer that leaks"
            );
            let error =
                admit_bound_resources(&claims, path, &free_cwd, SessionCell::Full).unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidConfig, "{label}");
            assert!(
                error.message.contains("cannot be inspected"),
                "{label}: {}",
                error.message
            );
        }
        if matches!(hidden_identity, DirectoryIdentity::Resource(_)) {
            // Running as a user the mode bits do not apply to; unreachable.
            return;
        }
        assert!(hidden_contains, "unreadable ancestor");
        assert!(
            hidden_admission
                .unwrap_err()
                .message
                .contains("cannot be inspected")
        );
    }

    /// The assumption admission stands on, checked instead of assumed.
    ///
    /// Admission is keyed on the DELIVERED configuration root, and for an
    /// isolated start the reason that covers the root the caller NAMED is that
    /// `build_environment`'s step 6 overwrites `CLAUDE_CONFIG_DIR` with the
    /// canonicalized isolation root. Nothing enforced it. If those two ever
    /// disagree, admission decides about one directory and the child is
    /// launched into another -- which is the whole shape of this leak family.
    #[cfg(unix)]
    #[test]
    fn the_named_isolation_root_and_the_delivered_configuration_root_must_be_one_directory() {
        use pseudomux_protocol::v1::ConfigIsolation;

        let parent = tempfile::tempdir().unwrap();
        let delivered = owner_only_child(parent.path(), "delivered");
        let elsewhere = owner_only_child(parent.path(), "elsewhere");
        // The caller may name the root through an alias; step 6 delivers the
        // canonical spelling, and the two are still ONE directory.
        let alias = parent.path().join("symlink-to-delivered");
        std::os::unix::fs::symlink(&delivered, &alias).unwrap();
        let isolation = |root: &Path| ConfigIsolation {
            root: root.to_string_lossy().into_owned(),
        };

        require_isolation_root_is_the_effective_root(Some(&isolation(&alias)), &delivered).unwrap();
        require_isolation_root_is_the_effective_root(None, &elsewhere).unwrap();

        let error =
            require_isolation_root_is_the_effective_root(Some(&isolation(&delivered)), &elsewhere)
                .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidConfig);
        assert!(
            error.message.contains("different directories"),
            "{}",
            error.message
        );
    }

    /// The wiring the two rules stand on: a published session's own transcript
    /// source is what says which root and cwd are taken, and by which cell.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_published_session_claims_the_root_and_cwd_of_its_own_transcript() {
        let runtime = tempfile::tempdir().unwrap();
        let config_root = owner_only_dir();
        let cwd = owner_only_dir();
        let session_id = SessionId::new_v4();
        let mut sessions = HashMap::new();
        sessions.insert(
            session_id,
            SessionMetadata {
                generation_id: SessionGenerationId::new(),
                owner: SessionOwner::Caller,
                terminal: Arc::new(RmuxTerminalControl::new(Box::new(
                    FakeStartupTerminal::new(["published"]),
                ))),
                transcript: Arc::new(
                    FileTranscriptSource::new(config_root.path(), cwd.path(), session_id).unwrap(),
                ),
                private_session_name: "claiming-private-session".to_owned(),
                cell: SessionCell::Minified,
                _sensitive_launch: empty_sensitive_launch(runtime.path(), session_id),
                _lifecycle: lifecycle_with_probe(
                    Arc::new(AtomicBool::new(false)),
                    &Arc::new(TrackedTasks::default()),
                ),
            },
        );

        let claims = live_resource_claims(&sessions);
        assert_eq!(
            incumbent_cell_for_config_root(&claims, config_root.path(), SessionCell::Full),
            Some(SessionCell::Minified),
            "a live cell's root must be visible to the next applicant"
        );
        assert_eq!(
            incumbent_cell_for_cwd(&claims, cwd.path(), SessionCell::Full),
            Some(SessionCell::Minified),
            "a live cell's cwd must be visible to the next applicant"
        );
        assert_eq!(
            incumbent_cell_for_config_root(&claims, runtime.path(), SessionCell::Full),
            None
        );
        assert_eq!(
            incumbent_cell_for_cwd(&claims, runtime.path(), SessionCell::Full),
            None
        );
        assert_eq!(live_resource_claims(&HashMap::new()), Vec::new());

        // End to end over the same map: published metadata refuses the next
        // start on either resource by itself, and admits one that shares
        // neither.
        let free_root = owner_only_dir();
        let free_cwd = owner_only_dir();
        assert!(
            admit_bound_resources(
                &claims,
                config_root.path(),
                free_cwd.path(),
                SessionCell::Full,
            )
            .is_err(),
            "the held root alone must refuse"
        );
        assert!(
            admit_bound_resources(&claims, free_root.path(), cwd.path(), SessionCell::Full)
                .is_err(),
            "the held cwd alone must refuse"
        );
        assert_eq!(
            admit_bound_resources(
                &claims,
                free_root.path(),
                free_cwd.path(),
                SessionCell::Minified,
            )
            .unwrap(),
            SeedDisposition::Write
        );
    }

    #[test]
    fn new_identity_rejects_same_uuid_in_any_project_directory() {
        let root = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let config_root = root.path().join("claude");
        let project = config_root.join("projects").join("foreign-project");
        std::fs::create_dir_all(&project).unwrap();
        let session_id = SessionId::new_v4();
        std::fs::write(project.join(format!("{session_id}.jsonl")), "\n").unwrap();

        let mut resolved = resolved_with_environment(&[]);
        resolved.session_id = session_id;
        resolved.process.cwd = cwd.path().canonicalize().unwrap();
        let error = validate_transcript_identity(
            &SessionIdentity::New {
                session_id: Some(session_id),
            },
            &resolved,
            &config_root,
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::IdCollision);
        assert!(!error.message.contains("foreign-project"));
    }

    #[tokio::test]
    async fn startup_wait_returns_recognized_modal_without_waiting_for_ready_prompt() {
        let mut terminal = FakeStartupTerminal::new([
            "Claude is starting",
            "Permission required: allow this command or deny it?",
        ]);
        let state = wait_until_ready(&mut terminal, Duration::from_millis(200))
            .await
            .unwrap();
        let TerminalScreenState::NeedsInput(needs_input) = state else {
            panic!("expected startup to retain the interactive screen");
        };
        assert_eq!(needs_input.kind, NeedsInputKind::Permission);
        assert_eq!(needs_input.details, serde_json::Value::Null);

        let secret_screen = "unrecognized private startup screen";
        let mut terminal = FakeStartupTerminal::new([secret_screen]);
        let error = wait_until_ready(&mut terminal, Duration::ZERO)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::NeedsInput);
        assert!(!error.message.contains(secret_screen));
        let details = error.details.to_string();
        assert!(details.contains("screen_shape"));
        assert!(!details.contains(secret_screen));

        let diagnostics = startup_screen_diagnostics(&TerminalSnapshot {
            revision: 1,
            rows: 3,
            cols: 20,
            cursor: None,
            visible_text: "private account\n  ❯ Try something\n".to_owned(),
        });
        let serialized = diagnostics.to_string();
        assert!(diagnostics["contains_prompt_glyph"].as_bool().unwrap());
        assert!(!diagnostics["exact_prompt_glyph_line"].as_bool().unwrap());
        assert!(!serialized.contains("private account"));
        assert!(!serialized.contains("Try something"));
        assert!(diagnostics.get("cursor_row").is_none());
        assert!(diagnostics.get("cursor_col").is_none());
    }

    /// **Re-promotion trigger 3.** A child that refused a launch flag and a
    /// child that is merely slow reach the same refusal, and until now the
    /// refusal could not tell them apart.
    ///
    /// The screen text is the LIVE one. MEASURED on this host at Claude Code
    /// 2.1.223 and 2.1.226, byte-identical at both:
    ///
    /// ```text
    /// $ claude --pmux-probe-sentinel doctor
    /// error: unknown option '--pmux-probe-sentinel'
    /// ```
    ///
    /// stderr, exit 1, empty stdout, and the commander exits before `doctor`
    /// runs -- so the probe that produced this string executed nothing and
    /// spent no ledger ordinal.
    ///
    /// What this does NOT establish: that a real rejected launch reaches this
    /// diagnostic through a real rmux pane. The marker's text is measured and
    /// the predicate is tested; the path from a dying child's stderr to
    /// `TerminalSnapshot::visible_text` is not exercised here.
    #[tokio::test]
    async fn a_child_that_refused_a_launch_flag_is_named_as_a_repromotion_trigger() {
        let rejected = startup_screen_diagnostics(&TerminalSnapshot {
            revision: 1,
            rows: 2,
            cols: 80,
            cursor: None,
            visible_text: "error: unknown option '--strict-mcp-config'\n".to_owned(),
        });
        assert!(rejected["child_rejected_a_launch_flag"].as_bool().unwrap());
        assert_eq!(
            rejected["repromotion_trigger"].as_str(),
            Some(crate::compatibility::RepromotionTrigger::LaunchBundleRejected.id())
        );
        // The flag itself is NOT reproduced. Every other key in this object is
        // a structural fact and this one is too; a diagnostic that echoed the
        // screen would be the one place raw terminal text leaves the adapter.
        assert!(
            !rejected.to_string().contains("strict-mcp-config"),
            "the diagnostic reproduced the screen: {rejected}"
        );

        // A Claude that is merely slow says nothing about the launch bundle,
        // which is the half that makes the boolean worth having.
        let slow = startup_screen_diagnostics(&TerminalSnapshot {
            revision: 1,
            rows: 2,
            cols: 80,
            cursor: None,
            visible_text: "  Starting...\n".to_owned(),
        });
        assert!(!slow["child_rejected_a_launch_flag"].as_bool().unwrap());
        assert!(slow["repromotion_trigger"].is_null());

        // And it reaches a caller: `wait_until_ready` publishes the same
        // object as `screen_shape` on the refusal it returns.
        let mut terminal = FakeStartupTerminal::new(["error: unknown option '--disallowedTools'"]);
        let error = wait_until_ready_with_timings(
            &mut terminal,
            Duration::from_millis(20),
            Duration::from_millis(1),
            Duration::from_millis(1),
        )
        .await
        .expect_err("a child that rejected a flag never reaches a ready screen");
        assert_eq!(
            error.details["screen_shape"]["repromotion_trigger"].as_str(),
            Some(crate::compatibility::RepromotionTrigger::LaunchBundleRejected.id()),
            "the refusal an operator reads must name the trigger: {}",
            error.details
        );
        assert!(
            !error.details.to_string().contains("disallowedTools"),
            "the refusal reproduced the screen: {}",
            error.details
        );
    }

    /// A ready screen that keeps repainting never settles.
    ///
    /// The stability rule is one equality against the previously observed
    /// snapshot, and it is the whole of what "stable" means here. Read as `!=`
    /// it INVERTS: a screen that changes on every poll is treated as the same
    /// candidate and is declared ready after `stable_for`, while a screen
    /// holding perfectly still resets its own candidate on every poll and can
    /// never settle. Both directions are asserted, because the mutant that
    /// swaps them passes any test that only ever shows it one of the two.
    #[tokio::test]
    async fn a_ready_screen_that_keeps_repainting_never_settles_and_a_still_one_does() {
        let repainting = (0..4_000).map(|revision| {
            structured_startup_snapshot(
                revision,
                ["", "", "", "", "", "❯ Try something", "footer", "status"],
                5,
                2,
                true,
            )
        });
        let mut terminal = FakeStartupTerminal::from_snapshots(repainting);
        let error = wait_until_ready_with_timings(
            &mut terminal,
            Duration::from_millis(40),
            Duration::from_millis(3),
            Duration::from_millis(1),
        )
        .await
        .expect_err("a screen that repainted on every poll was never stable");
        assert_eq!(error.code, ErrorCode::NeedsInput);

        let still = structured_startup_snapshot(
            7,
            ["", "", "", "", "", "❯ Try something", "footer", "status"],
            5,
            2,
            true,
        );
        let mut terminal = FakeStartupTerminal::from_snapshots([still]);
        let state = wait_until_ready_with_timings(
            &mut terminal,
            Duration::from_millis(200),
            Duration::from_millis(3),
            Duration::from_millis(1),
        )
        .await
        .expect("a screen that held still is exactly what stability means");
        assert!(matches!(state, TerminalScreenState::Ready));
    }

    /// The startup diagnostic counts the lines that carry something.
    ///
    /// `!line.trim().is_empty()` is the whole of that sentence. Delete the `!`
    /// and every count, offset and "does the last line start with the prompt
    /// glyph" answer in the refusal an operator reads is computed over the
    /// BLANK lines instead -- a report that looks well-formed and describes a
    /// screen nobody rendered.
    #[test]
    fn the_startup_diagnostic_counts_the_lines_that_carry_something() {
        let diagnostics = startup_screen_diagnostics(&TerminalSnapshot {
            revision: 1,
            rows: 4,
            cols: 80,
            cursor: None,
            visible_text: "\n\n❯ private text\n".to_owned(),
        });
        assert_eq!(diagnostics["line_count"], json!(4));
        assert_eq!(
            diagnostics["non_empty_line_count"],
            json!(1),
            "three of the four lines are blank: {diagnostics}"
        );
        assert_eq!(
            diagnostics["prompt_glyph_offsets_from_bottom"],
            json!([1]),
            "the only line carrying anything is one row above the bottom: {diagnostics}"
        );
        assert_eq!(
            diagnostics["last_non_empty_starts_with_prompt_glyph"],
            json!(true)
        );
        assert!(!diagnostics.to_string().contains("private text"));
    }

    #[tokio::test]
    async fn transient_structured_ready_does_not_bypass_startup_stability() {
        let ready = structured_startup_snapshot(
            2,
            ["", "", "", "", "", "❯ Try something", "footer", "status"],
            5,
            2,
            true,
        );
        let modal = structured_startup_snapshot(
            3,
            [
                "",
                "Permission required",
                "Allow this command or deny it?",
                "",
                "",
                "choose",
                "footer",
                "status",
            ],
            5,
            2,
            true,
        );
        let mut terminal = FakeStartupTerminal::from_snapshots([ready, modal]);
        let state = wait_until_ready_with_timings(
            &mut terminal,
            Duration::from_millis(30),
            Duration::from_millis(3),
            Duration::from_millis(1),
        )
        .await
        .unwrap();
        assert!(matches!(
            state,
            TerminalScreenState::NeedsInput(ref needs_input)
                if needs_input.kind == NeedsInputKind::Permission
        ));
    }

    #[tokio::test]
    async fn stalled_startup_snapshot_is_bounded_by_the_readiness_deadline() {
        let secret = "private-stalled-startup-screen";
        let mut terminal =
            FakeStartupTerminal::new([secret]).with_snapshot_delay(Duration::from_secs(60));
        let started = tokio::time::Instant::now();
        let error = wait_until_ready_with_timings(
            &mut terminal,
            Duration::from_millis(5),
            Duration::from_millis(3),
            Duration::from_millis(1),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::NeedsInput);
        assert!(started.elapsed() < Duration::from_millis(100));
        assert!(!format!("{} {}", error.message, error.details).contains(secret));
    }

    #[test]
    fn startup_terminal_errors_and_cursor_diagnostics_are_redacted() {
        let secret = "private-startup-screen-or-matcher";
        for error in [
            TerminalBackendError::InvalidLaunch(secret.to_owned()),
            TerminalBackendError::ControlPlaneLost,
            TerminalBackendError::Rmux(secret.to_owned()),
            TerminalBackendError::ProcessBoundary(secret.to_owned()),
        ] {
            let body = map_startup_terminal_error(error);
            assert!(!format!("{} {}", body.message, body.details).contains(secret));
        }
        let loss = map_startup_terminal_error(TerminalBackendError::ControlPlaneLost);
        assert_eq!(loss.code, ErrorCode::DaemonLost);
        assert!(loss.retryable);

        let diagnostics = startup_screen_diagnostics(&TerminalSnapshot {
            revision: 9,
            rows: 4,
            cols: 80,
            cursor: Some(pseudomux_rmux::TerminalCursor {
                row: 1,
                col: 57,
                visible: true,
                style: 0,
            }),
            visible_text: format!("\n❯ {secret}\nfooter\nstatus"),
        });
        let rendered = diagnostics.to_string();
        assert!(!rendered.contains(secret));
        assert!(diagnostics.get("cursor_row").is_none());
        assert!(diagnostics.get("cursor_col").is_none());
    }

    #[tokio::test]
    async fn failed_start_terminal_remains_retryable_until_cleanup_is_confirmed() {
        let secret_screen = "unrecognized private startup screen";
        let mut terminal = FakeStartupTerminal::with_close_outcomes(
            [secret_screen],
            [
                FakeCloseOutcome::Reaped(false),
                FakeCloseOutcome::Reaped(true),
            ],
        );
        let startup_error = wait_until_ready(&mut terminal, Duration::ZERO)
            .await
            .unwrap_err();
        assert_eq!(startup_error.code, ErrorCode::NeedsInput);

        let session_id = SessionId::new_v4();
        let terminal = Arc::new(RmuxTerminalControl::new(Box::new(terminal)));
        let first = close_unpublished_terminal(session_id, &terminal)
            .await
            .unwrap_err();
        assert_eq!(first.code, ErrorCode::RecoveryFailed);
        assert!(first.retryable);
        close_unpublished_terminal(session_id, &terminal)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn failed_start_terminal_remains_retryable_after_cleanup_error() {
        let session_id = SessionId::new_v4();
        let terminal = Arc::new(RmuxTerminalControl::new(Box::new(
            FakeStartupTerminal::with_close_outcomes(
                ["unrecognized private startup screen"],
                [FakeCloseOutcome::Error, FakeCloseOutcome::Reaped(true)],
            ),
        )));
        let first = close_unpublished_terminal(session_id, &terminal)
            .await
            .unwrap_err();
        assert_eq!(first.code, ErrorCode::RmuxUnavailable);
        close_unpublished_terminal(session_id, &terminal)
            .await
            .unwrap();
    }

    #[test]
    fn unimplemented_disconnect_leases_fail_closed() {
        let mut turn = pseudomux_protocol::v1::TurnRequest {
            turn_id: pseudomux_protocol::v1::TurnId::new_v4(),
            prompt: "safe".into(),
            deadline_unix_ms: None,
            lease: pseudomux_protocol::v1::TurnLeasePolicy::default(),
        };
        assert!(validate_turn_lease(&turn).is_ok());
        turn.lease.on_disconnect = DisconnectAction::CancelTurn;
        assert_eq!(
            validate_turn_lease(&turn).unwrap_err().code,
            ErrorCode::UnsupportedFeature
        );
        turn.lease.on_disconnect = DisconnectAction::Continue;
        turn.lease.heartbeat_timeout_ms = Some(1_000);
        assert_eq!(
            validate_turn_lease(&turn).unwrap_err().code,
            ErrorCode::UnsupportedFeature
        );
    }

    // -- `diagnose` classification ------------------------------------------
    //
    // Every judgement `NativeService::diagnose` makes is made by
    // `build_diagnosis`, so every judgement is exercised here without a
    // sidecar, a Claude process or a clock. What is NOT proven here is that the
    // three reads feed it correctly; that is what the live sidecar
    // reproductions in `tests/private_runtime.rs` are for.

    fn diagnose_session(index: u128, name: &str) -> (SessionId, SessionGenerationId, String) {
        (
            SessionId::from_u128(index),
            SessionGenerationId::from_u128(index + 1_000),
            name.to_owned(),
        )
    }

    fn diagnose_live(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn diagnose_observed(
        entries: &[(SessionId, ActorStateObservation)],
    ) -> HashMap<SessionId, ActorStateObservation> {
        entries.iter().copied().collect()
    }

    /// The bug this whole command was rebuilt for.
    ///
    /// Both daemons are alive, the sidecar answers, the accept loop is
    /// perfect, and one session's private terminal is gone. Nothing in pmux
    /// polls an idle session's terminal, so the actor still says `ready`.
    #[test]
    fn a_ready_session_whose_private_terminal_vanished_is_a_fault() {
        let (session_id, generation_id, name) = diagnose_session(1, "pmux-alive");
        let (dead_id, dead_generation, dead_name) = diagnose_session(2, "pmux-dead");
        let live = diagnose_live(&["pmux-alive"]);
        let diagnosis = build_diagnosis(
            &[
                (session_id, generation_id, name),
                (dead_id, dead_generation, dead_name),
            ],
            Ok(&live),
            4,
            true,
            &diagnose_observed(&[
                (
                    session_id,
                    ActorStateObservation::Reported(SessionState::Ready),
                ),
                (
                    dead_id,
                    ActorStateObservation::Reported(SessionState::Ready),
                ),
            ]),
        );

        assert_eq!(diagnosis.runtime.outcome, ProbeOutcome::Pass);
        assert_eq!(
            diagnosis.runtime.finding,
            RuntimeFinding::PrivateRuntimeResponsive
        );
        assert_eq!(diagnosis.runtime.live_private_terminals, Some(1));
        assert_eq!(diagnosis.sessions[0].outcome, ProbeOutcome::Pass);
        assert_eq!(
            diagnosis.sessions[0].finding,
            SessionFinding::TerminalPresent
        );
        assert_eq!(diagnosis.sessions[0].private_terminal_present, Some(true));
        assert_eq!(diagnosis.sessions[1].outcome, ProbeOutcome::Fail);
        assert_eq!(
            diagnosis.sessions[1].finding,
            SessionFinding::TerminalMissing
        );
        assert_eq!(diagnosis.sessions[1].private_terminal_present, Some(false));
        // The whole report, which is what a boolean would have collapsed to.
        assert_eq!(diagnosis.outcome(), ProbeOutcome::Fail);
    }

    /// The discriminator is "would pmux still accept work here", and it is
    /// exhaustive over `SessionState` by construction: a new state that is not
    /// listed stops this compiling.
    #[test]
    fn only_a_state_that_still_offers_work_is_judged_against_the_sidecar() {
        let expected = [
            (SessionState::Creating, SessionFinding::TerminalMissing),
            (SessionState::Booting, SessionFinding::TerminalMissing),
            (SessionState::Ready, SessionFinding::TerminalMissing),
            (SessionState::Submitting, SessionFinding::TerminalMissing),
            (
                SessionState::AwaitingPromptAck,
                SessionFinding::TerminalMissing,
            ),
            (SessionState::Running, SessionFinding::TerminalMissing),
            (SessionState::NeedsInput, SessionFinding::TerminalMissing),
            (
                SessionState::TerminalCandidate,
                SessionFinding::TerminalMissing,
            ),
            (SessionState::Draining, SessionFinding::TerminalMissing),
            (SessionState::Cancelling, SessionFinding::TerminalMissing),
            // pmux has already declared these unusable. Their terminal's
            // absence proves nothing about the sidecar, and their presence in
            // the registry is not health either.
            (
                SessionState::Tainted,
                SessionFinding::SessionDeclaredUnusable,
            ),
            (
                SessionState::Closing,
                SessionFinding::SessionDeclaredUnusable,
            ),
            (
                SessionState::Closed,
                SessionFinding::SessionDeclaredUnusable,
            ),
            (
                SessionState::Failed,
                SessionFinding::SessionDeclaredUnusable,
            ),
        ];
        fn exhaustive(state: SessionState) {
            match state {
                SessionState::Creating
                | SessionState::Booting
                | SessionState::Ready
                | SessionState::Submitting
                | SessionState::AwaitingPromptAck
                | SessionState::Running
                | SessionState::NeedsInput
                | SessionState::TerminalCandidate
                | SessionState::Draining
                | SessionState::Cancelling
                | SessionState::Tainted
                | SessionState::Closing
                | SessionState::Closed
                | SessionState::Failed => (),
            }
        }
        for (state, finding) in expected {
            exhaustive(state);
            assert_eq!(
                session_finding(ActorStateObservation::Reported(state), Some(false)),
                finding,
                "{state:?} with an absent terminal"
            );
            // The same state with the terminal present never reports a fault.
            assert_ne!(
                session_finding(ActorStateObservation::Reported(state), Some(true)).outcome(),
                ProbeOutcome::Fail,
                "{state:?} with a present terminal"
            );
        }
    }

    /// A close that lands between the registry read and the actor read is the
    /// one way a healthy daemon can show a registered session with no terminal.
    /// It must never be reported as the fault above.
    #[test]
    fn a_teardown_racing_the_probe_is_unproven_and_never_a_fault() {
        for observation in [
            ActorStateObservation::Reported(SessionState::Closing),
            ActorStateObservation::Reported(SessionState::Closed),
            ActorStateObservation::Gone,
            ActorStateObservation::Unanswered,
        ] {
            assert_eq!(
                session_finding(observation, Some(false)).outcome(),
                ProbeOutcome::Unproven,
                "{observation:?}"
            );
        }
        assert_eq!(
            session_finding(ActorStateObservation::Gone, Some(false)),
            SessionFinding::SessionClosedDuringProbe
        );
        assert_eq!(
            session_finding(ActorStateObservation::Unanswered, Some(false)),
            SessionFinding::SessionActorUnresponsive
        );
    }

    /// Every way the sidecar can fail is a fault of the runtime and leaves
    /// every session unproven -- never healthy, and never blamed individually.
    #[test]
    fn a_broken_control_plane_is_a_runtime_fault_and_leaves_sessions_unproven() {
        let (session_id, generation_id, name) = diagnose_session(3, "pmux-any");
        for (fault, finding) in [
            (
                ControlPlaneFault::Unreachable,
                RuntimeFinding::ControlPlaneUnreachable,
            ),
            (
                ControlPlaneFault::Unresponsive,
                RuntimeFinding::ControlPlaneUnresponsive,
            ),
            (
                ControlPlaneFault::Refused,
                RuntimeFinding::ControlPlaneRefused,
            ),
        ] {
            let diagnosis = build_diagnosis(
                &[(session_id, generation_id, name.clone())],
                Err(&fault),
                9,
                true,
                &diagnose_observed(&[(
                    session_id,
                    ActorStateObservation::Reported(SessionState::Ready),
                )]),
            );
            assert_eq!(diagnosis.runtime.finding, finding, "{fault:?}");
            assert_eq!(diagnosis.runtime.outcome, ProbeOutcome::Fail, "{fault:?}");
            // No count is invented from an answer that never arrived.
            assert_eq!(diagnosis.runtime.live_private_terminals, None, "{fault:?}");
            assert_eq!(
                diagnosis.sessions[0].finding,
                SessionFinding::NotProbed,
                "{fault:?}"
            );
            assert_eq!(
                diagnosis.sessions[0].outcome,
                ProbeOutcome::Unproven,
                "{fault:?}"
            );
            assert_eq!(
                diagnosis.sessions[0].private_terminal_present, None,
                "{fault:?}"
            );
            assert_eq!(diagnosis.outcome(), ProbeOutcome::Fail, "{fault:?}");
        }
    }

    /// A broker whose accept loop has ended breaks every future start and
    /// nothing anywhere notices. The sidecar answering does not excuse it.
    #[test]
    fn a_stopped_launch_broker_is_a_fault_even_when_the_sidecar_answers() {
        let live = diagnose_live(&[]);
        let responsive = build_diagnosis(&[], Ok(&live), 1, true, &HashMap::new());
        assert_eq!(
            responsive.runtime.finding,
            RuntimeFinding::PrivateRuntimeResponsive
        );
        let stopped = build_diagnosis(&[], Ok(&live), 1, false, &HashMap::new());
        assert_eq!(stopped.runtime.finding, RuntimeFinding::LaunchBrokerStopped);
        assert_eq!(stopped.runtime.outcome, ProbeOutcome::Fail);
        assert_eq!(stopped.outcome(), ProbeOutcome::Fail);
    }

    /// A daemon holding nothing is idle, not broken. Absence of a warm
    /// instance is a capacity fact; a pool whose classes are all cold must not
    /// page anyone.
    ///
    /// Asserted against the classifier's OWN two verdicts rather than against
    /// `outcome()`. `build_diagnosis` is handed three reads and no service, so
    /// it cannot build the health tree, and `outcome()` folds every absent
    /// layer as `unproven` -- correctly, and the test below is the one that
    /// pins that. Asserting `outcome() == Pass` here would have forced the
    /// missing-layer rule to be weakened to keep a test about sessions green.
    #[test]
    fn a_daemon_with_no_sessions_and_a_responsive_runtime_is_healthy() {
        let live = diagnose_live(&[]);
        let diagnosis = build_diagnosis(&[], Ok(&live), 0, true, &HashMap::new());
        assert_eq!(diagnosis.sessions, Vec::new());
        assert_eq!(diagnosis.runtime.outcome, ProbeOutcome::Pass);
        assert_eq!(
            ProbeOutcome::fold(diagnosis.sessions.iter().map(|session| session.outcome)),
            ProbeOutcome::Pass
        );
    }

    /// The classifier alone establishes no layer, and a report with no layers
    /// is `unproven`.
    ///
    /// This is the missing-layer rule seen from the service side, and it is
    /// what makes the two assertions above safe to weaken: the healthy runtime
    /// and the empty session list are still `pass`, and the total is still not.
    #[test]
    fn a_classifier_only_report_establishes_no_layer_and_is_never_healthy() {
        let live = diagnose_live(&[]);
        let diagnosis = build_diagnosis(&[], Ok(&live), 0, true, &HashMap::new());
        assert!(diagnosis.layers.is_empty());
        assert_eq!(
            diagnosis.missing_layers(),
            HealthLayerName::ALL.to_vec(),
            "every layer is missing from a report the layer builder never touched"
        );
        assert_eq!(
            diagnosis.outcome(),
            ProbeOutcome::Unproven,
            "a report that established no layer must not read as healthy"
        );
    }

    /// A terminal the sidecar knows and the registry does not is the normal,
    /// transient shape of every in-flight start: pmux publishes a session only
    /// after its terminal exists. Counting it as a leak would be a rule that
    /// holds on an idle daemon and fires on a busy one.
    #[test]
    fn an_unclaimed_private_terminal_is_a_reported_fact_and_not_a_fault() {
        let (session_id, generation_id, name) = diagnose_session(4, "pmux-known");
        let live = diagnose_live(&["pmux-known", "pmux-still-starting"]);
        let diagnosis = build_diagnosis(
            &[(session_id, generation_id, name)],
            Ok(&live),
            2,
            true,
            &diagnose_observed(&[(
                session_id,
                ActorStateObservation::Reported(SessionState::Ready),
            )]),
        );
        assert_eq!(diagnosis.runtime.live_private_terminals, Some(2));
        // The runtime and the one session, not `outcome()`: see
        // `a_classifier_only_report_establishes_no_layer_and_is_never_healthy`
        // for why a classifier-only report folds to `unproven`.
        assert_eq!(diagnosis.runtime.outcome, ProbeOutcome::Pass);
        assert_eq!(diagnosis.sessions[0].outcome, ProbeOutcome::Pass);
    }

    /// Every pure layer builder, over every input that changes its verdict.
    ///
    /// Six of the seven builders are reachable without a service and are
    /// exercised here; only `configuration_layer` needs one, and it is covered
    /// by the live probe. `compatibility_layer` was a method purely because it
    /// read two numbers off `self`, and being one kept its every arm out of
    /// this test -- including the arm that made `pmux doctor` exit 1 forever on
    /// a correct Path A daemon. It takes the two numbers as arguments now.
    ///
    /// What this pins is the rule the whole tree exists for, in the form it now
    /// takes: a layer that COULD NOT establish its subject says
    /// `not_established`, which is `unproven`; a layer that HAD NO SUBJECT says
    /// `nothing_to_exercise`, which is `pass`; and a layer whose subject was
    /// DECLARED and is missing says `faulted`. The third is the distinction
    /// this test asserted the opposite of while `nothing_to_exercise` meant
    /// nothing more than "the set is empty".
    #[test]
    fn each_layer_reports_not_established_rather_than_health_when_it_proved_nothing() {
        let live = diagnose_live(&["pmux-one"]);

        // Control plane. An unreachable socket is the ONLY fault of this layer;
        // a deadline expiry and a refusal both mean a connection was made, so
        // this layer is proven and the fault belongs one level up.
        assert_eq!(
            control_plane_layer(Err(&ControlPlaneFault::Unreachable), 5).finding,
            LayerFinding::Faulted
        );
        for proven in [ControlPlaneFault::Unresponsive, ControlPlaneFault::Refused] {
            assert_eq!(
                control_plane_layer(Err(&proven), 5).finding,
                LayerFinding::Exercised,
                "{proven:?} means a connection existed, so the control plane is proven"
            );
        }
        assert_eq!(
            control_plane_layer(Ok(&live), 5).finding,
            LayerFinding::Exercised
        );

        // Private runtime. The layer all four false-healthy reproductions
        // failed at. Unreachable is NOT a fault here -- nothing was asked.
        assert_eq!(
            private_runtime_layer(Ok(&live), 5).finding,
            LayerFinding::Exercised
        );
        assert_eq!(
            private_runtime_layer(Err(&ControlPlaneFault::Unreachable), 5).finding,
            LayerFinding::NotEstablished,
            "no exchange was attempted, so nothing is claimed about the sidecar"
        );
        for faulted in [ControlPlaneFault::Unresponsive, ControlPlaneFault::Refused] {
            assert_eq!(
                private_runtime_layer(Err(&faulted), 5).finding,
                LayerFinding::Faulted,
                "{faulted:?}"
            );
        }

        // Launch broker. Both inputs move the finding, and the disagreement
        // arms are the reason there are two: a probe that completed against a
        // loop that has since finished is a fault, and so is a live loop whose
        // exchanges do not complete. A layer reading either input alone reports
        // one of those two as healthy.
        assert_eq!(
            launch_broker_layer(true, &BrokerProbe::Exchanged).finding,
            LayerFinding::Exercised
        );
        assert_eq!(
            launch_broker_layer(false, &BrokerProbe::ConnectRefused).finding,
            LayerFinding::Faulted
        );
        assert_eq!(
            launch_broker_layer(false, &BrokerProbe::Exchanged).finding,
            LayerFinding::Faulted,
            "an exchange completed against a loop that has ended is the LAST one it will serve"
        );
        for stalled in [
            BrokerProbe::TimedOut,
            BrokerProbe::ConnectFailed("no such file".to_owned()),
            BrokerProbe::UnexpectedAnswer("a rejection".to_owned()),
        ] {
            assert_eq!(
                launch_broker_layer(true, &stalled).finding,
                LayerFinding::Faulted,
                "a running accept loop whose exchange did not complete is not healthy: {stalled:?}"
            );
        }
        // The `exercised` arm has to say what it did NOT exercise, because the
        // word is the thing this layer got wrong before.
        assert!(
            launch_broker_layer(true, &BrokerProbe::Exchanged)
                .detail
                .contains("token lookup is NOT on this path"),
            "the exercised arm must name the step the probe skips"
        );

        // Pool. No pool configured is NOT a fault, and it is not `unproven`
        // either: a daemon booted without `--path-b-parent` declined a feature,
        // and there is nothing under this layer whose health could be in
        // question. It is vacuous, and vacuous folds to pass.
        let one_instance = vec!["pmux-pool-0".to_owned()];
        let pool_live = diagnose_live(&["pmux-pool-0"]);
        let unconfigured = pool_layer(None, &one_instance, Some(&pool_live));
        assert_eq!(unconfigured.finding, LayerFinding::NothingToExercise);
        assert_eq!(
            unconfigured.outcome,
            ProbeOutcome::Pass,
            "a daemon that was never asked to run Path B is not unprovable for not running it"
        );
        let subject = |mutate: fn(&mut PoolSubject)| {
            let mut subject = PoolSubject {
                pool_size: 15,
                declared_warm: 0,
                census: crate::pool::PoolCensus {
                    live: 2,
                    idle: 2,
                    in_flight: 0,
                    clearing: 0,
                    leased: 0,
                    reserved: 0,
                    tearing_down: 0,
                    leaked: 0,
                    capacity: 15,
                    halted: None,
                },
                conversation_leases: Vec::new(),
            };
            mutate(&mut subject);
            subject
        };
        assert_eq!(
            pool_layer(Some(&subject(|_| {})), &one_instance, Some(&pool_live)).finding,
            LayerFinding::Exercised
        );
        let cold = pool_layer(
            Some(&subject(|subject| subject.census.live = 0)),
            &[],
            Some(&pool_live),
        );
        assert_eq!(
            cold.finding,
            LayerFinding::NothingToExercise,
            "an empty pool nobody declared a floor for is a capacity fact, not a fault"
        );
        assert_eq!(cold.outcome, ProbeOutcome::Pass);
        assert!(
            cold.detail.contains("rather than a fault"),
            "{}",
            cold.detail
        );
        // The bug class, as a string assertion, because it SHIPPED as a string:
        // this arm closed with "and the next call of any class mints one", a
        // claim the predicate never tested and which was false in the measured
        // state that produced it.
        assert!(
            !cold.detail.contains("mints one"),
            "the vacuous arm promised a mint it did not test: {}",
            cold.detail
        );
        assert_eq!(
            pool_layer(
                Some(&subject(|subject| subject.census.leaked = 1)),
                &one_instance,
                Some(&pool_live)
            )
            .finding,
            LayerFinding::Faulted,
            "a leaked slot is permanent capacity loss and a page"
        );
        assert_eq!(
            pool_layer(
                Some(&subject(
                    |subject| subject.census.halted = Some("wrong_local_command")
                )),
                &one_instance,
                Some(&pool_live),
            )
            .finding,
            LayerFinding::Faulted
        );

        // Compatibility profile, every arm. The vacuous arm is the one that
        // made a correct Path A daemon permanently unprovable: no pool means
        // nothing on the daemon needs a promoted cell, so the registry is a
        // question with no subject rather than a subject with no answer.
        assert_eq!(
            compatibility_layer(0, Some(&refused_pool_claude())).finding,
            LayerFinding::Faulted,
            "with a pool, every mint is RequireTested and an empty registry fails all of them"
        );
        // The pool decides whether there is a subject; the COUNT never does.
        // `compatibility_layer(1, false)` asserted `Exercised` until promotion
        // shipped, and that pairing is the bug class: the finding says the
        // layer was exercised and the predicate only asked whether a list was
        // non-empty. Nothing exercises a compatibility cell on a daemon with no
        // pool -- and with `PROMOTED_PROFILES` non-empty, EVERY supported host
        // now holds one, so the old arm would have flipped every Path A daemon
        // to `exercised` without one more thing being exercised. Both counts
        // are asserted together so the invariant is the pairing, not a number.
        for admitted in [0, 1, 2] {
            let path_a = compatibility_layer(admitted, None);
            assert_eq!(
                path_a.finding,
                LayerFinding::NothingToExercise,
                "a pool-less daemon holding {admitted} cell(s) exercised none of them"
            );
            assert_eq!(
                path_a.outcome,
                ProbeOutcome::Pass,
                "a daemon that needs no promoted cell is not unprovable for not having one"
            );
            assert!(
                path_a.detail.contains("no stateless pool is configured"),
                "the vacuous arm must say WHY nothing needs a cell: {}",
                path_a.detail
            );
        }
        assert_eq!(
            compatibility_layer(2, Some(&admitted_pool_claude())).finding,
            LayerFinding::Exercised
        );

        // THE PAIRING `pmux doctor` SHIPPED WITH. A daemon holding a cell for
        // this platform, running a Claude no cell names: the count alone reads
        // `exercised`, the version reads `faulted`, and it is the version the
        // next `pmux ask` is refused over.
        let refused = refused_pool_claude();
        let PoolClaudeAdmission::Refused {
            version: refused_version,
            ..
        } = &refused
        else {
            panic!("the fixture is a refusal");
        };
        let stale = compatibility_layer(1, Some(&refused));
        assert_eq!(stale.finding, LayerFinding::Faulted);
        assert_eq!(stale.outcome, ProbeOutcome::Fail);
        for owed in [
            // DERIVED from the fixture, which is derived from the promoted
            // range. `2.1.223` was written here as a literal and stopped being
            // a refusal the day the range reached 2.1.226 -- at which point
            // this loop was asserting that a refusal names a version pmux now
            // admits.
            refused_version.as_str(),
            "--tested-claude-profile",
            "unsupported_claude_version",
        ] {
            assert!(
                stale.detail.contains(owed),
                "a refusal an operator has to act on must name {owed}: {}",
                stale.detail
            );
        }
        // An executable that could not be asked is UNPROVEN, never healthy and
        // never a fault: nothing was established either way.
        let unreadable = compatibility_layer(1, Some(&unreadable_pool_claude()));
        assert_eq!(unreadable.finding, LayerFinding::NotEstablished);
        assert_eq!(unreadable.outcome, ProbeOutcome::Unproven);
        assert!(
            unreadable.detail.contains("/usr/local/bin/claude"),
            "an unreadable executable must be NAMED, or nobody knows which one: {}",
            unreadable.detail
        );

        // A pool instance the sidecar does not report is a fault, and the
        // report names no instance while saying so.
        let missing = pool_layer(
            Some(&subject(|_| {})),
            &one_instance,
            Some(&diagnose_live(&[])),
        );
        assert_eq!(missing.finding, LayerFinding::Faulted);
        assert!(
            !missing.detail.contains("pmux-pool-0")
                && !serde_json::to_string(&missing.evidence)
                    .unwrap()
                    .contains("pmux-pool-0"),
            "the pool layer named an instance: {missing:?}"
        );

        // A control-plane probe that never completed leaves this layer's one
        // external question unanswered, and unanswered is UNPROVEN.
        //
        // MEASURED: after the private rmux sidecar was SIGKILLed under fifteen
        // concurrent callers, this returned `exercised` -- pass -- with
        // `instance_terminals_present: null` and one instance registered, over
        // a detail string that said "no instance terminal was looked for". The
        // two sibling layers built from the same probe, `private_runtime` and
        // `performance`, both report `not_established` in that state; this one
        // was the outlier, and its arm was reached by a condition
        // (`is_some_and`) that reads false both when the answer is "all
        // present" and when there is no answer at all.
        let unprobed = pool_layer(Some(&subject(|_| {})), &one_instance, None);
        assert_eq!(
            unprobed.finding,
            LayerFinding::NotEstablished,
            "a layer that looked for nothing has not exercised anything: {unprobed:?}"
        );
        assert_eq!(unprobed.outcome, ProbeOutcome::Unproven);
        assert!(
            unprobed.detail.contains("was not established"),
            "{}",
            unprobed.detail
        );
        assert_eq!(
            private_runtime_layer(Err(&ControlPlaneFault::Unreachable), 5).finding,
            unprobed.finding,
            "three layers are built from one probe; they must not disagree about whether it \
             answered"
        );
        // ...and an empty pool with nothing declared is still vacuous rather
        // than unproven, probe or no probe: there is no instance whose terminal
        // could be missing.
        assert_eq!(
            pool_layer(Some(&subject(|subject| subject.census.live = 0)), &[], None).finding,
            LayerFinding::NothingToExercise
        );

        // Sessions. Zero sessions is `nothing_to_exercise`, which is `pass`.
        // The surface still refuses to say "I proved every one of zero sessions
        // healthy" -- the finding says the opposite of that, in a word of its
        // own, and the detail names what was absent. What it no longer says is
        // "I could not establish the sessions layer", which was false and which
        // made every pure Path B daemon permanently unprovable.
        let no_sessions = sessions_layer(&[]);
        assert_eq!(no_sessions.finding, LayerFinding::NothingToExercise);
        assert_eq!(no_sessions.outcome, ProbeOutcome::Pass);
        assert!(
            no_sessions.detail.contains("capacity fact"),
            "{}",
            no_sessions.detail
        );
        let (session_id, generation_id, _) = diagnose_session(9, "pmux-one");
        let probe = |finding| {
            vec![SessionProbe::new(
                session_id,
                generation_id,
                finding,
                None,
                Some(true),
            )]
        };
        assert_eq!(
            sessions_layer(&probe(SessionFinding::TerminalPresent)).finding,
            LayerFinding::Exercised
        );
        assert_eq!(
            sessions_layer(&probe(SessionFinding::TerminalMissing)).finding,
            LayerFinding::Faulted
        );
        assert_eq!(
            sessions_layer(&probe(SessionFinding::SessionActorUnresponsive)).finding,
            LayerFinding::NotEstablished
        );

        // Performance. A failed exchange's duration measures a failure, not a
        // latency, so nothing is claimed.
        assert_eq!(
            performance_layer(Err(&ControlPlaneFault::Unreachable), 20_000, 10_000, &[]).finding,
            LayerFinding::NotEstablished
        );
        assert_eq!(
            performance_layer(Ok(&live), 5, 10_000, &[]).finding,
            LayerFinding::Exercised
        );
        assert_eq!(
            performance_layer(Ok(&live), 10_001, 10_000, &[]).finding,
            LayerFinding::Faulted,
            "one millisecond over the envelope the runtime itself enforces is over it"
        );
        assert_eq!(
            performance_layer(Ok(&live), 10_000, 10_000, &[]).finding,
            LayerFinding::Exercised,
            "the envelope is what the exchange is held TO, so landing on it is inside it; \
             read as `>=` this reports a healthy daemon as faulted at the one duration the \
             envelope names"
        );
        assert_eq!(
            performance_layer(
                Ok(&live),
                5,
                10_000,
                &probe(SessionFinding::SessionActorUnresponsive)
            )
            .finding,
            LayerFinding::Faulted,
            "a fast control plane does not excuse an actor that never answered"
        );
    }

    /// Fifteen leased Pi cells used to read as "15 live … 0 idle, 0 serving,
    /// 0 clearing, 0 reserved" — leased and tearing_down were in the JSON
    /// evidence and missing from the sentence an operator acts on.
    #[test]
    fn pool_exercised_sentence_names_leased_and_tearing_down() {
        let pool_live = diagnose_live(&["pmux-pool-0", "pmux-pool-1"]);
        let terminals = vec!["pmux-pool-0".to_owned(), "pmux-pool-1".to_owned()];
        let subject = PoolSubject {
            pool_size: 15,
            declared_warm: 0,
            census: crate::pool::PoolCensus {
                live: 2,
                idle: 0,
                in_flight: 0,
                clearing: 0,
                leased: 1,
                reserved: 0,
                tearing_down: 1,
                leaked: 0,
                capacity: 15,
                halted: None,
            },
            conversation_leases: vec![crate::pool::ConversationLease {
                conversation_id: "pi-root".to_owned(),
                cell: "s0e1".to_owned(),
                state: "leased".to_owned(),
            }],
        };
        let layer = pool_layer(Some(&subject), &terminals, Some(&pool_live));
        assert_eq!(layer.finding, LayerFinding::Exercised);
        assert!(
            layer.detail.contains("1 holding a conversation lease"),
            "{}",
            layer.detail
        );
        assert!(
            layer.detail.contains("1 tearing down"),
            "{}",
            layer.detail
        );
        assert_eq!(
            layer.evidence["leased"],
            1,
            "structured evidence already published leased; the sentence must match it"
        );
        assert_eq!(layer.evidence["conversation_leases"].as_array().unwrap().len(), 1);
        assert_eq!(layer.evidence["tearing_down"], 1);
    }

    /// A correct Path B daemon rolls up HEALTHY, and a genuinely unproven layer
    /// still does not.
    ///
    /// MEASURED before the encoding was split, against a live daemon with a
    /// warm pool of two idle instances and every other layer `pass`:
    ///
    /// ```text
    /// status: unproven
    /// unproven: ['sessions: the registry holds no sessions, so no session was exercised']
    /// pool | exercised | pass | the stateless pool holds 2 live instance(s) ...
    /// $ pmux doctor; echo $?
    /// pmux: doctor could not prove every check it ran
    /// 1
    /// ```
    ///
    /// It is PERMANENT, not transient: `226e336` removed pool instances from
    /// `DaemonDiagnosis::sessions` on purpose -- the session id is the one name
    /// no client may learn -- so a daemon serving only `pmux ask` has
    /// `sessions: []` on every probe it will ever answer. Any CI or liveness
    /// wiring of `pmux doctor` failed forever, and a genuine `unproven` was
    /// indistinguishable from the permanent one.
    ///
    /// The second half of this test is the property the tree exists for and is
    /// what stops the fix from being "make everything pass": a layer that
    /// genuinely could not be established still rolls up `unproven`.
    #[test]
    fn a_correct_path_b_daemon_rolls_up_healthy_and_a_real_gap_still_does_not() {
        let pool_live = diagnose_live(&["pmux-pool-0", "pmux-pool-1"]);
        let pool_terminals = vec!["pmux-pool-0".to_owned(), "pmux-pool-1".to_owned()];
        // The daemon the live measurement was taken against: a DECLARED warm
        // floor of two, held. The floor is in the fixture and not merely the
        // count, because a fixture that declares nothing cannot fail when the
        // declared-floor arm regresses.
        let subject = PoolSubject {
            pool_size: 2,
            declared_warm: 2,
            census: crate::pool::PoolCensus {
                live: 2,
                idle: 2,
                in_flight: 0,
                clearing: 0,
                leased: 0,
                reserved: 0,
                tearing_down: 0,
                leaked: 0,
                capacity: 2,
                halted: None,
            },
            conversation_leases: Vec::new(),
        };

        // The tree a healthy Path B daemon actually emits: every layer built by
        // its own producer, `sessions` empty because pool instances are never
        // registered as caller sessions.
        let path_b_layers = |sessions: &[SessionProbe]| {
            vec![
                HealthLayer::new(
                    HealthLayerName::Configuration,
                    LayerFinding::Exercised,
                    "booted",
                    json!({}),
                ),
                control_plane_layer(Ok(&pool_live), 1),
                private_runtime_layer(Ok(&pool_live), 1),
                launch_broker_layer(true, &BrokerProbe::Exchanged),
                compatibility_layer(1, Some(&admitted_pool_claude())),
                pool_layer(Some(&subject), &pool_terminals, Some(&pool_live)),
                sessions_layer(sessions),
                performance_layer(Ok(&pool_live), 1, 10_000, sessions),
            ]
        };

        let healthy = DaemonDiagnosis {
            layers: path_b_layers(&[]),
            runtime: RuntimeProbe::new(RuntimeFinding::PrivateRuntimeResponsive, 1, Some(2)),
            sessions: Vec::new(),
        };
        assert!(
            healthy.missing_layers().is_empty(),
            "the fixture must be a complete tree or it proves nothing about the fold"
        );
        assert_eq!(
            healthy.outcome(),
            ProbeOutcome::Pass,
            "a daemon with a warm pool, a live sidecar and no caller sessions is healthy; \
             layers: {:?}",
            healthy
                .layers
                .iter()
                .map(|layer| (layer.layer, layer.finding, layer.outcome))
                .collect::<Vec<_>>()
        );

        // And the guard. One session that left the registry mid-probe is a
        // layer that HAS a subject and no answer, which is the finding that
        // must never roll up as healthy. `SessionClosedDuringProbe` rather than
        // `SessionActorUnresponsive` because the latter also faults the
        // performance layer, and a fixture whose fold is decided by a second
        // layer proves nothing about the first.
        let (session_id, generation_id, _) = diagnose_session(31, "pmux-one");
        let vanished = vec![SessionProbe::new(
            session_id,
            generation_id,
            SessionFinding::SessionClosedDuringProbe,
            None,
            Some(true),
        )];
        let unproven = DaemonDiagnosis {
            layers: path_b_layers(&vanished),
            runtime: RuntimeProbe::new(RuntimeFinding::PrivateRuntimeResponsive, 1, Some(2)),
            sessions: vanished,
        };
        assert!(
            unproven
                .layers
                .iter()
                .filter(|layer| layer.layer != HealthLayerName::Sessions)
                .all(|layer| layer.outcome == ProbeOutcome::Pass),
            "the sessions layer must be the only thing deciding this fold"
        );
        assert_eq!(
            unproven
                .layer(HealthLayerName::Sessions)
                .map(|layer| layer.finding),
            Some(LayerFinding::NotEstablished),
            "an actor that never answered is not `nothing to exercise`"
        );
        assert_eq!(
            unproven.outcome(),
            ProbeOutcome::Unproven,
            "splitting the encoding must not make a real gap readable as health"
        );

        // A layer nobody reported at all is still `unproven`, which is the
        // other half of the same guarantee and is independent of the finding
        // set entirely.
        let silent = DaemonDiagnosis {
            layers: Vec::new(),
            runtime: RuntimeProbe::new(RuntimeFinding::PrivateRuntimeResponsive, 1, Some(2)),
            sessions: Vec::new(),
        };
        assert_eq!(silent.outcome(), ProbeOutcome::Unproven);
    }

    /// A correct Path A daemon -- no promoted profile, no pool -- rolls up
    /// HEALTHY.
    ///
    /// MEASURED against a real daemon booted with neither
    /// `--tested-claude-profile` nor `--path-b-parent`, which is a supported
    /// configuration exercised by
    /// `full_stack::start_without_tested_profile` and which served a real turn
    /// through the same socket seconds after this report:
    ///
    /// ```text
    /// status: unproven
    /// unproven: ['compatibility profile: no Claude compatibility cell is admitted, ...']
    /// $ pmux doctor; echo $?
    /// pmux: doctor could not prove every check it ran
    /// 1
    /// ```
    ///
    /// Permanent, like the `sessions: []` case before it: that daemon admits no
    /// cell on every probe it will ever answer, so `pmux doctor` exited 1 on it
    /// forever for having declined a feature. Nothing on it requires a promoted
    /// cell -- the pool is what makes one mandatory, and there is no pool -- so
    /// the layer has no subject rather than an unreachable one.
    #[test]
    fn a_correct_path_a_daemon_with_no_profile_and_no_pool_rolls_up_healthy() {
        let live = diagnose_live(&[]);
        let path_a = DaemonDiagnosis {
            layers: vec![
                HealthLayer::new(
                    HealthLayerName::Configuration,
                    LayerFinding::Exercised,
                    "booted",
                    json!({}),
                ),
                control_plane_layer(Ok(&live), 1),
                private_runtime_layer(Ok(&live), 1),
                launch_broker_layer(true, &BrokerProbe::Exchanged),
                compatibility_layer(0, None),
                pool_layer(None, &[], Some(&live)),
                sessions_layer(&[]),
                performance_layer(Ok(&live), 1, 10_000, &[]),
            ],
            runtime: RuntimeProbe::new(RuntimeFinding::PrivateRuntimeResponsive, 1, Some(0)),
            sessions: Vec::new(),
        };
        assert!(
            path_a.missing_layers().is_empty(),
            "the fixture must be a complete tree or it proves nothing about the fold"
        );
        assert_eq!(
            path_a.outcome(),
            ProbeOutcome::Pass,
            "a daemon that declined both Path B and a promoted profile is not unprovable for \
             declining them; layers: {:?}",
            path_a
                .layers
                .iter()
                .map(|layer| (layer.layer, layer.finding, layer.outcome))
                .collect::<Vec<_>>()
        );
    }

    /// A DECLARED warm floor the pool is holding none of folds NOT healthy.
    ///
    /// The regression the second encoding introduced, and the reason the empty
    /// set is not the question. MEASURED against a live daemon booted
    /// `--path-b-pool-size 2 --path-b-warm claude-sonnet-5/low=2`, whose Claude
    /// executable was then replaced and whose two instances were killed:
    ///
    /// ```text
    /// $ pmux ask --model sonnet --effort low 'Say OK.'      (x6)
    /// pmux: pmuxd error code=DaemonLost message="private rmux lease was lost ..."
    /// $ pmux doctor; echo $?
    /// status healthy   errors []   unproven []
    /// pool  pass  nothing_to_exercise
    ///   "...holds none, so there was nothing to exercise ... and the next call
    ///    of any class mints one"
    /// 0
    /// ```
    ///
    /// Six consecutive refusals under a `pass` whose own detail promised the
    /// next call would mint. Nothing else in the daemon records the condition:
    /// `spawn_rewarm` discards a failed mint with no log and no counter, so a
    /// health tree that calls it vacuous is the last word on it.
    ///
    /// The same census with NO floor declared is the control, and it must still
    /// pass -- otherwise this is the previous encoding again, one arm along.
    #[test]
    fn a_declared_warm_floor_the_pool_holds_none_of_is_a_fault_not_a_vacancy() {
        let live = diagnose_live(&[]);
        let drained = |declared_warm| PoolSubject {
            pool_size: 2,
            declared_warm,
            census: crate::pool::PoolCensus {
                live: 0,
                idle: 0,
                in_flight: 0,
                clearing: 0,
                leased: 0,
                reserved: 0,
                tearing_down: 0,
                leaked: 0,
                capacity: 2,
                halted: None,
            },
            conversation_leases: Vec::new(),
        };
        let tree = |declared_warm| DaemonDiagnosis {
            layers: vec![
                HealthLayer::new(
                    HealthLayerName::Configuration,
                    LayerFinding::Exercised,
                    "booted",
                    json!({}),
                ),
                control_plane_layer(Ok(&live), 1),
                private_runtime_layer(Ok(&live), 1),
                launch_broker_layer(true, &BrokerProbe::Exchanged),
                compatibility_layer(1, Some(&admitted_pool_claude())),
                pool_layer(Some(&drained(declared_warm)), &[], Some(&live)),
                sessions_layer(&[]),
                performance_layer(Ok(&live), 1, 10_000, &[]),
            ],
            runtime: RuntimeProbe::new(RuntimeFinding::PrivateRuntimeResponsive, 1, Some(0)),
            sessions: Vec::new(),
        };

        let declared = tree(2);
        assert!(declared.missing_layers().is_empty());
        let layer = declared
            .layer(HealthLayerName::Pool)
            .expect("the pool layer is present");
        assert_eq!(layer.finding, LayerFinding::Faulted);
        assert_eq!(layer.outcome, ProbeOutcome::Fail);
        assert!(
            layer
                .detail
                .contains("warm floor of 2 instance(s) is declared"),
            "the fault must name the floor it is measuring against: {}",
            layer.detail
        );
        assert!(
            !layer.detail.contains("mints one"),
            "no arm of this layer may promise a mint it did not test: {}",
            layer.detail
        );
        assert_eq!(
            layer.evidence.get("declared_warm"),
            Some(&json!(2)),
            "the evidence must carry the input the finding turned on: {layer:?}"
        );
        assert_eq!(
            declared.outcome(),
            ProbeOutcome::Fail,
            "a daemon holding none of a declared warm floor must not read as healthy, and this \
             one folds to the `unhealthy` that exits 1; layers: {:?}",
            declared
                .layers
                .iter()
                .map(|layer| (layer.layer, layer.finding, layer.outcome))
                .collect::<Vec<_>>()
        );

        // The control: the identical census with nothing declared. An empty
        // pool nobody asked to hold anything is vacuous, and vacuous passes.
        let undeclared = tree(0);
        let layer = undeclared
            .layer(HealthLayerName::Pool)
            .expect("the pool layer is present");
        assert_eq!(layer.finding, LayerFinding::NothingToExercise);
        assert_eq!(layer.evidence.get("declared_warm"), Some(&json!(0)));
        assert_eq!(
            undeclared.outcome(),
            ProbeOutcome::Pass,
            "a cold pool with no declared floor is a capacity fact, not a fault"
        );
    }

    /// No pool instance is named anywhere in a diagnosis.
    ///
    /// MEASURED live: the FIRST health tree this daemon ever produced listed a
    /// pool instance's `session_id` and `generation_id` in
    /// `DaemonDiagnosis::sessions`, and reported it as "left the registry while
    /// the probe was running" -- because the caller-only resolver had refused
    /// it. Two defects in one entry: the report was wrong, and it published the
    /// one name `SessionOwner` exists to hide, in a report any client may ask
    /// for. A caller who can learn a resource's name is one step from aliasing
    /// one.
    ///
    /// The pool's instances are still probed. `pool_layer` counts how many of
    /// them the sidecar reports and never which, and this test walks the whole
    /// serialized report for the name.
    #[test]
    fn a_diagnosis_never_names_a_pool_instance() {
        let subject = PoolSubject {
            pool_size: 15,
            declared_warm: 1,
            census: crate::pool::PoolCensus {
                live: 2,
                idle: 1,
                in_flight: 1,
                clearing: 1,
                leased: 0,
                reserved: 0,
                tearing_down: 0,
                leaked: 0,
                capacity: 15,
                halted: None,
            },
            conversation_leases: Vec::new(),
        };
        let pool_terminals = vec!["pmux-pool-slot0".to_owned(), "pmux-pool-slot1".to_owned()];
        let live = diagnose_live(&["pmux-pool-slot0", "pmux-pool-slot1", "pmux-caller"]);
        let (caller_id, caller_generation, caller_name) = diagnose_session(21, "pmux-caller");

        let mut diagnosis = build_diagnosis(
            &[(caller_id, caller_generation, caller_name)],
            Ok(&live),
            3,
            true,
            &diagnose_observed(&[(
                caller_id,
                ActorStateObservation::Reported(SessionState::Ready),
            )]),
        );
        diagnosis.layers = vec![pool_layer(Some(&subject), &pool_terminals, Some(&live))];

        // The caller's session IS named -- that is the whole point of the
        // per-session list, and its absence would be the opposite defect.
        assert_eq!(diagnosis.sessions.len(), 1);
        assert_eq!(diagnosis.sessions[0].session_id, caller_id);

        let rendered = serde_json::to_string(&diagnosis).expect("a diagnosis serializes");
        for named in &pool_terminals {
            assert!(
                !rendered.contains(named.as_str()),
                "the diagnosis names the pool instance {named}: {rendered}"
            );
        }
        // And the pool layer still says something true about them.
        let layer = diagnosis
            .layer(HealthLayerName::Pool)
            .expect("the pool layer is present");
        assert_eq!(layer.finding, LayerFinding::Exercised);
        assert_eq!(
            layer
                .evidence
                .get("registered_instances")
                .and_then(serde_json::Value::as_u64),
            Some(2)
        );
        assert_eq!(
            layer
                .evidence
                .get("instance_terminals_present")
                .and_then(serde_json::Value::as_u64),
            Some(2),
            "the sidecar reported both, counted and not named"
        );
    }

    /// Every layer's evidence is representable within protocol v1.
    ///
    /// MEASURED, not reasoned: the first live probe of the health tree got
    /// `invalid JSON frame: opaque JSON integer is outside the signed
    /// safe-integer range` and NO diagnosis at all, because the configuration
    /// layer put a `u64` FNV fingerprint in its evidence. `evidence` is opaque
    /// JSON, and protocol v1 refuses an opaque integer outside the signed safe
    /// range -- so one layer's evidence cost the entire report, including every
    /// layer that was fine.
    ///
    /// The runtime guard in `diagnose` replaces such a layer rather than
    /// dropping it. This test is the compile-time-adjacent half: every layer a
    /// pure builder can produce is checked here, so a new one that puts a raw
    /// `u64` in its evidence fails in the default suite instead of on a socket.
    #[test]
    fn every_layer_serializes_within_protocol_v1() {
        let live = diagnose_live(&["pmux-one"]);
        let halted = PoolSubject {
            pool_size: 15,
            declared_warm: 0,
            census: crate::pool::PoolCensus {
                live: 1,
                idle: 1,
                in_flight: 0,
                clearing: 0,
                leased: 0,
                reserved: 0,
                tearing_down: 0,
                leaked: 0,
                capacity: 15,
                halted: Some("wrong_local_command"),
            },
            conversation_leases: Vec::new(),
        };
        let drained = |declared_warm| PoolSubject {
            pool_size: 15,
            declared_warm,
            census: crate::pool::PoolCensus {
                live: 0,
                idle: 0,
                in_flight: 0,
                clearing: 0,
                leased: 0,
                reserved: 0,
                tearing_down: 0,
                leaked: 0,
                capacity: 15,
                halted: None,
            },
            conversation_leases: Vec::new(),
        };
        let (session_id, generation_id, _) = diagnose_session(11, "pmux-one");
        let sessions = vec![SessionProbe::new(
            session_id,
            generation_id,
            SessionFinding::TerminalPresent,
            Some(SessionState::Ready),
            Some(true),
        )];
        let layers = [
            control_plane_layer(Ok(&live), 1),
            control_plane_layer(Err(&ControlPlaneFault::Unreachable), u64::from(u32::MAX)),
            private_runtime_layer(Ok(&live), 1),
            private_runtime_layer(Err(&ControlPlaneFault::Unresponsive), 1),
            private_runtime_layer(Err(&ControlPlaneFault::Refused), 1),
            private_runtime_layer(Err(&ControlPlaneFault::Unreachable), 1),
            launch_broker_layer(true, &BrokerProbe::Exchanged),
            launch_broker_layer(false, &BrokerProbe::ConnectRefused),
            compatibility_layer(0, None),
            compatibility_layer(0, Some(&refused_pool_claude())),
            compatibility_layer(3, Some(&admitted_pool_claude())),
            compatibility_layer(3, Some(&unreadable_pool_claude())),
            pool_layer(None, &[], None),
            pool_layer(Some(&halted), &["pmux-pool-0".to_owned()], Some(&live)),
            pool_layer(Some(&drained(0)), &[], Some(&live)),
            pool_layer(Some(&drained(2)), &[], Some(&live)),
            sessions_layer(&[]),
            sessions_layer(&sessions),
            performance_layer(Ok(&live), 1, 10_000, &sessions),
            performance_layer(Err(&ControlPlaneFault::Refused), 1, 10_000, &sessions),
        ];
        for layer in layers {
            assert!(
                validate_v1_serializable(&layer).is_ok(),
                "{:?}/{:?} carries evidence protocol v1 cannot represent: {}",
                layer.layer,
                layer.finding,
                serde_json::to_string(&layer.evidence).unwrap_or_default()
            );
        }
    }

    /// Every layer states what it exercised, whatever it found.
    ///
    /// A `pass` with an empty `detail` is the boolean this tree replaced, one
    /// level down: "healthy" with nothing behind it. Checked over every builder
    /// and every finding rather than over one example.
    #[test]
    fn every_layer_states_what_it_exercised_even_when_it_passed() {
        let live = diagnose_live(&[]);
        let pool = |declared_warm, live_instances| PoolSubject {
            pool_size: 15,
            declared_warm,
            census: crate::pool::PoolCensus {
                live: live_instances,
                idle: live_instances,
                in_flight: 0,
                clearing: 0,
                leased: 0,
                reserved: 0,
                tearing_down: 0,
                leaked: 0,
                capacity: 15,
                halted: None,
            },
            conversation_leases: Vec::new(),
        };
        let layers = [
            control_plane_layer(Ok(&live), 1),
            control_plane_layer(Err(&ControlPlaneFault::Unreachable), 1),
            private_runtime_layer(Ok(&live), 1),
            private_runtime_layer(Err(&ControlPlaneFault::Refused), 1),
            private_runtime_layer(Err(&ControlPlaneFault::Unreachable), 1),
            launch_broker_layer(true, &BrokerProbe::Exchanged),
            launch_broker_layer(false, &BrokerProbe::ConnectRefused),
            compatibility_layer(0, None),
            compatibility_layer(0, Some(&refused_pool_claude())),
            compatibility_layer(1, Some(&admitted_pool_claude())),
            compatibility_layer(1, Some(&unreadable_pool_claude())),
            pool_layer(None, &[], None),
            pool_layer(Some(&pool(0, 1)), &["pmux-pool-0".to_owned()], Some(&live)),
            pool_layer(Some(&pool(0, 0)), &[], Some(&live)),
            pool_layer(Some(&pool(2, 0)), &[], Some(&live)),
            sessions_layer(&[]),
            performance_layer(Ok(&live), 1, 10_000, &[]),
            performance_layer(Err(&ControlPlaneFault::Unreachable), 1, 10_000, &[]),
        ];
        for layer in layers {
            assert!(
                !layer.detail.is_empty(),
                "{:?}/{:?} passed without saying what it exercised",
                layer.layer,
                layer.finding
            );
            assert_eq!(
                layer.outcome,
                layer.finding.outcome(),
                "{:?} published an outcome its finding does not derive",
                layer.layer
            );
        }
    }

    /// A read that never came back must produce an entry, not a gap. Dropping
    /// the session would delete it from the report for the one reason that
    /// most deserves a line.
    #[test]
    fn a_session_with_no_state_observation_is_reported_rather_than_omitted() {
        let (session_id, generation_id, name) = diagnose_session(5, "pmux-quiet");
        let live = diagnose_live(&["pmux-quiet"]);
        let diagnosis = build_diagnosis(
            &[(session_id, generation_id, name)],
            Ok(&live),
            2,
            true,
            &HashMap::new(),
        );
        assert_eq!(diagnosis.sessions.len(), 1);
        assert_eq!(
            diagnosis.sessions[0].finding,
            SessionFinding::SessionActorUnresponsive
        );
        assert_eq!(diagnosis.sessions[0].state, None);
        assert_eq!(diagnosis.outcome(), ProbeOutcome::Unproven);
    }

    // -----------------------------------------------------------------------
    // THE DIFFERENTIAL ENTRY-PATH TEST
    //
    // Leaks 1, 2 and 3 were each the same sentence -- THIS PATH LACKS THE
    // GUARD -- and each was found by reproducing one path after the guard had
    // been written for another. `start_session`, `run_once`, a stored agent and
    // the pool each build the request that reaches admission by their own
    // route, and every one of them resolves the configuration root by a
    // DIFFERENT mechanism: a caller's `environment.set`, the same value carried
    // inside a `RunOnceRequest`, a stored agent's own `set`, and the pool's
    // `config_isolation` root. A test that drives one of them says nothing
    // about the other three.
    //
    // So this drives ONE logical operation -- "start against a directory a live
    // minified cell holds, spelled like this" -- through every route, and
    // asserts the four answers are the SAME VALUE. It never asserts a
    // particular answer per path, because a rule that refuses on three paths
    // and admits on the fourth is exactly the shape of every leak in the
    // family, and only a comparison can see it.
    //
    // The route list is DERIVED. A hand-written one is the bug class this tree
    // keeps finding: it is right on the day it is written and silently narrows
    // to nothing afterwards. `derived_admission_routes` reads this crate's own
    // sources and reports every route that can reach `admit_bound_resources`;
    // `ADMISSION_ROUTES` must classify every one of them, as a route this test
    // DRIVES or as one that carries no start, with the reason. A route that
    // appears tomorrow and is in neither column fails the test by name.
    // -----------------------------------------------------------------------

    /// The source scanner is [`crate::source_scan`], shared with
    /// [`crate::driver_io`]'s rendering register so the two derivations cannot
    /// come to disagree about what production code is.
    #[cfg(unix)]
    use crate::source_scan::{DeclaredFunction, calls, declared_functions, without_comment_lines};

    /// This route's identifier, as the derivation prints it.
    #[cfg(unix)]
    fn route_id(function: &DeclaredFunction) -> String {
        format!("{}::{}", function.file, function.name)
    }

    /// Every function in `native.rs` from which `admit_bound_resources` is
    /// reachable, with whether a caller outside the module can reach it.
    ///
    /// Closed over `native.rs` ALONE, and that is exact rather than a
    /// convenience: `admit_bound_resources` is a private free function in this
    /// module, so every route to it anywhere in the daemon must first land on a
    /// function declared here. The externally visible members of this set are
    /// therefore the complete list of doors into admission, derived rather than
    /// remembered.
    #[cfg(unix)]
    fn native_admission_closure() -> BTreeMap<String, bool> {
        let declared: Vec<DeclaredFunction> = declared_functions()
            .into_iter()
            .filter(|function| function.file == "native.rs")
            .collect();
        let mut reached = BTreeMap::from([("admit_bound_resources".to_owned(), false)]);
        loop {
            let mut grew = false;
            for function in &declared {
                if reached.contains_key(&function.name) {
                    continue;
                }
                if reached.keys().any(|name| calls(&function.body, name)) {
                    reached.insert(function.name.clone(), function.externally_visible);
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }
        reached
    }

    /// Every route that can put a start in front of `admit_bound_resources`.
    ///
    /// Three scans, each precise, whose union is the derived list:
    ///
    /// 1. **The wire.** Every variant of protocol v1's `Request` whose arm in
    ///    [`NativeService::dispatch`] calls something in
    ///    [`native_admission_closure`]. The variant list comes from the
    ///    protocol's own source, so a method added tomorrow is classified here
    ///    the day it lands.
    /// 2. **The crate.** Every function OUTSIDE `native.rs` that calls one of
    ///    the externally visible doors. This is how the pool arrives: nothing
    ///    in `dispatch`'s `RunStateless` arm mentions a `native.rs` name, and
    ///    `stateless.rs` reaches `start_session_owned` by its own route.
    /// 3. **The builders.** Every function anywhere in the crate that
    ///    constructs a `StartSessionRequest` literal. A route that BUILDS the
    ///    request decides the directories admission will judge, and it is
    ///    invisible to both scans above -- `agent::resolve_agent_start`
    ///    rewrites the environment a start is admitted under and calls no door
    ///    at all.
    #[cfg(unix)]
    fn derived_admission_routes() -> BTreeSet<String> {
        const PROTOCOL_SOURCE: &str = include_str!("../../protocol/src/v1.rs");

        let closure = native_admission_closure();
        let declared = declared_functions();
        let mut routes = BTreeSet::new();

        // (1) The wire.
        let dispatch = declared
            .iter()
            .find(|function| function.file == "native.rs" && function.name == "dispatch")
            .expect("native.rs declares dispatch");
        let body = without_comment_lines(&dispatch.body);
        let (_, rest) = PROTOCOL_SOURCE
            .split_once("pub enum Request {")
            .expect("protocol v1 declares Request");
        let (variants, _) = rest.split_once("\n}").expect("Request is terminated");
        let variants: Vec<String> = variants
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with("//") && !line.starts_with("#["))
            .map(|line| {
                line.chars()
                    .take_while(|value| value.is_alphanumeric() || *value == '_')
                    .collect()
            })
            .collect();
        assert!(
            variants.len() >= 16,
            "the Request variant scan found only {} variant(s)",
            variants.len()
        );
        let mut arms: Vec<(usize, &str)> = variants
            .iter()
            .map(|variant| {
                let needle = format!("Request::{variant}");
                let at = body.find(&needle).unwrap_or_else(|| {
                    panic!("dispatch has no arm for Request::{variant}");
                });
                (at, variant.as_str())
            })
            .collect();
        arms.sort_unstable();
        for (index, (at, variant)) in arms.iter().enumerate() {
            let end = arms.get(index + 1).map_or(body.len(), |(next, _)| *next);
            if closure.keys().any(|name| calls(&body[*at..end], name)) {
                routes.insert(format!("Request::{variant}"));
            }
        }

        // (2) The crate.
        let doors: Vec<&String> = closure
            .iter()
            .filter(|(_, visible)| **visible)
            .map(|(name, _)| name)
            .collect();
        for function in &declared {
            if function.file == "native.rs" {
                continue;
            }
            if doors.iter().any(|door| calls(&function.body, door)) {
                routes.insert(route_id(function));
            }
        }

        // (3) The builders.
        for function in &declared {
            if function.body.contains("StartSessionRequest {") {
                routes.insert(route_id(function));
            }
        }
        routes
    }

    /// How one entry path spells "start against these two directories".
    ///
    /// One signature for all four, because a differential over routes with
    /// different signatures is a differential over the adapters as well.
    #[cfg(unix)]
    type RouteBuilder = fn(&Path, &Path, SessionIdentity) -> StartSessionRequest;

    /// What one derived route is.
    #[cfg(unix)]
    enum Route {
        /// A route this test drives, naming the builder it drives it through.
        Driven(&'static str),
        /// A route that cannot put a start in front of admission, and why.
        CarriesNoStart(&'static str),
    }

    /// Every route [`derived_admission_routes`] can report, classified.
    ///
    /// This table is checked against the derivation in BOTH directions: a
    /// derived route with no row fails, and a row naming a route the derivation
    /// no longer reports fails too. The second direction is the one that keeps
    /// a driver from quietly testing a path that has been renamed out from
    /// under it.
    #[cfg(unix)]
    const ADMISSION_ROUTES: &[(&str, Route)] = &[
        // -- The four routes that reach admission with a start ---------------
        ("Request::StartSession", Route::Driven("caller_start")),
        ("Request::RunOnce", Route::Driven("run_once_start")),
        (
            "stateless.rs::launch_request_for",
            Route::Driven("pool_start"),
        ),
        (
            "agent.rs::resolve_agent_start",
            Route::Driven("agent_start"),
        ),
        // -- Routes the derivation reports that carry no start of their own --
        (
            "stateless.rs::start_session_pool",
            Route::CarriesNoStart(
                "forwards the request `launch_request_for` built, unchanged, and is driven \
                 through that builder",
            ),
        ),
        (
            "native.rs::placeholder_start_request",
            Route::CarriesNoStart(
                "the value `std::mem::replace` leaves behind for one statement; the next \
                 statement overwrites it, so it is never resolved and never admitted",
            ),
        ),
    ];

    /// The admission answer one route gave, as a value two routes can be
    /// compared on.
    ///
    /// The seed disposition is part of it. `Write` and `VerifyOnly` are the
    /// difference between pmux writing into a directory and pmux refusing to,
    /// so two routes that both "admitted" and disagreed about that have not
    /// agreed.
    #[cfg(unix)]
    #[derive(Clone, Debug, Eq, PartialEq)]
    enum AdmissionAnswer {
        Admitted(SeedDisposition),
        Refused(ErrorCode),
    }

    /// The exact prefix `start_session_owned_with_retention` runs between the
    /// resolved request and the admission answer, with no process anywhere.
    ///
    /// It is a copy of four lines of the funnel, and
    /// [`every_entry_path_that_reaches_admission_answers_the_alias_family_identically`]
    /// pins those four lines against the funnel's own source so the copy cannot
    /// drift. A copy is what makes the four routes comparable at all: the
    /// funnel needs an `Arc<NativeService>`, which needs a private runtime,
    /// which needs an rmux sidecar -- and a test that cannot be reached without
    /// a sidecar is a test the fast lane does not run.
    #[cfg(unix)]
    fn admission_answer(
        request: &StartSessionRequest,
        claims: &[LiveResourceClaim],
    ) -> AdmissionAnswer {
        let decide = || -> Result<SeedDisposition, ErrorBody> {
            let mut identity_request = request.clone();
            {
                let claude = require_resolved_launch_mut(&mut identity_request)?;
                claude.settings.clear();
                claude.mcp_configs.clear();
                claude.system_prompt = SystemPromptPolicy::Default;
            }
            let identity_resolved =
                resolve_claude_launch(&identity_request).map_err(map_launch_error)?;
            let config_root = effective_config_root(&identity_resolved)?;
            require_isolation_root_is_the_effective_root(
                request.config_isolation.as_ref(),
                &config_root,
            )?;
            admit_bound_resources(
                claims,
                &config_root,
                &identity_resolved.process.cwd,
                request.cell,
            )
        };
        match decide() {
            Ok(disposition) => AdmissionAnswer::Admitted(disposition),
            Err(error) => AdmissionAnswer::Refused(error.code),
        }
    }

    /// The one answer every route gave, or every answer given with the routes
    /// that gave it.
    ///
    /// PARTITIONED, rather than compared against a reference route, and that is
    /// a correction rather than a flourish. This reported "every route whose
    /// answer differs from the FIRST one" until the discrimination experiment
    /// was run against it: with `agent_start` -- alphabetically first, so the
    /// reference -- made the only route that escaped admission, the failure
    /// named `["caller_start", "pool_start", "run_once_start"]`, which is the
    /// three routes that were RIGHT. A reader could still recover the truth
    /// from the map printed beside it, and that is exactly the standard this
    /// tree does not accept: a message promising more than it says. A partition
    /// has no privileged route and names each side for what it is.
    ///
    /// Grouped by equality rather than keyed into a map, because
    /// [`AdmissionAnswer`] is a comparison of PRODUCT types -- `SeedDisposition`
    /// and `ErrorCode` -- and deriving `Ord` on them so a test could key a map
    /// would be a test asking the product to grow an ordering nothing else
    /// needs.
    #[cfg(unix)]
    fn disagreement<'a>(
        answers: &'a BTreeMap<&'static str, AdmissionAnswer>,
    ) -> Result<&'a AdmissionAnswer, Vec<(&'a AdmissionAnswer, Vec<&'static str>)>> {
        let mut groups: Vec<(&'a AdmissionAnswer, Vec<&'static str>)> = Vec::new();
        for (route, answer) in answers {
            match groups.iter_mut().find(|(seen, _)| *seen == answer) {
                Some((_, routes)) => routes.push(route),
                None => groups.push((answer, vec![route])),
            }
        }
        let (first, _) = groups.first().expect("every differential compares routes");
        if groups.len() == 1 {
            Ok(first)
        } else {
            Err(groups)
        }
    }

    /// A launch configuration that resolves without a Claude anywhere.
    #[cfg(unix)]
    fn differential_launch() -> ClaudeLaunchConfig {
        ClaudeLaunchConfig {
            executable: "/bin/sh".to_owned(),
            model: None,
            effort: None,
            permission_mode: None,
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            settings: Vec::new(),
            mcp_configs: Vec::new(),
            plugin_dirs: Vec::new(),
            system_prompt: SystemPromptPolicy::Default,
            extra_args: Vec::new(),
        }
    }

    /// ROUTE 1: the wire's `start_session`, reaching the root through the one
    /// door LEAK 5, 5b and 8 all came through -- a plain
    /// `environment.set["CLAUDE_CONFIG_DIR"]`, the only spelling of this
    /// directory nothing canonicalizes.
    #[cfg(unix)]
    fn caller_start(root: &Path, cwd: &Path, identity: SessionIdentity) -> StartSessionRequest {
        StartSessionRequest {
            identity,
            cwd: cwd.to_string_lossy().into_owned(),
            claude: Some(differential_launch()),
            agent: None,
            environment: pseudomux_protocol::v1::EnvironmentSpec {
                snapshot: BTreeMap::from([("PATH".to_owned(), "/usr/bin:/bin".to_owned())]),
                set: BTreeMap::from([(
                    "CLAUDE_CONFIG_DIR".to_owned(),
                    root.to_string_lossy().into_owned(),
                )]),
                unset: BTreeSet::new(),
            },
            auth_policy: pseudomux_protocol::v1::AuthPolicy::Subscription,
            config_isolation: None,
            terminal: pseudomux_protocol::v1::TerminalSpec::default(),
            lifecycle: pseudomux_protocol::v1::LifecycleMode::Transcript,
            retention: RetentionPolicy::Persistent {
                idle_ttl_ms: 600_000,
            },
            compatibility: CompatibilityPolicy::AllowUntested,
            cell: SessionCell::Full,
        }
    }

    /// ROUTE 2: `run_once`, which reaches the funnel with `request.session` and
    /// a retention decision made at the call site.
    #[cfg(unix)]
    fn run_once_start(root: &Path, cwd: &Path, identity: SessionIdentity) -> StartSessionRequest {
        let wire = pseudomux_protocol::v1::RunOnceRequest {
            session: caller_start(root, cwd, identity),
            turn: pseudomux_protocol::v1::TurnRequest {
                turn_id: uuid::Uuid::new_v4(),
                prompt: "differential".to_owned(),
                deadline_unix_ms: None,
                lease: pseudomux_protocol::v1::TurnLeasePolicy::default(),
            },
        };
        wire.session
    }

    /// ROUTE 3: the pool, through its own builder and no caller string at all.
    ///
    /// `launch_request_for` is called here rather than reproduced, because it
    /// is the route: it is what decides that a mint reaches the root through
    /// `config_isolation` and the cell through `SessionCell::Minified`.
    #[cfg(unix)]
    fn pool_start(root: &Path, cwd: &Path, _identity: SessionIdentity) -> StartSessionRequest {
        let (class, _) = crate::pool::resolve_pool_class(
            "sonnet",
            Some(pseudomux_protocol::v1::EffortLevel::Medium),
        )
        .expect("sonnet/medium is an admitted pool class");
        crate::stateless::launch_request_for(
            &crate::pool::MintSpec {
                slot: 0,
                epoch: 1,
                class,
                root: root.to_path_buf(),
                cwd: cwd.to_path_buf(),
                claude_executable: PathBuf::from("/bin/sh"),
                system_prompt: "differential".to_owned(),
                instance_idle_ttl_ms: 600_000,
            },
            &pseudomux_protocol::v1::EnvironmentSpec {
                snapshot: BTreeMap::from([("PATH".to_owned(), "/usr/bin:/bin".to_owned())]),
                set: BTreeMap::new(),
                unset: BTreeSet::new(),
            },
        )
    }

    /// ROUTE 4: a stored agent, whose OWN `environment.set` supplies the root.
    ///
    /// `resolve_agent_start` is called here rather than reproduced, for the
    /// reason `launch_request_for` is: it is the route. An agent-named start
    /// arrives carrying no `claude` and no environment patch of its own, and
    /// leaves carrying the stored agent's -- so the directory admission judges
    /// was written into the store, possibly by a different operator on a
    /// different day, and reaches the guard by a path no inline start uses.
    #[cfg(unix)]
    fn agent_start(root: &Path, cwd: &Path, identity: SessionIdentity) -> StartSessionRequest {
        let spec = pseudomux_protocol::v1::AgentSpec {
            name: "differential".to_owned(),
            description: None,
            claude: differential_launch(),
            environment: pseudomux_protocol::v1::AgentEnvironmentSpec {
                set: BTreeMap::from([(
                    "CLAUDE_CONFIG_DIR".to_owned(),
                    root.to_string_lossy().into_owned(),
                )]),
                unset: BTreeSet::new(),
            },
            auth_policy: pseudomux_protocol::v1::AuthPolicy::Subscription,
            terminal: pseudomux_protocol::v1::TerminalSpec::default(),
            lifecycle: pseudomux_protocol::v1::LifecycleMode::Transcript,
            retention: RetentionPolicy::Persistent {
                idle_ttl_ms: 600_000,
            },
            compatibility: CompatibilityPolicy::AllowUntested,
            cell: SessionCell::Full,
            containment: pseudomux_protocol::v1::AgentContainment::default(),
        };
        let reference = pseudomux_protocol::v1::AgentRef {
            agent_id: uuid::Uuid::from_u128(9),
            version: pseudomux_protocol::v1::AgentVersion::FIRST,
        };
        let named = StartSessionRequest {
            identity,
            cwd: cwd.to_string_lossy().into_owned(),
            claude: None,
            agent: Some(reference),
            environment: pseudomux_protocol::v1::EnvironmentSpec {
                snapshot: BTreeMap::from([("PATH".to_owned(), "/usr/bin:/bin".to_owned())]),
                set: BTreeMap::new(),
                unset: BTreeSet::new(),
            },
            auth_policy: pseudomux_protocol::v1::AuthPolicy::default(),
            config_isolation: None,
            terminal: pseudomux_protocol::v1::TerminalSpec::default(),
            lifecycle: pseudomux_protocol::v1::LifecycleMode::default(),
            retention: RetentionPolicy::default(),
            compatibility: CompatibilityPolicy::default(),
            cell: SessionCell::default(),
        };
        let (resolved, _pin) = crate::agent::resolve_agent_start(&spec, "digest", reference, named);
        resolved
    }

    /// Every spelling of one directory this family of leaks taught us to try.
    ///
    /// Each row's premise is asserted before it is used, so a spelling that
    /// stops aliasing fails as a broken fixture rather than passing as a rule
    /// that held. The `..` row deliberately traverses a component that DOES NOT
    /// EXIST: `metadata` answers `NotFound` on it because the kernel resolves
    /// left to right, while `mkdir -p` creates the missing component and THEN
    /// resolves `..`, landing on the live directory -- which is exactly what
    /// Claude does to its own `CLAUDE_CONFIG_DIR`, and how LEAK 5b's intruder
    /// wrote its transcript inside a live cell.
    #[cfg(unix)]
    fn leak_family_spellings(directory: &Path) -> Vec<(&'static str, PathBuf)> {
        let name = directory.file_name().unwrap().to_str().unwrap();
        let mut spellings = vec![
            ("identity", directory.to_path_buf()),
            (
                "trailing slash",
                PathBuf::from(format!("{}/", directory.display())),
            ),
            (
                "dot-dot through a missing component",
                directory
                    .with_file_name(format!("absent-{name}"))
                    .join("..")
                    .join(name),
            ),
        ];
        // Created once per directory and reused: this runs inside the identity
        // loop, and a second `symlink(2)` on the same name is `EEXIST`.
        let link = directory.with_file_name(format!("link-to-{name}"));
        if std::fs::symlink_metadata(&link).is_err() {
            std::os::unix::fs::symlink(directory, &link).unwrap();
        }
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            directory,
            "the terminal-symlink row must point at the directory under test"
        );
        spellings.push(("terminal symlink", link));
        let inside = directory.join("projects");
        std::fs::create_dir_all(&inside).unwrap();
        spellings.push(("inside the live cell's subtree", inside));
        #[cfg(target_os = "macos")]
        {
            // The APFS firmlink namespace. Not a symlink, so `canonicalize`
            // returns it unchanged; this is the spelling that defeated a
            // comparison of canonicalized strings.
            let canonical = directory.canonicalize().unwrap();
            let firmlink =
                Path::new("/System/Volumes/Data").join(canonical.strip_prefix("/").unwrap());
            assert!(
                firmlink.is_dir(),
                "the firmlink alias must exist for this row to mean anything: {}",
                firmlink.display()
            );
            assert_ne!(
                firmlink.canonicalize().unwrap(),
                canonical,
                "canonicalize must not collapse the firmlink alias"
            );
            spellings.push(("firmlink", firmlink));
        }
        spellings
    }

    /// **THE DIFFERENTIAL ENTRY-PATH TEST.**
    ///
    /// Drives one logical operation through every derived entry path and
    /// asserts the four routes return the SAME admission answer for every
    /// spelling in the leak family -- and, so it cannot pass by refusing
    /// everything, that they also agree on an unheld pair, which every route
    /// must ADMIT.
    ///
    /// It asserts agreement rather than a verdict on purpose. "Refused" is
    /// asserted per route by
    /// [`a_live_minified_cells_resources_are_refused_under_every_alias_of_the_same_inode`],
    /// which is a statement about the RULE. This is a statement about the
    /// ROUTES, and the failure it exists to catch -- three routes refusing and
    /// a fourth admitting -- is invisible to any test that looks at one route.
    ///
    /// # It discriminates, and that is MEASURED rather than argued
    ///
    /// A differential that cannot fail is worse than none, so the guard was
    /// removed from one path at a time and this test run against each. Both
    /// edits were reverted and both files verified byte-identical afterwards.
    ///
    /// 1. **A guard that stops covering one route.** Deleting
    ///    `claim.cell == SessionCell::Minified ||` from [`claim_reaches`] leaves
    ///    the containment rule reaching only MINIFIED applicants -- which is the
    ///    pool and nothing else. Red, naming the route, the role and the
    ///    spelling:
    ///
    ///    ```text
    ///    the entry paths disagree about a configuration root spelled
    ///    `inside the live cell's subtree` with identity New { session_id: None };
    ///    every answer given, and the routes that gave it:
    ///    [(Admitted(Write), ["agent_start", "caller_start", "run_once_start"]),
    ///     (Refused(InvalidConfig), ["pool_start"])]
    ///    ```
    ///
    /// 2. **A route whose own construction escapes the guard.** Making
    ///    `agent::resolve_agent_start` carry the caller's `set` instead of the
    ///    stored agent's takes the configuration root out of the agent route's
    ///    request entirely. Every held spelling still agreed -- both answers are
    ///    `Refused(InvalidConfig)`, for two different reasons -- so it was
    ///    caught only by the unheld-pair control, which is exactly the row that
    ///    exists to stop this test passing by refusing everything:
    ///
    ///    ```text
    ///    the entry paths disagree about an unheld pair; every answer given, and
    ///    the routes that gave it:
    ///    [(Refused(InvalidConfig), ["agent_start"]),
    ///     (Admitted(Write), ["caller_start", "pool_start", "run_once_start"])]
    ///    ```
    ///
    /// The second experiment also found a defect in this harness: [`disagreement`]
    /// reported every route differing from the alphabetically FIRST one, so with
    /// `agent_start` the only route escaping it named the three that were right.
    /// It partitions now, and the output above is the corrected form.
    #[cfg(unix)]
    #[test]
    fn every_entry_path_that_reaches_admission_answers_the_alias_family_identically() {
        // ---- The route list is derived, and every derived route classified --
        let derived = derived_admission_routes();
        let classified: BTreeSet<String> = ADMISSION_ROUTES
            .iter()
            .map(|(route, _)| (*route).to_owned())
            .collect();
        let table: Vec<String> = ADMISSION_ROUTES
            .iter()
            .map(|(route, kind)| match kind {
                Route::Driven(builder) => format!("{route} -> driven by {builder}"),
                Route::CarriesNoStart(reason) => format!("{route} -> carries no start: {reason}"),
            })
            .collect();
        assert_eq!(
            derived,
            classified,
            "the derived route list and the classification table disagree; \
             derived-but-unclassified: {:?}; classified-but-no-longer-derived: {:?}; \
             the table says: {table:#?}",
            derived.difference(&classified).collect::<Vec<&String>>(),
            classified.difference(&derived).collect::<Vec<&String>>(),
        );

        // ---- The four lines `admission_answer` copies are the funnel's own --
        // Without this the differential is a statement about a helper. The
        // funnel needs a runtime a fast test cannot build, so the copy is
        // pinned against the source of the function it copies.
        let funnel = declared_functions()
            .into_iter()
            .find(|function| {
                function.file == "native.rs"
                    && function.name == "start_session_owned_with_retention"
            })
            .expect("native.rs declares the one start funnel");
        let body = without_comment_lines(&funnel.body);
        let mut cursor = 0;
        for statement in [
            "resolve_claude_launch(&identity_request)",
            "effective_config_root(&identity_resolved)",
            "require_isolation_root_is_the_effective_root(",
            "admit_bound_resources(",
        ] {
            let at = body[cursor..].find(statement).unwrap_or_else(|| {
                panic!(
                    "the one start funnel no longer runs `{statement}` in the order \
                     `admission_answer` copies; the differential would be a statement \
                     about a helper rather than about the daemon"
                )
            });
            cursor += at + statement.len();
        }

        let mut drivers: BTreeMap<&'static str, RouteBuilder> = BTreeMap::new();
        drivers.insert("caller_start", caller_start);
        drivers.insert("run_once_start", run_once_start);
        drivers.insert("pool_start", pool_start);
        drivers.insert("agent_start", agent_start);
        let driven: Vec<&'static str> = ADMISSION_ROUTES
            .iter()
            .filter_map(|(_, route)| match route {
                Route::Driven(builder) => Some(*builder),
                Route::CarriesNoStart(_) => None,
            })
            .collect();
        assert_eq!(
            driven.iter().copied().collect::<BTreeSet<&str>>(),
            drivers.keys().copied().collect::<BTreeSet<&str>>(),
            "every route classified `Driven` must name a builder this test runs"
        );

        // ---- The fixture ---------------------------------------------------
        let parent = tempfile::tempdir().unwrap();
        let held_root = owner_only_child(parent.path(), "held-root");
        let held_cwd = owner_only_child(parent.path(), "held-cwd");
        let free_root = owner_only_child(parent.path(), "free-root");
        let free_cwd = owner_only_child(parent.path(), "free-cwd");
        let claims = vec![minified_claim(&held_root, &held_cwd)];

        // ---- The differential ----------------------------------------------
        let mut compared = 0usize;
        for identity in [
            SessionIdentity::New { session_id: None },
            SessionIdentity::Resume {
                session_id: uuid::Uuid::from_u128(11),
            },
        ] {
            // Both bound resources, because a rule stated about one of them is
            // a rule that was never asked about the other -- which is how a
            // cwd inside a live cell's workspace was admitted.
            for (role, held, free_partner) in [
                ("configuration root", &held_root, &free_cwd),
                ("working directory", &held_cwd, &free_root),
            ] {
                for (label, spelling) in leak_family_spellings(held) {
                    let mut answers: BTreeMap<&'static str, AdmissionAnswer> = BTreeMap::new();
                    for (name, build) in &drivers {
                        let request = if role == "configuration root" {
                            build(&spelling, free_partner, identity.clone())
                        } else {
                            build(free_partner, &spelling, identity.clone())
                        };
                        answers.insert(name, admission_answer(&request, &claims));
                    }
                    let agreed = disagreement(&answers).unwrap_or_else(|split| {
                        panic!(
                            "the entry paths disagree about a {role} spelled `{label}` \
                             with identity {identity:?}; every answer given, and the \
                             routes that gave it: {split:#?}"
                        )
                    });
                    // ...and the one answer they agree on is a refusal, so the
                    // agreement is not agreement on a hole.
                    assert_eq!(
                        agreed,
                        &AdmissionAnswer::Refused(ErrorCode::InvalidConfig),
                        "every entry path agreed, and agreed on the wrong answer, for a \
                         {role} spelled `{label}`: {answers:?}"
                    );
                    compared += 1;
                }
            }

            // The direction that must be ADMITTED, so the test cannot pass by
            // refusing everything: an unheld pair, through every route, with
            // the same seed disposition.
            let mut answers: BTreeMap<&'static str, AdmissionAnswer> = BTreeMap::new();
            for (name, build) in &drivers {
                let request = build(&free_root, &free_cwd, identity.clone());
                answers.insert(name, admission_answer(&request, &claims));
            }
            let agreed = disagreement(&answers).unwrap_or_else(|split| {
                panic!(
                    "the entry paths disagree about an unheld pair; every answer given, \
                     and the routes that gave it: {split:#?}"
                )
            });
            assert_eq!(
                agreed,
                &AdmissionAnswer::Admitted(SeedDisposition::Write),
                "an unheld pair must be admitted to write through every route: {answers:?}"
            );
            compared += 1;
        }
        // The comparison count, so a loop that quietly stopped constructing
        // shapes reports differently from one that constructed them and agreed.
        // Two identities, two roles, at least five spellings, plus the unheld
        // control.
        assert!(
            compared >= 2 * (2 * 5 + 1),
            "the differential compared only {compared} shape(s)"
        );
    }
    /// The two instants the lifecycle stamp refuses, and the two it stores.
    ///
    /// MUTATION EVIDENCE: both comparisons in this rule survived a full-scope
    /// run. They can only be exercised by an instant a host clock does not
    /// produce -- the epoch itself, and one past protocol-v1's safe integer
    /// domain -- so the guard was unreachable from every test that existed
    /// while `0` went on meaning "no Stop hook was ever observed" on the wire.
    #[cfg(unix)]
    #[test]
    fn a_stop_instant_is_stored_only_when_it_is_representable_and_not_the_sentinel() {
        assert_eq!(
            representable_stop_instant(Duration::from_millis(1)),
            Some(1),
            "an ordinary instant is stored"
        );
        assert_eq!(
            representable_stop_instant(Duration::from_millis(MAX_SAFE_JSON_INTEGER)),
            Some(MAX_SAFE_JSON_INTEGER),
            "the last representable instant is inside the domain, not outside it"
        );
        assert_eq!(
            representable_stop_instant(Duration::ZERO),
            None,
            "the epoch is the never-observed sentinel and must not be stored as a measurement"
        );
        assert_eq!(
            representable_stop_instant(Duration::from_millis(MAX_SAFE_JSON_INTEGER + 1)),
            None,
            "an instant past the safe integer domain is dropped rather than clamped"
        );
        assert_eq!(
            representable_stop_instant(Duration::from_secs(u64::MAX)),
            None,
            "an instant that is not even a u64 of milliseconds is dropped"
        );
    }
    /// The strictest claim wins whichever side of the reduction it arrives on.
    ///
    /// MUTATION EVIDENCE: the left-hand comparison could be inverted and the
    /// suite stayed green -- a pair whose FIRST member is the minified cell
    /// then answers `Full`, and containment admits the next applicant against
    /// the most permissive holder of a shared root, which is the exact thing
    /// this function's documentation says it exists to prevent.
    #[test]
    fn the_strictest_cell_is_the_minified_one_from_either_side() {
        assert_eq!(strictest_cell([].into_iter()), None);
        assert_eq!(
            strictest_cell([SessionCell::Full].into_iter()),
            Some(SessionCell::Full)
        );
        assert_eq!(
            strictest_cell([SessionCell::Minified].into_iter()),
            Some(SessionCell::Minified)
        );
        assert_eq!(
            strictest_cell([SessionCell::Full, SessionCell::Minified].into_iter()),
            Some(SessionCell::Minified)
        );
        assert_eq!(
            strictest_cell([SessionCell::Minified, SessionCell::Full].into_iter()),
            Some(SessionCell::Minified),
            "the minified holder decides the answer from either side"
        );
        assert_eq!(
            strictest_cell([SessionCell::Full, SessionCell::Full].into_iter()),
            Some(SessionCell::Full),
            "and two ordinary holders do not become a minified one"
        );
    }

    /// The version token is read out of whatever `claude --version` decorated
    /// it with, and the decoration is what the trim exists for.
    ///
    /// MUTATION EVIDENCE: that trim could be turned into one that removes only
    /// `.` and nothing noticed, because every version string any test used was
    /// already a bare token. A parenthesised version then reports "no version"
    /// and a `RequireTested` launch refuses a host it is compatible with.
    #[test]
    fn a_decorated_version_token_still_normalizes_to_the_version() {
        for decorated in ["(2.1.226)", "claude 2.1.226,", "[2.1.226]"] {
            assert_eq!(
                normalize_claude_version(decorated).as_deref(),
                Some("2.1.226"),
                "{decorated:?}"
            );
        }
        // The trim removes decoration and not letters, which is the reason
        // `2.1` and `Claude unknown` are refused rather than salvaged: a token
        // is a version or it is not one.
        assert_eq!(normalize_claude_version("v2.1.226"), None);
    }
}
