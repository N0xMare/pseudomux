//! The seam between the pool and a live Claude.
//!
//! The pool owns admission, the class key, the slot, the epoch, the filesystem
//! roots and the state machine. It owns no process. Everything that touches a
//! child, a TUI, a transcript or a session registry is behind [`InstanceHost`],
//! which is what lets the whole state machine be exercised deterministically
//! without a Claude on the box.
//!
//! Nothing in this module is serializable, and that is deliberate:
//! [`InstanceHandle`] carries the pmux `SessionId` a pool instance is
//! registered under, and that id must never appear in any byte pmux writes to a
//! client socket. A caller who cannot name a resource cannot alias one.

use std::path::PathBuf;

use async_trait::async_trait;
use pseudomux_protocol::v1::{
    ErrorBody, SessionGenerationId, SessionId, StopReason, UsageBreakdown,
};

use super::class::InstanceClass;
use super::instance::{Epoch, SlotId};

/// Everything one mint needs, and nothing a caller supplied.
///
/// Every field here is derived from daemon configuration plus a slot identity.
/// There is no request byte in this struct, which is the property that makes
/// `admit_config_root` and `admit_cwd` sufficient rather than hopeful: the
/// pool's paths are unguessable-irrelevant, because nothing on the wire can
/// name them in the first place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MintSpec {
    pub slot: SlotId,
    pub epoch: Epoch,
    pub class: InstanceClass,
    /// `<pool_parent>/<slot>/<epoch>/root`, already created 0700 and empty.
    pub root: PathBuf,
    /// `<pool_parent>/<slot>/<epoch>/cwd`, already created 0700 and empty.
    pub cwd: PathBuf,
    /// Operator-configured, absolute.
    pub claude_executable: PathBuf,
    /// pmux-owned daemon configuration, delivered as a replace-mode system
    /// prompt so it survives `/clear`.
    pub system_prompt: String,
    /// The registry-level idle TTL for the underlying session. Distinct from
    /// the pool's own TTL sweep: this one is the generic reaper's bound, and
    /// the pool excludes itself from that reaper positively.
    pub instance_idle_ttl_ms: u64,
}

/// What the pool holds after a successful mint. Never on the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstanceHandle {
    pub session_id: SessionId,
    pub generation_id: SessionGenerationId,
    /// The child's pid, when the host could observe it. Recorded so a boot scan
    /// has an exact kill list rather than a cwd-scan heuristic, and so a
    /// `LEAKED` diagnostic can name the process an operator must go find.
    pub pid: Option<i32>,
    /// The Claude version the instance is running, published on every answer.
    pub claude_version: String,
}

/// One completed turn, as the host observed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostTurn {
    pub text: String,
    /// What the transcript said replied, when it carried a `message.model` row.
    /// pmux does not fabricate the missing case.
    pub reported_model: Option<String>,
    pub stop_reason: Option<StopReason>,
    pub usage: UsageBreakdown,
    /// How many `MessageScope::Sidechain` rows the transcript carried, when the
    /// host counted them.
    ///
    /// Structurally must be zero: a Path B cell launches with its tool surface
    /// denied, so a sidechain is unreachable. A non-zero count means the tool
    /// surface is not empty and the isolation claim is false, so the pool
    /// refuses to commit the turn rather than under-reporting its tokens.
    ///
    /// `None` means THIS HOST DOES NOT COUNT ROWS, and it is an `Option` for
    /// exactly that reason. It was a bare `usize`, and a host that cannot count
    /// had two choices under that type -- report `0`, which asserts a fact it
    /// never established, or report `1`, which invents one. Both are the same
    /// defect as a guard whose message promises more than its predicate tests,
    /// and the type is what makes the third answer sayable.
    ///
    /// **`None` is now a REFUSAL**, not a gap. `super::Pool::commit` -- private,
    /// so not an intra-doc link from public documentation -- rejects
    /// the turn with `refusal::sidechain_rows_not_counted` and destroys the
    /// instance. The residue this used to leave was exact and is worth keeping
    /// on the record: the other guard beside it tests
    /// `usage.sidechain != TokenUsage::default()`, so a sidechain row that
    /// carried NO usage at all -- a `Task` subagent whose every model call
    /// reported zero tokens -- passed both checks and committed, with the
    /// isolation claim it would have refuted simply unmade.
    ///
    /// The native host counts. `TranscriptAnalysis::sidechain_rows` counts every
    /// row of any kind that this turn appended on a sidechain, from the walk the
    /// engine already performs, and `TurnResult::sidechain_rows` carries it out;
    /// nothing re-reads the transcript and no cursor is stolen.
    pub sidechain_rows: Option<usize>,
}

/// Why a mint, a turn or a teardown failed.
#[derive(Clone, Debug, PartialEq)]
pub struct HostFailure {
    pub error: ErrorBody,
    /// True when the host could not prove the child it may have spawned is
    /// gone.
    ///
    /// A positive claim, and the pool treats it as decisive: it leaks the slot
    /// and RETAINS the tree rather than erasing it, because deleting a config
    /// root out from under a live Claude races that process's own writer. A
    /// host that returns `false` here is asserting no process survives, and
    /// that assertion is what licenses the pool to erase the tree.
    pub process_may_survive: bool,
}

impl HostFailure {
    /// A failure the host has proven left no process behind.
    #[must_use]
    pub fn reaped(error: ErrorBody) -> Self {
        Self {
            error,
            process_may_survive: false,
        }
    }

    /// A failure that may have left a child running.
    #[must_use]
    pub fn possibly_live(error: ErrorBody) -> Self {
        Self {
            error,
            process_may_survive: true,
        }
    }
}

/// Why a clear failed, and the one bit that decides the instance's fate.
#[derive(Clone, Debug, PartialEq)]
pub struct ClearFailure {
    pub error: ErrorBody,
    /// True only when the refusal was raised before `/clear` could reach the
    /// TUI, i.e. the driver positively claims nothing was typed.
    ///
    /// A positive claim rather than the absence of one, so the default --
    /// quarantine -- applies to every failure that does not make the claim,
    /// including any added later.
    pub clear_not_submitted: bool,
    /// The `assert_empty` reason, when the refusal is evidence that the
    /// INSTALLED CLAUDE's post-`/clear` preamble is not the one pmux measured.
    ///
    /// That is not one bad instance: it is pmux's model of the composer no
    /// longer matching the installed Claude, so it halts the pool. Which reason
    /// it was is carried rather than a bare `true`, because the operator's next
    /// step differs -- `wrong_local_command` sends them to the local-command
    /// menu and `unexpected_metadata_record` sends them to the preamble
    /// allowlist -- and the halt is the only place they will see it.
    ///
    /// The set is `AssertEmptyRefusal::is_a_version_drift_signal`, classified
    /// exhaustively there. It used to be the single literal
    /// `wrong_local_command`, while the doc on the predicate that read it
    /// already claimed the general thing; six other reasons meant the same
    /// thing and quarantined one instance each while the pool minted
    /// replacements into the identical drift. Re-promotion trigger 4,
    /// `docs/version-drift.md` sec.5 P2.
    pub preamble_mismatch: Option<&'static str>,
}

/// What a teardown proved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Destroyed {
    /// True only after the backend positively observed the owned process
    /// boundary empty and saw no descendant escape. Nothing on disk may be
    /// touched until this is true: a live Claude holds `history.jsonl` under a
    /// lock and will recreate what you delete, and deleting a config root out
    /// from under a live Claude races its own `.claude.json` writer.
    pub process_reaped: bool,
}

/// Everything the pool needs from a live Claude, and nothing else.
///
/// # What the integration step must provide
///
/// A `NativeService`-backed implementation whose four methods are:
///
/// - `mint`: build a `StartSessionRequest` from [`MintSpec`] alone -- identity
///   `New { session_id: None }`, `cwd` and `config_isolation.root` from the
///   spec, `claude.model`/`claude.effort` from `spec.class` (rendered by the
///   same `resolve_model_effort` call that produced the class),
///   `permission_mode: DontAsk`, `denied_tools: ["*"]`, `system_prompt:
///   Replace`, `retention: Persistent { idle_ttl_ms }`, `compatibility:
///   RequireTested`, `cell: Minified` -- then call
///   `NativeService::start_session_internal`. `SessionRegistry::register` runs
///   `require_tested_for_minified_cell` and `assert_empty_at_launch` before an
///   actor exists, so a mint that returns `Ok` has already carried the launch
///   proof.
/// - `run_turn`: `SessionRegistry::run_turn` plus the actor's terminal wait,
///   projecting the resulting `TurnResult` into [`HostTurn`]. Any outcome other
///   than a delivered, transcript-proven terminal must be an `Err`.
/// - `clear`: `SessionActorHandle::clear_and_rebind`, with
///   `driver_io::clear_was_not_submitted` read off the converted `ErrorBody` to
///   fill `clear_not_submitted`.
/// - `destroy`: `close_session(ClosePolicy::Force)` and `require_process_reaped`.
///   **It must not delete anything**; the pool owns the filesystem and erases
///   the roots itself, after this returns `process_reaped: true`.
///
/// Two further obligations the pool cannot enforce from here and the integration
/// must add: a `SessionOwner::Pool` marker checked in `SessionRegistry::actor`
/// (so every session-addressed wire method refuses a pool instance with a
/// `SessionNotFound` byte-identical to a real miss) and in
/// `SessionRegistry::expire_idle` (so the generic idle reaper declines pool
/// sessions positively rather than by accident).
#[async_trait]
pub trait InstanceHost: Send + Sync + 'static {
    /// Launch one instance into an already-minted, pristine slot directory.
    async fn mint(&self, spec: MintSpec) -> Result<InstanceHandle, HostFailure>;

    /// Run one turn to a transcript-proven terminal, or fail.
    async fn run_turn(
        &self,
        handle: &InstanceHandle,
        prompt: String,
        deadline_unix_ms: u64,
    ) -> Result<HostTurn, HostFailure>;

    /// Type `/clear`, resolve the rotation, prove the successor empty, bind it.
    async fn clear(&self, handle: &InstanceHandle) -> Result<(), ClearFailure>;

    /// Force-close and prove the owned process boundary empty. Touches no file.
    async fn destroy(&self, handle: &InstanceHandle) -> Result<Destroyed, HostFailure>;
}

/// Where the pool runs work no caller waits on.
///
/// A seam rather than a bare `tokio::spawn` for one reason: the clear that
/// follows a delivered turn must not be able to delay the answer, and a test
/// must be able to prove that. A test spawner queues the work and lets the test
/// assert the caller already had its bytes before draining it.
pub trait Spawner: Send + Sync + 'static {
    fn spawn(&self, work: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>);
}

/// The production spawner: detached, but tracked, so daemon shutdown has an
/// exact awaitable fence over work a dropped request left running.
pub struct TrackedSpawner {
    tasks: std::sync::Arc<crate::tasks::TrackedTasks>,
}

impl TrackedSpawner {
    #[must_use]
    pub fn new(tasks: std::sync::Arc<crate::tasks::TrackedTasks>) -> Self {
        Self { tasks }
    }
}

impl Spawner for TrackedSpawner {
    fn spawn(&self, work: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>) {
        let permit = self.tasks.track();
        tokio::spawn(async move {
            work.await;
            drop(permit);
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::{Spawner, TrackedSpawner};

    /// The production spawner really runs the work, and the shutdown fence
    /// really waits for it.
    ///
    /// SURVIVING MUTANT CLOSED: `host.rs:246 <impl Spawner for
    /// TrackedSpawner>::spawn with ()`. Every pool test in this tree
    /// substitutes a queueing spawner -- which is exactly what makes "the
    /// caller never waits on the clear" observable -- so the one [`Spawner`]
    /// the daemon actually installs had no test at all, and a `spawn` that
    /// silently dropped its future passed the whole suite. What it would drop
    /// is every post-answer `/clear` and every background re-warm: an instance
    /// that answered one caller and then sat in `Clearing` forever, holding a
    /// slot, with that caller's prompt still in its `history.jsonl`.
    ///
    /// Both halves are asserted, because dropping the PERMIT rather than the
    /// work is the same defect one layer down: `TrackedTasks::wait_idle` is the
    /// fence `NativeService::shutdown` waits on, and a fence that opens while
    /// the work is still running is a daemon that exits mid-`/clear`.
    #[tokio::test]
    async fn the_production_spawner_runs_its_work_and_the_shutdown_fence_waits_for_it() {
        let tasks = Arc::new(crate::tasks::TrackedTasks::default());
        let spawner = TrackedSpawner::new(Arc::clone(&tasks));
        let (announce_start, work_started) = tokio::sync::oneshot::channel::<()>();
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let finished = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&finished);

        spawner.spawn(Box::pin(async move {
            let _ = announce_start.send(());
            let _ = released.await;
            flag.store(true, Ordering::SeqCst);
        }));

        // The work RAN. A `spawn` that dropped its future never reaches here,
        // and neither does one that forgot to `tokio::spawn` at all.
        tokio::time::timeout(Duration::from_secs(10), work_started)
            .await
            .expect("the production spawner must actually run the work it is handed")
            .expect("the spawned work must report that it started");
        assert!(
            !finished.load(Ordering::SeqCst),
            "the work is parked on its release, so this test observes it mid-flight"
        );

        // ...and the fence is CLOSED while it runs, which is what makes it a
        // fence rather than a poll of something that already happened.
        assert!(
            tokio::time::timeout(Duration::from_millis(50), tasks.wait_idle())
                .await
                .is_err(),
            "the shutdown fence must not open while a tracked task is still running"
        );

        let _ = release.send(());
        tokio::time::timeout(Duration::from_secs(10), tasks.wait_idle())
            .await
            .expect("the fence must open once the tracked work is over");
        assert!(
            finished.load(Ordering::SeqCst),
            "the fence opened before the work it is tracking had finished"
        );
    }
}
