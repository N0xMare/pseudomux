use std::collections::{HashMap, HashSet, VecDeque};
use std::future::{Future, pending};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pseudomux_claude::{
    ContentBlock as ClaudeBlock, EngineWarning, LogicalAssistantMessage, LogicalMessageKey,
    ParseMode, ParsedRow, StopReason as ClaudeStopReason, TerminalOutcome as ClaudeTerminalOutcome,
    TranscriptAnalysis, TranscriptEngine, TranscriptError, TurnStatus,
};
use pseudomux_protocol::v1::{
    CancelOutcome, CancelTurnResult, ClearSessionResult, ClosePolicy, CloseSessionResult,
    CompatibilityReport, CompletionAuthority, CompletionProvenance, DisconnectAction, ErrorBody,
    ErrorCode, EventBatch, EventEnvelope, EventPayload, LogicalMessage, MAX_NATIVE_FRAME_BYTES,
    MAX_SAFE_JSON_INTEGER, MessageBlock, MessageScope, NeedsInput, NeedsInputKind,
    PromptAcknowledged, ProtocolWarning, ReplayGap, ResponseEnvelope, ResponseResult,
    SessionAgentPin, SessionCell, SessionGenerationId, SessionHandle, SessionId, SessionSnapshot,
    SessionState, SessionStateChanged, StopReason, StopReasonKind, TerminalCandidate, TimestampMs,
    TokenUsage, ToolCompleted, ToolRecord, ToolStarted, ToolStatus, TurnAccepted,
    TurnCancelledEvent, TurnId, TurnOutcome, TurnRequest, TurnResult, TurnSummary, TurnTimings,
    UsageBreakdown, validate_v1_serializable,
};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};

use super::backend::{
    Clock, DriverFailure, DriverResult, InterruptRecovery, TerminalControl, TerminalEvidence,
    TerminalScreenObservation, TranscriptBatch, TranscriptPosition, TranscriptSource,
    UNRECOGNISED_SCREEN_VETO, graduated_drain_ms,
};
use super::minified::{MinifiedTurnObservations, evaluate_minified_fast_path, minified_drain_ms};
use crate::driver_io::ScreenShape;
use crate::tasks::TrackedTasks;

const DEFAULT_IDLE_TTL_MS: u64 = 30 * 60 * 1_000;
const DEFAULT_TURN_HISTORY_CAPACITY: usize = 128;
const DEFAULT_TURN_HISTORY_BYTE_CAPACITY: usize = 64 * 1024 * 1024;
const DEFAULT_REPLAY_BYTE_CAPACITY: usize = 16 * 1024 * 1024;
const TERMINAL_FAILURE_RESERVE_BYTES: usize = 4 * 1024;
// `empty_event_batch_response_bytes` omits `events` through serde's
// `skip_serializing_if`. A non-empty page therefore adds exactly
// `"events":[` before the records and `],` before `next_sequence`.
const EVENT_ARRAY_FIELD_OVERHEAD_BYTES: usize = b"\"events\":[".len() + b"],".len();
// `next_sequence` is itself sent on the wire. The final event sequence must
// therefore remain one below the largest protocol-v1 safe integer.
const MAX_EVENT_SEQUENCE: u64 = MAX_SAFE_JSON_INTEGER - 1;
const CLOSE_SEQUENCE_RESERVE: u64 = 2;
const TERMINAL_SEQUENCE_EVENTS: u64 = 2;

#[derive(Clone, Debug)]
pub struct SessionActorConfig {
    pub replay_capacity: usize,
    /// Logical serialized bytes retained in the replay ring.
    pub replay_byte_capacity: usize,
    pub default_event_batch_size: usize,
    pub poll_interval: Duration,
    pub cancel_recovery_timeout: Duration,
    /// Bound for proving terminal and transcript quiescence after a writable
    /// terminal attachment may have injected input.
    pub attach_reconciliation_timeout: Duration,
    pub default_turn_timeout_ms: u64,
    pub idle_ttl_ms: u64,
    /// Maximum distinct TurnIds remembered by one live actor. Records are never
    /// evicted because forgetting an ID could cause duplicate prompt injection.
    pub turn_history_capacity: usize,
    /// Logical prompt/result bytes retained across all remembered turns.
    pub turn_history_byte_capacity: usize,
    /// Maximum native JSON payload. Values above the protocol ceiling are
    /// clamped to that ceiling; lower values are useful for deterministic tests.
    pub max_frame_bytes: usize,
    /// Largest event sequence this actor may emit. Production clamps this to
    /// `MAX_SAFE_JSON_INTEGER - 1` so the following `next_sequence` remains
    /// exactly representable in every v1 client. Lower values support focused
    /// exhaustion regressions without producing quadrillions of events.
    pub event_sequence_ceiling: u64,
}

impl Default for SessionActorConfig {
    fn default() -> Self {
        Self {
            replay_capacity: 256,
            replay_byte_capacity: DEFAULT_REPLAY_BYTE_CAPACITY,
            default_event_batch_size: 128,
            poll_interval: Duration::from_millis(20),
            cancel_recovery_timeout: Duration::from_secs(5),
            attach_reconciliation_timeout: Duration::from_secs(75),
            default_turn_timeout_ms: 10 * 60 * 1_000,
            idle_ttl_ms: DEFAULT_IDLE_TTL_MS,
            turn_history_capacity: DEFAULT_TURN_HISTORY_CAPACITY,
            turn_history_byte_capacity: DEFAULT_TURN_HISTORY_BYTE_CAPACITY,
            max_frame_bytes: MAX_NATIVE_FRAME_BYTES,
            event_sequence_ceiling: MAX_EVENT_SEQUENCE,
        }
    }
}

pub(crate) struct ActorInit {
    pub session_id: SessionId,
    pub generation_id: SessionGenerationId,
    pub cwd: String,
    pub compatibility: CompatibilityReport,
    /// The session's Claude process was launched with
    /// `--dangerously-skip-permissions`.
    pub dangerous_permission_bypass: bool,
    pub resumable: bool,
    /// The cell this session is driven as, for its whole life. Checked at spawn
    /// against the same admission rule the wire path checks before launch, so a
    /// direct embedder of the `pub` registry cannot reach the minified cell on
    /// an untested profile either.
    pub cell: SessionCell,
    /// The stored agent version this session resolved and PINNED at start, when
    /// it named one.
    ///
    /// Carried on `ActorInit` beside `cell` for the same reason and with the
    /// same lifetime rule: it is chosen once at start and there is deliberately
    /// no request that changes it mid-session. An `update_agent` mints a new
    /// immutable version; this session keeps the one it started under, by
    /// value, and never reads the store again.
    pub agent: Option<SessionAgentPin>,
    pub idle_ttl_ms: Option<u64>,
    pub initial_needs_input: Option<NeedsInput>,
    pub terminal: Arc<dyn TerminalControl>,
    pub transcript: Arc<dyn TranscriptSource>,
    /// Shutdown accounting for this actor's detached teardown work. Supplied by
    /// the registry, so an owner can fence on the same counter.
    pub detached_tasks: Arc<TrackedTasks>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StoredTurnTerminal {
    Result(Box<TurnResult>),
    Failed(ErrorBody),
}

/// Mutation evidence reported by the one-use writable attach proxy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WritableAttachCompletion {
    Unused,
    PotentiallyMutated,
}

/// The compatibility half of the minified cell's admission rule.
///
/// Three call sites need it -- the pre-launch refusal in `NativeService`, the
/// registration boundary in `SessionRegistry::register`, and the mid-session
/// selection -- and a rule with several copies is a rule with several futures.
/// The body is the one that used to live inline in
/// [`SessionActor::select_minified_cell`], moved verbatim.
///
/// An untested cell reached admission by an explicit `allow_untested` request
/// and runs on the conservative fallback drain; it has no evidence behind it
/// that the minified calibration could rest on.
pub(crate) fn require_tested_for_minified_cell(
    compatibility: &CompatibilityReport,
) -> Result<(), ErrorBody> {
    if compatibility.tested {
        return Ok(());
    }
    Err(ErrorBody::new(
        ErrorCode::UnsupportedClaudeVersion,
        "the minified cell requires a tested compatibility profile",
    )
    .with_details(json!({
        "claude_version": compatibility.claude_version,
        "os": compatibility.os,
        "arch": compatibility.arch,
        "recommendation": "run and review the guarded pmux Phase 0 cell, then admit its structured compatibility profile",
    })))
}

/// A minified cell never grants a writable terminal attachment.
///
/// A writable attach hands a second party an rmux grant and their keystrokes
/// then reach the PTY directly -- client to rmux socket to TUI -- so the actor
/// sees the reservation lifecycle and NONE of the bytes. Everything Path B
/// promises is stated in terms of things the actor can see, and this is the one
/// capability that moves the cell without being one of them. Concretely, on a
/// cell whose product claim is "after `/clear` nothing distinguishes this
/// instance from any other", an attached party can:
///
/// * leave text in the composer, which PREFIXES the next caller's prompt and is
///   invisible to `assert_empty_after_clear` until it has already landed in a
///   transcript;
/// * press up-arrow and recall a previous caller's prompt from the instance's
///   own `history.jsonl`, which `/clear` does not truncate -- Claude appends to
///   it, including a row for `/clear` itself, and recall is scoped to the cwd
///   rather than to the session id, so it spans every rotation by construction;
/// * type `/clear` or `/model` by hand, rotating Claude's session id underneath
///   the one pmux has bound, after which pmux's emptiness proof attests a file
///   nothing is writing to.
///
/// So the refusal is at the reservation, not at each consequence: the
/// consequences are open-ended and the capability is one place. Read-only
/// attachment is untouched -- it grants no input and is how an operator watches
/// a cell.
fn refuse_writable_attach_on_minified_cell(cell: SessionCell) -> Result<(), ErrorBody> {
    if cell != SessionCell::Minified {
        return Ok(());
    }
    Err(ErrorBody::new(
        ErrorCode::UnsupportedFeature,
        "a minified cell does not grant writable terminal attachments",
    )
    .with_details(json!({
        "field": "writable",
        "violation": "writable_attach_forbidden_on_minified_cell",
        "recommendation": "remint the cell; writable attach and run_turn are not a product",
    })))
}

/// The privileged clear-and-rebind capability, supplied per call.
///
/// Deliberately NOT a boundary a session is constructed with. `/clear` abandons
/// the bound transcript, so the ability to type it is the ability to make a
/// session's transcript authority stale; a session that was never meant to be
/// cleared should not carry that ability at all, and one it never holds cannot
/// be reached by a code path that forgot to check. The caller that owns the
/// concrete terminal/transcript pair hands it in for exactly one operation.
///
/// Implementations must not be composable in the other direction: typing the
/// command and identifying the transcript it opens are one operation, because a
/// clear whose successor was never identified leaves pmux tailing a file that
/// will never grow again.
#[async_trait]
pub trait ClearRebind: Send + Sync {
    /// Types `/clear` and returns the session id Claude rotated to. Nothing is
    /// armed on the returned id: establishing that authority boundary is the
    /// caller's obligation, and `SessionActor::clear_and_rebind` -- the private
    /// method reached through [`SessionActorHandle::clear_session`] -- is what
    /// does it. Named in prose rather than linked because it is private, and a
    /// public doc link to a private item is a hard rustdoc error under the
    /// workspace's `-D warnings` documentation gate.
    async fn clear_and_rebind(
        &self,
        session_id: SessionId,
        deadline_unix_ms: TimestampMs,
    ) -> DriverResult<SessionId>;
}

enum AttachReconciliation {
    Ready,
    NeedsInput(NeedsInput),
}

#[derive(Clone)]
pub struct SessionActorHandle {
    session_id: SessionId,
    generation_id: SessionGenerationId,
    sender: mpsc::Sender<ActorMessage>,
    sequence: watch::Receiver<u64>,
}

impl SessionActorHandle {
    pub(crate) fn spawn_actor(
        init: ActorInit,
        config: SessionActorConfig,
        clock: Arc<dyn Clock>,
    ) -> Result<(Self, SessionHandle), ErrorBody> {
        SessionActor::spawn(init, config, clock)
    }

    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    #[must_use]
    pub const fn generation_id(&self) -> SessionGenerationId {
        self.generation_id
    }

    pub async fn submit_turn(&self, turn: TurnRequest) -> Result<TurnAccepted, ErrorBody> {
        let (reply, response) = oneshot::channel();
        self.send(Command::Submit { turn, reply }).await?;
        response.await.map_err(|_| daemon_lost())?
    }

    pub async fn cancel_turn(&self, turn_id: TurnId) -> Result<CancelTurnResult, ErrorBody> {
        let (reply, response) = oneshot::channel();
        self.send(Command::Cancel { turn_id, reply }).await?;
        response.await.map_err(|_| daemon_lost())?
    }

    pub async fn snapshot(&self) -> Result<SessionSnapshot, ErrorBody> {
        let (reply, response) = oneshot::channel();
        self.send(Command::Snapshot { reply }).await?;
        response.await.map_err(|_| daemon_lost())
    }

    /// Drives this session as the minified (Path B) cell from here on.
    ///
    /// Refused on an uncalibrated cell: the fast path is calibrated against one
    /// measured shape, and a host whose compatibility cell was never admitted
    /// has no evidence that it is running that shape. Refused mid-turn, because
    /// the cell decides which proof a turn may finish on and a turn must not
    /// change proofs underneath itself. Refused while a writable terminal
    /// attachment is held, because a minified cell may not have one and a
    /// conversion is the only other way to acquire it.
    ///
    /// It makes NO emptiness claim, and it is not the admission path for a
    /// stateless cell. `SessionRegistry::register` proves the transcript has
    /// served no work before admitting a `SessionCell::Minified` *registration*;
    /// this converts a session an embedder already holds, whose history that
    /// embedder already knows, and it is unreachable from protocol v1 -- the
    /// wire only carries the cell as a `start_session` field. An embedder that
    /// wants the stateless guarantee must ask for the cell at registration,
    /// where it is proven, rather than here, where it is only permitted.
    pub async fn select_minified_cell(&self) -> Result<(), ErrorBody> {
        let (reply, response) = oneshot::channel();
        self.send(Command::SelectMinifiedCell { reply }).await?;
        response.await.map_err(|_| daemon_lost())?
    }

    /// Types `/clear` into this session's TUI and re-arms the tail on the
    /// session id Claude rotated to, returning that id.
    ///
    /// The returned id is Claude's, not the caller's: pmux's session handle is
    /// unchanged by a clear, which is what lets the rotation happen without
    /// invalidating anything the caller holds.
    ///
    /// Unfenced: the clear applies to whatever transcript is bound right now.
    /// The wire path uses [`Self::clear_session`] instead, which additionally
    /// requires the caller to name the transcript it believes is bound.
    pub async fn clear_and_rebind(
        &self,
        boundary: Arc<dyn ClearRebind>,
        deadline_unix_ms: TimestampMs,
    ) -> Result<SessionId, ErrorBody> {
        self.clear(boundary, None, deadline_unix_ms)
            .await
            .map(|result| result.transcript_session_id)
    }

    /// The fenced clear: only a caller whose view of the bound transcript is
    /// current may rotate it.
    ///
    /// `expected_transcript_session_id` is a compare-and-swap fence in exactly
    /// the sense `generation_id` already is one level up. Every stale value is
    /// refused, including one that is stale by exactly one rotation. There is no
    /// "your clear already landed" answer, and there deliberately never will be
    /// one derived from session state.
    ///
    /// Twice this answer was reintroduced, and twice it leaked. It is a claim
    /// about a transcript's PRESENT contents inferred from a proxy for "nothing
    /// has happened since" -- first the abandoned id, which nothing invalidated,
    /// then the event sequence, which the writable-attach path mutates the
    /// session without touching. The fence a session starts with is its own
    /// session id, so the value being answered is exactly what a restarted
    /// client, or a second caller that never saw the first clear, presents; a
    /// wrong `already cleared` drops that caller's turn into a transcript still
    /// carrying another caller's prompt. Refusing costs a caller one
    /// [`SessionSnapshot::transcript_session_id`] read and at most one redundant
    /// clear; answering wrongly costs it the guarantee it was buying.
    ///
    /// If exactly-once clear ever becomes a stated requirement it gets a
    /// caller-supplied idempotency token and a stored result, the way turns
    /// already do -- never an inference from session state.
    pub async fn clear_session(
        &self,
        boundary: Arc<dyn ClearRebind>,
        expected_transcript_session_id: SessionId,
        deadline_unix_ms: TimestampMs,
    ) -> Result<ClearSessionResult, ErrorBody> {
        self.clear(
            boundary,
            Some(expected_transcript_session_id),
            deadline_unix_ms,
        )
        .await
    }

    async fn clear(
        &self,
        boundary: Arc<dyn ClearRebind>,
        expected_transcript_session_id: Option<SessionId>,
        deadline_unix_ms: TimestampMs,
    ) -> Result<ClearSessionResult, ErrorBody> {
        let (reply, response) = oneshot::channel();
        self.send(Command::ClearAndRebind {
            boundary,
            expected_transcript_session_id,
            deadline_unix_ms,
            reply,
        })
        .await?;
        response.await.map_err(|_| daemon_lost())?
    }

    pub async fn close(&self, policy: ClosePolicy) -> Result<CloseSessionResult, ErrorBody> {
        let (reply, response) = oneshot::channel();
        self.send(Command::Close { policy, reply }).await?;
        response.await.map_err(|_| daemon_lost())?
    }

    pub(crate) async fn reserve_writable_attach(
        &self,
        attach_id: uuid::Uuid,
    ) -> Result<(), ErrorBody> {
        let (reply, response) = oneshot::channel();
        self.send(Command::ReserveWritableAttach { attach_id, reply })
            .await?;
        response.await.map_err(|_| daemon_lost())?
    }

    pub(crate) async fn release_writable_attach(
        &self,
        attach_id: uuid::Uuid,
        completion: WritableAttachCompletion,
    ) -> Result<(), ErrorBody> {
        let (reply, response) = oneshot::channel();
        self.send(Command::ReleaseWritableAttach {
            attach_id,
            completion,
            reply,
        })
        .await?;
        response.await.map_err(|_| daemon_lost())?
    }

    pub(crate) async fn expire_idle(
        &self,
        now_ms: TimestampMs,
    ) -> Result<Option<CloseSessionResult>, ErrorBody> {
        let (reply, response) = oneshot::channel();
        self.send(Command::ExpireIdle { now_ms, reply }).await?;
        response.await.map_err(|_| daemon_lost())?
    }

    pub async fn stored_turn(
        &self,
        turn_id: TurnId,
    ) -> Result<Option<StoredTurnTerminal>, ErrorBody> {
        let (reply, response) = oneshot::channel();
        self.send(Command::StoredTurn { turn_id, reply }).await?;
        response.await.map_err(|_| daemon_lost())
    }

    pub(crate) async fn events(
        &self,
        after_sequence: u64,
        wait_ms: u64,
        max_events: u32,
    ) -> Result<EventBatch, ErrorBody> {
        let mut sequence = self.sequence.clone();
        let mut batch = self.events_now(after_sequence, max_events).await?;
        if wait_ms == 0 || !batch.events.is_empty() || batch.replay_gap.is_some() {
            return Ok(batch);
        }

        if *sequence.borrow_and_update() > after_sequence
            || tokio::time::timeout(Duration::from_millis(wait_ms), sequence.changed())
                .await
                .is_ok()
        {
            batch = self.events_now(after_sequence, max_events).await?;
        }
        Ok(batch)
    }

    async fn events_now(
        &self,
        after_sequence: u64,
        max_events: u32,
    ) -> Result<EventBatch, ErrorBody> {
        let (reply, response) = oneshot::channel();
        self.send(Command::Events {
            after_sequence,
            max_events,
            reply,
        })
        .await?;
        response.await.map_err(|_| daemon_lost())?
    }

    async fn send(&self, command: Command) -> Result<(), ErrorBody> {
        self.sender
            .send(ActorMessage::Command(command))
            .await
            .map_err(|_| daemon_lost())
    }
}

enum Command {
    Submit {
        turn: TurnRequest,
        reply: oneshot::Sender<Result<TurnAccepted, ErrorBody>>,
    },
    Cancel {
        turn_id: TurnId,
        reply: oneshot::Sender<Result<CancelTurnResult, ErrorBody>>,
    },
    Snapshot {
        reply: oneshot::Sender<SessionSnapshot>,
    },
    Events {
        after_sequence: u64,
        max_events: u32,
        reply: oneshot::Sender<Result<EventBatch, ErrorBody>>,
    },
    StoredTurn {
        turn_id: TurnId,
        reply: oneshot::Sender<Option<StoredTurnTerminal>>,
    },
    Close {
        policy: ClosePolicy,
        reply: oneshot::Sender<Result<CloseSessionResult, ErrorBody>>,
    },
    ReserveWritableAttach {
        attach_id: uuid::Uuid,
        reply: oneshot::Sender<Result<(), ErrorBody>>,
    },
    ReleaseWritableAttach {
        attach_id: uuid::Uuid,
        completion: WritableAttachCompletion,
        reply: oneshot::Sender<Result<(), ErrorBody>>,
    },
    ExpireIdle {
        now_ms: TimestampMs,
        reply: oneshot::Sender<Result<Option<CloseSessionResult>, ErrorBody>>,
    },
    SelectMinifiedCell {
        reply: oneshot::Sender<Result<(), ErrorBody>>,
    },
    ClearAndRebind {
        boundary: Arc<dyn ClearRebind>,
        /// `None` means the caller supplied no fence and accepts whatever
        /// transcript is bound. Only the embedder-facing
        /// `SessionActorHandle::clear_and_rebind` sends that; every wire request
        /// carries a fence.
        expected_transcript_session_id: Option<SessionId>,
        deadline_unix_ms: TimestampMs,
        reply: oneshot::Sender<Result<ClearSessionResult, ErrorBody>>,
    },
}

enum ActorMessage {
    Command(Command),
    Worker(Box<WorkerUpdate>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkerSignal {
    Run,
    Abort,
}

enum WorkerUpdate {
    State {
        turn_id: TurnId,
        state: SessionState,
    },
    Event {
        turn_id: TurnId,
        payload: EventPayload,
    },
    NeedsInput {
        turn_id: TurnId,
        needs_input: Option<NeedsInput>,
        resume_state: SessionState,
    },
    Completed {
        turn_id: TurnId,
        result: Box<TurnResult>,
        deadline: TurnDeadline,
    },
    Failed {
        turn_id: TurnId,
        error: ErrorBody,
        process_reaped: bool,
    },
    Cancelled {
        turn_id: TurnId,
        outcome: CancelOutcome,
        recovered_to_ready: bool,
        completed_at_ms: TimestampMs,
        process_reaped: bool,
    },
    AttachReconciled {
        attach_id: uuid::Uuid,
        result: Result<AttachReconciliation, ErrorBody>,
    },
}

struct ActiveTurn {
    turn_id: TurnId,
    signal: watch::Sender<WorkerSignal>,
    cancel_waiters: Vec<oneshot::Sender<Result<CancelTurnResult, ErrorBody>>>,
}

struct TurnRecord {
    prompt_fingerprint: u64,
    normalized_prompt: Box<str>,
    submitted_at_ms: TimestampMs,
    terminal: Option<StoredTurnTerminal>,
}

struct ReplayRecord {
    event: EventEnvelope,
    encoded_bytes: usize,
}

struct SessionActor {
    session_id: SessionId,
    /// The id the transcript boundary is armed and polled under.
    ///
    /// Equal to `session_id` for the whole life of an ordinary session, and
    /// separate from it only because `/clear` ROTATES Claude's session id. The
    /// public id is pmux's handle and never moves; this one follows Claude.
    /// Collapsing them would mean a clear either invalidated the caller's
    /// handle or silently tailed an abandoned file.
    transcript_session_id: SessionId,
    generation_id: SessionGenerationId,
    cwd: String,
    compatibility: CompatibilityReport,
    dangerous_permission_bypass: bool,
    resumable: bool,
    terminal: Arc<dyn TerminalControl>,
    transcript: Arc<dyn TranscriptSource>,
    clock: Arc<dyn Clock>,
    config: SessionActorConfig,
    /// Shutdown accounting for the detached `close(Force)` this actor and its
    /// workers issue. See [`force_reap_terminal`].
    detached_tasks: Arc<TrackedTasks>,
    idle_ttl_ms: u64,
    state: SessionState,
    created_at_ms: TimestampMs,
    updated_at_ms: TimestampMs,
    needs_input: Option<NeedsInput>,
    needs_input_resume: Option<SessionState>,
    last_turn: Option<TurnSummary>,
    active: Option<ActiveTurn>,
    turns: HashMap<TurnId, TurnRecord>,
    turn_history_bytes: usize,
    replay: VecDeque<ReplayRecord>,
    replay_bytes: usize,
    next_sequence: u64,
    sequence: watch::Sender<u64>,
    sender: mpsc::Sender<ActorMessage>,
    process_reaped: bool,
    writable_attach: Option<uuid::Uuid>,
    writable_attach_release_pending: bool,
    cell: SessionCell,
    agent: Option<SessionAgentPin>,
}

impl SessionActor {
    pub(crate) fn spawn(
        init: ActorInit,
        config: SessionActorConfig,
        clock: Arc<dyn Clock>,
    ) -> Result<(SessionActorHandle, SessionHandle), ErrorBody> {
        // No minified-cell admission check here. This function is reachable only
        // through `SessionActorHandle::spawn_actor`, whose sole caller is
        // `SessionRegistry::register` -- which applies both halves of the rule
        // (tested profile, proven-empty transcript) immediately before, and is
        // itself the `pub` boundary a direct embedder has to cross. A copy here
        // could never fire, and a guard that cannot fire is not defence in
        // depth: it is a second statement of a rule that no test can reach and
        // that would go on reading as enforcement after the reachable one was
        // weakened.
        let (sender, receiver) = mpsc::channel(128);
        let (sequence, sequence_rx) = watch::channel(0);
        let now = checked_actor_timestamp(clock.now_ms(), "event_timestamp")?;
        let idle_ttl_ms = init.idle_ttl_ms.unwrap_or(config.idle_ttl_ms);
        checked_idle_deadline(now, idle_ttl_ms)?;
        let mut actor = Self {
            session_id: init.session_id,
            transcript_session_id: init.session_id,
            generation_id: init.generation_id,
            cwd: init.cwd,
            compatibility: init.compatibility,
            dangerous_permission_bypass: init.dangerous_permission_bypass,
            resumable: init.resumable,
            terminal: init.terminal,
            transcript: init.transcript,
            clock,
            detached_tasks: init.detached_tasks,
            idle_ttl_ms,
            config,
            state: SessionState::Creating,
            created_at_ms: now,
            updated_at_ms: now,
            needs_input: None,
            needs_input_resume: None,
            last_turn: None,
            active: None,
            turns: HashMap::new(),
            turn_history_bytes: 0,
            replay: VecDeque::new(),
            replay_bytes: 0,
            next_sequence: 1,
            sequence,
            sender: sender.clone(),
            process_reaped: false,
            writable_attach: None,
            writable_attach_release_pending: false,
            cell: init.cell,
            agent: init.agent,
        };
        actor.transition(
            SessionState::Booting,
            Some("actor_registered".to_owned()),
            None,
        )?;
        if let Some(needs_input) = init.initial_needs_input {
            actor.set_needs_input(needs_input, SessionState::Ready, None)?;
        } else {
            actor.transition(SessionState::Ready, Some("backend_ready".to_owned()), None)?;
        }
        let session_handle = SessionHandle {
            session_id: actor.session_id,
            generation_id: actor.generation_id,
            state: actor.state,
            compatibility: actor.compatibility.clone(),
            created_at_ms: actor.created_at_ms,
            last_sequence: actor
                .next_sequence
                .checked_sub(1)
                .expect("actor event sequences start at one"),
            agent: actor.agent.clone(),
        };
        let handle = SessionActorHandle {
            session_id: actor.session_id,
            generation_id: actor.generation_id,
            sender,
            sequence: sequence_rx,
        };
        tokio::spawn(actor.run(receiver));
        Ok((handle, session_handle))
    }

    async fn run(mut self, mut receiver: mpsc::Receiver<ActorMessage>) {
        let poll_interval = self.config.poll_interval.max(Duration::from_millis(1));
        let mut screen_poll = tokio::time::interval(poll_interval);
        screen_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                message = receiver.recv() => {
                    let Some(message) = message else {
                        break;
                    };
                    match message {
                        ActorMessage::Command(command) => self.handle_command(command).await,
                        ActorMessage::Worker(update) => self.handle_worker(*update),
                    }
                }
                _ = screen_poll.tick(), if self.active.is_none()
                    && self.state == SessionState::NeedsInput
                    && self.writable_attach.is_none() =>
                {
                    self.poll_startup_screen().await;
                }
            }
            // The actor owns a sender clone, so waiting for every sender to be
            // dropped cannot terminate this loop. A successfully reaped close
            // is the actor's terminal condition; the close reply has already
            // been delivered by `handle_command` before we reach this check.
            if self.state == SessionState::Closed {
                break;
            }
        }
        if let Some(active) = self.active.take() {
            let _ = active.signal.send(WorkerSignal::Abort);
        }
    }

    async fn handle_command(&mut self, command: Command) {
        match command {
            Command::Submit { turn, reply } => {
                let _ = reply.send(self.submit(turn));
            }
            Command::Cancel { turn_id, reply } => self.cancel(turn_id, reply),
            Command::Snapshot { reply } => {
                let _ = reply.send(self.snapshot());
            }
            Command::Events {
                after_sequence,
                max_events,
                reply,
            } => {
                let _ = reply.send(self.event_batch(after_sequence, max_events));
            }
            Command::StoredTurn { turn_id, reply } => {
                let terminal = self
                    .turns
                    .get(&turn_id)
                    .and_then(|record| record.terminal.clone());
                let _ = reply.send(terminal);
            }
            Command::Close { policy, reply } => {
                let _ = reply.send(self.close(policy).await);
            }
            Command::ReserveWritableAttach { attach_id, reply } => {
                let _ = reply.send(self.reserve_writable_attach(attach_id));
            }
            Command::ReleaseWritableAttach {
                attach_id,
                completion,
                reply,
            } => {
                let _ = reply.send(self.release_writable_attach(attach_id, completion));
            }
            Command::ExpireIdle { now_ms, reply } => {
                let _ = reply.send(self.expire_idle(now_ms).await);
            }
            Command::SelectMinifiedCell { reply } => {
                let _ = reply.send(self.select_minified_cell());
            }
            Command::ClearAndRebind {
                boundary,
                expected_transcript_session_id,
                deadline_unix_ms,
                reply,
            } => {
                let _ = reply.send(
                    self.clear_and_rebind(
                        &*boundary,
                        expected_transcript_session_id,
                        deadline_unix_ms,
                    )
                    .await,
                );
            }
        }
    }

    fn select_minified_cell(&mut self) -> Result<(), ErrorBody> {
        // The same admission rule the cell dimension already lives under, from
        // the one function all three sites share.
        require_tested_for_minified_cell(&self.compatibility)?;
        if self.active.is_some() {
            return Err(ErrorBody::new(
                ErrorCode::SessionBusy,
                "the cell cannot change while a turn is active",
            )
            .retryable(true));
        }
        // The other half of [`refuse_writable_attach_on_minified_cell`]. Without
        // it the cell could acquire, by conversion, precisely the reservation
        // the reservation path refuses -- and every downstream statement that
        // reads "a minified cell holds no writable attachment" would be false
        // for the one session that got there sideways.
        if self.writable_attach.is_some() {
            return Err(ErrorBody::new(
                ErrorCode::SessionBusy,
                "the cell cannot change while a writable terminal attachment is held",
            )
            .retryable(true));
        }
        let updated_at_ms = self.checked_now_ms("session_timestamp")?;
        self.cell = SessionCell::Minified;
        self.updated_at_ms = updated_at_ms;
        Ok(())
    }

    /// Clears this session's context and re-arms its tail on the rotated id.
    ///
    /// The ordering is the whole point. `/clear` abandons the bound transcript
    /// -- same inode, same length, no further appends -- so between the clear
    /// and the re-arm this session has no transcript authority at all. It
    /// therefore either finishes holding one, or holds none and refuses every
    /// later turn: `arm_at_eof` is the only operation that establishes the
    /// boundary, and a poll under an id the tail is not armed on refuses rather
    /// than following the rotation. Nothing between here and the next turn's
    /// own arm may poll.
    ///
    /// Never runs on a turn's critical path: it requires an idle session, so a
    /// slow rebind costs availability, never turn latency.
    async fn clear_and_rebind(
        &mut self,
        boundary: &dyn ClearRebind,
        expected_transcript_session_id: Option<SessionId>,
        deadline_unix_ms: TimestampMs,
    ) -> Result<ClearSessionResult, ErrorBody> {
        // Step 0, ahead of every other guard, and a refusal in every case. A
        // refusal is safe this early because it types nothing and moves nothing;
        // it was only ever the `Ok` that was unsound ahead of the busy and
        // writable-attach guards, because it answered for a transcript those
        // guards know another party may be mutating.
        //
        // There is no stale value this can answer. The one that looked
        // answerable -- stale by exactly one rotation, i.e. a retry of a clear
        // whose response was lost -- is indistinguishable on the wire from the
        // fence a session STARTS with (`expected == session_id`), so answering
        // it told a second caller, or a restarted one, that a transcript it had
        // never cleared was empty. Two attempts to bound that window by session
        // state both leaked: the abandoned id alone (nothing invalidates it) and
        // then the event sequence (`reserve_writable_attach`,
        // `release_writable_attach` and attach reconciliation all mutate this
        // session and emit nothing). Both were proxies for "nothing happened
        // since", and the mutation channel a writable attach opens does not pass
        // through this actor at all, so no actor-side proxy can see it.
        //
        // What a caller loses is one round trip, not a capability:
        // `SessionSnapshot::transcript_session_id` reports whether a clear
        // landed, and clearing an already-empty cell is semantically idempotent
        // and costs ~30ms.
        if let Some(expected) = expected_transcript_session_id
            && expected != self.transcript_session_id
        {
            return Err(ErrorBody::new(
                ErrorCode::IdConflict,
                "the clear names a transcript this session is not bound to",
            )
            .with_details(json!({
                "field": "expected_transcript_session_id",
                "violation": "stale_transcript_fence",
            })));
        }
        if self.cell != SessionCell::Minified {
            return Err(ErrorBody::new(
                ErrorCode::UnsupportedFeature,
                "clearing between turns is a minified-cell operation and this session is not one",
            ));
        }
        if self.active.is_some() {
            return Err(ErrorBody::new(
                ErrorCode::SessionBusy,
                "a turn is active and its transcript must not be abandoned underneath it",
            )
            .retryable(true));
        }
        // No writable-attach guard, because a minified cell cannot have one.
        // `reserve_writable_attach` refuses the reservation outright on this
        // cell and `select_minified_cell` refuses to convert a session that
        // holds one, so by the time control reaches here `writable_attach` is
        // `None` by construction. Restating it as a `SessionBusy` arm would add
        // a branch nothing can take -- and the previous version of this function
        // is exactly why that matters: it had that arm, and the fence answer
        // above it returned `Ok` in the same state the arm called busy.
        if self.state != SessionState::Ready {
            return Err(ErrorBody::new(
                ErrorCode::SessionBusy,
                format!("session is not ready to be cleared: {:?}", self.state),
            )
            .retryable(matches!(
                self.state,
                SessionState::Creating
                    | SessionState::Booting
                    | SessionState::NeedsInput
                    | SessionState::Submitting
                    | SessionState::AwaitingPromptAck
                    | SessionState::Running
                    | SessionState::TerminalCandidate
                    | SessionState::Draining
                    | SessionState::Cancelling
            )));
        }
        let rebound = match boundary
            .clear_and_rebind(self.transcript_session_id, deadline_unix_ms)
            .await
        {
            Ok(rebound) => rebound,
            // The one refusal that must NOT quarantine. The driver refuses
            // before submitting the command in cases that need no malformed
            // input to reach -- a deadline that has already passed, and a clear
            // issued before the first turn, where there is no transcript to
            // watch for a rotation yet -- and it says so by construction rather
            // than by a code the actor has to interpret. Nothing was typed, so
            // nothing was abandoned, the bound transcript is still this
            // session's authority, and every later turn is still provable from
            // it. Tainting here would convert a refused request into a
            // permanently dead Claude process that only `close_session` can
            // reclaim. Unmarked failures still poison: the flag is a positive
            // proof of coherence, so a path that forgets to set it fails closed.
            Err(error) => {
                let error = error.into_protocol();
                return Err(
                    if crate::driver_io::clear_was_not_submitted(&error.details) {
                        error
                    } else {
                        self.poison_after_failed_rebind(error)
                    },
                );
            }
        };
        // The authority boundary, established here and not left to the next
        // turn. Two reasons it is not deferred: a rebind that named a file the
        // locator will not corroborate has to be a refusal now, off the
        // critical path, rather than a turn that fails later; and until this
        // returns the session's armed identity and its bound identity disagree,
        // which is the one state no other code path is written to survive.
        if let Err(error) = self.transcript.arm_at_eof(rebound).await {
            return Err(self.poison_after_failed_rebind(error.into_protocol()));
        }
        // Recorded before anything else that can fail. The tail is armed on
        // `rebound` from here on, and an actor still bound to the abandoned id
        // would arm the next turn on an id nothing is writing under -- coherent
        // only by accident, since that arm fails closed rather than because the
        // two were kept in agreement.
        //
        // Nothing about the abandoned id is retained. It is deliberately not
        // remembered here: every version of this function that remembered it
        // eventually answered a later caller with it.
        self.transcript_session_id = rebound;
        self.updated_at_ms = self.checked_now_ms("session_timestamp")?;
        Ok(self.cleared_result())
    }

    /// The result of a clear that ran. `rotated` is always `true`, because the
    /// only way to reach a `ClearSessionResult` is to have typed `/clear` and
    /// bound the transcript it opened.
    fn cleared_result(&self) -> ClearSessionResult {
        ClearSessionResult {
            session_id: self.session_id,
            generation_id: self.generation_id,
            transcript_session_id: self.transcript_session_id,
            rotated: true,
            state: self.state,
        }
    }

    /// Refuses every later turn after a clear whose rebind did not complete.
    ///
    /// The clear may already have executed, in which case the bound transcript
    /// is abandoned and no later turn can be proven finished from it. That
    /// failure is already closed -- `Terminal` is unreachable without a prompt
    /// acknowledgement from the file pmux is reading -- but it would arrive as
    /// a bare turn timeout ten minutes later, with nothing to pull on. Tainting
    /// here converts it into an immediate refusal that names the rebind.
    fn poison_after_failed_rebind(&mut self, error: ErrorBody) -> ErrorBody {
        // `transition` deliberately refuses to move state it cannot also
        // publish an event for, so that sequence exhaustion cannot mutate a
        // session behind a saturated cursor. This is the one transition that
        // inverts that tradeoff. Every other caller that swallows the result
        // leaves the session in a state a later turn may legitimately run
        // from; this one would leave it Ready with an abandoned transcript
        // bound, which is precisely the "returns before the work is done"
        // outcome the Tainted state exists to make impossible. So the state
        // change is forced even when the event could not be recorded: an
        // unobservable refusal is bad, and a silent completion off a
        // transcript nobody is writing to is not comparable.
        if self
            .transition(SessionState::Tainted, Some(error.message.clone()), None)
            .is_err()
        {
            self.state = SessionState::Tainted;
        }
        error
    }

    fn reserve_writable_attach(&mut self, attach_id: uuid::Uuid) -> Result<(), ErrorBody> {
        refuse_writable_attach_on_minified_cell(self.cell)?;
        if self.writable_attach.is_some() {
            return Err(ErrorBody::new(
                ErrorCode::SessionBusy,
                "session already has a writable terminal attachment",
            )
            .retryable(true));
        }
        if !matches!(self.state, SessionState::Ready | SessionState::NeedsInput) {
            let retryable = matches!(
                self.state,
                SessionState::Creating
                    | SessionState::Booting
                    | SessionState::Submitting
                    | SessionState::AwaitingPromptAck
                    | SessionState::Running
                    | SessionState::TerminalCandidate
                    | SessionState::Draining
                    | SessionState::Cancelling
            );
            return Err(ErrorBody::new(
                ErrorCode::SessionBusy,
                format!(
                    "writable attach is unavailable while session is {:?}",
                    self.state
                ),
            )
            .retryable(retryable));
        }
        let updated_at_ms = self.checked_now_ms("session_timestamp")?;
        self.writable_attach = Some(attach_id);
        self.writable_attach_release_pending = false;
        self.updated_at_ms = updated_at_ms;
        Ok(())
    }

    fn release_writable_attach(
        &mut self,
        attach_id: uuid::Uuid,
        completion: WritableAttachCompletion,
    ) -> Result<(), ErrorBody> {
        match self.writable_attach {
            Some(current) if current == attach_id => {
                let updated_at_ms = self.checked_now_ms("session_timestamp")?;
                if completion == WritableAttachCompletion::Unused {
                    self.writable_attach = None;
                    self.writable_attach_release_pending = false;
                    self.updated_at_ms = updated_at_ms;
                    return Ok(());
                }
                if self.active.is_some() {
                    // The active worker already owns the transcript cursor. It
                    // retains the reservation until its terminal result proves
                    // ready+quiet and exact-cursor drain; no second API turn can
                    // be accepted meanwhile.
                    self.writable_attach_release_pending = true;
                    self.updated_at_ms = updated_at_ms;
                    return Ok(());
                }
                self.writable_attach_release_pending = true;
                self.updated_at_ms = updated_at_ms;
                self.spawn_attach_reconciliation(attach_id);
                Ok(())
            }
            Some(_) => Err(ErrorBody::new(
                ErrorCode::IdConflict,
                "writable attach reservation does not match the active attachment",
            )),
            None => Ok(()),
        }
    }

    fn spawn_attach_reconciliation(&self, attach_id: uuid::Uuid) {
        let timeout = self.config.attach_reconciliation_timeout.max(
            Duration::from_millis(self.compatibility.transcript_drain_ms)
                .saturating_add(self.config.poll_interval.saturating_mul(2)),
        );
        let session_id = self.session_id;
        // The transcript boundary resolves a file from the id it is handed, so
        // it gets the id Claude is writing under. The terminal is one TUI by
        // construction and takes the public handle.
        let transcript_session_id = self.transcript_session_id;
        let transcript = Arc::clone(&self.transcript);
        let terminal = Arc::clone(&self.terminal);
        let poll_interval = self.config.poll_interval.max(Duration::from_millis(1));
        let required_drain_ms = self.compatibility.transcript_drain_ms;
        let sender = self.sender.clone();
        tokio::spawn(async move {
            let reconciliation = tokio::time::timeout(timeout, async {
                let arm = transcript.arm_at_eof(transcript_session_id).await?;
                let cursor = arm.position;
                loop {
                    let batch = transcript.poll(transcript_session_id, &cursor).await?;
                    if batch.position != cursor || !batch.rows.is_empty() {
                        return Err(DriverFailure::new(
                            ErrorCode::RecoveryFailed,
                            "transcript changed after writable-attach reconciliation was armed",
                        ));
                    }
                    if batch.drain.satisfies(required_drain_ms) {
                        let evidence = terminal.attach_reconciliation_evidence(session_id).await?;
                        if evidence.ready_prompt && evidence.quiet {
                            // Close the terminal/transcript observation race: after
                            // terminal quiet is established, prove the exact same
                            // transcript cursor is still fully drained.
                            let final_batch =
                                transcript.poll(transcript_session_id, &cursor).await?;
                            if final_batch.position == cursor
                                && final_batch.rows.is_empty()
                                && final_batch.drain.satisfies(required_drain_ms)
                            {
                                return Ok(AttachReconciliation::Ready);
                            }
                            return Err(DriverFailure::new(
                                ErrorCode::RecoveryFailed,
                                "transcript changed during writable-attach terminal reconciliation",
                            ));
                        }
                        if let TerminalScreenObservation::NeedsInput(needs_input) =
                            terminal.observe_screen(session_id).await?
                        {
                            return Ok(AttachReconciliation::NeedsInput(needs_input));
                        }
                    }
                    tokio::time::sleep(poll_interval).await;
                }
            })
            .await;
            let result = match reconciliation {
                Ok(Ok(outcome)) => Ok(outcome),
                Ok(Err(error)) => Err(error.into_protocol()),
                Err(_) => {
                    if let Ok(TerminalScreenObservation::NeedsInput(needs_input)) =
                        terminal.observe_screen(session_id).await
                    {
                        Ok(AttachReconciliation::NeedsInput(needs_input))
                    } else {
                        Err(ErrorBody::new(
                            ErrorCode::RecoveryFailed,
                            "writable attachment did not reconcile to ready+quiet with an exact drained transcript cursor",
                        ))
                    }
                }
            };
            let _ = sender
                .send(ActorMessage::Worker(Box::new(
                    WorkerUpdate::AttachReconciled { attach_id, result },
                )))
                .await;
        });
    }

    fn finish_attach_reconciliation(
        &mut self,
        attach_id: uuid::Uuid,
        result: Result<AttachReconciliation, ErrorBody>,
    ) {
        if self.writable_attach != Some(attach_id) || !self.writable_attach_release_pending {
            return;
        }
        let Ok(updated_at_ms) = self.checked_now_ms("session_timestamp") else {
            return;
        };
        self.writable_attach = None;
        self.writable_attach_release_pending = false;
        self.updated_at_ms = updated_at_ms;
        match result {
            Ok(AttachReconciliation::Ready) => {
                self.resolve_needs_input(SessionState::Ready, None);
            }
            Ok(AttachReconciliation::NeedsInput(needs_input)) => {
                if let Err(error) = self.set_needs_input(needs_input, SessionState::Ready, None) {
                    let _ = self.transition(SessionState::Tainted, Some(error.message), None);
                }
            }
            Err(error) => {
                let _ = self.transition(SessionState::Tainted, Some(error.message), None);
            }
        }
    }

    fn release_attach_after_proven_turn(&mut self) {
        if self.writable_attach_release_pending
            && let Some(attach_id) = self.writable_attach
        {
            // The turn worker's proof may have been sampled immediately before
            // detach. Always take a fresh post-detach cursor/evidence sample.
            self.spawn_attach_reconciliation(attach_id);
        }
    }

    fn release_attach_after_failed_turn(&mut self) {
        if self.writable_attach_release_pending {
            self.writable_attach = None;
            self.writable_attach_release_pending = false;
            if let Ok(updated_at_ms) = self.checked_now_ms("session_timestamp") {
                self.updated_at_ms = updated_at_ms;
            }
        }
    }

    async fn expire_idle(
        &mut self,
        now_ms: TimestampMs,
    ) -> Result<Option<CloseSessionResult>, ErrorBody> {
        let deadline = checked_idle_deadline(self.updated_at_ms, self.idle_ttl_ms)?;
        if !matches!(self.state, SessionState::Ready | SessionState::NeedsInput)
            || self.active.is_some()
            || self.writable_attach.is_some()
            || now_ms < deadline
        {
            return Ok(None);
        }
        self.close(ClosePolicy::Force).await.map(Some)
    }

    fn submit(&mut self, turn: TurnRequest) -> Result<TurnAccepted, ErrorBody> {
        validate_v1_serializable(&turn).map_err(|_| {
            ErrorBody::new(
                ErrorCode::InvalidConfig,
                "turn request is outside the protocol-v1 wire domain",
            )
        })?;
        if turn.lease.on_disconnect != DisconnectAction::Continue
            || turn.lease.heartbeat_timeout_ms.is_some()
        {
            return Err(ErrorBody::new(
                ErrorCode::UnsupportedFeature,
                "disconnect actions and heartbeat leases require a future leased connection API",
            ));
        }
        // Validate the complete bounded prompt before reserving a TurnId,
        // retaining prompt bytes, emitting state, or scheduling terminal I/O.
        // The terminal adapter repeats this validation at its trust boundary.
        let normalized_prompt = crate::driver_io::validate_prompt(&turn.prompt)
            .map_err(DriverFailure::into_protocol)?;
        let fingerprint = prompt_fingerprint(&normalized_prompt);
        if let Some(record) = self.turns.get(&turn.turn_id) {
            if record.prompt_fingerprint != fingerprint
                || record.normalized_prompt.as_ref() != normalized_prompt
            {
                return Err(ErrorBody::new(
                    ErrorCode::IdConflict,
                    "turn_id was already used with a different prompt",
                ));
            }
            let terminal = record.terminal.clone();
            let replay_from_sequence = self.next_sequence;
            if let Some(terminal) = terminal {
                self.ensure_sequence_slots(1 + CLOSE_SEQUENCE_RESERVE)?;
                match terminal {
                    StoredTurnTerminal::Result(mut result) => {
                        result.final_sequence = replay_from_sequence;
                        self.emit(Some(turn.turn_id), EventPayload::TurnCompleted(result))?;
                    }
                    StoredTurnTerminal::Failed(error) => {
                        self.emit(Some(turn.turn_id), EventPayload::TurnFailed(error))?;
                    }
                }
            }
            return Ok(TurnAccepted {
                session_id: self.session_id,
                generation_id: self.generation_id,
                turn_id: turn.turn_id,
                replayed: true,
                state: self.state,
                // A completed idempotent retry gets a fresh terminal event. Point
                // the subscriber at that event, not one past it.
                next_sequence: replay_from_sequence,
            });
        }
        let submitted_at_ms = self.checked_now_ms("turn_submitted_at")?;
        let effective_deadline_ms = checked_turn_deadline_unix_ms(
            turn.deadline_unix_ms,
            submitted_at_ms,
            self.config.default_turn_timeout_ms,
        )?;
        if effective_deadline_ms <= submitted_at_ms {
            return Err(ErrorBody::new(
                ErrorCode::TurnTimeout,
                "turn deadline already elapsed",
            ));
        }
        if let Some(active) = self.active.as_ref() {
            return Err(ErrorBody::new(
                ErrorCode::SessionBusy,
                format!("session already has active turn {}", active.turn_id),
            )
            .retryable(true));
        }
        if self.writable_attach.is_some() {
            return Err(ErrorBody::new(
                ErrorCode::SessionBusy,
                "a writable terminal attachment currently owns interactive input",
            )
            .retryable(true));
        }
        if self.state != SessionState::Ready {
            #[allow(clippy::items_after_statements)]
            let (code, retryable) = match self.state {
                SessionState::NeedsInput => (
                    self.needs_input
                        .as_ref()
                        .map_or(ErrorCode::NeedsInput, needs_input_error_code),
                    true,
                ),
                SessionState::Tainted => (ErrorCode::RecoveryFailed, false),
                SessionState::Closed | SessionState::Closing => (ErrorCode::SessionNotFound, false),
                SessionState::Failed => (ErrorCode::TranscriptUnavailable, false),
                _ => (ErrorCode::SessionBusy, true),
            };
            // A BLOCKED SESSION NAMES WHAT IS BLOCKING IT. `{:?}` of the state
            // rendered "session is not ready: NeedsInput" while `self.needs_input`
            // held `kind: trust` and "Claude requires workspace trust
            // confirmation" -- the actor knew exactly which modal was on the
            // screen and told the caller only that one was. MEASURED over the
            // real socket: `pmux turn` against a freshly started session in an
            // untrusted cwd printed the state name and nothing an operator could
            // act on, while `pmux inspect` on the same session printed the kind.
            let mut error = ErrorBody::new(code, self.not_ready_message()).retryable(retryable);
            if let Some(needs_input) = self.needs_input.as_ref() {
                error = error.with_details(json!({
                    "violation": "session_not_ready",
                    "state": format!("{:?}", self.state),
                    "needs_input_kind": needs_input.kind,
                    "needs_input_message": needs_input.message,
                    "recommendation": needs_input_recommendation(needs_input.kind),
                }));
            }
            return Err(error);
        }

        // Acceptance consumes one state event and must retain enough sequence
        // space to publish one terminal state plus its terminal outcome and to
        // close the process afterwards. This check happens before reserving the
        // TurnId or mutating the terminal.
        self.ensure_sequence_slots(1 + TERMINAL_SEQUENCE_EVENTS + CLOSE_SEQUENCE_RESERVE)?;

        if self.turns.len() >= self.config.turn_history_capacity {
            return Err(
                self.turn_history_capacity_error("the per-session TurnId capacity is exhausted", 0)
            );
        }
        let record_bytes = std::mem::size_of::<TurnRecord>()
            .saturating_add(normalized_prompt.len())
            .saturating_add(TERMINAL_FAILURE_RESERVE_BYTES);
        if self
            .turn_history_bytes
            .checked_add(record_bytes)
            .is_none_or(|total| total > self.config.turn_history_byte_capacity)
        {
            return Err(self.turn_history_capacity_error(
                "the per-session turn history byte capacity is exhausted",
                record_bytes,
            ));
        }

        self.turn_history_bytes += record_bytes;
        self.turns.insert(
            turn.turn_id,
            TurnRecord {
                prompt_fingerprint: fingerprint,
                normalized_prompt: normalized_prompt.into_boxed_str(),
                submitted_at_ms,
                terminal: None,
            },
        );
        let (signal, signal_rx) = watch::channel(WorkerSignal::Run);
        self.active = Some(ActiveTurn {
            turn_id: turn.turn_id,
            signal,
            cancel_waiters: Vec::new(),
        });
        self.transition(SessionState::Submitting, None, Some(turn.turn_id))?;

        let worker = TurnWorker {
            session_id: self.session_id,
            transcript_session_id: self.transcript_session_id,
            cell: self.cell,
            generation_id: self.generation_id,
            turn,
            submitted_at_ms,
            compatibility: self.compatibility.clone(),
            dangerous_permission_bypass: self.dangerous_permission_bypass,
            terminal: Arc::clone(&self.terminal),
            transcript: Arc::clone(&self.transcript),
            clock: Arc::clone(&self.clock),
            config: self.config.clone(),
            detached_tasks: Arc::clone(&self.detached_tasks),
            signal: signal_rx,
            sender: self.sender.clone(),
        };
        tokio::spawn(worker.run());

        Ok(TurnAccepted {
            session_id: self.session_id,
            generation_id: self.generation_id,
            turn_id: self.active.as_ref().expect("active was set").turn_id,
            replayed: false,
            state: self.state,
            next_sequence: self.next_sequence,
        })
    }

    fn cancel(
        &mut self,
        turn_id: TurnId,
        reply: oneshot::Sender<Result<CancelTurnResult, ErrorBody>>,
    ) {
        if self
            .turns
            .get(&turn_id)
            .is_some_and(|record| record.terminal.is_some())
        {
            let _ = reply.send(Ok(CancelTurnResult {
                session_id: self.session_id,
                generation_id: self.generation_id,
                turn_id,
                outcome: CancelOutcome::AlreadyTerminal,
                session_state: self.state,
            }));
            return;
        }
        let Some(active_turn_id) = self.active.as_ref().map(|active| active.turn_id) else {
            let _ = reply.send(Err(ErrorBody::new(
                ErrorCode::IdConflict,
                "turn is not active in this session",
            )));
            return;
        };
        if active_turn_id != turn_id {
            let _ = reply.send(Err(ErrorBody::new(
                ErrorCode::IdConflict,
                "turn_id does not match the active turn",
            )));
            return;
        }

        let begin_recovery = self.state != SessionState::Cancelling;
        if begin_recovery {
            if let Err(error) =
                self.ensure_sequence_slots(1 + TERMINAL_SEQUENCE_EVENTS + CLOSE_SEQUENCE_RESERVE)
            {
                let _ = reply.send(Err(error));
                return;
            }
            if let Err(error) = self.transition(SessionState::Cancelling, None, Some(turn_id)) {
                let _ = reply.send(Err(error));
                return;
            }
        }
        let active = self.active.as_mut().expect("active turn was checked");
        active.cancel_waiters.push(reply);
        let _ = active.signal.send(WorkerSignal::Abort);
        if begin_recovery {
            self.spawn_cancel_recovery(turn_id);
        }
    }

    async fn close(&mut self, policy: ClosePolicy) -> Result<CloseSessionResult, ErrorBody> {
        if self.state == SessionState::Closed {
            return Ok(CloseSessionResult {
                session_id: self.session_id,
                generation_id: self.generation_id,
                already_closed: true,
                process_reaped: self.process_reaped,
            });
        }
        self.writable_attach = None;
        self.writable_attach_release_pending = false;
        if let Some(active) = self.active.take() {
            let _ = active.signal.send(WorkerSignal::Abort);
            for waiter in active.cancel_waiters {
                let _ = waiter.send(Err(ErrorBody::new(
                    ErrorCode::Cancelled,
                    "session closed while cancellation was pending",
                )));
            }
        }
        self.transition(SessionState::Closing, None, None)?;
        if self.process_reaped {
            self.transition(SessionState::Closed, None, None)?;
            return Ok(CloseSessionResult {
                session_id: self.session_id,
                generation_id: self.generation_id,
                already_closed: false,
                process_reaped: true,
            });
        }
        match self.terminal.close(self.session_id, policy).await {
            Ok(process_reaped) => {
                self.process_reaped = process_reaped;
                if process_reaped {
                    self.transition(SessionState::Closed, None, None)?;
                }
                Ok(CloseSessionResult {
                    session_id: self.session_id,
                    generation_id: self.generation_id,
                    already_closed: false,
                    process_reaped,
                })
            }
            Err(error) => {
                let protocol = error.into_protocol();
                let _ = self.transition(SessionState::Failed, Some(protocol.message.clone()), None);
                Err(protocol)
            }
        }
    }

    fn handle_worker(&mut self, update: WorkerUpdate) {
        let update = match update {
            WorkerUpdate::AttachReconciled { attach_id, result } => {
                self.finish_attach_reconciliation(attach_id, result);
                return;
            }
            update => update,
        };
        let turn_id = match &update {
            WorkerUpdate::State { turn_id, .. }
            | WorkerUpdate::Event { turn_id, .. }
            | WorkerUpdate::NeedsInput { turn_id, .. }
            | WorkerUpdate::Completed { turn_id, .. }
            | WorkerUpdate::Failed { turn_id, .. }
            | WorkerUpdate::Cancelled { turn_id, .. } => *turn_id,
            WorkerUpdate::AttachReconciled { .. } => unreachable!("handled above"),
        };
        if self.active.as_ref().map(|active| active.turn_id) != Some(turn_id) {
            return;
        }
        if self.state == SessionState::Cancelling
            && !matches!(
                &update,
                WorkerUpdate::Cancelled { .. } | WorkerUpdate::Failed { .. }
            )
        {
            return;
        }

        let required_sequence_slots = match &update {
            WorkerUpdate::NeedsInput {
                needs_input: Some(_),
                ..
            } => 2 + TERMINAL_SEQUENCE_EVENTS + CLOSE_SEQUENCE_RESERVE,
            WorkerUpdate::State { .. }
            | WorkerUpdate::Event { .. }
            | WorkerUpdate::NeedsInput {
                needs_input: None, ..
            } => 1 + TERMINAL_SEQUENCE_EVENTS + CLOSE_SEQUENCE_RESERVE,
            WorkerUpdate::Completed { .. }
            | WorkerUpdate::Failed { .. }
            | WorkerUpdate::Cancelled { .. } => TERMINAL_SEQUENCE_EVENTS + CLOSE_SEQUENCE_RESERVE,
            WorkerUpdate::AttachReconciled { .. } => unreachable!("handled above"),
        };
        if let Err(error) = self.ensure_sequence_slots(required_sequence_slots) {
            self.fail_active(turn_id, error, false);
            return;
        }

        match update {
            WorkerUpdate::State { state, .. } => {
                if self.state != SessionState::Cancelling {
                    if self.state == SessionState::NeedsInput && self.needs_input.is_some() {
                        self.needs_input_resume = Some(state);
                    } else {
                        let _ = self.transition(state, None, Some(turn_id));
                    }
                }
            }
            WorkerUpdate::Event { payload, .. } => {
                if let Err(error) = self.emit(Some(turn_id), payload) {
                    self.fail_active(turn_id, error, false);
                }
            }
            WorkerUpdate::NeedsInput {
                needs_input,
                resume_state,
                ..
            } => match needs_input {
                Some(needs_input) => {
                    if let Err(error) =
                        self.set_needs_input(needs_input, resume_state, Some(turn_id))
                    {
                        self.fail_active(turn_id, error, false);
                    }
                }
                None => self.resolve_needs_input(resume_state, Some(turn_id)),
            },
            WorkerUpdate::Completed {
                result, deadline, ..
            } => {
                // The actor is the terminal-outcome authority. Even if the
                // worker observed all success evidence before its lease, a
                // queued update may not commit success after that lease.
                if deadline.expired(self.clock.as_ref()) {
                    self.timeout_completed_at_commit(turn_id);
                } else {
                    self.complete_active(turn_id, result, deadline);
                }
            }
            WorkerUpdate::Failed {
                error,
                process_reaped,
                ..
            } => self.fail_active(turn_id, error, process_reaped),
            WorkerUpdate::Cancelled {
                outcome,
                recovered_to_ready,
                completed_at_ms,
                process_reaped,
                ..
            } => self.finish_cancel(
                turn_id,
                outcome,
                recovered_to_ready,
                completed_at_ms,
                process_reaped,
            ),
            WorkerUpdate::AttachReconciled { .. } => unreachable!("handled above"),
        }
    }

    fn complete_active(
        &mut self,
        turn_id: TurnId,
        mut result: Box<TurnResult>,
        deadline: TurnDeadline,
    ) {
        self.active = None;
        if let Err(error) = self.transition(SessionState::Ready, None, Some(turn_id)) {
            // The turn is over but its terminal state is unpublishable. Route
            // it exactly like every sibling terminal path: store a compact
            // failure, taint the session, and release the attach reservation,
            // so the caller learns the outcome instead of waiting forever.
            self.poison_after_unpublishable_terminal(turn_id, error);
            return;
        }
        self.release_attach_after_proven_turn();
        result.final_sequence = self.next_sequence;
        let completed_at_ms = result.timings.completed_at_ms;
        let wire_bytes = turn_result_wire_bytes(&result);
        let stored = StoredTurnTerminal::Result(Box::new((*result).clone()));
        let stored_bytes = stored_terminal_bytes(&stored);
        // Serialization and result-size validation can be material for a
        // near-frame-limit response. This is the final check immediately
        // before mutating the actor's terminal record.
        if deadline.expired(self.clock.as_ref()) {
            self.finish_completed_with_error(turn_id, self.clock.now_ms(), turn_timeout_error());
            return;
        }
        if wire_bytes == usize::MAX {
            self.finish_completed_with_error(
                turn_id,
                completed_at_ms,
                ErrorBody::new(
                    ErrorCode::RecoveryFailed,
                    "turn result cannot be serialized within the protocol-v1 domain",
                ),
            );
            return;
        }
        if wire_bytes > self.frame_limit() {
            let error = ErrorBody::new(
                ErrorCode::ResultTooLarge,
                "exact turn result exceeds the native frame limit",
            )
            .with_details(json!({
                "actual_bytes": diagnostic_usize(wire_bytes),
                "maximum_bytes": diagnostic_usize(self.frame_limit()),
            }));
            self.finish_completed_with_error(turn_id, completed_at_ms, error);
            return;
        }

        if let Err(error) = self.try_store_terminal_with_bytes(turn_id, stored, stored_bytes) {
            self.finish_completed_with_error(turn_id, completed_at_ms, error);
            return;
        }

        let final_sequence = result.final_sequence;
        let outcome = result.outcome;
        if let Err(error) = self.emit(Some(turn_id), EventPayload::TurnCompleted(result)) {
            self.poison_after_unpublishable_terminal(turn_id, error);
            return;
        }
        self.last_turn = Some(TurnSummary {
            turn_id,
            outcome,
            completed_at_ms,
            final_sequence,
        });
    }

    fn timeout_completed_at_commit(&mut self, turn_id: TurnId) {
        self.active = None;
        if let Err(error) = self.transition(SessionState::Ready, None, Some(turn_id)) {
            self.poison_after_unpublishable_terminal(turn_id, error);
            return;
        }
        self.release_attach_after_proven_turn();
        self.finish_completed_with_error(turn_id, self.clock.now_ms(), turn_timeout_error());
    }

    fn finish_completed_with_error(
        &mut self,
        turn_id: TurnId,
        completed_at_ms: TimestampMs,
        error: ErrorBody,
    ) {
        let completed_at_ms = checked_actor_timestamp(completed_at_ms, "turn_completed_at");
        let error = match &completed_at_ms {
            Ok(_) => error,
            Err(timestamp_error) => timestamp_error.clone(),
        };
        let error = self.store_bounded_failure(turn_id, error);
        let final_sequence = self.next_sequence;
        if let Err(emit_error) = self.emit(Some(turn_id), EventPayload::TurnFailed(error)) {
            self.poison_after_unpublishable_terminal(turn_id, emit_error);
            return;
        }
        self.last_turn = completed_at_ms.ok().map(|completed_at_ms| TurnSummary {
            turn_id,
            outcome: TurnOutcome::Failed,
            completed_at_ms,
            final_sequence,
        });
    }

    fn frame_limit(&self) -> usize {
        self.config.max_frame_bytes.min(MAX_NATIVE_FRAME_BYTES)
    }

    /// Why this session will not take a turn, in one sentence.
    ///
    /// `NeedsInput` is the only state whose name is not the whole answer: it
    /// says a modal is up and not which one, while the actor is holding the
    /// kind and Claude's own words for it.
    fn not_ready_message(&self) -> String {
        match self.needs_input.as_ref() {
            Some(needs_input) => format!(
                "session is not ready: {:?} ({:?}: {})",
                self.state, needs_input.kind, needs_input.message
            ),
            None => format!("session is not ready: {:?}", self.state),
        }
    }

    fn turn_history_capacity_error(
        &self,
        message: &'static str,
        additional_bytes: usize,
    ) -> ErrorBody {
        ErrorBody::new(ErrorCode::TurnHistoryCapacityExceeded, message).with_details(json!({
            "retained_turns": diagnostic_usize(self.turns.len()),
            "maximum_turns": diagnostic_usize(self.config.turn_history_capacity),
            "retained_bytes": diagnostic_usize(self.turn_history_bytes),
            "additional_bytes": diagnostic_usize(additional_bytes),
            "maximum_bytes": diagnostic_usize(self.config.turn_history_byte_capacity),
        }))
    }

    fn try_store_terminal(
        &mut self,
        turn_id: TurnId,
        terminal: StoredTurnTerminal,
    ) -> Result<(), ErrorBody> {
        let encoded_bytes = stored_terminal_bytes(&terminal);
        self.try_store_terminal_with_bytes(turn_id, terminal, encoded_bytes)
    }

    fn try_store_terminal_with_bytes(
        &mut self,
        turn_id: TurnId,
        terminal: StoredTurnTerminal,
        encoded_bytes: usize,
    ) -> Result<(), ErrorBody> {
        if encoded_bytes == usize::MAX {
            return Err(ErrorBody::new(
                ErrorCode::RecoveryFailed,
                "terminal outcome cannot be serialized within the protocol-v1 domain",
            ));
        }
        let additional_bytes = encoded_bytes.saturating_sub(TERMINAL_FAILURE_RESERVE_BYTES);
        if self
            .turn_history_bytes
            .checked_add(additional_bytes)
            .is_none_or(|total| total > self.config.turn_history_byte_capacity)
        {
            return Err(self.turn_history_capacity_error(
                "the exact terminal outcome does not fit the per-session turn history",
                additional_bytes,
            ));
        }
        let record = self.turns.get_mut(&turn_id).ok_or_else(|| {
            ErrorBody::new(
                ErrorCode::Internal,
                "terminal outcome has no reserved TurnId record",
            )
        })?;
        if record.terminal.is_some() {
            return Err(ErrorBody::new(
                ErrorCode::Internal,
                "terminal outcome was already recorded",
            ));
        }
        record.terminal = Some(terminal);
        self.turn_history_bytes = self.turn_history_bytes.saturating_add(additional_bytes);
        Ok(())
    }

    fn store_bounded_failure(&mut self, turn_id: TurnId, error: ErrorBody) -> ErrorBody {
        let error = self.bounded_terminal_error(turn_id, error);
        if let Err(capacity_error) =
            self.try_store_terminal(turn_id, StoredTurnTerminal::Failed(error.clone()))
        {
            let capacity_error = self.bounded_terminal_error(turn_id, capacity_error);
            self.try_store_terminal(turn_id, StoredTurnTerminal::Failed(capacity_error.clone()))
                .expect("each accepted turn reserves space for a compact terminal failure");
            return capacity_error;
        }
        error
    }

    fn replace_terminal_with_failure(&mut self, turn_id: TurnId, error: ErrorBody) -> ErrorBody {
        let previous = self
            .turns
            .get_mut(&turn_id)
            .and_then(|record| record.terminal.take());
        if let Some(previous) = previous {
            let previous_additional =
                stored_terminal_bytes(&previous).saturating_sub(TERMINAL_FAILURE_RESERVE_BYTES);
            self.turn_history_bytes = self.turn_history_bytes.saturating_sub(previous_additional);
        }
        self.store_bounded_failure(turn_id, error)
    }

    fn poison_after_unpublishable_terminal(&mut self, turn_id: TurnId, error: ErrorBody) {
        let _ = self.replace_terminal_with_failure(turn_id, error);
        self.state = SessionState::Tainted;
        self.needs_input = None;
        self.needs_input_resume = None;
        self.release_attach_after_failed_turn();
        // There is deliberately no synthetic terminal event or summary: the
        // sequence domain could not represent one. One-shot callers observe
        // the stored failure and clean up; persistent callers reconcile the
        // tainted snapshot and close the exact generation.
        self.last_turn = None;
    }

    fn bounded_terminal_error(&self, turn_id: TurnId, error: ErrorBody) -> ErrorBody {
        let actual_bytes =
            terminal_error_wire_bytes(self.session_id, self.generation_id, turn_id, &error);
        if actual_bytes == usize::MAX {
            return ErrorBody::new(
                ErrorCode::RecoveryFailed,
                "terminal error cannot be serialized within the protocol-v1 domain",
            );
        }
        if actual_bytes <= self.frame_limit() {
            return error;
        }
        ErrorBody::new(
            ErrorCode::ResultTooLarge,
            "terminal error exceeds the native frame limit",
        )
        .with_details(json!({
            "actual_bytes": diagnostic_usize(actual_bytes),
            "maximum_bytes": diagnostic_usize(self.frame_limit()),
            "original_code": format!("{:?}", error.code),
        }))
    }

    fn fail_active(&mut self, turn_id: TurnId, error: ErrorBody, process_reaped: bool) {
        let active = self.active.take().expect("active turn was checked");
        self.process_reaped |= process_reaped;
        let completed_at_ms = self.checked_now_ms("turn_completed_at").ok();
        let error = self.store_bounded_failure(turn_id, error);
        let was_cancelling = self.state == SessionState::Cancelling;
        let target = if was_cancelling {
            SessionState::Tainted
        } else {
            SessionState::Failed
        };
        let transition = self.transition(target, Some(error.message.clone()), Some(turn_id));
        self.release_attach_after_failed_turn();
        let final_sequence = self.next_sequence;
        let terminal_event = transition.and_then(|()| {
            self.emit(Some(turn_id), EventPayload::TurnFailed(error.clone()))
                .map(|_| ())
        });
        if let Err(publish_error) = terminal_event {
            self.poison_after_unpublishable_terminal(turn_id, publish_error);
        } else {
            self.last_turn = completed_at_ms.map(|completed_at_ms| TurnSummary {
                turn_id,
                outcome: TurnOutcome::Failed,
                completed_at_ms,
                final_sequence,
            });
        }
        for waiter in active.cancel_waiters {
            let _ = waiter.send(Ok(CancelTurnResult {
                session_id: self.session_id,
                generation_id: self.generation_id,
                turn_id,
                outcome: CancelOutcome::RecoveryFailed,
                session_state: self.state,
            }));
        }
    }

    fn spawn_cancel_recovery(&self, turn_id: TurnId) {
        let terminal = Arc::clone(&self.terminal);
        let transcript = Arc::clone(&self.transcript);
        let sender = self.sender.clone();
        let session_id = self.session_id;
        let transcript_session_id = self.transcript_session_id;
        let clock = Arc::clone(&self.clock);
        let detached_tasks = Arc::clone(&self.detached_tasks);
        let recovery_timeout = self.config.cancel_recovery_timeout;
        let poll_interval = self.config.poll_interval;
        let transcript_drain_ms = self.compatibility.transcript_drain_ms;
        tokio::spawn(async move {
            let interrupt_deadline = tokio::time::Instant::now() + recovery_timeout;
            let interrupt = tokio::time::timeout_at(
                interrupt_deadline,
                terminal.interrupt(session_id, turn_id),
            )
            .await;
            let update = match interrupt {
                Ok(Ok(InterruptRecovery::RecoveredToReady))
                    if stabilize_cancelled_transcript(
                        transcript.as_ref(),
                        transcript_session_id,
                        transcript_drain_ms,
                        poll_interval,
                        tokio::time::Instant::now()
                            + recovery_timeout
                            + Duration::from_millis(transcript_drain_ms),
                    )
                    .await =>
                {
                    WorkerUpdate::Cancelled {
                        turn_id,
                        outcome: CancelOutcome::Cancelled,
                        recovered_to_ready: true,
                        completed_at_ms: clock.now_ms(),
                        process_reaped: false,
                    }
                }
                Ok(Ok(InterruptRecovery::RecoveredToReady | InterruptRecovery::RecoveryFailed))
                | Err(_) => {
                    let completed_at_ms = clock.now_ms();
                    // Recovery proof and forced cleanup have independent
                    // budgets. Exhausting the former must never suppress the
                    // latter and leave an interactive process alive.
                    let process_reaped = force_reap_terminal(
                        Arc::clone(&terminal),
                        &detached_tasks,
                        session_id,
                        recovery_timeout,
                    )
                    .await;
                    WorkerUpdate::Cancelled {
                        turn_id,
                        outcome: CancelOutcome::RecoveryFailed,
                        recovered_to_ready: false,
                        completed_at_ms,
                        process_reaped,
                    }
                }
                Ok(Err(error)) => {
                    let process_reaped = force_reap_terminal(
                        Arc::clone(&terminal),
                        &detached_tasks,
                        session_id,
                        recovery_timeout,
                    )
                    .await;
                    WorkerUpdate::Failed {
                        turn_id,
                        error: error.into_protocol(),
                        process_reaped,
                    }
                }
            };
            let _ = sender.send(ActorMessage::Worker(Box::new(update))).await;
        });
    }

    fn finish_cancel(
        &mut self,
        turn_id: TurnId,
        outcome: CancelOutcome,
        recovered_to_ready: bool,
        completed_at_ms: TimestampMs,
        process_reaped: bool,
    ) {
        if let Err(error) = checked_actor_timestamp(completed_at_ms, "turn_completed_at") {
            self.fail_active(turn_id, error, process_reaped);
            return;
        }
        let active = self.active.take().expect("active turn was checked");
        self.process_reaped |= process_reaped;
        let target = if recovered_to_ready && !self.writable_attach_release_pending {
            SessionState::Ready
        } else {
            SessionState::Tainted
        };
        if let Err(error) = self.transition(target, None, Some(turn_id)) {
            self.poison_after_unpublishable_terminal(turn_id, error);
            for waiter in active.cancel_waiters {
                let _ = waiter.send(Ok(CancelTurnResult {
                    session_id: self.session_id,
                    generation_id: self.generation_id,
                    turn_id,
                    outcome: CancelOutcome::RecoveryFailed,
                    session_state: self.state,
                }));
            }
            return;
        }
        if self.writable_attach_release_pending {
            self.release_attach_after_failed_turn();
        }
        let mut final_sequence = self.next_sequence;
        if let Err(error) = self.emit(
            Some(turn_id),
            EventPayload::TurnCancelled(TurnCancelledEvent {
                outcome,
                recovered_to_ready,
            }),
        ) {
            self.poison_after_unpublishable_terminal(turn_id, error);
            for waiter in active.cancel_waiters {
                let _ = waiter.send(Ok(CancelTurnResult {
                    session_id: self.session_id,
                    generation_id: self.generation_id,
                    turn_id,
                    outcome: CancelOutcome::RecoveryFailed,
                    session_state: self.state,
                }));
            }
            return;
        }
        let mut terminal_outcome = if recovered_to_ready {
            TurnOutcome::Cancelled
        } else {
            TurnOutcome::Failed
        };
        if recovered_to_ready {
            let submitted_at_ms = self
                .turns
                .get(&turn_id)
                .map_or(completed_at_ms, |record| record.submitted_at_ms);
            let result = TurnResult {
                session_id: self.session_id,
                generation_id: self.generation_id,
                turn_id,
                outcome: TurnOutcome::Cancelled,
                text: String::new(),
                final_blocks: Vec::new(),
                tools: Vec::new(),
                model: None,
                stop_reason: None,
                usage: UsageBreakdown::default(),
                // A cancelled turn committed no transcript analysis, so it
                // counted no rows of any kind. Zero here is the empty window,
                // the same one `TranscriptAnalysis` reports before a prompt is
                // acknowledged.
                sidechain_rows: 0,
                timings: TurnTimings {
                    submitted_at_ms,
                    prompt_acknowledged_at_ms: None,
                    terminal_candidate_at_ms: None,
                    completed_at_ms,
                    drain_ms: None,
                    last_transcript_activity_at_ms: None,
                    stop_hook_at_ms: None,
                    // A cancelled turn reached no drain gate, so it has no
                    // arrival-order measurement to report either.
                    turn_duration_observed_at_ms: None,
                    post_turn_duration_row_observed_at_ms: None,
                },
                warnings: compatibility_warnings(&self.compatibility)
                    .into_iter()
                    .chain(permission_bypass_warnings(self.dangerous_permission_bypass))
                    .collect(),
                claude_version: self.compatibility.claude_version.clone(),
                compatibility: self.compatibility.clone(),
                completion: CompletionProvenance {
                    authority: CompletionAuthority::Transcript,
                    prompt_acknowledged: false,
                    terminal_message_observed: false,
                    terminal_prompt_observed: true,
                    terminal_quiet_observed: true,
                    transcript_drained: true,
                    lifecycle_hook_observed: false,
                },
                final_sequence,
            };
            if let Err(error) =
                self.try_store_terminal(turn_id, StoredTurnTerminal::Result(Box::new(result)))
            {
                let error = self.store_bounded_failure(turn_id, error);
                final_sequence = self.next_sequence;
                if let Err(emit_error) = self.emit(Some(turn_id), EventPayload::TurnFailed(error)) {
                    self.poison_after_unpublishable_terminal(turn_id, emit_error);
                    for waiter in active.cancel_waiters {
                        let _ = waiter.send(Ok(CancelTurnResult {
                            session_id: self.session_id,
                            generation_id: self.generation_id,
                            turn_id,
                            outcome: CancelOutcome::RecoveryFailed,
                            session_state: self.state,
                        }));
                    }
                    return;
                }
                terminal_outcome = TurnOutcome::Failed;
            }
        } else {
            let _ = self.store_bounded_failure(
                turn_id,
                ErrorBody::new(
                    ErrorCode::RecoveryFailed,
                    "Claude did not recover to a ready prompt after cancellation",
                ),
            );
            // The TurnCancelled event already carries recovery failure. The
            // compact stored error is for idempotent RunOnce/retry lookup.
        }
        self.last_turn = Some(TurnSummary {
            turn_id,
            outcome: terminal_outcome,
            completed_at_ms,
            final_sequence,
        });
        for waiter in active.cancel_waiters {
            let _ = waiter.send(Ok(CancelTurnResult {
                session_id: self.session_id,
                generation_id: self.generation_id,
                turn_id,
                outcome,
                session_state: self.state,
            }));
        }
    }

    async fn poll_startup_screen(&mut self) {
        match self.terminal.observe_screen(self.session_id).await {
            Ok(TerminalScreenObservation::Ready) => {
                self.resolve_needs_input(SessionState::Ready, None);
            }
            Ok(TerminalScreenObservation::NeedsInput(needs_input)) => {
                if let Err(error) = self.set_needs_input(needs_input, SessionState::Ready, None) {
                    let _ = self.transition(SessionState::Failed, Some(error.message), None);
                }
            }
            Ok(
                TerminalScreenObservation::Recognised(_)
                | TerminalScreenObservation::Unrecognised(_),
            ) => {
                // A startup modal is not considered resolved until Claude's
                // unambiguous ready prompt returns. Both arms sit here for that
                // one reason and not because they are the same screen: this
                // poll only ever RESOLVES a modal, it never raises one, so
                // neither an unrecognized screen nor a populated composer has
                // anything to add. What a persistently unrecognized screen
                // costs is decided on the turn path, where a turn is running to
                // a deadline that a startup poll does not have.
            }
            Err(error) => {
                self.needs_input = None;
                self.needs_input_resume = None;
                let _ = self.transition(SessionState::Failed, Some(error.message), None);
            }
        }
    }

    fn set_needs_input(
        &mut self,
        needs_input: NeedsInput,
        resume_state: SessionState,
        turn_id: Option<TurnId>,
    ) -> Result<(), ErrorBody> {
        let changed = self.needs_input.as_ref() != Some(&needs_input);
        let transition_needed = self.state != SessionState::NeedsInput;
        let required = u64::from(transition_needed) + u64::from(changed) + CLOSE_SEQUENCE_RESERVE;
        self.ensure_sequence_slots(required)?;
        if transition_needed {
            self.transition(
                SessionState::NeedsInput,
                Some("interactive_input_required".to_owned()),
                turn_id,
            )?;
        }
        self.needs_input = Some(needs_input.clone());
        self.needs_input_resume = Some(resume_state);
        if changed {
            self.emit(turn_id, EventPayload::NeedsInput(needs_input))?;
        }
        Ok(())
    }

    fn resolve_needs_input(&mut self, fallback: SessionState, turn_id: Option<TurnId>) {
        if self.needs_input.is_none() || self.state != SessionState::NeedsInput {
            return;
        }
        let resume_state = self.needs_input_resume.unwrap_or(fallback);
        let _ = self.transition(
            resume_state,
            Some("interactive_input_resolved".to_owned()),
            turn_id,
        );
    }

    fn transition(
        &mut self,
        current: SessionState,
        reason: Option<String>,
        turn_id: Option<TurnId>,
    ) -> Result<(), ErrorBody> {
        let previous = self.state;
        if previous == current {
            return Ok(());
        }
        if !is_valid_session_transition(previous, current) {
            return Err(ErrorBody::new(
                ErrorCode::Internal,
                format!("invalid session transition {previous:?} -> {current:?}"),
            ));
        }
        // A state change is not committed unless its public event can be
        // represented. This keeps sequence exhaustion fail-closed instead of
        // mutating state behind an unobservable saturated cursor.
        self.emit(
            turn_id,
            EventPayload::SessionStateChanged(SessionStateChanged {
                previous,
                current,
                reason,
            }),
        )?;
        if previous == SessionState::NeedsInput && current != SessionState::NeedsInput {
            self.needs_input = None;
            self.needs_input_resume = None;
        }
        self.state = current;
        Ok(())
    }

    fn emit(&mut self, turn_id: Option<TurnId>, payload: EventPayload) -> Result<u64, ErrorBody> {
        let timestamp_ms = self.checked_now_ms("event_timestamp")?;
        let sequence = self.next_sequence;
        if sequence > self.event_sequence_ceiling() {
            return Err(self.sequence_capacity_error(1));
        }
        let next_sequence = sequence.checked_add(1).ok_or_else(|| {
            ErrorBody::new(
                ErrorCode::RecoveryFailed,
                "event sequence cannot advance without overflow",
            )
        })?;
        debug_assert!(next_sequence <= MAX_SAFE_JSON_INTEGER);
        let payload_type = event_payload_name(&payload);
        let mut event = EventEnvelope::new(
            self.session_id,
            self.generation_id,
            turn_id,
            sequence,
            timestamp_ms,
            payload,
        );
        let actual_bytes = single_event_response_bytes(&event);
        if actual_bytes == usize::MAX {
            return Err(ErrorBody::new(
                ErrorCode::RecoveryFailed,
                "event payload cannot be serialized within the protocol-v1 domain",
            ));
        }
        if actual_bytes > self.frame_limit() {
            let actual_bytes = u64::try_from(actual_bytes)
                .ok()
                .filter(|bytes| *bytes <= MAX_SAFE_JSON_INTEGER)
                .ok_or_else(|| {
                    ErrorBody::new(
                        ErrorCode::RecoveryFailed,
                        "event size cannot be represented within the protocol-v1 domain",
                    )
                })?;
            event = EventEnvelope::new(
                self.session_id,
                self.generation_id,
                turn_id,
                sequence,
                timestamp_ms,
                EventPayload::Warning(ProtocolWarning {
                    code: "event_payload_too_large".to_owned(),
                    message: "event payload was omitted because it exceeds the native frame limit"
                        .to_owned(),
                    details: json!({
                        "event_type": payload_type,
                        "actual_bytes": actual_bytes,
                        "maximum_bytes": diagnostic_usize(self.frame_limit()),
                    }),
                }),
            );
            let bounded_bytes = single_event_response_bytes(&event);
            if bounded_bytes == usize::MAX || bounded_bytes > self.frame_limit() {
                return Err(ErrorBody::new(
                    ErrorCode::ResultTooLarge,
                    "bounded event warning exceeds the configured native frame limit",
                )
                .with_details(json!({
                    "actual_bytes": diagnostic_usize(bounded_bytes),
                    "maximum_bytes": diagnostic_usize(self.frame_limit()),
                })));
            }
        }
        let encoded_bytes = serialized_bytes(&event);
        if encoded_bytes == usize::MAX {
            return Err(ErrorBody::new(
                ErrorCode::RecoveryFailed,
                "bounded event payload cannot be serialized within the protocol-v1 domain",
            ));
        }
        let replay_bytes = self
            .replay_bytes
            .checked_add(encoded_bytes)
            .ok_or_else(|| {
                ErrorBody::new(
                    ErrorCode::RecoveryFailed,
                    "event replay byte accounting overflowed",
                )
            })?;
        self.updated_at_ms = timestamp_ms;
        self.replay_bytes = replay_bytes;
        self.replay.push_back(ReplayRecord {
            event,
            encoded_bytes,
        });
        while self.replay.len() > self.config.replay_capacity
            || self.replay_bytes > self.config.replay_byte_capacity
        {
            let Some(removed) = self.replay.pop_front() else {
                break;
            };
            self.replay_bytes = self.replay_bytes.saturating_sub(removed.encoded_bytes);
        }
        self.next_sequence = next_sequence;
        self.sequence.send_replace(sequence);
        Ok(sequence)
    }

    fn event_sequence_ceiling(&self) -> u64 {
        self.config.event_sequence_ceiling.min(MAX_EVENT_SEQUENCE)
    }

    fn remaining_sequence_slots(&self) -> u64 {
        self.event_sequence_ceiling()
            .checked_sub(self.next_sequence)
            .map_or(0, |remaining| remaining + 1)
    }

    fn ensure_sequence_slots(&self, required: u64) -> Result<(), ErrorBody> {
        self.checked_now_ms("event_timestamp")?;
        if self.remaining_sequence_slots() < required {
            return Err(self.sequence_capacity_error(required));
        }
        Ok(())
    }

    fn checked_now_ms(&self, resource: &'static str) -> Result<TimestampMs, ErrorBody> {
        let timestamp_ms = checked_actor_timestamp(self.clock.now_ms(), resource)?;
        checked_idle_deadline(timestamp_ms, self.idle_ttl_ms)?;
        Ok(timestamp_ms)
    }

    fn sequence_capacity_error(&self, required: u64) -> ErrorBody {
        ErrorBody::new(
            ErrorCode::RecoveryFailed,
            "session event-sequence capacity is exhausted; close and explicitly resume",
        )
        .with_details(json!({
            "resource": "event_sequence",
            "next_sequence": self.next_sequence,
            "maximum_event_sequence": self.event_sequence_ceiling(),
            "remaining_events": self.remaining_sequence_slots(),
            "required_events": required,
        }))
    }

    fn event_batch(&self, after_sequence: u64, max_events: u32) -> Result<EventBatch, ErrorBody> {
        if after_sequence >= self.next_sequence {
            return Err(ErrorBody::new(
                ErrorCode::InvalidConfig,
                "event cursor is ahead of the session's published history",
            )
            .with_details(json!({
                "requested_after": diagnostic_u64(after_sequence),
                "last_sequence": self.next_sequence - 1,
            })));
        }
        let oldest_available = self
            .replay
            .front()
            .map_or(self.next_sequence, |record| record.event.sequence);
        if after_sequence < oldest_available.saturating_sub(1) {
            let snapshot = self.snapshot();
            let next_sequence = self.next_sequence;
            let batch = EventBatch {
                events: Vec::new(),
                next_sequence,
                replay_gap: Some(ReplayGap {
                    requested_after: after_sequence,
                    oldest_available,
                    next_sequence,
                    snapshot: Box::new(snapshot),
                }),
            };
            return self.ensure_event_batch_fits(batch);
        }
        let limit = if max_events == 0 {
            self.config.default_event_batch_size
        } else {
            max_events as usize
        };
        debug_assert_eq!(
            u64::try_from(self.replay.len())
                .ok()
                .and_then(|retained| oldest_available.checked_add(retained)),
            Some(self.next_sequence),
            "the replay ring must remain one contiguous sequence suffix"
        );
        let page = replay_page_range(oldest_available, self.replay.len(), after_sequence, limit);
        let page_had_records = page.start < page.end;
        let events = fitting_event_page(self.replay.range(page), self.frame_limit())?;
        if page_had_records && events.is_empty() {
            return Err(ErrorBody::new(
                ErrorCode::RecoveryFailed,
                "retained event cannot fit without skipping its replay cursor",
            ));
        }
        let next_sequence = match events.last() {
            Some(event) => event.sequence.checked_add(1).ok_or_else(|| {
                ErrorBody::new(
                    ErrorCode::RecoveryFailed,
                    "event cursor cannot advance without overflow",
                )
            })?,
            None if after_sequence < self.next_sequence => after_sequence
                .checked_add(1)
                .filter(|next| *next <= MAX_SAFE_JSON_INTEGER)
                .ok_or_else(|| self.sequence_capacity_error(1))?,
            None => self.next_sequence,
        };
        self.ensure_event_batch_fits(EventBatch {
            events,
            next_sequence,
            replay_gap: None,
        })
    }

    fn ensure_event_batch_fits(&self, batch: EventBatch) -> Result<EventBatch, ErrorBody> {
        let actual_bytes = event_batch_response_bytes(&batch);
        if actual_bytes == usize::MAX {
            return Err(ErrorBody::new(
                ErrorCode::RecoveryFailed,
                "event batch cannot be serialized within the protocol-v1 domain",
            ));
        }
        if actual_bytes <= self.frame_limit() {
            return Ok(batch);
        }
        Err(ErrorBody::new(
            ErrorCode::ResultTooLarge,
            "event batch exceeds the native frame limit",
        )
        .with_details(json!({
            "actual_bytes": diagnostic_usize(actual_bytes),
            "maximum_bytes": diagnostic_usize(self.frame_limit()),
        })))
    }

    fn snapshot(&self) -> SessionSnapshot {
        let idle_deadline_ms =
            (matches!(self.state, SessionState::Ready | SessionState::NeedsInput)
                && self.active.is_none()
                && self.writable_attach.is_none())
            .then(|| {
                self.updated_at_ms
                    .checked_add(self.idle_ttl_ms)
                    .expect("actor timestamps preserve a representable idle deadline")
            });
        SessionSnapshot {
            session_id: self.session_id,
            generation_id: self.generation_id,
            transcript_session_id: self.transcript_session_id,
            cell: self.cell,
            state: self.state,
            cwd: self.cwd.clone(),
            active_turn_id: self.active.as_ref().map(|active| active.turn_id),
            claude_version: Some(self.compatibility.claude_version.clone()),
            compatibility: self.compatibility.clone(),
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
            idle_deadline_ms,
            resumable: self.resumable,
            last_sequence: self
                .next_sequence
                .checked_sub(1)
                .expect("actor event sequences start at one"),
            last_turn: self.last_turn.clone(),
            needs_input: self.needs_input.clone(),
            agent: self.agent.clone(),
        }
    }
}

/// Selects a page directly from the replay ring's contiguous sequence suffix.
///
/// Computing the retained offset avoids rescanning every earlier record for
/// each subsequent page. `event_batch` establishes the gap boundary before
/// calling this helper, so an invalid or overflowing cursor safely selects an
/// empty suffix.
fn replay_page_range(
    oldest_available: u64,
    replay_len: usize,
    after_sequence: u64,
    limit: usize,
) -> std::ops::Range<usize> {
    let start = after_sequence
        .checked_add(1)
        .and_then(|first_requested| first_requested.checked_sub(oldest_available))
        .and_then(|offset| usize::try_from(offset).ok())
        .unwrap_or(replay_len)
        .min(replay_len);
    let end = start.saturating_add(limit.max(1)).min(replay_len);
    start..end
}

fn checked_actor_timestamp(
    timestamp_ms: TimestampMs,
    resource: &'static str,
) -> Result<TimestampMs, ErrorBody> {
    if timestamp_ms <= MAX_SAFE_JSON_INTEGER {
        return Ok(timestamp_ms);
    }
    Err(ErrorBody::new(
        ErrorCode::RecoveryFailed,
        "actor timestamp is outside protocol-v1's safe-integer domain",
    )
    .with_details(json!({
        "resource": resource,
        "maximum": MAX_SAFE_JSON_INTEGER,
    })))
}

fn checked_idle_deadline(
    updated_at_ms: TimestampMs,
    idle_ttl_ms: u64,
) -> Result<TimestampMs, ErrorBody> {
    updated_at_ms
        .checked_add(idle_ttl_ms)
        .filter(|deadline| *deadline <= MAX_SAFE_JSON_INTEGER)
        .ok_or_else(|| {
            ErrorBody::new(
                ErrorCode::InvalidConfig,
                "idle deadline is outside protocol-v1's safe-integer domain",
            )
            .with_details(json!({
                "updated_at_ms": diagnostic_u64(updated_at_ms),
                "idle_ttl_ms": diagnostic_u64(idle_ttl_ms),
                "maximum": MAX_SAFE_JSON_INTEGER,
            }))
        })
}

fn diagnostic_u64(value: u64) -> Value {
    if value <= MAX_SAFE_JSON_INTEGER {
        value.into()
    } else {
        value.to_string().into()
    }
}

fn diagnostic_usize(value: usize) -> Value {
    u64::try_from(value).map_or_else(|_| value.to_string().into(), diagnostic_u64)
}

fn serialized_bytes(value: &impl Serialize) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |encoded| encoded.len())
}

fn response_bytes(result: ResponseResult) -> usize {
    serialized_bytes(&ResponseEnvelope::success(uuid::Uuid::nil(), result))
}

fn event_batch_response_bytes(batch: &EventBatch) -> usize {
    response_bytes(ResponseResult::Events(batch.clone()))
}

fn empty_event_batch_response_bytes(next_sequence: u64) -> usize {
    event_batch_response_bytes(&EventBatch {
        events: Vec::new(),
        next_sequence,
        replay_gap: None,
    })
}

fn fitting_event_page<'a>(
    records: impl Iterator<Item = &'a ReplayRecord>,
    frame_limit: usize,
) -> Result<Vec<EventEnvelope>, ErrorBody> {
    let mut events = Vec::new();
    let mut event_array_bytes = EVENT_ARRAY_FIELD_OVERHEAD_BYTES;
    for record in records {
        let separator = usize::from(!events.is_empty());
        let candidate_event_array_bytes = event_array_bytes
            .checked_add(separator)
            .and_then(|bytes| bytes.checked_add(record.encoded_bytes))
            .ok_or_else(|| {
                ErrorBody::new(
                    ErrorCode::RecoveryFailed,
                    "event page byte accounting overflowed",
                )
            })?;
        let candidate_next_sequence = record.event.sequence.checked_add(1).ok_or_else(|| {
            ErrorBody::new(
                ErrorCode::RecoveryFailed,
                "event cursor cannot advance without overflow",
            )
        })?;
        let candidate_bytes = empty_event_batch_response_bytes(candidate_next_sequence)
            .checked_add(candidate_event_array_bytes)
            .ok_or_else(|| {
                ErrorBody::new(
                    ErrorCode::RecoveryFailed,
                    "event page envelope accounting overflowed",
                )
            })?;
        if candidate_bytes > frame_limit {
            break;
        }
        event_array_bytes = candidate_event_array_bytes;
        events.push(record.event.clone());
    }
    Ok(events)
}

fn single_event_response_bytes(event: &EventEnvelope) -> usize {
    event_batch_response_bytes(&EventBatch {
        events: vec![event.clone()],
        next_sequence: MAX_SAFE_JSON_INTEGER,
        replay_gap: None,
    })
}

fn turn_result_wire_bytes(result: &TurnResult) -> usize {
    let mut worst_case = result.clone();
    worst_case.final_sequence = MAX_SAFE_JSON_INTEGER;
    let direct = response_bytes(ResponseResult::TurnResult(Box::new(worst_case.clone())));
    let event = EventEnvelope::new(
        worst_case.session_id,
        worst_case.generation_id,
        Some(worst_case.turn_id),
        MAX_EVENT_SEQUENCE,
        MAX_SAFE_JSON_INTEGER,
        EventPayload::TurnCompleted(Box::new(worst_case)),
    );
    direct.max(single_event_response_bytes(&event))
}

fn terminal_error_wire_bytes(
    session_id: SessionId,
    generation_id: SessionGenerationId,
    turn_id: TurnId,
    error: &ErrorBody,
) -> usize {
    let direct = serialized_bytes(&ResponseEnvelope::failure(uuid::Uuid::nil(), error.clone()));
    let event = EventEnvelope::new(
        session_id,
        generation_id,
        Some(turn_id),
        MAX_EVENT_SEQUENCE,
        MAX_SAFE_JSON_INTEGER,
        EventPayload::TurnFailed(error.clone()),
    );
    direct.max(single_event_response_bytes(&event))
}

fn stored_terminal_bytes(terminal: &StoredTurnTerminal) -> usize {
    match terminal {
        StoredTurnTerminal::Result(result) => serialized_bytes(result.as_ref()),
        StoredTurnTerminal::Failed(error) => serialized_bytes(error),
    }
}

const fn event_payload_name(payload: &EventPayload) -> &'static str {
    match payload {
        EventPayload::SessionStateChanged(_) => "session_state_changed",
        EventPayload::PromptAcknowledged(_) => "prompt_acknowledged",
        EventPayload::LogicalMessage(_) => "logical_message",
        EventPayload::ToolStarted(_) => "tool_started",
        EventPayload::ToolCompleted(_) => "tool_completed",
        EventPayload::RateLimit(_) => "rate_limit",
        EventPayload::NeedsInput(_) => "needs_input",
        EventPayload::TerminalCandidate(_) => "terminal_candidate",
        EventPayload::TurnCompleted(_) => "turn_completed",
        EventPayload::TurnCancelled(_) => "turn_cancelled",
        EventPayload::TurnFailed(_) => "turn_failed",
        EventPayload::Warning(_) => "warning",
        EventPayload::ReplayGap(_) => "replay_gap",
        EventPayload::Heartbeat(_) => "heartbeat",
    }
}

/// One fixed wall-clock/monotonic boundary for the entire accepted turn.
///
/// The wall value is the public lease. The monotonic value prevents a backward
/// system-clock adjustment from extending an already accepted turn. Neither is
/// recomputed after a blocking operation.
#[derive(Clone, Copy)]
struct TurnDeadline {
    unix_ms: TimestampMs,
    monotonic: Option<tokio::time::Instant>,
}

impl TurnDeadline {
    fn new(
        clock: &dyn Clock,
        requested_unix_ms: Option<TimestampMs>,
        submitted_at_ms: TimestampMs,
        default_timeout_ms: u64,
    ) -> Result<Self, ErrorBody> {
        let unix_ms =
            checked_turn_deadline_unix_ms(requested_unix_ms, submitted_at_ms, default_timeout_ms)?;
        let remaining_ms = unix_ms.saturating_sub(clock.now_ms());
        let monotonic =
            tokio::time::Instant::now().checked_add(Duration::from_millis(remaining_ms));
        Ok(Self { unix_ms, monotonic })
    }

    fn expired(self, clock: &dyn Clock) -> bool {
        self.monotonic
            .is_some_and(|deadline| tokio::time::Instant::now() >= deadline)
            || clock.now_ms() >= self.unix_ms
    }

    async fn elapsed(self) {
        match self.monotonic {
            Some(deadline) => tokio::time::sleep_until(deadline).await,
            None => pending::<()>().await,
        }
    }
}

fn checked_turn_deadline_unix_ms(
    requested_unix_ms: Option<TimestampMs>,
    submitted_at_ms: TimestampMs,
    default_timeout_ms: u64,
) -> Result<TimestampMs, ErrorBody> {
    match requested_unix_ms {
        Some(requested) if requested <= MAX_SAFE_JSON_INTEGER => Ok(requested),
        Some(_) => Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            "turn deadline is outside protocol-v1's safe-integer domain",
        )),
        None => submitted_at_ms
            .checked_add(default_timeout_ms)
            .filter(|deadline| *deadline <= MAX_SAFE_JSON_INTEGER)
            .ok_or_else(|| {
                ErrorBody::new(
                    ErrorCode::InvalidConfig,
                    "default turn deadline is outside protocol-v1's safe-integer domain",
                )
            }),
    }
}

enum TurnAwait<T> {
    Ready(T),
    Aborted,
    TimedOut,
}

async fn await_turn_step<T>(
    deadline: TurnDeadline,
    clock: &dyn Clock,
    signal: &mut watch::Receiver<WorkerSignal>,
    future: impl Future<Output = T>,
) -> TurnAwait<T> {
    if deadline.expired(clock) {
        return TurnAwait::TimedOut;
    }
    tokio::select! {
        biased;
        () = deadline.elapsed() => TurnAwait::TimedOut,
        _ = signal.changed() => TurnAwait::Aborted,
        value = future => {
            if deadline.expired(clock) {
                TurnAwait::TimedOut
            } else {
                TurnAwait::Ready(value)
            }
        }
    }
}

fn turn_timeout_error() -> ErrorBody {
    ErrorBody::new(ErrorCode::TurnTimeout, "turn deadline elapsed")
}

/// [`UNRECOGNISED_SCREEN_VETO`] in the unit the monitor's clock counts in.
///
/// DERIVED from the `Duration`, not written twice. The window is stated once,
/// where its measurement and its trade are argued, and the monitor converts;
/// two literals would be free to disagree, and the one that decided turns would
/// not be the one the doc comment justified.
const UNRECOGNISED_SCREEN_VETO_MS: u64 = UNRECOGNISED_SCREEN_VETO.as_millis() as u64;

/// The refusal a turn ends in when it sat on a screen no rule matched while its
/// transcript stood still.
///
/// # Why this is not a new `ErrorCode`
///
/// `crates/service/src/pool/refusal.rs` states the standing rule and its
/// reason: both shipped clients hard-reject an unknown code, so a daemon that
/// invents one costs an older caller the WHOLE response frame rather than just
/// the label. The specificity lives in `details.violation`, which is opaque
/// JSON and pins nothing.
///
/// `NeedsInput` is the honest choice among the codes that already exist,
/// because it is the one whose operator response is already correct: a person
/// must look at this screen. It is deliberately NOT `TurnTimeout` -- that is
/// the code the 600,000 ms silent hang already reported, and reusing it here
/// would make the veto indistinguishable from the failure it exists to replace.
fn unrecognised_screen_veto(shape: ScreenShape, held_ms: u64) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::NeedsInput,
        "Claude held a screen pmux does not recognize and the transcript stopped advancing",
    )
    .with_details(json!({
        "violation": "unrecognised_screen_veto",
        "held_ms": diagnostic_u64(held_ms),
        "veto_after_ms": diagnostic_u64(UNRECOGNISED_SCREEN_VETO_MS),
        // What "naming what was on screen" is allowed to mean. Structure only:
        // the text carries the caller's prompt, the account and the cwd, and it
        // never leaves the terminal adapter.
        "screen": shape.to_json(),
    }))
    .advising(
        "Claude rendered something pmux has no rule for and stopped producing transcript rows. \
         Remint the cell. If this screen is legitimate, it is a rendering pmux must be taught \
         rather than one it should wait out.",
    )
}

struct TurnWorker {
    session_id: SessionId,
    /// The id this turn's transcript reads are armed and polled under. Captured
    /// at spawn: a clear is refused while a turn is active, so it cannot rotate
    /// underneath the worker holding it.
    transcript_session_id: SessionId,
    /// Which completion proof this turn is *allowed* to try for. Per turn, from
    /// the actor's cell at spawn.
    cell: SessionCell,
    generation_id: SessionGenerationId,
    turn: TurnRequest,
    submitted_at_ms: TimestampMs,
    compatibility: CompatibilityReport,
    dangerous_permission_bypass: bool,
    terminal: Arc<dyn TerminalControl>,
    transcript: Arc<dyn TranscriptSource>,
    clock: Arc<dyn Clock>,
    config: SessionActorConfig,
    detached_tasks: Arc<TrackedTasks>,
    signal: watch::Receiver<WorkerSignal>,
    sender: mpsc::Sender<ActorMessage>,
}

impl TurnWorker {
    async fn run(mut self) {
        let deadline = match TurnDeadline::new(
            self.clock.as_ref(),
            self.turn.deadline_unix_ms,
            self.submitted_at_ms,
            self.config.default_turn_timeout_ms,
        ) {
            Ok(deadline) => deadline,
            Err(error) => {
                self.fail(error).await;
                return;
            }
        };
        if self.signal_value() != WorkerSignal::Run {
            return;
        }
        let transcript = Arc::clone(&self.transcript);
        let arm = match await_turn_step(
            deadline,
            self.clock.as_ref(),
            &mut self.signal,
            transcript.arm_at_eof(self.transcript_session_id),
        )
        .await
        {
            TurnAwait::Ready(Ok(arm)) => arm,
            TurnAwait::Ready(Err(error)) => {
                self.fail_driver(error).await;
                return;
            }
            TurnAwait::Aborted => return,
            TurnAwait::TimedOut => {
                self.timeout().await;
                return;
            }
        };
        let mut engine = TranscriptEngine::new(ParseMode::Strict);
        for row in arm.historical_rows {
            if let Err(error) = engine.ingest(row) {
                self.fail(map_transcript_error(error)).await;
                return;
            }
        }
        if let Err(error) = engine.arm_turn(&self.turn.prompt) {
            self.fail(map_transcript_error(error)).await;
            return;
        }
        let mut position = arm.position;
        // This is the last actor-side check before terminal input. The terminal
        // adapter receives the same immutable deadline and rechecks again at
        // its irreversible Enter boundary.
        if deadline.expired(self.clock.as_ref()) {
            self.timeout().await;
            return;
        }
        let terminal = Arc::clone(&self.terminal);
        let submitted = await_turn_step(
            deadline,
            self.clock.as_ref(),
            &mut self.signal,
            terminal.submit_prompt(
                self.session_id,
                self.turn.turn_id,
                &self.turn.prompt,
                deadline.unix_ms,
            ),
        )
        .await;
        match submitted {
            TurnAwait::Ready(Ok(())) => {}
            TurnAwait::Ready(Err(error)) => {
                self.fail_driver(error).await;
                return;
            }
            TurnAwait::Aborted => return,
            TurnAwait::TimedOut => {
                self.timeout().await;
                return;
            }
        }
        self.state(SessionState::AwaitingPromptAck).await;

        let mut emitted_ack = false;
        let mut emitted_messages = HashSet::new();
        let mut started_tools = HashSet::new();
        let mut completed_tools = HashSet::new();
        let mut emitted_warnings = HashSet::new();
        let mut terminal_candidate_at_ms = None;
        let mut prompt_acknowledged_at_ms = None;
        let mut arrival_order = ArrivalOrderObservations::default();
        let mut resume_state = SessionState::AwaitingPromptAck;
        let mut active_needs_input: Option<NeedsInput> = None;
        // Sticky for the whole turn, and created here because there is nowhere
        // else it could be. `active_needs_input` beside it is live state that
        // clears when the modal goes away, so reading it at commit time answers
        // "is a modal on screen now?" -- almost always no, including on the turn
        // where a permission prompt appeared and was dismissed. The minified
        // cell's check 9 asks the other question: did this turn ever show one.
        // Nothing clears this until the next turn's worker starts with a fresh
        // one.
        let mut needs_input_observed = false;
        let mut consecutive_other_screens = 0_u8;
        // The instant the current unbroken run of unrecognized screens began.
        // `None` whenever the last observation was anything else, or whenever a
        // transcript row arrived: see [`UNRECOGNISED_SCREEN_VETO_MS`].
        //
        // The START of the run, and deliberately not the shape seen there. The
        // refusal names the frame that was on screen when the veto fired, which
        // is the LAST one, because that is the frame still on the PTY when
        // the veto fires. This carried the first frame's
        // shape for exactly as long as it took to notice that nothing read it.
        let mut unrecognised_screen_since: Option<u64> = None;

        loop {
            if deadline.expired(self.clock.as_ref()) {
                self.timeout().await;
                return;
            }
            if self.signal_value() != WorkerSignal::Run {
                return;
            }

            let terminal = Arc::clone(&self.terminal);
            let observation = match await_turn_step(
                deadline,
                self.clock.as_ref(),
                &mut self.signal,
                terminal.observe_screen(self.session_id),
            )
            .await
            {
                TurnAwait::Ready(Ok(observation)) => observation,
                TurnAwait::Ready(Err(error)) => {
                    self.fail_driver(error).await;
                    return;
                }
                TurnAwait::Aborted => return,
                TurnAwait::TimedOut => {
                    self.timeout().await;
                    return;
                }
            };
            match observation {
                TerminalScreenObservation::NeedsInput(needs_input) => {
                    consecutive_other_screens = 0;
                    unrecognised_screen_since = None;
                    // Recorded on every observation, not only on a change: a
                    // modal that is observed, dismissed, and observed again is
                    // still a modal this turn showed.
                    needs_input_observed = true;
                    if active_needs_input.as_ref() != Some(&needs_input) {
                        active_needs_input = Some(needs_input.clone());
                        self.needs_input(Some(needs_input), resume_state).await;
                    }
                }
                TerminalScreenObservation::Ready => {
                    consecutive_other_screens = 0;
                    unrecognised_screen_since = None;
                    if active_needs_input.take().is_some() {
                        self.needs_input(None, resume_state).await;
                    }
                }
                TerminalScreenObservation::Recognised(_)
                | TerminalScreenObservation::Unrecognised(_) => {
                    if let TerminalScreenObservation::Unrecognised(shape) = observation {
                        // The veto. A screen no rule matched, held continuously
                        // while the transcript stands still, is the shape of
                        // every silent hang this driver has produced: nothing to
                        // report, nothing to wait for, and a deadline of up to
                        // 600,000 ms to reach.
                        //
                        // The clock only starts, and only keeps running, while
                        // BOTH halves are true. A transcript that is still
                        // arriving is a live turn whatever the screen looks
                        // like, so `unrecognised_screen_since` is cleared at the
                        // bottom of this loop on any row -- this can only ever
                        // fire on a turn that has stopped making progress by
                        // either measure, which is what makes it a liveness veto
                        // and not a second opinion about completion.
                        let now = self.clock.now_ms();
                        let held_ms = match unrecognised_screen_since {
                            None => {
                                unrecognised_screen_since = Some(now);
                                0
                            }
                            Some(since) => now.saturating_sub(since),
                        };
                        if held_ms >= UNRECOGNISED_SCREEN_VETO_MS {
                            self.fail(unrecognised_screen_veto(shape, held_ms)).await;
                            return;
                        }
                    } else {
                        unrecognised_screen_since = None;
                    }
                    if active_needs_input.is_some() {
                        consecutive_other_screens = consecutive_other_screens.saturating_add(1);
                        if consecutive_other_screens >= 2 {
                            active_needs_input = None;
                            consecutive_other_screens = 0;
                            self.needs_input(None, resume_state).await;
                        }
                    } else {
                        consecutive_other_screens = 0;
                    }
                }
            }

            let transcript = Arc::clone(&self.transcript);
            let batch = match await_turn_step(
                deadline,
                self.clock.as_ref(),
                &mut self.signal,
                transcript.poll(self.transcript_session_id, &position),
            )
            .await
            {
                TurnAwait::Ready(Ok(batch)) => batch,
                TurnAwait::Ready(Err(error)) => {
                    self.fail_driver(error).await;
                    return;
                }
                TurnAwait::Aborted => return,
                TurnAwait::TimedOut => {
                    self.timeout().await;
                    return;
                }
            };
            if let Err(error) = validate_position(&position, &batch) {
                self.fail_driver(error).await;
                return;
            }
            position = batch.position.clone();
            // The other half of the veto's conjunction. A row that arrived is
            // proof this turn is alive, whatever the screen is rendering, so it
            // restarts the unrecognized-screen clock rather than merely pausing
            // it.
            if !batch.rows.is_empty() {
                unrecognised_screen_since = None;
            }
            // Measurement, taken before ingest consumes the rows and before the
            // analysis that could fail this turn, so what is measured is the read
            // itself rather than the outcome of interpreting it.
            arrival_order.observe_read(self.clock.as_ref(), &batch.rows);
            for row in batch.rows {
                if let Err(error) = engine.ingest(row) {
                    self.fail(map_transcript_error(error)).await;
                    return;
                }
            }
            let analysis = match engine.analyze() {
                Ok(analysis) => analysis,
                Err(error) => {
                    self.fail(map_transcript_error(error)).await;
                    return;
                }
            };

            if let Some(acknowledgement) = analysis.acknowledgement.as_ref()
                && !emitted_ack
            {
                emitted_ack = true;
                let now =
                    match checked_actor_timestamp(self.clock.now_ms(), "prompt_acknowledged_at") {
                        Ok(now) => now,
                        Err(error) => {
                            self.fail(error).await;
                            return;
                        }
                    };
                prompt_acknowledged_at_ms = Some(now);
                let transcript_offset = engine
                    .rows()
                    .find(|row| row.common.uuid.as_deref() == Some(&acknowledgement.row_uuid))
                    .map_or(0, |row| row.source.byte_offset);
                self.event(EventPayload::PromptAcknowledged(PromptAcknowledged {
                    prompt_uuid: acknowledgement.row_uuid.clone(),
                    prompt_id: acknowledgement.prompt_id.clone(),
                    transcript_offset,
                }))
                .await;
                resume_state = SessionState::Running;
                self.state(SessionState::Running).await;
            }
            if let Err(error) = self
                .emit_analysis_deltas(
                    &analysis,
                    &mut emitted_messages,
                    &mut started_tools,
                    &mut completed_tools,
                    &mut emitted_warnings,
                )
                .await
            {
                self.fail(error).await;
                return;
            }

            if let TurnStatus::Terminal(final_turn) = &analysis.status {
                if terminal_candidate_at_ms.is_none() {
                    let now =
                        match checked_actor_timestamp(self.clock.now_ms(), "terminal_candidate_at")
                        {
                            Ok(now) => now,
                            Err(error) => {
                                self.fail(error).await;
                                return;
                            }
                        };
                    terminal_candidate_at_ms = Some(now);
                    self.state(SessionState::TerminalCandidate).await;
                    self.event(EventPayload::TerminalCandidate(TerminalCandidate {
                        message_id: logical_key_string(&final_turn.message_key),
                        stop_reason: final_turn.stop_reason.as_ref().map(map_stop_reason),
                    }))
                    .await;
                    resume_state = SessionState::Draining;
                    self.state(SessionState::Draining).await;
                }
                if active_needs_input.is_some() {
                    match await_turn_step(
                        deadline,
                        self.clock.as_ref(),
                        &mut self.signal,
                        tokio::time::sleep(self.config.poll_interval),
                    )
                    .await
                    {
                        TurnAwait::Ready(()) => {}
                        TurnAwait::Aborted => return,
                        TurnAwait::TimedOut => {
                            self.timeout().await;
                            return;
                        }
                    }
                    continue;
                }
                let terminal = Arc::clone(&self.terminal);
                let evidence = match await_turn_step(
                    deadline,
                    self.clock.as_ref(),
                    &mut self.signal,
                    terminal.completion_evidence(self.session_id, self.turn.turn_id),
                )
                .await
                {
                    TurnAwait::Ready(Ok(evidence)) => evidence,
                    TurnAwait::Ready(Err(error)) => {
                        self.fail_driver(error).await;
                        return;
                    }
                    TurnAwait::Aborted => return,
                    TurnAwait::TimedOut => {
                        self.timeout().await;
                        return;
                    }
                };
                // The graduated drain. Bound once, above both evaluation sites,
                // because a value that differed between the gate and the
                // confirming re-poll below would save nothing: the re-poll
                // `continue`s on an unsatisfied drain, so the longer of the two
                // requirements is the one the turn actually pays.
                //
                // `ready_prompt && quiet` stay in the conjunction unconditionally.
                // The marker is transcript evidence and the screen is the
                // liveness gate; substituting one for the other would also lose
                // the NeedsInput short-circuit above.
                let full_drain_ms = graduated_drain_ms(
                    self.compatibility.transcript_drain_ms,
                    analysis.turn_duration_seen,
                );
                // What the minified cell is *offered*. What it *earns* is
                // decided below, once the analysis and the timings are in the
                // form they will be published in. Offering the shorter value
                // here only lets an admissible turn reach that decision without
                // first waiting for a requirement it may not owe; a turn that
                // then fails a check is sent back around for the full window.
                // The Full cell is offered exactly what it always was.
                let offered_drain_ms =
                    minified_drain_ms(full_drain_ms, self.cell == SessionCell::Minified);
                if evidence.ready_prompt
                    && evidence.quiet
                    && batch.drain.satisfies(offered_drain_ms)
                {
                    // Terminal evidence is observed independently from the JSONL tail. Re-poll
                    // from the exact analyzed cursor after that evidence so a row appended in the
                    // observation window cannot be omitted from the committed result.
                    let transcript = Arc::clone(&self.transcript);
                    let confirmation = match await_turn_step(
                        deadline,
                        self.clock.as_ref(),
                        &mut self.signal,
                        transcript.poll(self.transcript_session_id, &position),
                    )
                    .await
                    {
                        TurnAwait::Ready(Ok(batch)) => batch,
                        TurnAwait::Ready(Err(error)) => {
                            self.fail_driver(error).await;
                            return;
                        }
                        TurnAwait::Aborted => return,
                        TurnAwait::TimedOut => {
                            self.timeout().await;
                            return;
                        }
                    };
                    if let Err(error) = validate_position(&position, &confirmation) {
                        self.fail_driver(error).await;
                        return;
                    }
                    let cursor_unchanged = confirmation.position == position;
                    let rows_unchanged = confirmation.rows.is_empty();
                    position = confirmation.position.clone();
                    // The confirming re-poll is a strictly later read, so a row
                    // it carries is exactly the late row this measurement exists
                    // to catch. Folding it in here rather than waiting for the
                    // next loop iteration keeps the marker's own instant honest:
                    // a marker first seen by this read must not be stamped with
                    // the following poll's clock.
                    arrival_order.observe_read(self.clock.as_ref(), &confirmation.rows);
                    for row in confirmation.rows {
                        if let Err(error) = engine.ingest(row) {
                            self.fail(map_transcript_error(error)).await;
                            return;
                        }
                    }
                    if !cursor_unchanged
                        || !rows_unchanged
                        || !confirmation.drain.satisfies(offered_drain_ms)
                    {
                        continue;
                    }

                    if deadline.expired(self.clock.as_ref()) {
                        self.timeout().await;
                        return;
                    }
                    let completed_at_ms =
                        match checked_actor_timestamp(self.clock.now_ms(), "turn_completed_at") {
                            Ok(now) => now,
                            Err(error) => {
                                self.fail(error).await;
                                return;
                            }
                        };
                    let result = build_turn_result(
                        ResultContext {
                            session_id: self.session_id,
                            generation_id: self.generation_id,
                            turn_id: self.turn.turn_id,
                            submitted_at_ms: self.submitted_at_ms,
                            prompt_acknowledged_at_ms,
                            terminal_candidate_at_ms,
                            completed_at_ms,
                            drain_stable_for_ms: confirmation.drain.stable_for_ms,
                            compatibility: &self.compatibility,
                            dangerous_permission_bypass: self.dangerous_permission_bypass,
                            evidence,
                            arrival_order,
                        },
                        &analysis,
                    );
                    // Path B's shorter proof is earned here or not at all.
                    //
                    // Asked after the confirming re-poll and against
                    // `result.timings`, so the checks read the arrival-order
                    // pair the operator will see rather than a second copy of
                    // it. The re-poll came back with an unmoved cursor and no
                    // rows, so this analysis is the committed one.
                    //
                    // A refusal is about this turn only. It never fails the
                    // turn and never disables the cell: the turn goes back
                    // around the loop and finishes on the Full cell's drain,
                    // which is the whole cost of being wrong about
                    // admissibility. Being wrong the other way would commit a
                    // truncated turn, and the two are not comparable.
                    if self.cell == SessionCell::Minified {
                        let verdict = evaluate_minified_fast_path(MinifiedTurnObservations {
                            analysis: &analysis,
                            timings: &result.timings,
                            needs_input_observed,
                        });
                        let earned_drain_ms =
                            minified_drain_ms(full_drain_ms, verdict.is_admissible());
                        if !confirmation.drain.satisfies(earned_drain_ms) {
                            continue;
                        }
                    }
                    if validate_v1_serializable(&result).is_err() {
                        self.fail(unserializable_transcript_payload_error()).await;
                        return;
                    }
                    // Result construction and actor-channel backpressure are
                    // not allowed to turn an expired lease into success. The
                    // actor rechecks this same boundary once more at commit.
                    if deadline.expired(self.clock.as_ref()) {
                        self.timeout().await;
                        return;
                    }
                    self.send(WorkerUpdate::Completed {
                        turn_id: self.turn.turn_id,
                        result: Box::new(result),
                        deadline,
                    })
                    .await;
                    return;
                }
            }
            match await_turn_step(
                deadline,
                self.clock.as_ref(),
                &mut self.signal,
                tokio::time::sleep(self.config.poll_interval),
            )
            .await
            {
                TurnAwait::Ready(()) => {}
                TurnAwait::Aborted => return,
                TurnAwait::TimedOut => {
                    self.timeout().await;
                    return;
                }
            }
        }
    }

    async fn emit_analysis_deltas(
        &self,
        analysis: &TranscriptAnalysis,
        emitted_messages: &mut HashSet<LogicalMessageKey>,
        started_tools: &mut HashSet<String>,
        completed_tools: &mut HashSet<String>,
        emitted_warnings: &mut HashSet<String>,
    ) -> Result<(), ErrorBody> {
        for message in &analysis.messages {
            if (message.stop_reason.is_some() || message.is_api_error)
                && emitted_messages.insert(message.key.clone())
            {
                self.transcript_event(EventPayload::LogicalMessage(map_logical_message(message)))
                    .await?;
            }
        }
        for tool in &analysis.tools {
            if started_tools.insert(tool.tool_use_id.clone()) {
                self.transcript_event(EventPayload::ToolStarted(ToolStarted {
                    tool_use_id: tool.tool_use_id.clone(),
                    name: tool.name.clone(),
                    input: tool.input.clone(),
                }))
                .await?;
            }
            if let Some(result) = tool.result.as_ref()
                && completed_tools.insert(tool.tool_use_id.clone())
            {
                self.transcript_event(EventPayload::ToolCompleted(ToolCompleted {
                    tool_use_id: tool.tool_use_id.clone(),
                    output: result.content.clone(),
                    is_error: result.is_error.unwrap_or(false),
                }))
                .await?;
            }
        }
        for warning in &analysis.warnings {
            let key = format!("{warning:?}");
            if emitted_warnings.insert(key) {
                self.transcript_event(EventPayload::Warning(map_warning(warning)))
                    .await?;
            }
        }
        Ok(())
    }

    async fn state(&self, state: SessionState) {
        self.send(WorkerUpdate::State {
            turn_id: self.turn.turn_id,
            state,
        })
        .await;
    }

    async fn event(&self, payload: EventPayload) {
        self.send(WorkerUpdate::Event {
            turn_id: self.turn.turn_id,
            payload,
        })
        .await;
    }

    async fn transcript_event(&self, payload: EventPayload) -> Result<(), ErrorBody> {
        validate_v1_serializable(&payload)
            .map_err(|_| unserializable_transcript_payload_error())?;
        self.event(payload).await;
        Ok(())
    }

    async fn needs_input(&self, needs_input: Option<NeedsInput>, resume_state: SessionState) {
        self.send(WorkerUpdate::NeedsInput {
            turn_id: self.turn.turn_id,
            needs_input,
            resume_state,
        })
        .await;
    }

    async fn fail_driver(&self, error: DriverFailure) {
        if error.code == ErrorCode::TurnTimeout {
            self.timeout().await;
        } else {
            self.fail(error.into_protocol()).await;
        }
    }

    async fn timeout(&self) {
        // Publish the actor-owned outcome before cleanup. `run_once` and an
        // idempotent retry can now observe only this stored timeout, while the
        // worker independently proves the terminal process boundary reaped.
        self.send(WorkerUpdate::Failed {
            turn_id: self.turn.turn_id,
            error: turn_timeout_error(),
            process_reaped: false,
        })
        .await;
        let _ = force_reap_terminal(
            Arc::clone(&self.terminal),
            &self.detached_tasks,
            self.session_id,
            self.config.cancel_recovery_timeout,
        )
        .await;
    }

    async fn fail(&self, error: ErrorBody) {
        let process_reaped = force_reap_terminal(
            Arc::clone(&self.terminal),
            &self.detached_tasks,
            self.session_id,
            self.config.cancel_recovery_timeout,
        )
        .await;
        self.send(WorkerUpdate::Failed {
            turn_id: self.turn.turn_id,
            error,
            process_reaped,
        })
        .await;
    }

    async fn send(&self, update: WorkerUpdate) {
        let _ = self
            .sender
            .send(ActorMessage::Worker(Box::new(update)))
            .await;
    }

    fn signal_value(&self) -> WorkerSignal {
        *self.signal.borrow()
    }
}

/// Forces teardown, and reports only whether the reap was *proven* within
/// `timeout`.
///
/// The close is deliberately detached rather than awaited in place, and this
/// function therefore takes an owned `Arc` instead of a borrow.
///
/// Every other cancellable terminal call is made drop-safe beneath the
/// `TerminalSession` trait, in `pseudomux_rmux::backend`. Close cannot be:
/// `RmuxTerminal::close` is a compound `&mut self` state machine -- observe the
/// process boundary, request rmux cleanup, wait for the reap, force-reap the
/// exact POSIX session members -- and dropping it midway leaves that machine
/// half-run. The hazard is not only a poisoned transport (the SDK treats a
/// dropped in-flight request as a permanent transport failure; see
/// rmux-sdk transport/cancellation.rs:27-34) but a partial teardown: a kill
/// requested and never confirmed, with the interactive process still alive and
/// nothing left running that intends to reap it.
///
/// So the whole close runs on its own task. `tokio::time::timeout` around a
/// `JoinHandle` cancels only the *wait*: dropping a `JoinHandle` detaches the
/// task instead of aborting it, so an over-deadline close keeps running to
/// completion in the background. This function's `false` therefore means
/// "not proven reaped by the deadline", exactly as before, and never
/// "abandoned".
///
/// That is also why the task takes a [`TrackedTasks`] permit. "Keeps running in
/// the background" is only an acceptable answer while something can still prove
/// it finished; without the permit, a service shutdown could return with a kill
/// request it issued still in flight, which is the same
/// requested-but-never-confirmed teardown the detach exists to prevent. The
/// permit is taken on the caller's task and moved into the spawned one, so the
/// count covers the whole close and not merely the wait.
async fn force_reap_terminal(
    terminal: Arc<dyn TerminalControl>,
    detached_tasks: &Arc<TrackedTasks>,
    session_id: SessionId,
    timeout: Duration,
) -> bool {
    let task_permit = detached_tasks.track();
    let close = tokio::spawn(async move {
        let reaped = terminal.close(session_id, ClosePolicy::Force).await;
        drop(task_permit);
        reaped
    });
    matches!(tokio::time::timeout(timeout, close).await, Ok(Ok(Ok(true))))
}

/// Cancellation may produce one or more late transcript rows after Claude's
/// prompt has returned. A subsequent turn must not arm until both authorities
/// are quiet, otherwise those rows can be mistaken for the next turn's active
/// graph. Start at the post-interrupt EOF, validate every later complete row,
/// and require the same exact drain criterion used for ordinary completion.
async fn stabilize_cancelled_transcript(
    transcript: &dyn TranscriptSource,
    session_id: SessionId,
    required_stable_ms: u64,
    poll_interval: Duration,
    deadline: tokio::time::Instant,
) -> bool {
    let arm = match tokio::time::timeout_at(deadline, transcript.arm_at_eof(session_id)).await {
        Ok(Ok(arm)) => arm,
        Ok(Err(_)) | Err(_) => return false,
    };
    let mut position = arm.position;

    loop {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        let batch =
            match tokio::time::timeout_at(deadline, transcript.poll(session_id, &position)).await {
                Ok(Ok(batch)) => batch,
                Ok(Err(_)) | Err(_) => return false,
            };
        if validate_position(&position, &batch).is_err() {
            return false;
        }
        let unchanged = batch.position == position && batch.rows.is_empty();
        position = batch.position.clone();
        if unchanged && batch.drain.satisfies(required_stable_ms) {
            return true;
        }

        let now = tokio::time::Instant::now();
        if now >= deadline {
            return false;
        }
        tokio::time::sleep_until((now + poll_interval).min(deadline)).await;
    }
}

fn validate_position(
    previous: &TranscriptPosition,
    batch: &TranscriptBatch,
) -> Result<(), DriverFailure> {
    if batch.position.generation != previous.generation {
        return Err(DriverFailure::new(
            ErrorCode::TranscriptUnavailable,
            "transcript file generation changed during an active turn",
        ));
    }
    if batch.position.offset < previous.offset {
        return Err(DriverFailure::new(
            ErrorCode::SchemaDrift,
            "transcript cursor moved backwards during an active turn",
        ));
    }
    Ok(())
}

/// Arrival-order facts about Claude's `turn_duration` marker, folded in one
/// transcript read at a time.
///
/// Measurement only: no completion decision, warning, event, or state
/// transition reads any of this, and the instrument cannot contaminate what it
/// measures because it writes nothing Claude can see -- it only stamps a clock
/// against reads pmux was already performing.
///
/// The two published instants are meaningful only as a pair, so they are folded
/// by one method rather than tracked as loose locals: the second says whether
/// anything the analysis reads arrived after the first, and publishing it alone
/// would say "late" without saying late relative to what.
#[derive(Clone, Copy, Debug, Default)]
struct ArrivalOrderObservations {
    /// Whether a read carrying the marker has already been folded in. Held
    /// separately from the instant so an unpublishable clock reading suppresses
    /// the field without re-arming the window on the next read, which would
    /// otherwise stamp the marker later than pmux actually saw it.
    marker_observed: bool,
    turn_duration_observed_at_ms: Option<TimestampMs>,
    post_turn_duration_row_observed_at_ms: Option<TimestampMs>,
}

impl ArrivalOrderObservations {
    /// Folds in the rows delivered by one completed transcript read.
    ///
    /// The read, not the row, is the unit -- and that is exact rather than
    /// approximate. pmux can only admit completion after a whole read has been
    /// ingested and analyzed, so rows delivered alongside the marker were
    /// already in hand and no marker-triggered completion could have dropped
    /// them. Only a row from a strictly later read could have been lost.
    ///
    /// The clock is read only when there is a fact to stamp, so a turn whose
    /// transcript carries no marker performs no clock read here at all.
    fn observe_read(&mut self, clock: &dyn Clock, rows: &[ParsedRow]) {
        if self.marker_observed {
            if self.turn_duration_observed_at_ms.is_some()
                && self.post_turn_duration_row_observed_at_ms.is_none()
                && rows.iter().any(ParsedRow::is_analysis_changing)
            {
                self.post_turn_duration_row_observed_at_ms = publishable_instant(clock.now_ms());
            }
            return;
        }
        if rows.iter().any(ParsedRow::is_turn_duration_marker) {
            self.marker_observed = true;
            self.turn_duration_observed_at_ms = publishable_instant(clock.now_ms());
        }
    }
}

/// A measured instant is published only when protocol v1 can represent it.
///
/// Absent, never clamped, and never an error: an out-of-domain reading is a
/// missing measurement, and failing a turn over one would let a pure instrument
/// change the outcome it exists to observe.
fn publishable_instant(timestamp_ms: TimestampMs) -> Option<TimestampMs> {
    (timestamp_ms <= MAX_SAFE_JSON_INTEGER).then_some(timestamp_ms)
}

struct ResultContext<'a> {
    session_id: SessionId,
    generation_id: SessionGenerationId,
    turn_id: TurnId,
    submitted_at_ms: TimestampMs,
    prompt_acknowledged_at_ms: Option<TimestampMs>,
    terminal_candidate_at_ms: Option<TimestampMs>,
    completed_at_ms: TimestampMs,
    /// The observed `TranscriptDrainEvidence::stable_for_ms` of the poll that
    /// satisfied the drain gate, carried verbatim. Named for what it is rather
    /// than for the wire field it feeds, because the wire field's name invites
    /// the wrong reading: this is how long the transcript had been unchanged,
    /// not how long the actor waited after the terminal candidate.
    drain_stable_for_ms: u64,
    compatibility: &'a CompatibilityReport,
    dangerous_permission_bypass: bool,
    evidence: TerminalEvidence,
    /// Observation-order measurement for the `turn_duration` marker, carried
    /// verbatim to the wire. Nothing else in result construction reads it.
    arrival_order: ArrivalOrderObservations,
}

fn build_turn_result(context: ResultContext<'_>, analysis: &TranscriptAnalysis) -> TurnResult {
    let TurnStatus::Terminal(final_turn) = &analysis.status else {
        unreachable!("result construction requires a transcript terminal candidate")
    };
    let main = map_usage(&analysis.usage.tokens);
    let sidechain = map_usage(&analysis.sidechain_usage.tokens);
    let combined = map_usage(&analysis.combined_usage.tokens);
    let final_blocks = final_turn
        .final_text_blocks
        .iter()
        .map(|text| MessageBlock::Text { text: text.clone() })
        .collect();
    let tools = analysis.tools.iter().map(map_tool_record).collect();
    let mut warnings: Vec<_> = analysis.warnings.iter().map(map_warning).collect();
    warnings.extend(compatibility_warnings(context.compatibility));
    warnings.extend(permission_bypass_warnings(
        context.dangerous_permission_bypass,
    ));
    if context.evidence.lifecycle_expected && !context.evidence.lifecycle_hook_observed {
        warnings.push(ProtocolWarning {
            code: "lifecycle_hook_missing".to_owned(),
            message: "Hybrid lifecycle hook was not observed before transcript completion"
                .to_owned(),
            details: Value::Null,
        });
    }
    // A `stop_hook_summary` row on the chain proves some Stop hook ran. Under
    // the hybrid lifecycle that hook is pmux's own and is already rendered as
    // `completion.lifecycle_hook_observed` below, so repeating it here would add
    // a second name for one fact. In transcript mode pmux installed no hook, so
    // the row is the only evidence that the CALLER's Stop hook fired inside a
    // pmux turn -- worth saying out loud, because caller hooks merge additively
    // and a caller hook that blocks the stop is rejected as schema drift.
    if analysis.stop_hook_summary_seen && !context.evidence.lifecycle_expected {
        warnings.push(ProtocolWarning {
            code: "caller_stop_hook_observed".to_owned(),
            message: "a caller-installed Claude Stop hook ran during this turn".to_owned(),
            details: Value::Null,
        });
    }
    // Claude retried the model request inside this turn and recovered. pmux
    // completed normally, so nothing else in the result would say so -- and
    // without it a turn stretched by eight transport retries is indistinguish-
    // able from a turn pmux itself stalled. The count is the ladder length, not
    // the number of network incidents: one dropped connection emits one row per
    // attempt.
    if analysis.api_error_retries_seen > 0 {
        warnings.push(ProtocolWarning {
            code: "claude_api_retry_observed".to_owned(),
            message: "Claude retried the model request after a transport error during this turn"
                .to_owned(),
            details: json!({ "api_error_rows": analysis.api_error_retries_seen }),
        });
    }
    TurnResult {
        session_id: context.session_id,
        generation_id: context.generation_id,
        turn_id: context.turn_id,
        outcome: match final_turn.outcome {
            ClaudeTerminalOutcome::ApiError => TurnOutcome::Failed,
            _ => TurnOutcome::Completed,
        },
        text: final_turn.final_text.clone(),
        final_blocks,
        tools,
        model: final_turn.model.clone(),
        stop_reason: final_turn.stop_reason.as_ref().map(map_stop_reason),
        usage: UsageBreakdown {
            main,
            sidechain,
            combined,
            cost_usd: None,
        },
        // Carried through from the analysis rather than recomputed. It is a
        // COUNT and `usage.sidechain` is a token total; a turn can have the
        // first without the second, and that turn is the one a tool-less cell
        // must refuse.
        sidechain_rows: analysis.sidechain_rows,
        timings: TurnTimings {
            submitted_at_ms: context.submitted_at_ms,
            prompt_acknowledged_at_ms: context.prompt_acknowledged_at_ms,
            terminal_candidate_at_ms: context.terminal_candidate_at_ms,
            completed_at_ms: context.completed_at_ms,
            drain_ms: Some(context.drain_stable_for_ms),
            // Anchor the observed stability onto the wall clock so a consumer
            // can say *when* the transcript last moved instead of only how long
            // pmux then waited. `checked_sub` rather than a saturating one: a
            // stability that predates the epoch-relative completion timestamp is
            // unrepresentable, and an unrepresentable observation is reported
            // absent rather than published as a plausible instant.
            last_transcript_activity_at_ms: context
                .completed_at_ms
                .checked_sub(context.drain_stable_for_ms),
            // Carried verbatim from terminal evidence, never derived and never
            // differenced here. The consumer's quantity is the signed
            // `stop_hook_at_ms - last_transcript_activity_at_ms`; publishing
            // both instants keeps its sign observable.
            stop_hook_at_ms: context.evidence.lifecycle_hook_at_ms,
            // Both carried verbatim from the reads that observed them, for the
            // same reason the two fields above are: the consumer's quantity is a
            // signed difference, and pre-subtracting here would clamp exactly
            // the boundary that decides whether a `turn_duration` fast path
            // could have dropped a row.
            turn_duration_observed_at_ms: context.arrival_order.turn_duration_observed_at_ms,
            post_turn_duration_row_observed_at_ms: context
                .arrival_order
                .post_turn_duration_row_observed_at_ms,
        },
        warnings,
        claude_version: context.compatibility.claude_version.clone(),
        compatibility: context.compatibility.clone(),
        completion: CompletionProvenance {
            authority: CompletionAuthority::Transcript,
            prompt_acknowledged: analysis.acknowledgement.is_some(),
            terminal_message_observed: true,
            terminal_prompt_observed: context.evidence.ready_prompt,
            terminal_quiet_observed: context.evidence.quiet,
            transcript_drained: true,
            lifecycle_hook_observed: context.evidence.lifecycle_hook_observed,
        },
        final_sequence: 0,
    }
}

fn map_logical_message(message: &LogicalAssistantMessage) -> LogicalMessage {
    let (message_id, request_id) = match &message.key {
        LogicalMessageKey::MessageId(id) => (id.clone(), None),
        LogicalMessageKey::RequestId(id) => (format!("request:{id}"), Some(id.clone())),
        LogicalMessageKey::RowUuid(uuid) => (uuid.clone(), None),
    };
    LogicalMessage {
        message_id,
        request_id,
        scope: MessageScope::Main,
        blocks: message
            .blocks
            .iter()
            .filter_map(map_message_block)
            .collect(),
        model: message.model.clone(),
        stop_reason: message.stop_reason.as_ref().map(map_stop_reason),
        usage: message.usage.as_ref().map(|usage| map_usage(&usage.tokens)),
        terminal: message.is_api_error
            || matches!(
                message.stop_reason,
                Some(
                    ClaudeStopReason::EndTurn
                        | ClaudeStopReason::MaxTokens
                        | ClaudeStopReason::StopSequence
                        | ClaudeStopReason::Refusal
                )
            ),
    }
}

fn map_message_block(block: &ClaudeBlock) -> Option<MessageBlock> {
    match block {
        ClaudeBlock::Text { text } => Some(MessageBlock::Text { text: text.clone() }),
        ClaudeBlock::Thinking { .. } => None,
        ClaudeBlock::ToolUse { id, name, input } => Some(MessageBlock::ToolUse {
            id: id.clone(),
            name: name.clone(),
            input: input.clone(),
        }),
        ClaudeBlock::Unknown { declared_type, raw } => Some(MessageBlock::Unknown {
            block_type: declared_type
                .clone()
                .unwrap_or_else(|| "unknown".to_owned()),
            data: raw.clone(),
        }),
    }
}

fn map_tool_record(tool: &pseudomux_claude::ToolRecord) -> ToolRecord {
    let is_error = tool
        .result
        .as_ref()
        .and_then(|result| result.is_error)
        .unwrap_or(false);
    ToolRecord {
        tool_use_id: tool.tool_use_id.clone(),
        name: tool.name.clone(),
        input: tool.input.clone(),
        output: tool.result.as_ref().map(|result| result.content.clone()),
        status: match tool.result.as_ref() {
            None => ToolStatus::Requested,
            Some(_) if is_error => ToolStatus::Failed,
            Some(_) => ToolStatus::Completed,
        },
        started_at_ms: None,
        completed_at_ms: None,
    }
}

fn map_usage(usage: &pseudomux_claude::TokenUsage) -> TokenUsage {
    TokenUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_creation_input_tokens: usage.cache_creation_input_tokens,
        cache_read_input_tokens: usage.cache_read_input_tokens,
    }
}

fn map_stop_reason(reason: &ClaudeStopReason) -> StopReason {
    let (kind, raw) = match reason {
        ClaudeStopReason::EndTurn => (StopReasonKind::EndTurn, None),
        ClaudeStopReason::MaxTokens => (StopReasonKind::MaxTokens, None),
        ClaudeStopReason::StopSequence => (StopReasonKind::StopSequence, None),
        ClaudeStopReason::ToolUse => (StopReasonKind::ToolUse, None),
        ClaudeStopReason::PauseTurn => (StopReasonKind::PauseTurn, None),
        ClaudeStopReason::Refusal => (StopReasonKind::Refusal, None),
        ClaudeStopReason::Unknown(value) => (StopReasonKind::Unknown, Some(value.clone())),
    };
    StopReason { kind, raw }
}

fn compatibility_warnings(report: &CompatibilityReport) -> Vec<ProtocolWarning> {
    if report.tested {
        return Vec::new();
    }
    vec![ProtocolWarning {
        code: "untested_compatibility_profile".to_owned(),
        message: "this turn used an explicit allow_untested compatibility override".to_owned(),
        details: json!({
            "claude_version": report.claude_version,
            "os": report.os,
            "arch": report.arch,
            "terminal_profile": report.terminal_profile,
            "input_transport": report.input_transport,
            "tested": false,
            "transcript_drain_ms": report.transcript_drain_ms,
        }),
    }]
}

/// The permission bypass disables Claude's own prompts for the whole session,
/// so it is republished on every turn rather than only at launch: a result read
/// in isolation still says the agent was unsupervised when it produced it.
fn permission_bypass_warnings(dangerous_permission_bypass: bool) -> Vec<ProtocolWarning> {
    if !dangerous_permission_bypass {
        return Vec::new();
    }
    vec![ProtocolWarning {
        code: "dangerous_permission_bypass".to_owned(),
        message: "this session launched Claude with --dangerously-skip-permissions".to_owned(),
        details: Value::Null,
    }]
}

fn map_warning(warning: &EngineWarning) -> ProtocolWarning {
    ProtocolWarning {
        code: match warning {
            EngineWarning::UnknownRow { .. } => "unknown_transcript_row",
            EngineWarning::UnknownContentBlock { .. } => "unknown_content_block",
            EngineWarning::ConflictingUsage { .. } => "conflicting_usage",
            EngineWarning::OrphanToolResult { .. } => "orphan_tool_result",
        }
        .to_owned(),
        message: format!("{warning:?}"),
        details: Value::Null,
    }
}

fn unserializable_transcript_payload_error() -> ErrorBody {
    ErrorBody::new(
        ErrorCode::RecoveryFailed,
        "transcript-derived output cannot be serialized within protocol v1",
    )
}

fn map_transcript_error(error: TranscriptError) -> ErrorBody {
    let code = match error {
        TranscriptError::UnexpectedTypedPrompt { .. }
        | TranscriptError::MultiplePromptAcknowledgements => ErrorCode::PromptNotAcknowledged,
        TranscriptError::NoTurnArmed | TranscriptError::TurnAlreadyArmed => ErrorCode::Internal,
        _ => ErrorCode::SchemaDrift,
    };
    ErrorBody::new(code, error.to_string()).with_details(json!({
        "source": "pseudomux_claude"
    }))
}

fn logical_key_string(key: &LogicalMessageKey) -> String {
    match key {
        LogicalMessageKey::MessageId(value)
        | LogicalMessageKey::RequestId(value)
        | LogicalMessageKey::RowUuid(value) => value.clone(),
    }
}

/// Returns whether `current` is a legal direct actor-state successor of
/// `previous`.
///
/// This is exposed from the service crate so the integration state-machine
/// model can use the production transition contract instead of maintaining a
/// second, drift-prone copy.
#[doc(hidden)]
pub fn is_valid_session_transition(previous: SessionState, current: SessionState) -> bool {
    use SessionState as S;
    matches!(
        (previous, current),
        (S::Creating, S::Booting)
            | (
                S::Booting,
                S::Ready | S::NeedsInput | S::Failed | S::Closing
            )
            | (
                S::Ready,
                S::Submitting | S::NeedsInput | S::Tainted | S::Closing
            )
            | (
                S::Submitting,
                S::AwaitingPromptAck | S::NeedsInput | S::Cancelling | S::Failed | S::Closing
            )
            | (
                S::AwaitingPromptAck,
                S::Running | S::NeedsInput | S::Cancelling | S::Failed | S::Closing
            )
            | (
                S::Running,
                S::NeedsInput | S::TerminalCandidate | S::Cancelling | S::Failed | S::Closing
            )
            | (
                S::TerminalCandidate,
                S::NeedsInput | S::Draining | S::Cancelling | S::Failed | S::Closing
            )
            | (
                S::Draining,
                S::Ready | S::NeedsInput | S::Cancelling | S::Failed | S::Closing
            )
            | (
                S::NeedsInput,
                S::Ready
                    | S::AwaitingPromptAck
                    | S::Running
                    | S::TerminalCandidate
                    | S::Draining
                    | S::Cancelling
                    | S::Tainted
                    | S::Failed
                    | S::Closing
            )
            | (S::Cancelling, S::Ready | S::Tainted | S::Closing)
            | (S::Tainted, S::Closing)
            | (S::Failed, S::Closing)
            | (S::Closing, S::Closed | S::Failed)
    )
}

fn needs_input_error_code(needs_input: &NeedsInput) -> ErrorCode {
    match needs_input.kind {
        NeedsInputKind::Trust => ErrorCode::NeedsTrust,
        NeedsInputKind::Login => ErrorCode::NeedsLogin,
        NeedsInputKind::Permission => ErrorCode::NeedsPermission,
        NeedsInputKind::Update => ErrorCode::NeedsUpdate,
        NeedsInputKind::Quota => ErrorCode::RateLimited,
        NeedsInputKind::UnknownModal => ErrorCode::NeedsInput,
    }
}

/// What an operator can actually DO about each modal.
///
/// Exhaustive over [`NeedsInputKind`], so a kind added later is a compile error
/// here rather than a session that reports a blocked state with no way out.
/// pmux cannot answer any of these itself: a modal is interactive input, and
/// the one capability that could type into a session is a writable attach.
const fn needs_input_recommendation(kind: NeedsInputKind) -> &'static str {
    match kind {
        NeedsInputKind::Trust => {
            "Claude is holding its workspace-trust modal. The pool must mint a pre-trusted empty cwd; remint the cell. pmux will not answer the modal."
        }
        NeedsInputKind::Login => {
            "Claude is not authenticated in this configuration root. Run `claude` against the pool Claude once and log in. pmux will not answer the modal."
        }
        NeedsInputKind::Permission => {
            "Claude is holding a permission prompt. A minified cell denies tools; remint the cell. pmux will not answer the modal."
        }
        NeedsInputKind::Update => {
            "Claude is holding an update prompt. Update the pool's `--pool-claude` executable and remint. pmux will not answer the modal."
        }
        NeedsInputKind::Quota => {
            "Claude reports the account is out of quota. Wait for the window to reset or use a different account; pmux cannot clear this from here."
        }
        NeedsInputKind::UnknownModal => {
            "Claude is holding a modal pmux does not recognize. Remint the cell. pmux will not answer the modal."
        }
    }
}

fn prompt_fingerprint(prompt: &str) -> u64 {
    // Deterministic FNV-1a is only a fast lookup discriminator. Exact normalized
    // prompt equality is also required, so a hash collision cannot replay a turn.
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in prompt.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn daemon_lost() -> ErrorBody {
    ErrorBody::new(ErrorCode::DaemonLost, "session actor is unavailable").retryable(true)
}

#[cfg(test)]
mod replay_scaling_tests {
    use super::*;

    #[test]
    fn full_replay_sweep_selects_each_retained_record_exactly_once() {
        const SMALL: usize = 512;
        const LARGE: usize = SMALL * 8;
        const PAGE_EVENTS: usize = 17;

        fn selected_records(retained: usize) -> usize {
            let oldest_available = 10_001_u64;
            let mut after_sequence = oldest_available - 1;
            let mut consumed = 0;
            let mut selected = 0;

            while consumed < retained {
                let page =
                    replay_page_range(oldest_available, retained, after_sequence, PAGE_EVENTS);
                assert_eq!(page.start, consumed);
                assert!(page.end > page.start, "every retained page must progress");
                selected += page.len();
                consumed = page.end;
                after_sequence = oldest_available + u64::try_from(consumed).unwrap() - 1;
            }

            assert_eq!(consumed, retained);
            selected
        }

        let small_work = selected_records(SMALL);
        let large_work = selected_records(LARGE);
        assert_eq!(small_work, SMALL);
        assert_eq!(large_work, LARGE);
        assert_eq!(large_work, small_work * 8);
    }

    #[test]
    fn replay_page_selection_bounds_ahead_and_overflowing_cursors() {
        assert_eq!(replay_page_range(100, 8, 99, 3), 0..3);
        assert_eq!(replay_page_range(100, 8, 102, 3), 3..6);
        assert_eq!(replay_page_range(100, 8, 107, 3), 8..8);
        assert_eq!(replay_page_range(100, 8, u64::MAX, 3), 8..8);
    }

    #[test]
    fn zero_one_and_two_event_page_accounting_exactly_matches_serde() {
        let first = accounting_event(1, "first");
        let second = accounting_event(2, "second-with-a-different-size");

        let empty = EventBatch {
            events: Vec::new(),
            next_sequence: 1,
            replay_gap: None,
        };
        assert_eq!(
            event_batch_response_bytes(&empty),
            empty_event_batch_response_bytes(1)
        );

        let one = EventBatch {
            events: vec![first.clone()],
            next_sequence: 2,
            replay_gap: None,
        };
        assert_eq!(
            event_batch_response_bytes(&one),
            empty_event_batch_response_bytes(2)
                + EVENT_ARRAY_FIELD_OVERHEAD_BYTES
                + serialized_bytes(&first)
        );

        let two = EventBatch {
            events: vec![first.clone(), second.clone()],
            next_sequence: 3,
            replay_gap: None,
        };
        assert_eq!(
            event_batch_response_bytes(&two),
            empty_event_batch_response_bytes(3)
                + EVENT_ARRAY_FIELD_OVERHEAD_BYTES
                + serialized_bytes(&first)
                + 1
                + serialized_bytes(&second),
            "the second event requires exactly one JSON array comma"
        );
    }

    #[test]
    fn event_page_selection_is_exact_at_just_fit_and_one_byte_below() {
        let events = [
            accounting_event(1, "first"),
            accounting_event(2, "second-with-a-different-size"),
            accounting_event(3, "third"),
        ];
        let records = events
            .iter()
            .cloned()
            .map(|event| ReplayRecord {
                encoded_bytes: serialized_bytes(&event),
                event,
            })
            .collect::<Vec<_>>();
        let one_bytes = event_batch_response_bytes(&EventBatch {
            events: events[..1].to_vec(),
            next_sequence: 2,
            replay_gap: None,
        });
        let two_bytes = event_batch_response_bytes(&EventBatch {
            events: events[..2].to_vec(),
            next_sequence: 3,
            replay_gap: None,
        });

        assert!(
            fitting_event_page(records.iter(), one_bytes - 1)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            fitting_event_page(records.iter(), one_bytes)
                .unwrap()
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!(
            fitting_event_page(records.iter(), two_bytes - 1)
                .unwrap()
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![1],
            "one byte below the exact two-event envelope must stop before sequence two"
        );
        assert_eq!(
            fitting_event_page(records.iter(), two_bytes)
                .unwrap()
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2],
            "an exactly fitting second event must not be deferred"
        );
    }

    #[test]
    fn actor_diagnostics_never_embed_an_unsafe_json_integer() {
        assert_eq!(
            diagnostic_u64(MAX_SAFE_JSON_INTEGER),
            json!(MAX_SAFE_JSON_INTEGER)
        );
        assert_eq!(
            diagnostic_u64(MAX_SAFE_JSON_INTEGER + 1),
            json!((MAX_SAFE_JSON_INTEGER + 1).to_string())
        );
        if usize::BITS > 53 {
            let unsafe_size = usize::try_from(MAX_SAFE_JSON_INTEGER + 1).unwrap();
            assert_eq!(
                diagnostic_usize(unsafe_size),
                json!(unsafe_size.to_string())
            );
        }
    }

    fn accounting_event(sequence: u64, message: &str) -> EventEnvelope {
        EventEnvelope::new(
            uuid::Uuid::from_u128(1),
            SessionGenerationId::from_u128(2),
            None,
            sequence,
            1_000,
            EventPayload::Warning(ProtocolWarning {
                code: "accounting".to_owned(),
                message: message.to_owned(),
                details: Value::Null,
            }),
        )
    }
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;
    use pseudomux_protocol::v1::{InputTransport, TerminalProfile};

    #[test]
    fn untested_compatibility_is_always_a_structured_result_warning() {
        let report = CompatibilityReport {
            claude_version: "2.1.207".to_owned(),
            os: "linux".to_owned(),
            arch: "x86_64".to_owned(),
            terminal_profile: TerminalProfile::Transparent,
            input_transport: InputTransport::Sdk,
            tested: false,
            transcript_drain_ms: 2_000,
        };
        let warnings = compatibility_warnings(&report);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, "untested_compatibility_profile");
        assert_eq!(warnings[0].details["tested"], false);
        assert_eq!(warnings[0].details["transcript_drain_ms"], 2_000);

        let mut tested = report;
        tested.tested = true;
        assert!(compatibility_warnings(&tested).is_empty());
    }
}

#[cfg(test)]
mod permission_bypass_tests {
    use super::*;
    use pseudomux_claude::{FinalTurn, TerminalOutcome, UsageTotals};
    use pseudomux_protocol::v1::{InputTransport, TerminalProfile};

    fn completed_analysis() -> TranscriptAnalysis {
        TranscriptAnalysis {
            status: TurnStatus::Terminal(FinalTurn {
                outcome: TerminalOutcome::Completed,
                message_key: LogicalMessageKey::MessageId("msg_1".to_owned()),
                stop_reason: Some(ClaudeStopReason::EndTurn),
                final_text: "done".to_owned(),
                final_text_blocks: vec!["done".to_owned()],
                model: None,
            }),
            acknowledgement: None,
            active_chain: Vec::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            usage: UsageTotals::default(),
            sidechain_usage: UsageTotals::default(),
            combined_usage: UsageTotals::default(),
            turn_duration_seen: true,
            stop_hook_summary_seen: false,
            api_error_retries_seen: 0,
            sidechain_rows: 0,
            warnings: Vec::new(),
        }
    }

    fn turn_result(dangerous_permission_bypass: bool) -> TurnResult {
        let compatibility = CompatibilityReport {
            claude_version: "2.1.220".to_owned(),
            os: "macos".to_owned(),
            arch: "aarch64".to_owned(),
            terminal_profile: TerminalProfile::Transparent,
            input_transport: InputTransport::Sdk,
            tested: true,
            transcript_drain_ms: 1,
        };
        build_turn_result(
            ResultContext {
                session_id: uuid::Uuid::from_u128(1),
                generation_id: SessionGenerationId::from_u128(2),
                turn_id: uuid::Uuid::from_u128(3),
                submitted_at_ms: 1_000,
                prompt_acknowledged_at_ms: Some(1_010),
                terminal_candidate_at_ms: Some(1_020),
                completed_at_ms: 1_030,
                drain_stable_for_ms: 1,
                compatibility: &compatibility,
                dangerous_permission_bypass,
                evidence: TerminalEvidence {
                    ready_prompt: true,
                    quiet: true,
                    lifecycle_expected: false,
                    lifecycle_hook_observed: false,
                    lifecycle_hook_at_ms: None,
                },
                arrival_order: ArrivalOrderObservations::default(),
            },
            &completed_analysis(),
        )
    }

    #[test]
    fn permission_bypass_is_a_per_turn_result_warning_only_for_bypass_sessions() {
        let bypass = turn_result(true);
        assert_eq!(bypass.warnings.len(), 1);
        assert_eq!(bypass.warnings[0].code, "dangerous_permission_bypass");
        assert_eq!(
            bypass.warnings[0].message,
            "this session launched Claude with --dangerously-skip-permissions"
        );

        let ordinary = turn_result(false);
        assert!(
            ordinary
                .warnings
                .iter()
                .all(|warning| warning.code != "dangerous_permission_bypass"),
            "a session without the bypass must never carry the audit warning"
        );
        assert!(ordinary.warnings.is_empty());
    }
}

#[cfg(test)]
mod stop_hook_summary_tests {
    use super::*;
    use pseudomux_claude::{CompleteLine, JsonlParser, SourceLocation};
    use pseudomux_protocol::v1::{InputTransport, TerminalProfile};

    /// A scripted transcript whose `stop_hook_summary` and `turn_duration` rows
    /// are copied from the session that failed live ordinal 49,
    /// `1aa963e5-ad99-47ee-9c32-cf67854cdea2.jsonl` lines 16 and 17, byte for
    /// byte except that the recording machine's home directory reads `<HOME>`,
    /// which is the one substitution `tools/evidence_common/portable_paths.py`
    /// makes and which nothing in the parser under test reads. The `hookInfos`
    /// command and the `cwd` are carried because a row that dropped them would
    /// stop being the row that was observed.
    const SCRIPTED_TRANSCRIPT: [&str; 4] = [
        r#"{"parentUuid":null,"sessionId":"1aa963e5-ad99-47ee-9c32-cf67854cdea2","type":"user","message":{"role":"user","content":"Reply with the word ready."},"uuid":"synthetic-typed-prompt","promptSource":"typed","promptId":"prompt-1"}"#,
        r#"{"parentUuid":"synthetic-typed-prompt","sessionId":"1aa963e5-ad99-47ee-9c32-cf67854cdea2","type":"assistant","requestId":"req_synthetic","uuid":"255be144-39d6-4cbf-8065-4bf67375dfad","message":{"id":"msg_synthetic","model":"claude-opus-4-6","content":[{"type":"text","text":"ready"}],"stop_reason":"end_turn","usage":{"input_tokens":11,"output_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
        r#"{"parentUuid":"255be144-39d6-4cbf-8065-4bf67375dfad","isSidechain":false,"type":"system","subtype":"stop_hook_summary","hookCount":1,"hookInfos":[{"command":"'<HOME>/pmux-phase12-campaigns/release/pmux-hook' --socket '/private/tmp/pmux-p0-7570efeb-yne40lxn/private-runtime/pmux-cX04Wh/hh-1aa963e5ad9947ee.sock' --session-id '1aa963e5-ad99-47ee-9c32-cf67854cdea2' --event 'Stop'","durationMs":14}],"hookErrors":[],"hookAdditionalContext":[],"preventedContinuation":false,"stopReason":"","hasOutput":false,"level":"suggestion","timestamp":"2026-07-30T01:28:04.414Z","uuid":"f517343d-d00e-419c-b005-9cc8c5a464be","toolUseID":"a6f35cb2-2e63-4b8a-a56a-68d00b630856","session_id":"1aa963e5-ad99-47ee-9c32-cf67854cdea2","userType":"external","entrypoint":"cli","cwd":"<HOME>/dev/pmux-phase12-cwd","sessionId":"1aa963e5-ad99-47ee-9c32-cf67854cdea2","version":"2.1.220","gitBranch":"HEAD"}"#,
        r#"{"parentUuid":"f517343d-d00e-419c-b005-9cc8c5a464be","isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":4327,"messageCount":7,"timestamp":"2026-07-30T01:28:04.415Z","uuid":"7b14ce22-8235-44b2-9385-190db20c1a5d","isMeta":false,"userType":"external","entrypoint":"cli","cwd":"<HOME>/dev/pmux-phase12-cwd","sessionId":"1aa963e5-ad99-47ee-9c32-cf67854cdea2","version":"2.1.220","gitBranch":"HEAD"}"#,
    ];

    fn scripted_analysis() -> TranscriptAnalysis {
        let parser = JsonlParser::new(ParseMode::Strict);
        let mut engine = TranscriptEngine::new(ParseMode::Strict);
        engine.arm_turn("Reply with the word ready.").unwrap();
        for (index, row) in SCRIPTED_TRANSCRIPT.iter().enumerate() {
            let parsed = parser
                .parse(&CompleteLine {
                    location: SourceLocation {
                        line: index as u64 + 1,
                        byte_offset: 0,
                    },
                    bytes: row.as_bytes().to_vec(),
                })
                .unwrap();
            engine.ingest(parsed).unwrap();
        }
        engine.analyze().unwrap()
    }

    fn turn_result(analysis: &TranscriptAnalysis, lifecycle_expected: bool) -> TurnResult {
        let compatibility = CompatibilityReport {
            claude_version: "2.1.220".to_owned(),
            os: "macos".to_owned(),
            arch: "aarch64".to_owned(),
            terminal_profile: TerminalProfile::Transparent,
            input_transport: InputTransport::Sdk,
            tested: true,
            transcript_drain_ms: 1,
        };
        build_turn_result(
            ResultContext {
                session_id: uuid::Uuid::from_u128(1),
                generation_id: SessionGenerationId::from_u128(2),
                turn_id: uuid::Uuid::from_u128(3),
                submitted_at_ms: 1_000,
                prompt_acknowledged_at_ms: Some(1_010),
                terminal_candidate_at_ms: Some(1_020),
                completed_at_ms: 1_030,
                drain_stable_for_ms: 1,
                compatibility: &compatibility,
                dangerous_permission_bypass: false,
                evidence: TerminalEvidence {
                    ready_prompt: true,
                    quiet: true,
                    lifecycle_expected,
                    lifecycle_hook_observed: lifecycle_expected,
                    lifecycle_hook_at_ms: lifecycle_expected.then_some(1_025),
                },
                arrival_order: ArrivalOrderObservations::default(),
            },
            analysis,
        )
    }

    #[test]
    fn an_observed_stop_hook_summary_completes_the_turn_and_is_never_detected_silently() {
        let analysis = scripted_analysis();
        assert!(analysis.stop_hook_summary_seen);
        assert!(analysis.turn_duration_seen);

        // Transcript mode: pmux installed no hook, so the row is evidence that
        // the caller's own Stop hook ran, and default output must say so.
        let transcript_mode = turn_result(&analysis, false);
        assert_eq!(transcript_mode.outcome, TurnOutcome::Completed);
        assert_eq!(transcript_mode.text, "ready");
        assert_eq!(
            transcript_mode.completion.authority,
            CompletionAuthority::Transcript
        );
        let codes: Vec<_> = transcript_mode
            .warnings
            .iter()
            .map(|warning| warning.code.as_str())
            .collect();
        assert_eq!(codes, ["caller_stop_hook_observed"]);
        assert_eq!(
            transcript_mode.warnings[0].message,
            "a caller-installed Claude Stop hook ran during this turn"
        );

        // Hybrid mode: the hook is pmux's own, and the already-rendered
        // provenance field is the single place that fact is reported.
        let hybrid = turn_result(&analysis, true);
        assert_eq!(hybrid.outcome, TurnOutcome::Completed);
        assert!(hybrid.completion.lifecycle_hook_observed);
        assert!(
            hybrid.warnings.is_empty(),
            "hybrid must not double-report its own hook: {:?}",
            hybrid.warnings
        );
    }

    /// A recovered retry ladder must reach default output. pmux completes the
    /// turn normally, so without this warning a turn stretched by Claude's own
    /// retries is indistinguishable from a turn pmux stalled -- the silent-
    /// detection failure mode.
    #[test]
    fn a_recovered_api_error_ladder_completes_the_turn_and_is_never_detected_silently() {
        let rows = [
            r#"{"parentUuid":null,"sessionId":"s","type":"user","message":{"role":"user","content":"Reply with the word ready."},"uuid":"prompt","promptSource":"typed","promptId":"prompt-1"}"#.to_owned(),
            api_error_row("prompt", "retry-1", 1),
            api_error_row("retry-1", "retry-2", 2),
            r#"{"parentUuid":"retry-2","sessionId":"s","type":"assistant","requestId":"req_after_retry","uuid":"answer","message":{"id":"msg_after_retry","model":"claude-opus-4-6","content":[{"type":"text","text":"ready"}],"stop_reason":"end_turn","usage":{"input_tokens":11,"output_tokens":2}}}"#.to_owned(),
        ];
        let parser = JsonlParser::new(ParseMode::Strict);
        let mut engine = TranscriptEngine::new(ParseMode::Strict);
        engine.arm_turn("Reply with the word ready.").unwrap();
        for (index, row) in rows.iter().enumerate() {
            let parsed = parser
                .parse(&CompleteLine {
                    location: SourceLocation {
                        line: index as u64 + 1,
                        byte_offset: 0,
                    },
                    bytes: row.as_bytes().to_vec(),
                })
                .unwrap();
            engine.ingest(parsed).unwrap();
        }
        let analysis = engine.analyze().unwrap();
        assert_eq!(analysis.api_error_retries_seen, 2);

        let result = turn_result(&analysis, false);
        assert_eq!(result.outcome, TurnOutcome::Completed);
        assert_eq!(result.text, "ready");
        let retry_warnings: Vec<_> = result
            .warnings
            .iter()
            .filter(|warning| warning.code == "claude_api_retry_observed")
            .collect();
        assert_eq!(
            retry_warnings.len(),
            1,
            "the recovered retry must be rendered exactly once: {:?}",
            result.warnings
        );
        assert_eq!(retry_warnings[0].details["api_error_rows"], 2);
    }

    /// The real field shape: every key comes from the 115 `api_error` rows
    /// observed on this machine, and `maxRetries` was 10 in all of them.
    fn api_error_row(parent: &str, uuid: &str, retry_attempt: u64) -> String {
        format!(
            r#"{{"parentUuid":"{parent}","isSidechain":false,"type":"system","subtype":"api_error","error":"Connection error (ECONNRESET)","level":"error","retryAttempt":{retry_attempt},"maxRetries":10,"retryInMs":1000,"uuid":"{uuid}","timestamp":"2026-07-30T01:28:03.001Z","sessionId":"s","cwd":"<HOME>/dev/pmux-phase12-cwd","gitBranch":"HEAD","version":"2.1.220","entrypoint":"cli","userType":"external","slug":"reply-with-ready"}}"#
        )
    }
}

/// Arrival-order measurement for Claude's `turn_duration` marker.
///
/// Every case here folds reads through the production `observe_read`, so the
/// predicates the worker applies are the predicates under test. What is being
/// pinned is not the arithmetic but the two claims a consumer will rely on:
/// that the published marker instant is the read that first carried it, and that
/// the second instant appears if and only if something the analysis reads
/// arrived on a strictly later read.
#[cfg(test)]
mod arrival_order_tests {
    use super::*;
    use pseudomux_claude::{CompleteLine, JsonlParser, SourceLocation};
    use pseudomux_protocol::v1::{InputTransport, TerminalProfile};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    const PROMPT: &str = "Reply with the word ready.";
    const TYPED_PROMPT: &str = r#"{"parentUuid":null,"sessionId":"s","type":"user","message":{"role":"user","content":"Reply with the word ready."},"uuid":"prompt","promptSource":"typed","promptId":"prompt-1"}"#;
    const ASSISTANT_ANSWER: &str = r#"{"parentUuid":"prompt","sessionId":"s","type":"assistant","requestId":"req","uuid":"answer","message":{"id":"msg","model":"claude-opus-4-6","content":[{"type":"text","text":"ready"}],"stop_reason":"end_turn","usage":{"input_tokens":11,"output_tokens":2}}}"#;
    const STOP_HOOK_SUMMARY: &str = r#"{"parentUuid":"answer","isSidechain":false,"type":"system","subtype":"stop_hook_summary","hookCount":1,"hookInfos":[],"hookErrors":[],"hookAdditionalContext":[],"preventedContinuation":false,"stopReason":"","hasOutput":false,"level":"suggestion","timestamp":"2026-07-30T01:28:04.414Z","uuid":"stop-hook","sessionId":"s","version":"2.1.220"}"#;
    const TURN_DURATION: &str = r#"{"parentUuid":"stop-hook","isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":4327,"messageCount":7,"timestamp":"2026-07-30T01:28:04.415Z","uuid":"turn-duration","isMeta":false,"sessionId":"s","version":"2.1.220"}"#;
    /// Legal after the marker and read by the analysis: it moves sidechain and
    /// combined usage. A *semantic main-chain* row there would be schema drift
    /// and fail the turn, so this is the shape a real late arrival can take.
    const SIDECHAIN_ASSISTANT: &str = r#"{"parentUuid":"prompt","isSidechain":true,"sessionId":"s","type":"assistant","requestId":"req-side","uuid":"sidechain-answer","message":{"id":"msg-side","model":"claude-opus-4-6","content":[{"type":"text","text":"sub-agent"}],"stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":7}}}"#;
    /// Off-graph: the engine drops it before analysis, so its arrival is not a
    /// late arrival.
    const METADATA_RECORD: &str = r#"{"type":"file-history-snapshot","sessionId":"s","messageId":"msg","snapshot":{"trackedFileBackups":{}},"isSnapshotUpdate":false}"#;

    /// Hands out one scripted instant per read and counts the reads.
    ///
    /// The count matters as much as the values: the measurement must not read the
    /// clock when it has nothing to stamp, both because that is what keeps it
    /// free on turns Claude wrote no marker for, and because a clock read is
    /// itself an observable event for any deterministic caller.
    struct ScriptedClock {
        samples: Mutex<VecDeque<TimestampMs>>,
        reads: AtomicU64,
    }

    impl ScriptedClock {
        fn new(samples: impl IntoIterator<Item = TimestampMs>) -> Self {
            Self {
                samples: Mutex::new(samples.into_iter().collect()),
                reads: AtomicU64::new(0),
            }
        }

        fn reads(&self) -> u64 {
            self.reads.load(Ordering::SeqCst)
        }
    }

    impl Clock for ScriptedClock {
        fn now_ms(&self) -> u64 {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.samples
                .lock()
                .unwrap()
                .pop_front()
                .expect("the measurement read the clock more often than scripted")
        }
    }

    fn parse(line: &str) -> ParsedRow {
        JsonlParser::new(ParseMode::Strict)
            .parse(&CompleteLine {
                location: SourceLocation {
                    line: 1,
                    byte_offset: 0,
                },
                bytes: line.as_bytes().to_vec(),
            })
            .expect("fixture rows are valid strict-mode JSONL")
    }

    /// Folds a sequence of transcript reads exactly as the worker does.
    fn observe(clock: &ScriptedClock, reads: &[&[&str]]) -> ArrivalOrderObservations {
        let mut observations = ArrivalOrderObservations::default();
        for read in reads {
            let rows: Vec<ParsedRow> = read.iter().map(|line| parse(line)).collect();
            observations.observe_read(clock, &rows);
        }
        observations
    }

    fn completed_analysis() -> TranscriptAnalysis {
        let mut engine = TranscriptEngine::new(ParseMode::Strict);
        engine.arm_turn(PROMPT).unwrap();
        for line in [
            TYPED_PROMPT,
            ASSISTANT_ANSWER,
            STOP_HOOK_SUMMARY,
            TURN_DURATION,
        ] {
            engine.ingest(parse(line)).unwrap();
        }
        engine.analyze().unwrap()
    }

    fn published(arrival_order: ArrivalOrderObservations) -> TurnTimings {
        let compatibility = CompatibilityReport {
            claude_version: "2.1.220".to_owned(),
            os: "macos".to_owned(),
            arch: "aarch64".to_owned(),
            terminal_profile: TerminalProfile::Transparent,
            input_transport: InputTransport::Sdk,
            tested: true,
            transcript_drain_ms: 1,
        };
        build_turn_result(
            ResultContext {
                session_id: uuid::Uuid::from_u128(1),
                generation_id: SessionGenerationId::from_u128(2),
                turn_id: uuid::Uuid::from_u128(3),
                submitted_at_ms: 1_000,
                prompt_acknowledged_at_ms: Some(1_010),
                terminal_candidate_at_ms: Some(1_020),
                completed_at_ms: 3_000,
                drain_stable_for_ms: 1,
                compatibility: &compatibility,
                dangerous_permission_bypass: false,
                evidence: TerminalEvidence {
                    ready_prompt: true,
                    quiet: true,
                    lifecycle_expected: false,
                    lifecycle_hook_observed: false,
                    lifecycle_hook_at_ms: None,
                },
                arrival_order,
            },
            &completed_analysis(),
        )
        .timings
    }

    #[test]
    fn a_marker_with_nothing_after_it_publishes_one_instant_and_omits_the_other() {
        // The shape every transcript on this machine showed: answer, then the
        // markers, then a confirming read that carried nothing.
        let clock = ScriptedClock::new([2_000]);
        let observations = observe(
            &clock,
            &[
                &[TYPED_PROMPT, ASSISTANT_ANSWER],
                &[STOP_HOOK_SUMMARY, TURN_DURATION],
                &[],
            ],
        );

        assert_eq!(observations.turn_duration_observed_at_ms, Some(2_000));
        assert_eq!(observations.post_turn_duration_row_observed_at_ms, None);
        assert_eq!(
            clock.reads(),
            1,
            "one stamp, one clock read: the reads before and after the marker \
             had nothing to record"
        );

        let timings = published(observations);
        assert_eq!(timings.turn_duration_observed_at_ms, Some(2_000));
        assert_eq!(timings.post_turn_duration_row_observed_at_ms, None);
    }

    #[test]
    fn a_turn_whose_transcript_carries_no_marker_never_reads_the_clock() {
        // Older Claude builds write no `turn_duration` row, and 4% of turns on
        // current ones do not either. The instrument must cost nothing there --
        // and must publish nothing, rather than an instant a consumer could
        // mistake for "the marker was seen at the start of the turn".
        let clock = ScriptedClock::new([]);
        let observations = observe(
            &clock,
            &[&[TYPED_PROMPT, ASSISTANT_ANSWER], &[STOP_HOOK_SUMMARY], &[]],
        );

        assert_eq!(observations.turn_duration_observed_at_ms, None);
        assert_eq!(observations.post_turn_duration_row_observed_at_ms, None);
        assert_eq!(clock.reads(), 0);
    }

    #[test]
    fn an_analysis_changing_row_on_a_strictly_later_read_is_published_as_the_late_arrival() {
        let clock = ScriptedClock::new([2_000, 2_050]);
        let observations = observe(
            &clock,
            &[
                &[TYPED_PROMPT, ASSISTANT_ANSWER],
                &[STOP_HOOK_SUMMARY, TURN_DURATION],
                // The confirming re-poll caught a row the marker did not cover.
                &[SIDECHAIN_ASSISTANT],
                // A second late row must not overwrite the first: the reportable
                // fact is when the drain window started earning its cost.
                &[SIDECHAIN_ASSISTANT],
            ],
        );

        assert_eq!(observations.turn_duration_observed_at_ms, Some(2_000));
        assert_eq!(
            observations.post_turn_duration_row_observed_at_ms,
            Some(2_050)
        );
        assert_eq!(clock.reads(), 2);

        let timings = published(observations);
        assert_eq!(timings.turn_duration_observed_at_ms, Some(2_000));
        assert_eq!(
            timings.post_turn_duration_row_observed_at_ms,
            Some(2_050),
            "a late arrival must reach the wire; it is the observation that \
             would condemn a marker-based fast path"
        );
    }

    #[test]
    fn rows_delivered_by_the_marker_read_itself_are_not_late_arrivals() {
        // pmux could not have completed before ingesting the whole read, so a row
        // beside the marker was never at risk. Calling it late would report the
        // drain as necessary on a turn where it was not.
        let clock = ScriptedClock::new([2_000]);
        let observations = observe(
            &clock,
            &[
                &[TYPED_PROMPT, ASSISTANT_ANSWER],
                &[SIDECHAIN_ASSISTANT, STOP_HOOK_SUMMARY, TURN_DURATION],
                // And an off-graph metadata record on a later read is not a late
                // arrival either: the analysis never reads it.
                &[METADATA_RECORD],
            ],
        );

        assert_eq!(observations.turn_duration_observed_at_ms, Some(2_000));
        assert_eq!(observations.post_turn_duration_row_observed_at_ms, None);
        assert_eq!(clock.reads(), 1);
    }

    #[test]
    fn an_unpublishable_clock_reading_leaves_the_pair_absent_rather_than_half_stated() {
        // A reading outside protocol v1's safe-integer domain is a missing
        // measurement. It must not fail the turn, must not clamp, and must not
        // let the second field be published without the first -- "something
        // arrived late" is meaningless without saying late relative to what.
        let clock = ScriptedClock::new([MAX_SAFE_JSON_INTEGER + 1]);
        let observations = observe(
            &clock,
            &[
                &[STOP_HOOK_SUMMARY, TURN_DURATION],
                &[SIDECHAIN_ASSISTANT],
                // The window is already open, so a second marker must not re-arm
                // it and stamp a later instant than the one pmux really saw.
                &[TURN_DURATION],
            ],
        );

        assert!(observations.marker_observed);
        assert_eq!(observations.turn_duration_observed_at_ms, None);
        assert_eq!(observations.post_turn_duration_row_observed_at_ms, None);
        assert_eq!(clock.reads(), 1);

        let timings = published(observations);
        assert_eq!(timings.turn_duration_observed_at_ms, None);
        assert_eq!(timings.post_turn_duration_row_observed_at_ms, None);
    }
}
