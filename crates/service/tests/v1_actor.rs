use std::collections::VecDeque;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pseudomux_claude::{
    CompleteLine, JsonlParser, ParseMode, ParsedRow, RowKind, RowScope, SourceLocation,
};
use pseudomux_protocol::v1::{
    CancelOutcome, CancelTurnRequest, ClosePolicy, CloseSessionRequest, CompatibilityReport,
    CompletionAuthority, ErrorCode, EventPayload, InputTransport, InspectSessionRequest,
    MAX_SAFE_JSON_INTEGER, MAX_SUBSCRIBE_EVENTS, MAX_SUBSCRIBE_WAIT_MS, MessageBlock, NeedsInput,
    NeedsInputKind, ResponseEnvelope, ResponseResult, RunTurnRequest, SessionGenerationId,
    SessionId, SessionState, StopReasonKind, SubscribeEventsRequest, TerminalProfile, ToolStatus,
    TurnId, TurnOutcome, TurnRequest,
};
use pseudomux_rmux::TerminalSnapshot;
use pseudomux_service::driver_io::{RecognisedScreen, ScreenShape};
use pseudomux_service::v1::{
    Clock, DriverFailure, DriverResult, InterruptRecovery, POST_MARKER_CATCH_WINDOW_FLOOR_MS,
    SessionActorConfig, SessionCell, SessionRegistration, SessionRegistry, StoredTurnTerminal,
    TURN_DURATION_DRAIN_FLOOR_MS, TerminalControl, TerminalEvidence, TerminalScreenObservation,
    TranscriptArm, TranscriptBatch, TranscriptDrainEvidence, TranscriptPosition, TranscriptSource,
    UNRECOGNISED_SCREEN_VETO, WritableAttachCompletion, graduated_drain_ms,
    post_marker_catch_window_ms,
};

/// One observation of a screen no rule pmux owns matched.
///
/// The shape is DERIVED from a frame rather than written field by field, for the
/// same reason production derives it: a `ScreenShape` literal in a test is a
/// second describer of a frame, and it is free to describe one that no capture
/// could ever produce.
fn unrecognised_screen() -> TerminalScreenObservation {
    TerminalScreenObservation::Unrecognised(ScreenShape::of(&TerminalSnapshot {
        revision: 7,
        rows: 24,
        cols: 80,
        cursor: None,
        visible_text: "a rendering pmux has no rule for".to_owned(),
    }))
}

#[derive(Default)]
struct TestClock(AtomicU64);

impl TestClock {
    fn starting_at(value: u64) -> Self {
        Self(AtomicU64::new(value))
    }

    fn set(&self, value: u64) {
        self.0.store(value, Ordering::SeqCst);
    }
}

impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.0.fetch_add(1, Ordering::SeqCst)
    }
}

struct ArmedClock {
    fallback: AtomicU64,
    samples: Mutex<VecDeque<u64>>,
}

impl ArmedClock {
    fn new(fallback: u64) -> Self {
        Self {
            fallback: AtomicU64::new(fallback),
            samples: Mutex::new(VecDeque::new()),
        }
    }

    fn arm(&self, samples: impl IntoIterator<Item = u64>) {
        let mut queued = self.samples.lock().unwrap();
        assert!(queued.is_empty(), "the prior clock script was not consumed");
        queued.extend(samples);
    }
}

impl Clock for ArmedClock {
    fn now_ms(&self) -> u64 {
        self.samples
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| self.fallback.load(Ordering::SeqCst))
    }
}

struct FakeTerminal {
    evidence: AtomicU8,
    /// `TerminalEvidence::lifecycle_hook_at_ms`, kept beside the packed
    /// booleans; `0` reproduces the absent instant.
    evidence_hook_at_ms: AtomicU64,
    interrupt: Mutex<InterruptRecovery>,
    submissions: Mutex<Vec<(SessionId, TurnId, String)>>,
    submit_failure: Mutex<Option<DriverFailure>>,
    close_failure: Mutex<Option<DriverFailure>>,
    closes: AtomicU64,
    close_reaped: AtomicU8,
    screen: Mutex<TerminalScreenObservation>,
}

impl FakeTerminal {
    fn new(evidence: TerminalEvidence, interrupt: InterruptRecovery) -> Self {
        let terminal = Self {
            evidence: AtomicU8::new(0),
            evidence_hook_at_ms: AtomicU64::new(0),
            interrupt: Mutex::new(interrupt),
            submissions: Mutex::new(Vec::new()),
            submit_failure: Mutex::new(None),
            close_failure: Mutex::new(None),
            closes: AtomicU64::new(0),
            close_reaped: AtomicU8::new(1),
            screen: Mutex::new(TerminalScreenObservation::Ready),
        };
        terminal.set_evidence(evidence);
        terminal
    }

    fn set_evidence(&self, evidence: TerminalEvidence) {
        let bits = u8::from(evidence.ready_prompt)
            | (u8::from(evidence.quiet) << 1)
            | (u8::from(evidence.lifecycle_expected) << 2)
            | (u8::from(evidence.lifecycle_hook_observed) << 3);
        self.evidence.store(bits, Ordering::SeqCst);
        self.evidence_hook_at_ms
            .store(evidence.lifecycle_hook_at_ms.unwrap_or(0), Ordering::SeqCst);
    }

    fn evidence(&self) -> TerminalEvidence {
        let bits = self.evidence.load(Ordering::SeqCst);
        TerminalEvidence {
            ready_prompt: bits & 1 != 0,
            quiet: bits & 2 != 0,
            lifecycle_expected: bits & 4 != 0,
            lifecycle_hook_observed: bits & 8 != 0,
            lifecycle_hook_at_ms: Some(self.evidence_hook_at_ms.load(Ordering::SeqCst))
                .filter(|stamped| *stamped != 0),
        }
    }

    fn set_screen(&self, screen: TerminalScreenObservation) {
        *self.screen.lock().unwrap() = screen;
    }

    fn set_close_reaped(&self, process_reaped: bool) {
        self.close_reaped
            .store(u8::from(process_reaped), Ordering::SeqCst);
    }

    fn set_submit_failure(&self, failure: DriverFailure) {
        *self.submit_failure.lock().unwrap() = Some(failure);
    }

    fn set_close_failure(&self, failure: DriverFailure) {
        *self.close_failure.lock().unwrap() = Some(failure);
    }

    fn clear_close_failure(&self) {
        *self.close_failure.lock().unwrap() = None;
    }
}

#[async_trait]
impl TerminalControl for FakeTerminal {
    async fn submit_prompt(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        prompt: &str,
        _deadline_unix_ms: u64,
    ) -> DriverResult<()> {
        self.submissions
            .lock()
            .unwrap()
            .push((session_id, turn_id, prompt.to_owned()));
        if let Some(failure) = self.submit_failure.lock().unwrap().clone() {
            Err(failure)
        } else {
            Ok(())
        }
    }

    async fn completion_evidence(
        &self,
        _session_id: SessionId,
        _turn_id: TurnId,
    ) -> DriverResult<TerminalEvidence> {
        Ok(self.evidence())
    }

    async fn observe_screen(
        &self,
        _session_id: SessionId,
    ) -> DriverResult<TerminalScreenObservation> {
        Ok(self.screen.lock().unwrap().clone())
    }

    async fn interrupt(
        &self,
        _session_id: SessionId,
        _turn_id: TurnId,
    ) -> DriverResult<InterruptRecovery> {
        Ok(*self.interrupt.lock().unwrap())
    }

    async fn close(&self, _session_id: SessionId, _policy: ClosePolicy) -> DriverResult<bool> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        if let Some(failure) = self.close_failure.lock().unwrap().clone() {
            Err(failure)
        } else {
            Ok(self.close_reaped.load(Ordering::SeqCst) != 0)
        }
    }
}

struct ScriptedTranscript {
    arm: DriverResult<TranscriptArm>,
    polls: Mutex<VecDeque<DriverResult<TranscriptBatch>>>,
    fallback: TranscriptBatch,
    poll_delay: Duration,
}

struct CancellationUnstableTranscript {
    arm_calls: AtomicU64,
}

struct CancellationLateThenFreshTranscript {
    arm_calls: AtomicU64,
    poll_calls: AtomicU64,
}

#[derive(Clone, Copy, Debug)]
enum TimingSample {
    PromptAcknowledged,
    TerminalCandidate,
    Completed,
}

type OpaqueRouteBuilder = fn(&str) -> Vec<ParsedRow>;

struct TimingMutationTranscript {
    clock: Arc<ArmedClock>,
    sample: TimingSample,
    target: u64,
    poll_calls: AtomicU64,
}

impl TimingMutationTranscript {
    fn arm_clock(&self) {
        use pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER as MAX;

        let samples = match self.sample {
            TimingSample::PromptAcknowledged => vec![MAX - 2, self.target],
            TimingSample::TerminalCandidate => vec![MAX - 3, MAX - 2, self.target],
            TimingSample::Completed => vec![MAX - 3, MAX - 2, self.target],
        };
        self.clock.arm(samples);
    }
}

#[async_trait]
impl TranscriptSource for TimingMutationTranscript {
    async fn arm_at_eof(&self, _session_id: SessionId) -> DriverResult<TranscriptArm> {
        Ok(TranscriptArm::default())
    }

    async fn poll(
        &self,
        _session_id: SessionId,
        position: &TranscriptPosition,
    ) -> DriverResult<TranscriptBatch> {
        let call = self.poll_calls.fetch_add(1, Ordering::SeqCst);
        if (call == 0 && !matches!(self.sample, TimingSample::Completed))
            || (call == 1 && matches!(self.sample, TimingSample::Completed))
        {
            self.arm_clock();
        }
        if call == 0 {
            Ok(TranscriptBatch {
                position: TranscriptPosition {
                    generation: 0,
                    offset: 2,
                },
                rows: simple_turn_rows("timing", "safe answer"),
                drain: TranscriptDrainEvidence {
                    at_eof: true,
                    has_partial_line: false,
                    stable_for_ms: 100,
                },
            })
        } else {
            Ok(TranscriptBatch {
                position: position.clone(),
                rows: Vec::new(),
                drain: TranscriptDrainEvidence {
                    at_eof: true,
                    has_partial_line: false,
                    stable_for_ms: 100,
                },
            })
        }
    }
}

#[async_trait]
impl TranscriptSource for CancellationUnstableTranscript {
    async fn arm_at_eof(&self, _session_id: SessionId) -> DriverResult<TranscriptArm> {
        if self.arm_calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(TranscriptArm::default())
        } else {
            Err(DriverFailure::new(
                ErrorCode::SchemaDrift,
                "post-interrupt transcript could not be stabilized",
            ))
        }
    }

    async fn poll(
        &self,
        _session_id: SessionId,
        position: &TranscriptPosition,
    ) -> DriverResult<TranscriptBatch> {
        Ok(TranscriptBatch {
            position: position.clone(),
            rows: Vec::new(),
            drain: TranscriptDrainEvidence::default(),
        })
    }
}

#[async_trait]
impl TranscriptSource for CancellationLateThenFreshTranscript {
    async fn arm_at_eof(&self, _session_id: SessionId) -> DriverResult<TranscriptArm> {
        let call = self.arm_calls.fetch_add(1, Ordering::SeqCst);
        Ok(TranscriptArm {
            position: TranscriptPosition {
                generation: 0,
                offset: if call >= 2 { 2 } else { 0 },
            },
            historical_rows: Vec::new(),
        })
    }

    async fn poll(
        &self,
        _session_id: SessionId,
        position: &TranscriptPosition,
    ) -> DriverResult<TranscriptBatch> {
        let stable = TranscriptDrainEvidence {
            at_eof: true,
            has_partial_line: false,
            stable_for_ms: 100,
        };
        Ok(match self.poll_calls.fetch_add(1, Ordering::SeqCst) {
            // The original active-turn poll is already in flight when cancel
            // starts. Delay it so the post-interrupt arm owns the next call.
            0 => {
                tokio::time::sleep(Duration::from_millis(25)).await;
                TranscriptBatch {
                    position: position.clone(),
                    rows: Vec::new(),
                    drain: TranscriptDrainEvidence::default(),
                }
            }
            // Claude appends a late terminal chain after Ctrl-C. Cancellation
            // must advance its private cursor and restart the drain proof.
            1 => TranscriptBatch {
                position: TranscriptPosition {
                    generation: 0,
                    offset: 2,
                },
                rows: simple_turn_rows("cancelled prompt", "late cancelled answer"),
                drain: stable,
            },
            2 => TranscriptBatch {
                position: TranscriptPosition {
                    generation: 0,
                    offset: 2,
                },
                rows: Vec::new(),
                drain: stable,
            },
            // The immediately following turn arms at offset 2 and sees only
            // its fresh rows. The late cancelled chain must not correlate.
            3 => TranscriptBatch {
                position: TranscriptPosition {
                    generation: 0,
                    offset: 4,
                },
                rows: simple_turn_rows("after cancel", "fresh answer"),
                drain: stable,
            },
            _ => TranscriptBatch {
                position: TranscriptPosition {
                    generation: 0,
                    offset: 4,
                },
                rows: Vec::new(),
                drain: stable,
            },
        })
    }
}

impl ScriptedTranscript {
    fn pending() -> Self {
        Self {
            arm: Ok(TranscriptArm::default()),
            polls: Mutex::new(VecDeque::new()),
            fallback: TranscriptBatch {
                position: TranscriptPosition::default(),
                rows: Vec::new(),
                drain: TranscriptDrainEvidence {
                    at_eof: true,
                    has_partial_line: false,
                    stable_for_ms: 100,
                },
            },
            poll_delay: Duration::ZERO,
        }
    }

    fn with_rows(historical_rows: Vec<ParsedRow>, rows: Vec<ParsedRow>) -> Self {
        let position = TranscriptPosition {
            generation: 0,
            offset: rows.len() as u64,
        };
        let batch = TranscriptBatch {
            position: position.clone(),
            rows,
            drain: TranscriptDrainEvidence {
                at_eof: true,
                has_partial_line: false,
                stable_for_ms: 100,
            },
        };
        Self {
            arm: Ok(TranscriptArm {
                position: TranscriptPosition::default(),
                historical_rows,
            }),
            polls: Mutex::new(VecDeque::from([Ok(batch)])),
            fallback: TranscriptBatch {
                position,
                rows: Vec::new(),
                drain: TranscriptDrainEvidence {
                    at_eof: true,
                    has_partial_line: false,
                    stable_for_ms: 100,
                },
            },
            poll_delay: Duration::ZERO,
        }
    }

    fn failing(code: ErrorCode, message: &str) -> Self {
        Self {
            arm: Err(DriverFailure::new(code, message)),
            polls: Mutex::new(VecDeque::new()),
            fallback: TranscriptBatch::default(),
            poll_delay: Duration::ZERO,
        }
    }

    fn stable_at_eof(poll_delay: Duration) -> Self {
        Self {
            arm: Ok(TranscriptArm::default()),
            polls: Mutex::new(VecDeque::new()),
            fallback: TranscriptBatch {
                position: TranscriptPosition::default(),
                rows: Vec::new(),
                drain: TranscriptDrainEvidence {
                    at_eof: true,
                    has_partial_line: false,
                    stable_for_ms: 100,
                },
            },
            poll_delay,
        }
    }
}

#[async_trait]
impl TranscriptSource for ScriptedTranscript {
    async fn arm_at_eof(&self, _session_id: SessionId) -> DriverResult<TranscriptArm> {
        self.arm.clone()
    }

    async fn poll(
        &self,
        _session_id: SessionId,
        _position: &TranscriptPosition,
    ) -> DriverResult<TranscriptBatch> {
        tokio::time::sleep(self.poll_delay).await;
        self.polls
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok(self.fallback.clone()))
    }
}

#[tokio::test]
async fn one_active_turn_and_prompt_hash_idempotency_are_enforced() {
    let registry = registry(64);
    let session_id = id(1);
    let terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence::default(),
        InterruptRecovery::RecoveredToReady,
    ));
    register(
        &registry,
        session_id,
        terminal,
        Arc::new(ScriptedTranscript::pending()),
    )
    .await;

    let first = turn(session_id, id(11), "same\r\nprompt");
    let accepted = registry.run_turn(first.clone()).await.unwrap();
    assert!(!accepted.replayed);

    let replay = registry
        .run_turn(turn(session_id, id(11), "same\nprompt"))
        .await
        .unwrap();
    assert!(replay.replayed, "line-ending normalization is idempotent");

    let conflict = registry
        .run_turn(turn(session_id, id(11), "different"))
        .await
        .unwrap_err();
    assert_eq!(conflict.code, ErrorCode::IdConflict);

    let busy = registry
        .run_turn(turn(session_id, id(12), "other turn"))
        .await
        .unwrap_err();
    assert_eq!(busy.code, ErrorCode::SessionBusy);

    let cancelled = registry
        .cancel_turn(CancelTurnRequest {
            session_id,
            generation_id: generation(session_id),
            turn_id: id(11),
        })
        .await
        .unwrap();
    assert_eq!(cancelled.outcome, CancelOutcome::Cancelled);
    assert_eq!(cancelled.session_state, SessionState::Ready);
    let stored = registry
        .stored_turn(session_id, generation(session_id), id(11))
        .await
        .unwrap()
        .expect("cancelled turn is replayable");
    let StoredTurnTerminal::Result(result) = stored else {
        panic!("successful cancellation must store a result");
    };
    assert!(result.completion.transcript_drained);
}

#[tokio::test]
async fn stale_generation_operations_cannot_target_a_resumed_process() {
    let registry = registry(64);
    let session_id = id(8_001);
    let generation_a = SessionGenerationId::from_u128(8_002);
    let generation_b = SessionGenerationId::from_u128(8_003);
    let terminal_a = Arc::new(FakeTerminal::new(
        TerminalEvidence::default(),
        InterruptRecovery::RecoveredToReady,
    ));
    register_with_generation(
        &registry,
        session_id,
        generation_a,
        terminal_a,
        Arc::new(ScriptedTranscript::pending()),
        None,
    )
    .await;
    let closed = registry
        .close(CloseSessionRequest {
            session_id,
            generation_id: generation_a,
            policy: ClosePolicy::Force,
        })
        .await
        .unwrap();
    assert!(closed.process_reaped);
    registry.unregister(session_id, generation_a).await.unwrap();

    register_with_generation(
        &registry,
        session_id,
        generation_b,
        Arc::new(FakeTerminal::new(
            TerminalEvidence::default(),
            InterruptRecovery::RecoveredToReady,
        )),
        Arc::new(ScriptedTranscript::pending()),
        None,
    )
    .await;

    let stale_turn = RunTurnRequest {
        generation_id: generation_a,
        ..turn(session_id, id(8_004), "must not reach generation B")
    };
    let errors = [
        registry.run_turn(stale_turn).await.unwrap_err(),
        registry
            .cancel_turn(CancelTurnRequest {
                session_id,
                generation_id: generation_a,
                turn_id: id(8_004),
            })
            .await
            .unwrap_err(),
        registry
            .inspect(InspectSessionRequest {
                session_id,
                generation_id: generation_a,
            })
            .await
            .unwrap_err(),
        registry
            .reserve_writable_attach(session_id, generation_a, id(8_005))
            .await
            .unwrap_err(),
        registry
            .close(CloseSessionRequest {
                session_id,
                generation_id: generation_a,
                policy: ClosePolicy::Force,
            })
            .await
            .unwrap_err(),
        registry
            .events(SubscribeEventsRequest {
                session_id,
                generation_id: generation_a,
                after_sequence: 0,
                wait_ms: 0,
                max_events: 8,
            })
            .await
            .unwrap_err(),
    ];
    for error in errors {
        assert_eq!(error.code, ErrorCode::StaleSessionGeneration);
        assert!(!error.retryable);
        assert!(error.details.get("current_generation_id").is_none());
    }

    let snapshot = registry
        .inspect(InspectSessionRequest {
            session_id,
            generation_id: generation_b,
        })
        .await
        .unwrap();
    assert_eq!(snapshot.generation_id, generation_b);
    assert_eq!(snapshot.state, SessionState::Ready);
}

#[tokio::test]
async fn expired_new_turns_fail_before_injection_but_existing_ids_still_replay() {
    let registry = registry(64);
    let session_id = id(13);
    let expired_turn_id = id(131);
    let completed_turn_id = id(132);
    let terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence {
            ready_prompt: true,
            quiet: true,
            ..TerminalEvidence::default()
        },
        InterruptRecovery::RecoveredToReady,
    ));
    register(
        &registry,
        session_id,
        terminal.clone(),
        Arc::new(ScriptedTranscript::with_rows(
            Vec::new(),
            simple_turn_rows("on time", "completed"),
        )),
    )
    .await;

    let mut expired = turn(session_id, expired_turn_id, "too late");
    expired.turn.deadline_unix_ms = Some(0);
    let error = registry.run_turn(expired).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::TurnTimeout);
    assert!(terminal.submissions.lock().unwrap().is_empty());
    assert!(
        registry
            .stored_turn(session_id, generation(session_id), expired_turn_id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        snapshot(&registry, session_id).await.state,
        SessionState::Ready
    );

    registry
        .run_turn(turn(session_id, completed_turn_id, "on time"))
        .await
        .unwrap();
    let _ = wait_for_terminal(&registry, session_id, completed_turn_id).await;
    assert_eq!(terminal.submissions.lock().unwrap().len(), 1);

    let mut replay = turn(session_id, completed_turn_id, "on time");
    replay.turn.deadline_unix_ms = Some(0);
    let accepted = registry.run_turn(replay).await.unwrap();
    assert!(accepted.replayed);
    assert_eq!(terminal.submissions.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn simultaneous_distinct_turns_accept_exactly_one() {
    let registry = Arc::new(registry(64));
    let session_id = id(10);
    register(
        &registry,
        session_id,
        Arc::new(FakeTerminal::new(
            TerminalEvidence::default(),
            InterruptRecovery::RecoveredToReady,
        )),
        Arc::new(ScriptedTranscript::pending()),
    )
    .await;

    let first_registry = Arc::clone(&registry);
    let second_registry = Arc::clone(&registry);
    let first = tokio::spawn(async move {
        first_registry
            .run_turn(turn(session_id, id(101), "first"))
            .await
    });
    let second = tokio::spawn(async move {
        second_registry
            .run_turn(turn(session_id, id(102), "second"))
            .await
    });
    let (first, second) = tokio::join!(first, second);
    let outcomes = [first.unwrap(), second.unwrap()];
    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter_map(|outcome| outcome.as_ref().err())
            .filter(|error| error.code == ErrorCode::SessionBusy)
            .count(),
        1
    );
    let active_turn = snapshot(&registry, session_id)
        .await
        .active_turn_id
        .unwrap();
    registry
        .cancel_turn(CancelTurnRequest {
            session_id,
            generation_id: generation(session_id),
            turn_id: active_turn,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn ready_and_quiet_terminal_without_transcript_candidate_never_completes() {
    let registry = registry(64);
    let session_id = id(11);
    let turn_id = id(111);
    register(
        &registry,
        session_id,
        Arc::new(FakeTerminal::new(
            TerminalEvidence {
                ready_prompt: true,
                quiet: true,
                ..TerminalEvidence::default()
            },
            InterruptRecovery::RecoveredToReady,
        )),
        Arc::new(ScriptedTranscript::pending()),
    )
    .await;
    registry
        .run_turn(turn(session_id, turn_id, "no transcript"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(
        registry
            .stored_turn(session_id, generation(session_id), turn_id)
            .await
            .unwrap()
            .is_none()
    );
    assert_ne!(
        snapshot(&registry, session_id).await.state,
        SessionState::Ready
    );
    registry
        .cancel_turn(CancelTurnRequest {
            session_id,
            generation_id: generation(session_id),
            turn_id,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn transcript_terminal_needs_ready_quiet_and_drain_before_completion() {
    let registry = registry(64);
    let session_id = id(2);
    let turn_id = id(21);
    let terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence {
            ready_prompt: false,
            quiet: true,
            ..TerminalEvidence::default()
        },
        InterruptRecovery::RecoveredToReady,
    ));
    let rows = simple_turn_rows("gate", "answer");
    register(
        &registry,
        session_id,
        terminal.clone(),
        Arc::new(ScriptedTranscript::with_rows(Vec::new(), rows)),
    )
    .await;
    registry
        .run_turn(turn(session_id, turn_id, "gate"))
        .await
        .unwrap();

    wait_for_state(&registry, session_id, SessionState::Draining).await;
    assert!(
        registry
            .stored_turn(session_id, generation(session_id), turn_id)
            .await
            .unwrap()
            .is_none()
    );

    terminal.set_evidence(TerminalEvidence {
        ready_prompt: true,
        quiet: true,
        lifecycle_expected: true,
        lifecycle_hook_observed: false,
        lifecycle_hook_at_ms: None,
    });
    let StoredTurnTerminal::Result(result) =
        wait_for_terminal(&registry, session_id, turn_id).await
    else {
        panic!("expected a result")
    };
    assert_eq!(result.text, "answer");
    assert!(result.completion.prompt_acknowledged);
    assert!(result.completion.terminal_message_observed);
    assert!(result.completion.terminal_prompt_observed);
    assert!(result.completion.terminal_quiet_observed);
    assert!(result.completion.transcript_drained);
    assert!(result.compatibility.tested);
    assert_eq!(result.compatibility.transcript_drain_ms, 10);
    assert!(!result.completion.lifecycle_hook_observed);
    // No hook observed, so there is no instant to publish. The measurement is
    // absent rather than defaulted to a plausible-looking timestamp.
    assert_eq!(result.timings.stop_hook_at_ms, None);
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.code == "lifecycle_hook_missing")
    );
}

#[tokio::test]
async fn post_terminal_evidence_repoll_ingests_late_rows_before_commit() {
    let registry = registry(64);
    let session_id = id(201);
    let turn_id = id(2011);
    let terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence {
            ready_prompt: true,
            quiet: true,
            ..TerminalEvidence::default()
        },
        InterruptRecovery::RecoveredToReady,
    ));
    let initial_rows = simple_turn_rows("late row", "premature answer");
    let late_row = parse_line(
        2,
        br#"{"parentUuid":"answer-row","sessionId":"test","type":"future-semantic","payload":true}"#,
    );
    let initial_position = TranscriptPosition {
        generation: 0,
        offset: 2,
    };
    let late_position = TranscriptPosition {
        generation: 0,
        offset: 3,
    };
    let stable_drain = TranscriptDrainEvidence {
        at_eof: true,
        has_partial_line: false,
        stable_for_ms: 100,
    };
    let transcript = Arc::new(ScriptedTranscript {
        arm: Ok(TranscriptArm::default()),
        polls: Mutex::new(VecDeque::from([
            Ok(TranscriptBatch {
                position: initial_position,
                rows: initial_rows,
                drain: stable_drain,
            }),
            Ok(TranscriptBatch {
                position: late_position.clone(),
                rows: vec![late_row],
                drain: stable_drain,
            }),
        ])),
        fallback: TranscriptBatch {
            position: late_position,
            rows: Vec::new(),
            drain: stable_drain,
        },
        poll_delay: Duration::ZERO,
    });
    register(&registry, session_id, terminal.clone(), transcript).await;
    registry
        .run_turn(turn(session_id, turn_id, "late row"))
        .await
        .unwrap();

    let StoredTurnTerminal::Failed(error) = wait_for_terminal(&registry, session_id, turn_id).await
    else {
        panic!("late post-evidence row must prevent result commit")
    };
    assert_eq!(error.code, ErrorCode::SchemaDrift);
    assert_eq!(terminal.closes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn completed_turn_idempotent_retry_reemits_the_stored_result() {
    let registry = registry(64);
    let session_id = id(22);
    let turn_id = id(221);
    register(
        &registry,
        session_id,
        Arc::new(FakeTerminal::new(
            TerminalEvidence {
                ready_prompt: true,
                quiet: true,
                ..TerminalEvidence::default()
            },
            InterruptRecovery::RecoveredToReady,
        )),
        Arc::new(ScriptedTranscript::with_rows(
            Vec::new(),
            simple_turn_rows("retry me", "same result"),
        )),
    )
    .await;
    registry
        .run_turn(turn(session_id, turn_id, "retry me"))
        .await
        .unwrap();
    let _ = wait_for_terminal(&registry, session_id, turn_id).await;

    let accepted = registry
        .run_turn(turn(session_id, turn_id, "retry me"))
        .await
        .unwrap();
    assert!(accepted.replayed);
    let events = registry
        .events(SubscribeEventsRequest {
            session_id,
            generation_id: generation(session_id),
            after_sequence: accepted.next_sequence.saturating_sub(1),
            wait_ms: 0,
            max_events: 8,
        })
        .await
        .unwrap();
    let result = events
        .events
        .iter()
        .find_map(|event| match &event.event {
            EventPayload::TurnCompleted(result) if event.turn_id == Some(turn_id) => Some(result),
            _ => None,
        })
        .expect("the existing terminal result must be recoverable by event subscribers");
    assert_eq!(result.text, "same result");
}

#[tokio::test]
async fn actor_maps_fragmented_fixture_without_prior_sidechain_team_or_meta_leaks() {
    let registry = registry(128);
    let session_id = id(3);
    let turn_id = id(31);
    let terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence {
            ready_prompt: true,
            quiet: true,
            lifecycle_expected: true,
            lifecycle_hook_observed: true,
            lifecycle_hook_at_ms: Some(1_700_000_000_042),
        },
        InterruptRecovery::RecoveredToReady,
    ));
    let rows = fixture_rows("../../claude/tests/fixtures/fragmented_tool_turn.jsonl");
    register(
        &registry,
        session_id,
        terminal,
        Arc::new(ScriptedTranscript::with_rows(
            rows[..2].to_vec(),
            rows[2..].to_vec(),
        )),
    )
    .await;
    registry
        .run_turn(turn(session_id, turn_id, "Inspect README"))
        .await
        .unwrap();

    let StoredTurnTerminal::Result(result) =
        wait_for_terminal(&registry, session_id, turn_id).await
    else {
        panic!("expected result")
    };
    assert_eq!(result.session_id, session_id);
    assert_eq!(result.generation_id, generation(session_id));
    assert_eq!(result.turn_id, turn_id);
    assert_eq!(result.outcome, TurnOutcome::Completed);
    assert_eq!(result.text, "Done.");
    assert_eq!(
        result.final_blocks,
        vec![MessageBlock::Text {
            text: "Done.".to_owned()
        }]
    );
    assert!(!result.text.contains("stale"));
    assert!(!result.text.contains("SIDECHAIN"));
    assert!(!result.text.contains("TEAM"));
    assert!(!result.text.contains("META"));
    assert_eq!(result.usage.main.input_tokens, 220);
    assert_eq!(result.usage.main.output_tokens, 8);
    assert_eq!(result.usage.main.cache_creation_input_tokens, 2);
    assert_eq!(result.usage.main.cache_read_input_tokens, 7);
    assert_eq!(result.usage.sidechain.input_tokens, 900);
    assert_eq!(result.usage.sidechain.output_tokens, 900);
    assert_eq!(result.usage.sidechain.cache_creation_input_tokens, 0);
    assert_eq!(result.usage.sidechain.cache_read_input_tokens, 0);
    assert_eq!(result.usage.combined.input_tokens, 1_120);
    assert_eq!(result.usage.combined.output_tokens, 908);
    assert_eq!(result.usage.combined.cache_creation_input_tokens, 2);
    assert_eq!(result.usage.combined.cache_read_input_tokens, 7);
    assert_eq!(result.usage.cost_usd, None);
    assert_eq!(result.tools.len(), 1);
    assert_eq!(result.tools[0].tool_use_id, "tool-1");
    assert_eq!(result.tools[0].name, "Read");
    assert_eq!(
        result.tools[0].input,
        serde_json::json!({"file_path": "README.md"})
    );
    assert_eq!(result.tools[0].output.as_ref().unwrap(), "# Project");
    assert_eq!(result.tools[0].status, ToolStatus::Completed);
    assert_eq!(result.tools[0].started_at_ms, None);
    assert_eq!(result.tools[0].completed_at_ms, None);
    assert_eq!(result.model.as_deref(), Some("claude-test"));
    let stop_reason = result.stop_reason.as_ref().expect("terminal stop reason");
    assert_eq!(stop_reason.kind, StopReasonKind::EndTurn);
    assert_eq!(stop_reason.raw, None);
    assert!(result.timings.submitted_at_ms > 0);
    assert!(
        result
            .timings
            .prompt_acknowledged_at_ms
            .is_some_and(|value| value >= result.timings.submitted_at_ms)
    );
    assert!(
        result
            .timings
            .terminal_candidate_at_ms
            .is_some_and(|value| value >= result.timings.prompt_acknowledged_at_ms.unwrap())
    );
    assert!(result.timings.completed_at_ms >= result.timings.terminal_candidate_at_ms.unwrap());
    assert_eq!(result.timings.drain_ms, Some(100));
    // The observed Stop instant reaches the wire verbatim, so a consumer can
    // take the signed difference against `last_transcript_activity_at_ms`.
    assert_eq!(result.timings.stop_hook_at_ms, Some(1_700_000_000_042));
    assert!(result.warnings.is_empty());
    assert_eq!(result.claude_version, "2.1.207");
    assert_eq!(result.compatibility, compatibility_with_drain(10));
    assert_eq!(result.completion.authority, CompletionAuthority::Transcript);
    assert!(result.completion.prompt_acknowledged);
    assert!(result.completion.terminal_message_observed);
    assert!(result.completion.terminal_prompt_observed);
    assert!(result.completion.terminal_quiet_observed);
    assert!(result.completion.transcript_drained);
    assert!(result.completion.lifecycle_hook_observed);
    assert!(result.final_sequence > 0);
    assert!(
        result
            .warnings
            .iter()
            .all(|warning| warning.code != "lifecycle_hook_missing")
    );
    let encoded = serde_json::to_string(&result).unwrap();
    for excluded in [
        "stale answer",
        "SIDECHAIN",
        "TEAM",
        "META",
        "I should inspect it",
        "The read succeeded",
    ] {
        assert!(
            !encoded.contains(excluded),
            "turn result leaked excluded transcript content: {excluded}"
        );
    }

    let events = registry
        .events(SubscribeEventsRequest {
            session_id,
            generation_id: generation(session_id),
            after_sequence: 0,
            wait_ms: 0,
            max_events: 128,
        })
        .await
        .unwrap();
    assert!(
        events
            .events
            .windows(2)
            .all(|pair| pair[1].sequence == pair[0].sequence + 1)
    );
    assert!(
        events
            .events
            .iter()
            .any(|event| matches!(event.event, EventPayload::PromptAcknowledged(_)))
    );
    assert!(
        events
            .events
            .iter()
            .any(|event| matches!(event.event, EventPayload::ToolStarted(_)))
    );
    assert!(
        events
            .events
            .iter()
            .any(|event| matches!(event.event, EventPayload::ToolCompleted(_)))
    );
    let event_result = events
        .events
        .iter()
        .find_map(|event| match &event.event {
            EventPayload::TurnCompleted(event_result) => Some(event_result),
            _ => None,
        })
        .expect("turn-completed event");
    assert_eq!(event_result, &result);
}

#[tokio::test]
async fn bounded_replay_reports_gap_with_current_snapshot() {
    let registry = registry(4);
    let session_id = id(4);
    let turn_id = id(41);
    register(
        &registry,
        session_id,
        Arc::new(FakeTerminal::new(
            TerminalEvidence {
                ready_prompt: true,
                quiet: true,
                ..TerminalEvidence::default()
            },
            InterruptRecovery::RecoveredToReady,
        )),
        Arc::new(ScriptedTranscript::with_rows(
            Vec::new(),
            simple_turn_rows("replay", "done"),
        )),
    )
    .await;
    registry
        .run_turn(turn(session_id, turn_id, "replay"))
        .await
        .unwrap();
    let _ = wait_for_terminal(&registry, session_id, turn_id).await;

    let gap_batch = registry
        .events(SubscribeEventsRequest {
            session_id,
            generation_id: generation(session_id),
            after_sequence: 0,
            wait_ms: 0,
            max_events: 0,
        })
        .await
        .unwrap();
    let gap = gap_batch.replay_gap.expect("old cursor must have a gap");
    assert!(gap.oldest_available > 1);
    assert_eq!(gap.requested_after, 0);
    assert_eq!(gap.next_sequence, gap.snapshot.last_sequence + 1);
    assert_eq!(gap_batch.next_sequence, gap.next_sequence);
    assert!(gap_batch.events.is_empty());

    let after = gap.snapshot.last_sequence - 2;
    let recent = registry
        .events(SubscribeEventsRequest {
            session_id,
            generation_id: generation(session_id),
            after_sequence: after,
            wait_ms: 0,
            max_events: 2,
        })
        .await
        .unwrap();
    assert!(recent.replay_gap.is_none());
    assert_eq!(recent.events[0].sequence, after + 1);
    assert_eq!(recent.events[1].sequence, after + 2);
    assert_eq!(recent.next_sequence, after + 3);
}

#[tokio::test]
async fn event_page_exact_frame_boundary_never_skips_a_retained_sequence() {
    let baseline = registry(8);
    let baseline_session = id(4_200);
    register(
        &baseline,
        baseline_session,
        Arc::new(FakeTerminal::new(
            TerminalEvidence::default(),
            InterruptRecovery::RecoveredToReady,
        )),
        Arc::new(ScriptedTranscript::pending()),
    )
    .await;
    let baseline_batch = baseline
        .events(SubscribeEventsRequest {
            session_id: baseline_session,
            generation_id: generation(baseline_session),
            after_sequence: 0,
            wait_ms: 0,
            max_events: 128,
        })
        .await
        .unwrap();
    assert_eq!(baseline_batch.events.len(), 2);
    let exact_two_event_bytes = serde_json::to_vec(&ResponseEnvelope::success(
        id(4_201),
        ResponseResult::Events(baseline_batch),
    ))
    .unwrap()
    .len();
    baseline
        .close(CloseSessionRequest {
            session_id: baseline_session,
            generation_id: generation(baseline_session),
            policy: ClosePolicy::Force,
        })
        .await
        .unwrap();
    wait_for_actor_exit(&baseline, baseline_session).await;

    let mut exact_config = test_actor_config(8);
    exact_config.max_frame_bytes = exact_two_event_bytes;
    let exact = registry_with_config(exact_config);
    let exact_session = id(4_202);
    register(
        &exact,
        exact_session,
        Arc::new(FakeTerminal::new(
            TerminalEvidence::default(),
            InterruptRecovery::RecoveredToReady,
        )),
        Arc::new(ScriptedTranscript::pending()),
    )
    .await;
    let exact_batch = exact
        .events(SubscribeEventsRequest {
            session_id: exact_session,
            generation_id: generation(exact_session),
            after_sequence: 0,
            wait_ms: 0,
            max_events: 128,
        })
        .await
        .unwrap();
    assert_eq!(exact_batch.events.len(), 2);
    assert_eq!(exact_batch.next_sequence, 3);
    exact
        .close(CloseSessionRequest {
            session_id: exact_session,
            generation_id: generation(exact_session),
            policy: ClosePolicy::Force,
        })
        .await
        .unwrap();
    wait_for_actor_exit(&exact, exact_session).await;

    let mut one_below_config = test_actor_config(8);
    one_below_config.max_frame_bytes = exact_two_event_bytes - 1;
    let one_below = registry_with_config(one_below_config);
    let one_below_session = id(4_203);
    register(
        &one_below,
        one_below_session,
        Arc::new(FakeTerminal::new(
            TerminalEvidence::default(),
            InterruptRecovery::RecoveredToReady,
        )),
        Arc::new(ScriptedTranscript::pending()),
    )
    .await;
    let first_page = one_below
        .events(SubscribeEventsRequest {
            session_id: one_below_session,
            generation_id: generation(one_below_session),
            after_sequence: 0,
            wait_ms: 0,
            max_events: 128,
        })
        .await
        .unwrap();
    assert_eq!(
        first_page
            .events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1]
    );
    assert_eq!(first_page.next_sequence, 2);
    let second_page = one_below
        .events(SubscribeEventsRequest {
            session_id: one_below_session,
            generation_id: generation(one_below_session),
            after_sequence: 1,
            wait_ms: 0,
            max_events: 128,
        })
        .await
        .unwrap();
    assert_eq!(
        second_page
            .events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![2]
    );
    assert_eq!(second_page.next_sequence, 3);
    let mut candidate_events = first_page.events;
    candidate_events.extend(second_page.events);
    let candidate_bytes = serde_json::to_vec(&ResponseEnvelope::success(
        id(4_204),
        ResponseResult::Events(pseudomux_protocol::v1::EventBatch {
            events: candidate_events,
            next_sequence: 3,
            replay_gap: None,
        }),
    ))
    .unwrap()
    .len();
    assert_eq!(candidate_bytes, exact_two_event_bytes);
    assert!(candidate_bytes > exact_two_event_bytes - 1);
    one_below
        .close(CloseSessionRequest {
            session_id: one_below_session,
            generation_id: generation(one_below_session),
            policy: ClosePolicy::Force,
        })
        .await
        .unwrap();
    wait_for_actor_exit(&one_below, one_below_session).await;
}

#[tokio::test]
async fn future_subscription_cursor_fails_before_changing_actor_history() {
    let registry = registry(8);
    let session_id = id(42);
    register(
        &registry,
        session_id,
        Arc::new(FakeTerminal::new(
            TerminalEvidence::default(),
            InterruptRecovery::RecoveredToReady,
        )),
        Arc::new(ScriptedTranscript::pending()),
    )
    .await;
    let before = snapshot(&registry, session_id).await;
    let history_before = registry
        .events(SubscribeEventsRequest {
            session_id,
            generation_id: generation(session_id),
            after_sequence: 0,
            wait_ms: 0,
            max_events: 128,
        })
        .await
        .unwrap();

    let safe_future = before.last_sequence + 1;
    let error = tokio::time::timeout(
        Duration::from_millis(100),
        registry.events(SubscribeEventsRequest {
            session_id,
            generation_id: generation(session_id),
            after_sequence: safe_future,
            wait_ms: 30_000,
            max_events: 128,
        }),
    )
    .await
    .expect("a future cursor must fail before entering the long poll")
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidConfig);
    assert_eq!(error.details["requested_after"], safe_future);
    assert_eq!(error.details["last_sequence"], before.last_sequence);
    serde_json::to_vec(&ResponseEnvelope::failure(id(4_299), error)).unwrap();

    let unsafe_error = tokio::time::timeout(
        Duration::from_millis(100),
        registry.events(SubscribeEventsRequest {
            session_id,
            generation_id: generation(session_id),
            after_sequence: u64::MAX,
            wait_ms: 30_000,
            max_events: 128,
        }),
    )
    .await
    .expect("an unsafe cursor must fail in preflight before actor lookup or long poll")
    .unwrap_err();
    assert_eq!(unsafe_error.code, ErrorCode::InvalidConfig);
    assert!(unsafe_error.details.is_null());
    serde_json::to_vec(&ResponseEnvelope::failure(id(4_300), unsafe_error))
        .expect("the unsafe direct cursor rejection must be serializable");

    assert_eq!(snapshot(&registry, session_id).await, before);
    let history_after = registry
        .events(SubscribeEventsRequest {
            session_id,
            generation_id: generation(session_id),
            after_sequence: 0,
            wait_ms: 0,
            max_events: 128,
        })
        .await
        .unwrap();
    assert_eq!(history_after, history_before);
}

#[tokio::test]
async fn direct_registry_subscription_preflight_rejects_wire_and_service_bounds_before_lookup() {
    let registry = registry(64);
    let session_id = id(3_109);
    let generation_id = generation(session_id);
    let invalid = [
        SubscribeEventsRequest {
            session_id,
            generation_id,
            after_sequence: MAX_SAFE_JSON_INTEGER + 1,
            wait_ms: 0,
            max_events: 1,
        },
        SubscribeEventsRequest {
            session_id,
            generation_id,
            after_sequence: 0,
            wait_ms: MAX_SAFE_JSON_INTEGER + 1,
            max_events: 1,
        },
        SubscribeEventsRequest {
            session_id,
            generation_id,
            after_sequence: 0,
            wait_ms: u64::MAX,
            max_events: 1,
        },
        SubscribeEventsRequest {
            session_id,
            generation_id,
            after_sequence: 0,
            wait_ms: MAX_SUBSCRIBE_WAIT_MS + 1,
            max_events: 1,
        },
        SubscribeEventsRequest {
            session_id,
            generation_id,
            after_sequence: 0,
            wait_ms: 0,
            max_events: MAX_SUBSCRIBE_EVENTS + 1,
        },
    ];
    for request in invalid {
        let error = registry.events(request).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidConfig);
        assert!(!error.retryable);
        assert!(
            serde_json::to_vec(&error).is_ok(),
            "the rejection itself must remain protocol-v1 serializable"
        );
    }

    let boundary = registry
        .events(SubscribeEventsRequest {
            session_id,
            generation_id,
            after_sequence: 0,
            wait_ms: MAX_SUBSCRIBE_WAIT_MS,
            max_events: MAX_SUBSCRIBE_EVENTS,
        })
        .await
        .unwrap_err();
    assert_eq!(
        boundary.code,
        ErrorCode::SessionNotFound,
        "valid public bounds must proceed to actor lookup"
    );
}

#[tokio::test]
async fn cancellation_recovers_or_taints_and_confirmed_close_terminates_the_actor() {
    let registry = registry(64);
    for (number, recovery, expected_outcome, expected_state) in [
        (
            5,
            InterruptRecovery::RecoveredToReady,
            CancelOutcome::Cancelled,
            SessionState::Ready,
        ),
        (
            6,
            InterruptRecovery::RecoveryFailed,
            CancelOutcome::RecoveryFailed,
            SessionState::Tainted,
        ),
    ] {
        let session_id = id(number);
        let turn_id = id(number + 100);
        let terminal = Arc::new(FakeTerminal::new(TerminalEvidence::default(), recovery));
        register(
            &registry,
            session_id,
            terminal.clone(),
            Arc::new(ScriptedTranscript::pending()),
        )
        .await;
        registry
            .run_turn(turn(session_id, turn_id, "cancel me"))
            .await
            .unwrap();
        let result = registry
            .cancel_turn(CancelTurnRequest {
                session_id,
                generation_id: generation(session_id),
                turn_id,
            })
            .await
            .unwrap();
        assert_eq!(result.outcome, expected_outcome);
        assert_eq!(result.session_state, expected_state);
        assert_eq!(snapshot(&registry, session_id).await.state, expected_state);

        if expected_state == SessionState::Tainted {
            let closed = registry
                .close(CloseSessionRequest {
                    session_id,
                    generation_id: generation(session_id),
                    policy: ClosePolicy::Force,
                })
                .await
                .unwrap();
            assert!(closed.process_reaped);
            wait_for_actor_exit(&registry, session_id).await;
            let error = registry
                .close(CloseSessionRequest {
                    session_id,
                    generation_id: generation(session_id),
                    policy: ClosePolicy::Force,
                })
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::DaemonLost);
            assert_eq!(terminal.closes.load(Ordering::SeqCst), 1);
        }
    }
}

#[tokio::test]
async fn cancellation_requires_post_interrupt_transcript_stability() {
    let registry = registry(64);
    let session_id = id(6_100);
    let turn_id = id(6_101);
    let terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence::default(),
        InterruptRecovery::RecoveredToReady,
    ));
    register(
        &registry,
        session_id,
        terminal.clone(),
        Arc::new(CancellationUnstableTranscript {
            arm_calls: AtomicU64::new(0),
        }),
    )
    .await;
    registry
        .run_turn(turn(session_id, turn_id, "cancel after submission"))
        .await
        .unwrap();
    wait_for_state(&registry, session_id, SessionState::AwaitingPromptAck).await;

    let cancelled = registry
        .cancel_turn(CancelTurnRequest {
            session_id,
            generation_id: generation(session_id),
            turn_id,
        })
        .await
        .unwrap();
    assert_eq!(cancelled.outcome, CancelOutcome::RecoveryFailed);
    assert_eq!(cancelled.session_state, SessionState::Tainted);
    assert_eq!(terminal.closes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancellation_drains_late_rows_before_an_immediate_fresh_turn() {
    let registry = registry(64);
    let session_id = id(6_200);
    let cancelled_turn_id = id(6_201);
    let fresh_turn_id = id(6_202);
    let terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence {
            ready_prompt: true,
            quiet: true,
            ..TerminalEvidence::default()
        },
        InterruptRecovery::RecoveredToReady,
    ));
    register(
        &registry,
        session_id,
        terminal.clone(),
        Arc::new(CancellationLateThenFreshTranscript {
            arm_calls: AtomicU64::new(0),
            poll_calls: AtomicU64::new(0),
        }),
    )
    .await;
    registry
        .run_turn(turn(session_id, cancelled_turn_id, "cancelled prompt"))
        .await
        .unwrap();
    wait_for_state(&registry, session_id, SessionState::AwaitingPromptAck).await;

    let cancelled = registry
        .cancel_turn(CancelTurnRequest {
            session_id,
            generation_id: generation(session_id),
            turn_id: cancelled_turn_id,
        })
        .await
        .unwrap();
    assert_eq!(cancelled.outcome, CancelOutcome::Cancelled);
    assert_eq!(cancelled.session_state, SessionState::Ready);

    registry
        .run_turn(turn(session_id, fresh_turn_id, "after cancel"))
        .await
        .unwrap();
    let StoredTurnTerminal::Result(result) =
        wait_for_terminal(&registry, session_id, fresh_turn_id).await
    else {
        panic!("fresh turn after cancellation must complete")
    };
    assert_eq!(result.text, "fresh answer");
    assert!(!result.text.contains("late cancelled answer"));
    assert_eq!(terminal.submissions.lock().unwrap().len(), 2);
    assert_eq!(terminal.closes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn unconfirmed_close_stays_retryable_until_process_reaping_is_confirmed() {
    let registry = registry(16);
    let session_id = id(61);
    let terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence::default(),
        InterruptRecovery::RecoveredToReady,
    ));
    terminal.set_close_reaped(false);
    register(
        &registry,
        session_id,
        terminal.clone(),
        Arc::new(ScriptedTranscript::pending()),
    )
    .await;

    let first = registry
        .close(CloseSessionRequest {
            session_id,
            generation_id: generation(session_id),
            policy: ClosePolicy::Graceful,
        })
        .await
        .unwrap();
    assert!(!first.process_reaped);
    assert_eq!(
        snapshot(&registry, session_id).await.state,
        SessionState::Closing
    );

    terminal.set_close_reaped(true);
    let second = registry
        .close(CloseSessionRequest {
            session_id,
            generation_id: generation(session_id),
            policy: ClosePolicy::Force,
        })
        .await
        .unwrap();
    assert!(second.process_reaped);
    assert!(!second.already_closed);
    assert_eq!(terminal.closes.load(Ordering::SeqCst), 2);
    wait_for_actor_exit(&registry, session_id).await;
}

#[tokio::test]
async fn ambiguous_submit_failure_is_force_reaped_stored_and_never_reinvoked_on_replay() {
    let registry = registry(64);
    let session_id = id(7_500);
    let turn_id = id(7_501);
    let terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence::default(),
        InterruptRecovery::RecoveredToReady,
    ));
    terminal.set_submit_failure(
        DriverFailure::new(
            ErrorCode::RecoveryFailed,
            "terminal Enter acknowledgement was ambiguous; submission was not retried",
        )
        .with_details(serde_json::json!({ "enter_attempted": true })),
    );
    register(
        &registry,
        session_id,
        terminal.clone(),
        Arc::new(ScriptedTranscript::pending()),
    )
    .await;

    let request = turn(session_id, turn_id, "ambiguous submit");
    let accepted = registry.run_turn(request.clone()).await.unwrap();
    assert!(!accepted.replayed);
    let StoredTurnTerminal::Failed(first) = wait_for_terminal(&registry, session_id, turn_id).await
    else {
        panic!("ambiguous submit must store a terminal failure")
    };
    assert_eq!(first.code, ErrorCode::RecoveryFailed);
    assert_eq!(first.details["enter_attempted"], true);
    assert!(!first.retryable);
    assert_eq!(terminal.submissions.lock().unwrap().len(), 1);
    assert_eq!(terminal.closes.load(Ordering::SeqCst), 1);
    assert_eq!(
        snapshot(&registry, session_id).await.state,
        SessionState::Failed
    );

    let replay = registry.run_turn(request).await.unwrap();
    assert!(replay.replayed);
    let StoredTurnTerminal::Failed(replayed) = registry
        .stored_turn(session_id, generation(session_id), turn_id)
        .await
        .unwrap()
        .expect("ambiguous submit failure remains replayable")
    else {
        panic!("replay must retain the stored failure")
    };
    assert_eq!(replayed, first);
    assert_eq!(terminal.submissions.lock().unwrap().len(), 1);
    assert_eq!(terminal.closes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn unsafe_numeric_driver_error_details_are_replaced_before_event_emission() {
    let registry = registry(64);
    let session_id = id(7_510);
    let turn_id = id(7_511);
    let terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence::default(),
        InterruptRecovery::RecoveredToReady,
    ));
    terminal.set_submit_failure(
        DriverFailure::new(ErrorCode::RecoveryFailed, "unsafe injected diagnostics").with_details(
            serde_json::json!({
                "unsafe_integer": pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER + 1,
            }),
        ),
    );
    register(
        &registry,
        session_id,
        terminal,
        Arc::new(ScriptedTranscript::pending()),
    )
    .await;

    registry
        .run_turn(turn(session_id, turn_id, "fail safely"))
        .await
        .unwrap();
    let StoredTurnTerminal::Failed(error) = wait_for_terminal(&registry, session_id, turn_id).await
    else {
        panic!("unsafe diagnostics must produce a bounded stored failure")
    };
    assert_eq!(error.code, ErrorCode::RecoveryFailed);
    assert_eq!(error.message, "unsafe injected diagnostics");
    assert!(error.details.is_null());

    let batch = registry
        .events(SubscribeEventsRequest {
            session_id,
            generation_id: generation(session_id),
            after_sequence: 0,
            wait_ms: 0,
            max_events: 128,
        })
        .await
        .unwrap();
    let wire = serde_json::to_vec(&ResponseEnvelope::success(
        id(7_512),
        ResponseResult::Events(batch),
    ))
    .unwrap();
    assert!(
        !String::from_utf8(wire)
            .unwrap()
            .contains(&(pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER + 1).to_string())
    );
}

#[tokio::test]
async fn transcript_opaque_numbers_fail_before_any_public_producer_can_serialize_them() {
    // Tool/content deltas are intentionally published before TurnResult. An
    // unsafe opaque value therefore reaches the shared producer preflight at
    // the first applicable event and cannot independently reach final-result
    // construction. This test proves that earliest fail-closed boundary and
    // every distinct transcript-owned opaque route without claiming otherwise.
    let unsafe_numbers = [
        ("9007199254740992", "positive"),
        ("-9007199254740992", "negative"),
    ];
    let routes: [(OpaqueRouteBuilder, ErrorCode); 3] = [
        (unsafe_tool_input_rows, ErrorCode::RecoveryFailed),
        (unsafe_tool_result_rows, ErrorCode::RecoveryFailed),
        (unsafe_unknown_content_rows, ErrorCode::SchemaDrift),
    ];

    for (number_index, (unsafe_number, sign)) in unsafe_numbers.into_iter().enumerate() {
        for (route_index, (build_rows, expected_code)) in routes.into_iter().enumerate() {
            let registry = registry(64);
            let session_id = id(7_520 + (number_index * 10 + route_index) as u128);
            let turn_id = id(7_620 + (number_index * 10 + route_index) as u128);
            let terminal = Arc::new(FakeTerminal::new(
                TerminalEvidence {
                    ready_prompt: true,
                    quiet: true,
                    ..TerminalEvidence::default()
                },
                InterruptRecovery::RecoveredToReady,
            ));
            register(
                &registry,
                session_id,
                terminal.clone(),
                Arc::new(ScriptedTranscript::with_rows(
                    Vec::new(),
                    build_rows(unsafe_number),
                )),
            )
            .await;

            registry
                .run_turn(turn(session_id, turn_id, "opaque"))
                .await
                .unwrap();
            let StoredTurnTerminal::Failed(error) =
                wait_for_terminal(&registry, session_id, turn_id).await
            else {
                panic!("{sign} unsafe number on route {route_index} must fail closed")
            };
            assert_eq!(
                error.code, expected_code,
                "{sign} unsafe number on route {route_index} returned {error:?}"
            );
            assert!(
                error.details.is_null()
                    || error.details == serde_json::json!({"source": "pseudomux_claude"}),
                "the bounded failure must not preserve opaque diagnostics: {error:?}"
            );
            assert_eq!(
                terminal.closes.load(Ordering::SeqCst),
                1,
                "a fatal transcript/publication mismatch must reap the terminal"
            );

            let batch = registry
                .events(SubscribeEventsRequest {
                    session_id,
                    generation_id: generation(session_id),
                    after_sequence: 0,
                    wait_ms: 0,
                    max_events: 128,
                })
                .await
                .unwrap();
            let wire = serde_json::to_string(&ResponseEnvelope::success(
                id(7_720 + (number_index * 10 + route_index) as u128),
                ResponseResult::Events(batch),
            ))
            .unwrap();
            assert!(
                !wire.contains(unsafe_number),
                "{sign} unsafe number leaked through route {route_index}: {wire}"
            );

            let replayed = registry
                .run_turn(turn(session_id, turn_id, "opaque"))
                .await
                .unwrap();
            assert!(replayed.replayed);
            let replayed_terminal = registry
                .stored_turn(session_id, generation(session_id), turn_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(replayed_terminal, StoredTurnTerminal::Failed(error));

            registry
                .close(CloseSessionRequest {
                    session_id,
                    generation_id: generation(session_id),
                    policy: ClosePolicy::Force,
                })
                .await
                .unwrap();
        }
    }
}

#[tokio::test]
async fn noncausal_unknown_row_warning_never_copies_its_unsafe_raw_payload() {
    for (index, unsafe_number) in ["9007199254740992", "-9007199254740992"]
        .into_iter()
        .enumerate()
    {
        let registry = registry(64);
        let session_id = id(7_800 + index as u128);
        let turn_id = id(7_810 + index as u128);
        register(
            &registry,
            session_id,
            Arc::new(FakeTerminal::new(
                TerminalEvidence {
                    ready_prompt: true,
                    quiet: true,
                    ..TerminalEvidence::default()
                },
                InterruptRecovery::RecoveredToReady,
            )),
            Arc::new(ScriptedTranscript::with_rows(
                Vec::new(),
                unknown_off_branch_rows(unsafe_number),
            )),
        )
        .await;

        registry
            .run_turn(turn(session_id, turn_id, "opaque"))
            .await
            .unwrap();
        let StoredTurnTerminal::Result(result) =
            wait_for_terminal(&registry, session_id, turn_id).await
        else {
            panic!("a noncausal unknown row must remain a warning")
        };
        assert_eq!(result.text, "safe answer");
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.code == "unknown_transcript_row")
        );
        let wire = serde_json::to_string(&*result).unwrap();
        assert!(!wire.contains(unsafe_number));

        registry
            .close(CloseSessionRequest {
                session_id,
                generation_id: generation(session_id),
                policy: ClosePolicy::Force,
            })
            .await
            .unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn worker_timing_producers_check_near_safe_max_and_one_past_at_the_sample() {
    use pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER as MAX;

    let samples = [
        (TimingSample::PromptAcknowledged, "prompt_acknowledged_at"),
        (TimingSample::TerminalCandidate, "terminal_candidate_at"),
        (TimingSample::Completed, "turn_completed_at"),
    ];
    for (sample_index, (sample, resource)) in samples.into_iter().enumerate() {
        for (target_index, target) in [MAX - 1, MAX + 1].into_iter().enumerate() {
            let clock = Arc::new(ArmedClock::new(1_000_000));
            let registry = SessionRegistry::with_clock(test_actor_config(64), clock.clone());
            let session_id = id(7_900 + (sample_index * 10 + target_index) as u128);
            let turn_id = id(7_950 + (sample_index * 10 + target_index) as u128);
            let terminal = Arc::new(FakeTerminal::new(
                TerminalEvidence {
                    ready_prompt: true,
                    quiet: true,
                    ..TerminalEvidence::default()
                },
                InterruptRecovery::RecoveredToReady,
            ));
            register(
                &registry,
                session_id,
                terminal.clone(),
                Arc::new(TimingMutationTranscript {
                    clock,
                    sample,
                    target,
                    poll_calls: AtomicU64::new(0),
                }),
            )
            .await;

            let mut request = turn(session_id, turn_id, "timing");
            request.turn.deadline_unix_ms = Some(MAX);
            registry.run_turn(request).await.unwrap();
            let stored = wait_for_terminal(&registry, session_id, turn_id).await;
            if target <= MAX {
                let StoredTurnTerminal::Result(result) = stored else {
                    panic!("near-boundary {sample:?} sample must remain representable")
                };
                let observed = match sample {
                    TimingSample::PromptAcknowledged => result.timings.prompt_acknowledged_at_ms,
                    TimingSample::TerminalCandidate => result.timings.terminal_candidate_at_ms,
                    TimingSample::Completed => Some(result.timings.completed_at_ms),
                };
                assert_eq!(observed, Some(target));
                serde_json::to_vec(&*result).unwrap();
                assert_eq!(terminal.closes.load(Ordering::SeqCst), 0);
            } else {
                let StoredTurnTerminal::Failed(error) = stored else {
                    panic!("one-past {sample:?} sample must fail closed")
                };
                assert_eq!(error.code, ErrorCode::RecoveryFailed);
                assert_eq!(error.details["resource"], resource);
                assert_eq!(error.details["maximum"], MAX);
                let encoded = serde_json::to_string(&error).unwrap();
                assert!(!encoded.contains(&target.to_string()));
                assert_eq!(
                    terminal.closes.load(Ordering::SeqCst),
                    1,
                    "an invalid transcript-derived timing must reap the terminal"
                );
            }

            registry
                .close(CloseSessionRequest {
                    session_id,
                    generation_id: generation(session_id),
                    policy: ClosePolicy::Force,
                })
                .await
                .unwrap();
        }
    }
}

#[tokio::test]
async fn missing_transcript_and_active_schema_drift_are_typed_failures() {
    let registry = registry(64);
    let missing_session = id(7);
    let missing_turn = id(71);
    let missing_terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence::default(),
        InterruptRecovery::RecoveredToReady,
    ));
    register(
        &registry,
        missing_session,
        missing_terminal.clone(),
        Arc::new(ScriptedTranscript::failing(
            ErrorCode::TranscriptUnavailable,
            "main JSONL was not created",
        )),
    )
    .await;
    registry
        .run_turn(turn(missing_session, missing_turn, "missing"))
        .await
        .unwrap();
    let StoredTurnTerminal::Failed(error) =
        wait_for_terminal(&registry, missing_session, missing_turn).await
    else {
        panic!("expected missing transcript failure")
    };
    assert_eq!(error.code, ErrorCode::TranscriptUnavailable);
    assert_eq!(
        snapshot(&registry, missing_session).await.state,
        SessionState::Failed
    );
    assert_eq!(
        missing_terminal.closes.load(Ordering::SeqCst),
        1,
        "fatal driver failures must force-reap the interactive process"
    );

    let schema_session = id(8);
    let schema_turn = id(81);
    register(
        &registry,
        schema_session,
        Arc::new(FakeTerminal::new(
            TerminalEvidence {
                ready_prompt: true,
                quiet: true,
                ..TerminalEvidence::default()
            },
            InterruptRecovery::RecoveredToReady,
        )),
        Arc::new(ScriptedTranscript::with_rows(
            Vec::new(),
            fixture_rows("../../claude/tests/fixtures/unknown_active.jsonl"),
        )),
    )
    .await;
    registry
        .run_turn(turn(schema_session, schema_turn, "hello"))
        .await
        .unwrap();
    let StoredTurnTerminal::Failed(error) =
        wait_for_terminal(&registry, schema_session, schema_turn).await
    else {
        panic!("expected schema failure")
    };
    assert_eq!(error.code, ErrorCode::SchemaDrift);
}

#[tokio::test]
async fn idle_metadata_defaults_to_thirty_minutes() {
    let registry = registry(16);
    let session_id = id(9);
    register(
        &registry,
        session_id,
        Arc::new(FakeTerminal::new(
            TerminalEvidence::default(),
            InterruptRecovery::RecoveredToReady,
        )),
        Arc::new(ScriptedTranscript::pending()),
    )
    .await;
    let snapshot = snapshot(&registry, session_id).await;
    assert_eq!(snapshot.state, SessionState::Ready);
    assert_eq!(
        snapshot.idle_deadline_ms.unwrap() - snapshot.updated_at_ms,
        30 * 60 * 1_000
    );
    assert_eq!(snapshot.active_turn_id, None);
}

#[tokio::test]
async fn idle_expiration_is_atomic_and_never_closes_an_active_turn() {
    let registry = registry(16);
    let session_id = id(91);
    let turn_id = id(911);
    let terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence::default(),
        InterruptRecovery::RecoveredToReady,
    ));
    register(
        &registry,
        session_id,
        terminal.clone(),
        Arc::new(ScriptedTranscript::pending()),
    )
    .await;
    registry
        .run_turn(turn(session_id, turn_id, "still active"))
        .await
        .unwrap();
    assert!(
        registry
            .expire_idle(session_id, generation(session_id), u64::MAX)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(terminal.closes.load(Ordering::SeqCst), 0);
    registry
        .cancel_turn(CancelTurnRequest {
            session_id,
            generation_id: generation(session_id),
            turn_id,
        })
        .await
        .unwrap();

    let expired = registry
        .expire_idle(session_id, generation(session_id), u64::MAX)
        .await
        .unwrap()
        .expect("ready idle session should expire");
    assert!(expired.process_reaped);
    assert_eq!(terminal.closes.load(Ordering::SeqCst), 1);
    wait_for_actor_exit(&registry, session_id).await;
}

#[tokio::test]
async fn unattached_startup_needs_input_obeys_the_idle_ttl() {
    let registry = registry(16);
    let session_id = id(919);
    let terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence::default(),
        InterruptRecovery::RecoveredToReady,
    ));
    let trust = needs_input(NeedsInputKind::Trust);
    terminal.set_screen(TerminalScreenObservation::NeedsInput(trust.clone()));
    register_with_initial(
        &registry,
        session_id,
        terminal.clone(),
        Arc::new(ScriptedTranscript::pending()),
        Some(trust),
    )
    .await;

    let before = snapshot(&registry, session_id).await;
    assert_eq!(before.state, SessionState::NeedsInput);
    assert!(before.idle_deadline_ms.is_some());
    let expired = registry
        .expire_idle(session_id, generation(session_id), u64::MAX)
        .await
        .unwrap()
        .expect("unattached startup modal should not outlive its idle TTL");
    assert!(expired.process_reaped);
    assert_eq!(terminal.closes.load(Ordering::SeqCst), 1);
}

/// A session blocked on a modal names the modal AND what to do about it.
///
/// MEASURED over the real socket before the change: `pmux turn` against a
/// freshly started session in an untrusted cwd printed
///
/// ```text
/// pmux: pmuxd error code=NeedsTrust message="session is not ready: NeedsInput" retryable=false
/// ```
///
/// while `pmux inspect` on the SAME session, one call later, reported
/// `needs_input: {kind: trust, message: "Claude requires workspace trust
/// confirmation"}`. The actor was holding the answer and rendering `{:?}` of the
/// state instead. The recommendation is exhaustive over `NeedsInputKind`, so a
/// kind added later cannot reach a caller with no way out.
#[tokio::test]
async fn a_turn_refused_by_a_modal_names_the_modal_and_what_to_do_about_it() {
    let modal_registry = registry(16);
    let session_id = id(9_231);
    let terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence::default(),
        InterruptRecovery::RecoveredToReady,
    ));
    let trust = needs_input(NeedsInputKind::Trust);
    terminal.set_screen(TerminalScreenObservation::NeedsInput(trust.clone()));
    register_with_initial(
        &modal_registry,
        session_id,
        terminal,
        Arc::new(ScriptedTranscript::pending()),
        Some(trust.clone()),
    )
    .await;

    let refused = modal_registry
        .run_turn(turn(session_id, id(9_232), "blocked behind a modal"))
        .await
        .unwrap_err();

    assert_eq!(refused.code, ErrorCode::NeedsTrust);
    assert!(
        refused.message.contains("Trust"),
        "the refusal does not name the modal that is blocking the session: {}",
        refused.message
    );
    assert!(
        refused.message.contains(&trust.message),
        "the refusal drops Claude's own words for the modal: {}",
        refused.message
    );
    assert_eq!(refused.details["needs_input_kind"], "trust");
    let recommendation = refused.details["recommendation"]
        .as_str()
        .expect("a blocked session must publish a recommendation");
    assert!(
        recommendation.contains("remint"),
        "the recommendation does not name an action a caller can take: {recommendation}"
    );

    // A state that is not `NeedsInput` has no modal to name and must not
    // invent one.
    let busy = registry(16);
    let busy_session = id(9_233);
    register(
        &busy,
        busy_session,
        Arc::new(FakeTerminal::new(
            TerminalEvidence::default(),
            InterruptRecovery::RecoveredToReady,
        )),
        Arc::new(ScriptedTranscript::pending()),
    )
    .await;
    busy.run_turn(turn(busy_session, id(9_234), "first"))
        .await
        .unwrap();
    let second = busy
        .run_turn(turn(busy_session, id(9_235), "second"))
        .await
        .unwrap_err();
    assert!(
        second.details["recommendation"].is_null(),
        "a session with no modal published a modal recommendation: {:?}",
        second.details
    );
}

#[tokio::test]
async fn writable_attach_reservation_serializes_ready_input_and_rejects_running_sessions() {
    let registry = registry(64);
    let session_id = id(912);
    let turn_id = id(913);
    let attach_id = id(914);
    let terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence::default(),
        InterruptRecovery::RecoveredToReady,
    ));
    let mut acknowledgement_only = simple_turn_rows("running turn", "unused");
    acknowledgement_only.truncate(1);
    register(
        &registry,
        session_id,
        terminal,
        Arc::new(ScriptedTranscript::with_rows(
            Vec::new(),
            acknowledgement_only,
        )),
    )
    .await;

    registry
        .reserve_writable_attach(session_id, generation(session_id), attach_id)
        .await
        .unwrap();
    let duplicate = registry
        .reserve_writable_attach(session_id, generation(session_id), id(915))
        .await
        .unwrap_err();
    assert_eq!(duplicate.code, ErrorCode::SessionBusy);
    let blocked_turn = registry
        .run_turn(turn(session_id, turn_id, "running turn"))
        .await
        .unwrap_err();
    assert_eq!(blocked_turn.code, ErrorCode::SessionBusy);
    assert!(blocked_turn.retryable);
    assert!(
        registry
            .expire_idle(session_id, generation(session_id), u64::MAX)
            .await
            .unwrap()
            .is_none(),
        "an attach reservation owns the idle terminal"
    );
    let wrong_release = registry
        .release_writable_attach(
            session_id,
            generation(session_id),
            id(915),
            WritableAttachCompletion::Unused,
        )
        .await
        .unwrap_err();
    assert_eq!(wrong_release.code, ErrorCode::IdConflict);
    registry
        .release_writable_attach(
            session_id,
            generation(session_id),
            attach_id,
            WritableAttachCompletion::Unused,
        )
        .await
        .unwrap();

    registry
        .run_turn(turn(session_id, turn_id, "running turn"))
        .await
        .unwrap();
    wait_for_state(&registry, session_id, SessionState::Running).await;
    let running = registry
        .reserve_writable_attach(session_id, generation(session_id), id(916))
        .await
        .unwrap_err();
    assert_eq!(running.code, ErrorCode::SessionBusy);
    assert!(running.retryable);
    registry
        .cancel_turn(CancelTurnRequest {
            session_id,
            generation_id: generation(session_id),
            turn_id,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn potentially_mutating_attach_stays_unavailable_and_taints_when_busy() {
    let registry = registry(64);
    let session_id = id(9_101);
    let attach_id = id(9_102);
    let terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence {
            ready_prompt: false,
            quiet: true,
            ..TerminalEvidence::default()
        },
        InterruptRecovery::RecoveredToReady,
    ));
    // An ordinary busy cell: a composer holding the caller's own text. This
    // test is about attach capability while busy, not about a screen pmux
    // cannot read, so it names the recognized screen rather than borrowing the
    // unrecognized one.
    terminal.set_screen(TerminalScreenObservation::Recognised(
        RecognisedScreen::ComposerHoldingText,
    ));
    register(
        &registry,
        session_id,
        terminal,
        Arc::new(ScriptedTranscript::stable_at_eof(Duration::ZERO)),
    )
    .await;
    registry
        .reserve_writable_attach(session_id, generation(session_id), attach_id)
        .await
        .unwrap();
    registry
        .release_writable_attach(
            session_id,
            generation(session_id),
            attach_id,
            WritableAttachCompletion::PotentiallyMutated,
        )
        .await
        .unwrap();

    let blocked = registry
        .run_turn(turn(session_id, id(9_103), "must wait for reconciliation"))
        .await
        .unwrap_err();
    assert_eq!(blocked.code, ErrorCode::SessionBusy);
    wait_for_state(&registry, session_id, SessionState::Tainted).await;
    let failed_closed = registry
        .run_turn(turn(session_id, id(9_104), "must remain unavailable"))
        .await
        .unwrap_err();
    assert_eq!(failed_closed.code, ErrorCode::RecoveryFailed);
    assert!(!failed_closed.retryable);
}

#[tokio::test]
async fn post_attach_ready_quiet_and_exact_drain_resolves_startup_modal() {
    let registry = registry(64);
    let session_id = id(9_111);
    let attach_id = id(9_112);
    let terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence {
            ready_prompt: true,
            quiet: true,
            ..TerminalEvidence::default()
        },
        InterruptRecovery::RecoveredToReady,
    ));
    let modal = needs_input(NeedsInputKind::Trust);
    terminal.set_screen(TerminalScreenObservation::NeedsInput(modal.clone()));
    register_with_initial(
        &registry,
        session_id,
        terminal.clone(),
        Arc::new(ScriptedTranscript::stable_at_eof(Duration::from_millis(5))),
        Some(modal),
    )
    .await;
    registry
        .reserve_writable_attach(session_id, generation(session_id), attach_id)
        .await
        .unwrap();
    terminal.set_screen(TerminalScreenObservation::Ready);
    registry
        .release_writable_attach(
            session_id,
            generation(session_id),
            attach_id,
            WritableAttachCompletion::PotentiallyMutated,
        )
        .await
        .unwrap();

    let blocked = registry
        .run_turn(turn(session_id, id(9_113), "not before reconciliation"))
        .await
        .unwrap_err();
    assert_eq!(blocked.code, ErrorCode::SessionBusy);
    assert_eq!(
        snapshot(&registry, session_id).await.state,
        SessionState::NeedsInput
    );
    for _ in 0..100 {
        let snapshot = snapshot(&registry, session_id).await;
        if snapshot.state == SessionState::Ready && snapshot.idle_deadline_ms.is_some() {
            assert!(snapshot.needs_input.is_none());
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("ready attach reconciliation did not release the reservation");
}

#[tokio::test]
async fn close_is_serviced_while_attach_reconciliation_is_pending() {
    let registry = registry(64);
    let session_id = id(9_121);
    let attach_id = id(9_122);
    register(
        &registry,
        session_id,
        Arc::new(FakeTerminal::new(
            TerminalEvidence::default(),
            InterruptRecovery::RecoveredToReady,
        )),
        Arc::new(ScriptedTranscript::stable_at_eof(Duration::from_secs(1))),
    )
    .await;
    registry
        .reserve_writable_attach(session_id, generation(session_id), attach_id)
        .await
        .unwrap();
    registry
        .release_writable_attach(
            session_id,
            generation(session_id),
            attach_id,
            WritableAttachCompletion::PotentiallyMutated,
        )
        .await
        .unwrap();

    let during_reconciliation = tokio::time::timeout(
        Duration::from_millis(20),
        registry.inspect(InspectSessionRequest {
            session_id,
            generation_id: generation(session_id),
        }),
    )
    .await
    .expect("reconciliation must not block inspection")
    .unwrap();
    assert!(during_reconciliation.idle_deadline_ms.is_none());

    let closed = tokio::time::timeout(
        Duration::from_millis(20),
        registry.close(CloseSessionRequest {
            session_id,
            generation_id: generation(session_id),
            policy: ClosePolicy::Force,
        }),
    )
    .await
    .expect("reconciliation must not block the actor mailbox")
    .unwrap();
    assert!(closed.process_reaped);
}

#[tokio::test]
async fn startup_needs_input_returns_a_live_handle_and_resolves_only_at_ready() {
    let registry = registry(64);
    let session_id = id(901);
    let terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence::default(),
        InterruptRecovery::RecoveredToReady,
    ));
    let trust = needs_input(NeedsInputKind::Trust);
    terminal.set_screen(TerminalScreenObservation::NeedsInput(trust.clone()));
    let handle = register_with_initial(
        &registry,
        session_id,
        terminal.clone(),
        Arc::new(ScriptedTranscript::pending()),
        Some(trust.clone()),
    )
    .await;

    assert_eq!(handle.state, SessionState::NeedsInput);
    let blocked = snapshot(&registry, session_id).await;
    assert_eq!(blocked.state, SessionState::NeedsInput);
    assert_eq!(blocked.needs_input, Some(trust.clone()));
    assert_eq!(blocked.active_turn_id, None);
    let error = registry
        .run_turn(turn(session_id, id(902), "wait for trust"))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NeedsTrust);
    assert!(error.retryable);

    let events = registry
        .events(SubscribeEventsRequest {
            session_id,
            generation_id: generation(session_id),
            after_sequence: 0,
            wait_ms: 0,
            max_events: 64,
        })
        .await
        .unwrap();
    assert_eq!(
        events
            .events
            .iter()
            .filter(|event| matches!(event.event, EventPayload::NeedsInput(_)))
            .count(),
        1,
        "unchanged modal observations must not spam events"
    );

    terminal.set_screen(unrecognised_screen());
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert_eq!(
        snapshot(&registry, session_id).await.state,
        SessionState::NeedsInput,
        "an ambiguous startup screen must not be promoted to ready"
    );
    terminal.set_screen(TerminalScreenObservation::Ready);
    wait_for_state(&registry, session_id, SessionState::Ready).await;
    assert_eq!(snapshot(&registry, session_id).await.needs_input, None);
}

#[tokio::test]
async fn active_modal_preserves_the_underlying_turn_phase_until_user_resolution() {
    let registry = registry(128);
    let session_id = id(903);
    let turn_id = id(904);
    let terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence::default(),
        InterruptRecovery::RecoveredToReady,
    ));
    register(
        &registry,
        session_id,
        terminal.clone(),
        Arc::new(ScriptedTranscript::with_rows(
            Vec::new(),
            simple_turn_rows("permission turn", "done"),
        )),
    )
    .await;
    let permission = needs_input(NeedsInputKind::Permission);
    terminal.set_screen(TerminalScreenObservation::NeedsInput(permission.clone()));
    registry
        .run_turn(turn(session_id, turn_id, "permission turn"))
        .await
        .unwrap();

    wait_for_state(&registry, session_id, SessionState::NeedsInput).await;
    let attach_id = id(917);
    registry
        .reserve_writable_attach(session_id, generation(session_id), attach_id)
        .await
        .expect("NeedsInput must admit a human writable attachment");
    wait_for_event(&registry, session_id, |event| {
        matches!(event, EventPayload::TerminalCandidate(_))
    })
    .await;
    let blocked = snapshot(&registry, session_id).await;
    assert_eq!(blocked.active_turn_id, Some(turn_id));
    assert_eq!(blocked.needs_input, Some(permission));
    assert!(
        registry
            .stored_turn(session_id, generation(session_id), turn_id)
            .await
            .unwrap()
            .is_none(),
        "a modal must prevent terminal completion"
    );
    assert_eq!(terminal.submissions.lock().unwrap().len(), 1);

    terminal.set_screen(unrecognised_screen());
    wait_for_state(&registry, session_id, SessionState::Draining).await;
    assert_eq!(snapshot(&registry, session_id).await.needs_input, None);

    terminal.set_screen(TerminalScreenObservation::Ready);
    terminal.set_evidence(TerminalEvidence {
        ready_prompt: true,
        quiet: true,
        ..TerminalEvidence::default()
    });
    let StoredTurnTerminal::Result(result) =
        wait_for_terminal(&registry, session_id, turn_id).await
    else {
        panic!("expected a completed result after modal resolution")
    };
    assert_eq!(result.text, "done");
    let blocked = registry
        .run_turn(turn(session_id, id(918), "must wait for detach"))
        .await
        .unwrap_err();
    assert_eq!(blocked.code, ErrorCode::SessionBusy);
    registry
        .release_writable_attach(
            session_id,
            generation(session_id),
            attach_id,
            WritableAttachCompletion::Unused,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn cancellation_close_and_deadline_remain_effective_while_input_is_required() {
    let registry = registry(128);

    let cancel_session = id(905);
    let cancel_turn = id(906);
    let cancel_terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence::default(),
        InterruptRecovery::RecoveredToReady,
    ));
    register(
        &registry,
        cancel_session,
        cancel_terminal.clone(),
        Arc::new(ScriptedTranscript::pending()),
    )
    .await;
    cancel_terminal.set_screen(TerminalScreenObservation::NeedsInput(needs_input(
        NeedsInputKind::Permission,
    )));
    registry
        .run_turn(turn(cancel_session, cancel_turn, "cancel modal"))
        .await
        .unwrap();
    wait_for_state(&registry, cancel_session, SessionState::NeedsInput).await;
    let cancelled = registry
        .cancel_turn(CancelTurnRequest {
            session_id: cancel_session,
            generation_id: generation(cancel_session),
            turn_id: cancel_turn,
        })
        .await
        .unwrap();
    assert_eq!(cancelled.outcome, CancelOutcome::Cancelled);
    assert_eq!(cancelled.session_state, SessionState::Ready);
    assert_eq!(snapshot(&registry, cancel_session).await.needs_input, None);

    let close_session = id(907);
    let close_terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence::default(),
        InterruptRecovery::RecoveredToReady,
    ));
    let login = needs_input(NeedsInputKind::Login);
    close_terminal.set_screen(TerminalScreenObservation::NeedsInput(login.clone()));
    let handle = register_with_initial(
        &registry,
        close_session,
        close_terminal.clone(),
        Arc::new(ScriptedTranscript::pending()),
        Some(login),
    )
    .await;
    assert_eq!(handle.state, SessionState::NeedsInput);
    let closed = registry
        .close(CloseSessionRequest {
            session_id: close_session,
            generation_id: generation(close_session),
            policy: ClosePolicy::Force,
        })
        .await
        .unwrap();
    assert!(closed.process_reaped);
    wait_for_actor_exit(&registry, close_session).await;

    let deadline_registry = registry_with_config(test_actor_config(128));
    let deadline_session = id(908);
    let deadline_turn = id(909);
    let deadline_terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence::default(),
        InterruptRecovery::RecoveredToReady,
    ));
    register(
        &deadline_registry,
        deadline_session,
        deadline_terminal.clone(),
        Arc::new(ScriptedTranscript::pending()),
    )
    .await;
    deadline_terminal.set_screen(TerminalScreenObservation::NeedsInput(needs_input(
        NeedsInputKind::Permission,
    )));
    let mut request = turn(deadline_session, deadline_turn, "deadline modal");
    request.turn.deadline_unix_ms = Some(1_000_010);
    deadline_registry.run_turn(request).await.unwrap();
    let StoredTurnTerminal::Failed(error) =
        wait_for_terminal(&deadline_registry, deadline_session, deadline_turn).await
    else {
        panic!("expected a deadline failure")
    };
    assert_eq!(error.code, ErrorCode::TurnTimeout);
    assert_eq!(deadline_terminal.closes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn oversized_exact_result_becomes_one_replayable_failure_without_reinjection() {
    let mut config = test_actor_config(128);
    config.max_frame_bytes = 1_024;
    let registry = registry_with_config(config);
    let session_id = id(920);
    let turn_id = id(921);
    let terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence {
            ready_prompt: true,
            quiet: true,
            ..TerminalEvidence::default()
        },
        InterruptRecovery::RecoveredToReady,
    ));
    register(
        &registry,
        session_id,
        terminal.clone(),
        Arc::new(ScriptedTranscript::with_rows(
            Vec::new(),
            simple_turn_rows("large result", &"x".repeat(8 * 1_024)),
        )),
    )
    .await;

    let request = turn(session_id, turn_id, "large result");
    let accepted = registry.run_turn(request.clone()).await.unwrap();
    assert!(!accepted.replayed);
    let StoredTurnTerminal::Failed(first_error) =
        wait_for_terminal(&registry, session_id, turn_id).await
    else {
        panic!("oversized result must be stored as a terminal failure")
    };
    assert_eq!(first_error.code, ErrorCode::ResultTooLarge);
    assert_eq!(terminal.submissions.lock().unwrap().len(), 1);
    assert_eq!(
        snapshot(&registry, session_id).await.state,
        SessionState::Ready
    );

    let mut after_sequence = 0;
    let last_sequence = snapshot(&registry, session_id).await.last_sequence;
    let mut completed_events = 0;
    let mut failed_events = 0;
    while after_sequence < last_sequence {
        let batch = registry
            .events(SubscribeEventsRequest {
                session_id,
                generation_id: generation(session_id),
                after_sequence,
                wait_ms: 0,
                max_events: 128,
            })
            .await
            .unwrap();
        let wire_bytes = serde_json::to_vec(&ResponseEnvelope::success(
            id(999),
            ResponseResult::Events(batch.clone()),
        ))
        .unwrap()
        .len();
        assert!(
            wire_bytes <= 1_024,
            "event response exceeded configured frame"
        );
        for event in &batch.events {
            completed_events += usize::from(matches!(event.event, EventPayload::TurnCompleted(_)));
            failed_events += usize::from(matches!(event.event, EventPayload::TurnFailed(_)));
        }
        let next = batch.next_sequence.saturating_sub(1);
        assert!(
            next > after_sequence,
            "bounded event paging must make progress"
        );
        after_sequence = next;
    }
    assert_eq!(completed_events, 0);
    assert_eq!(failed_events, 1);

    let replay = registry.run_turn(request).await.unwrap();
    assert!(replay.replayed);
    assert_eq!(terminal.submissions.lock().unwrap().len(), 1);
    let replayed = registry
        .events(SubscribeEventsRequest {
            session_id,
            generation_id: generation(session_id),
            after_sequence: replay.next_sequence.saturating_sub(1),
            wait_ms: 0,
            max_events: 1,
        })
        .await
        .unwrap();
    assert!(matches!(
        replayed.events.as_slice(),
        [event] if matches!(event.event, EventPayload::TurnFailed(ref error)
            if error.code == ErrorCode::ResultTooLarge)
    ));
}

#[tokio::test]
async fn full_turn_history_rejects_new_ids_before_injection_and_keeps_old_idempotency() {
    let mut config = test_actor_config(64);
    config.turn_history_capacity = 1;
    let registry = registry_with_config(config);
    let session_id = id(930);
    let first_turn_id = id(931);
    let terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence {
            ready_prompt: true,
            quiet: true,
            ..TerminalEvidence::default()
        },
        InterruptRecovery::RecoveredToReady,
    ));
    register(
        &registry,
        session_id,
        terminal.clone(),
        Arc::new(ScriptedTranscript::with_rows(
            Vec::new(),
            simple_turn_rows("first", "done"),
        )),
    )
    .await;
    registry
        .run_turn(turn(session_id, first_turn_id, "first"))
        .await
        .unwrap();
    let _ = wait_for_terminal(&registry, session_id, first_turn_id).await;

    let capacity = registry
        .run_turn(turn(session_id, id(932), "second"))
        .await
        .unwrap_err();
    assert_eq!(capacity.code, ErrorCode::TurnHistoryCapacityExceeded);
    assert_eq!(capacity.details["maximum_turns"], 1);
    assert_eq!(terminal.submissions.lock().unwrap().len(), 1);

    let replay = registry
        .run_turn(turn(session_id, first_turn_id, "first"))
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(terminal.submissions.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn turn_history_byte_capacity_rejects_large_prompt_before_injection() {
    let mut config = test_actor_config(64);
    config.turn_history_byte_capacity = 4 * 1_024;
    let registry = registry_with_config(config);
    let session_id = id(940);
    let terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence::default(),
        InterruptRecovery::RecoveredToReady,
    ));
    register(
        &registry,
        session_id,
        terminal.clone(),
        Arc::new(ScriptedTranscript::pending()),
    )
    .await;

    let error = registry
        .run_turn(turn(session_id, id(941), &"p".repeat(8 * 1_024)))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::TurnHistoryCapacityExceeded);
    assert!(terminal.submissions.lock().unwrap().is_empty());
}

#[tokio::test]
async fn event_sequence_exhaustion_rejects_before_turn_mutation_and_preserves_close_reserve() {
    let mut config = test_actor_config(64);
    // Registration emits booting=1 and ready=2. Only sequences 3 and 4 remain,
    // exactly enough for the mandatory closing/closed lifecycle.
    config.event_sequence_ceiling = 4;
    let registry = registry_with_config(config);
    let session_id = id(950);
    let terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence::default(),
        InterruptRecovery::RecoveredToReady,
    ));
    register(
        &registry,
        session_id,
        terminal.clone(),
        Arc::new(ScriptedTranscript::pending()),
    )
    .await;

    let error = registry
        .run_turn(turn(session_id, id(951), "must not be injected"))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::RecoveryFailed);
    assert_eq!(error.details["resource"], "event_sequence");
    assert_eq!(error.details["next_sequence"], 3);
    assert_eq!(error.details["remaining_events"], 2);
    assert!(terminal.submissions.lock().unwrap().is_empty());
    assert!(
        registry
            .stored_turn(session_id, generation(session_id), id(951))
            .await
            .unwrap()
            .is_none()
    );
    let before_close = snapshot(&registry, session_id).await;
    assert_eq!(before_close.state, SessionState::Ready);
    assert_eq!(before_close.last_sequence, 2);

    let closed = registry
        .close(CloseSessionRequest {
            session_id,
            generation_id: generation(session_id),
            policy: ClosePolicy::Force,
        })
        .await
        .unwrap();
    assert!(closed.process_reaped);
    assert_eq!(terminal.closes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn direct_turn_deadline_domain_is_checked_before_turn_or_terminal_mutation() {
    let registry = registry(64);
    let session_id = id(9_520);
    let turn_id = id(9_521);
    let terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence::default(),
        InterruptRecovery::RecoveredToReady,
    ));
    register(
        &registry,
        session_id,
        terminal.clone(),
        Arc::new(ScriptedTranscript::pending()),
    )
    .await;
    let before = snapshot(&registry, session_id).await;

    for unsafe_deadline in [pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER + 1, u64::MAX] {
        let mut request = turn(session_id, turn_id, "deadline domain");
        request.turn.deadline_unix_ms = Some(unsafe_deadline);
        let error = registry.run_turn(request).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidConfig);
        serde_json::to_vec(&error).unwrap();
        assert_eq!(snapshot(&registry, session_id).await, before);
        assert!(
            registry
                .stored_turn(session_id, generation(session_id), turn_id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(terminal.submissions.lock().unwrap().is_empty());
    }

    let mut corrected = turn(session_id, turn_id, "deadline domain");
    corrected.turn.deadline_unix_ms = Some(pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER);
    let accepted = registry.run_turn(corrected).await.unwrap();
    assert!(!accepted.replayed);
    registry
        .close(CloseSessionRequest {
            session_id,
            generation_id: generation(session_id),
            policy: ClosePolicy::Force,
        })
        .await
        .unwrap();

    let mut config = test_actor_config(64);
    config.default_turn_timeout_ms = pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER + 1;
    let default_registry = registry_with_config(config);
    let default_session = id(9_522);
    let default_terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence::default(),
        InterruptRecovery::RecoveredToReady,
    ));
    register(
        &default_registry,
        default_session,
        default_terminal.clone(),
        Arc::new(ScriptedTranscript::pending()),
    )
    .await;
    let error = default_registry
        .run_turn(turn(default_session, id(9_523), "unsafe default"))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidConfig);
    assert!(default_terminal.submissions.lock().unwrap().is_empty());
    assert!(
        default_registry
            .stored_turn(default_session, generation(default_session), id(9_523))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn direct_registration_rejects_invalid_compatibility_and_idle_domains_before_publication() {
    let registry = registry(64);
    let mut next_id = 9_530_u128;
    for drain_ms in [
        0,
        60_001,
        pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER + 1,
        u64::MAX,
    ] {
        let session_id = id(next_id);
        next_id += 1;
        let error = registry
            .register(SessionRegistration {
                agent: None,
                owner: pseudomux_service::v1::SessionOwner::Caller,
                session_id,
                generation_id: generation(session_id),
                cwd: "/tmp/project".to_owned(),
                compatibility: compatibility_with_drain(drain_ms),
                dangerous_permission_bypass: false,
                resumable: true,
                cell: SessionCell::Full,
                idle_ttl_ms: None,
                initial_needs_input: None,
                terminal: Arc::new(FakeTerminal::new(
                    TerminalEvidence::default(),
                    InterruptRecovery::RecoveredToReady,
                )),
                transcript: Arc::new(ScriptedTranscript::pending()),
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidConfig);
        serde_json::to_vec(&error).unwrap();
        assert!(
            registry
                .inspect(InspectSessionRequest {
                    session_id,
                    generation_id: generation(session_id),
                })
                .await
                .is_err()
        );
    }

    for idle_ttl_ms in [pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER + 1, u64::MAX] {
        let session_id = id(next_id);
        next_id += 1;
        let error = registry
            .register(SessionRegistration {
                agent: None,
                owner: pseudomux_service::v1::SessionOwner::Caller,
                session_id,
                generation_id: generation(session_id),
                cwd: "/tmp/project".to_owned(),
                compatibility: compatibility_with_drain(1),
                dangerous_permission_bypass: false,
                resumable: true,
                cell: SessionCell::Full,
                idle_ttl_ms: Some(idle_ttl_ms),
                initial_needs_input: None,
                terminal: Arc::new(FakeTerminal::new(
                    TerminalEvidence::default(),
                    InterruptRecovery::RecoveredToReady,
                )),
                transcript: Arc::new(ScriptedTranscript::pending()),
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidConfig);
        serde_json::to_vec(&error).unwrap();
    }

    for drain_ms in [1, 60_000] {
        let session_id = id(next_id);
        next_id += 1;
        let handle = registry
            .register(SessionRegistration {
                agent: None,
                owner: pseudomux_service::v1::SessionOwner::Caller,
                session_id,
                generation_id: generation(session_id),
                cwd: "/tmp/project".to_owned(),
                compatibility: compatibility_with_drain(drain_ms),
                dangerous_permission_bypass: false,
                resumable: true,
                cell: SessionCell::Full,
                idle_ttl_ms: Some(1),
                initial_needs_input: None,
                terminal: Arc::new(FakeTerminal::new(
                    TerminalEvidence::default(),
                    InterruptRecovery::RecoveredToReady,
                )),
                transcript: Arc::new(ScriptedTranscript::pending()),
            })
            .await
            .unwrap();
        serde_json::to_vec(&handle).unwrap();
        registry
            .close(CloseSessionRequest {
                session_id,
                generation_id: generation(session_id),
                policy: ClosePolicy::Force,
            })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn close_strips_unsafe_backend_details_and_remains_retryable_and_serializable() {
    let registry = registry(64);
    let unsafe_values = [
        serde_json::json!(pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER + 1),
        serde_json::json!(-(pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER as i64) - 1),
    ];
    for (index, unsafe_value) in unsafe_values.into_iter().enumerate() {
        let session_id = id(9_550 + index as u128);
        let terminal = Arc::new(FakeTerminal::new(
            TerminalEvidence::default(),
            InterruptRecovery::RecoveredToReady,
        ));
        terminal.set_close_failure(
            DriverFailure::new(ErrorCode::RmuxUnavailable, "backend close failed")
                .retryable(true)
                .with_details(serde_json::json!({"nested": [unsafe_value]})),
        );
        register(
            &registry,
            session_id,
            terminal.clone(),
            Arc::new(ScriptedTranscript::pending()),
        )
        .await;

        let request = CloseSessionRequest {
            session_id,
            generation_id: generation(session_id),
            policy: ClosePolicy::Force,
        };
        let error = registry.close(request.clone()).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::RmuxUnavailable);
        assert_eq!(error.message, "backend close failed");
        assert!(error.retryable);
        assert!(error.details.is_null());
        serde_json::to_vec(&ResponseEnvelope::failure(id(9_559), error)).unwrap();

        terminal.clear_close_failure();
        let closed = registry.close(request).await.unwrap();
        assert!(closed.process_reaped);
        assert_eq!(terminal.closes.load(Ordering::SeqCst), 2);
    }
}

#[tokio::test]
async fn idle_deadline_safe_integer_boundary_is_inclusive_and_one_past_rejects_publication() {
    const IDLE_TTL_MS: u64 = 10;
    let mut config = test_actor_config(64);
    config.idle_ttl_ms = IDLE_TTL_MS;

    let boundary_clock = Arc::new(TestClock::starting_at(
        // Registration samples once for actor creation and once for each of
        // the booting/ready events. The ready event is the snapshot timestamp.
        pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER - IDLE_TTL_MS - 2,
    ));
    let boundary_registry = SessionRegistry::with_clock(config.clone(), boundary_clock.clone());
    let boundary_session = id(959);
    register(
        &boundary_registry,
        boundary_session,
        Arc::new(FakeTerminal::new(
            TerminalEvidence::default(),
            InterruptRecovery::RecoveredToReady,
        )),
        Arc::new(ScriptedTranscript::pending()),
    )
    .await;
    let boundary_snapshot = snapshot(&boundary_registry, boundary_session).await;
    assert_eq!(
        boundary_snapshot.idle_deadline_ms,
        Some(pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER)
    );
    boundary_clock.set(1_000_000);
    boundary_registry
        .close(CloseSessionRequest {
            session_id: boundary_session,
            generation_id: generation(boundary_session),
            policy: ClosePolicy::Force,
        })
        .await
        .unwrap();
    wait_for_actor_exit(&boundary_registry, boundary_session).await;

    let rejected_registry = SessionRegistry::with_clock(
        config,
        Arc::new(TestClock::starting_at(
            pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER - IDLE_TTL_MS + 1,
        )),
    );
    let rejected_session = id(9_591);
    let error = rejected_registry
        .register(SessionRegistration {
            agent: None,
            owner: pseudomux_service::v1::SessionOwner::Caller,
            session_id: rejected_session,
            generation_id: generation(rejected_session),
            cwd: "/tmp/project".to_owned(),
            compatibility: CompatibilityReport {
                claude_version: "2.1.207".to_owned(),
                os: std::env::consts::OS.to_owned(),
                arch: std::env::consts::ARCH.to_owned(),
                terminal_profile: TerminalProfile::Transparent,
                input_transport: InputTransport::Sdk,
                tested: true,
                transcript_drain_ms: 10,
            },
            dangerous_permission_bypass: false,
            resumable: true,
            cell: SessionCell::Full,
            idle_ttl_ms: None,
            initial_needs_input: None,
            terminal: Arc::new(FakeTerminal::new(
                TerminalEvidence::default(),
                InterruptRecovery::RecoveredToReady,
            )),
            transcript: Arc::new(ScriptedTranscript::pending()),
        })
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidConfig);
    assert_eq!(
        error.details["maximum"],
        pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER
    );
    serde_json::to_vec(&error).unwrap();
    assert!(
        rejected_registry
            .inspect(InspectSessionRequest {
                session_id: rejected_session,
                generation_id: generation(rejected_session),
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn out_of_domain_clock_rejects_registration_without_panicking_or_publishing_a_handle() {
    let registry = SessionRegistry::with_clock(
        test_actor_config(64),
        Arc::new(TestClock::starting_at(
            pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER + 1,
        )),
    );
    let session_id = id(960);
    let error = registry
        .register(SessionRegistration {
            agent: None,
            owner: pseudomux_service::v1::SessionOwner::Caller,
            session_id,
            generation_id: generation(session_id),
            cwd: "/tmp/project".to_owned(),
            compatibility: CompatibilityReport {
                claude_version: "2.1.207".to_owned(),
                os: std::env::consts::OS.to_owned(),
                arch: std::env::consts::ARCH.to_owned(),
                terminal_profile: TerminalProfile::Transparent,
                input_transport: InputTransport::Sdk,
                tested: true,
                transcript_drain_ms: 10,
            },
            dangerous_permission_bypass: false,
            resumable: true,
            cell: SessionCell::Full,
            idle_ttl_ms: None,
            initial_needs_input: None,
            terminal: Arc::new(FakeTerminal::new(
                TerminalEvidence::default(),
                InterruptRecovery::RecoveredToReady,
            )),
            transcript: Arc::new(ScriptedTranscript::pending()),
        })
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::RecoveryFailed);
    assert_eq!(error.details["resource"], "event_timestamp");
    assert!(
        registry
            .inspect(InspectSessionRequest {
                session_id,
                generation_id: generation(session_id),
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn out_of_domain_clock_rejects_attach_reservation_before_snapshot_mutation() {
    let clock = Arc::new(TestClock::starting_at(1_000_000));
    let registry = SessionRegistry::with_clock(test_actor_config(64), clock.clone());
    let session_id = id(961);
    register(
        &registry,
        session_id,
        Arc::new(FakeTerminal::new(
            TerminalEvidence::default(),
            InterruptRecovery::RecoveredToReady,
        )),
        Arc::new(ScriptedTranscript::pending()),
    )
    .await;
    let before = snapshot(&registry, session_id).await;
    let attach_id = id(962);

    clock.set(pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER + 1);
    let error = registry
        .reserve_writable_attach(session_id, generation(session_id), attach_id)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::RecoveryFailed);
    assert_eq!(error.details["resource"], "session_timestamp");
    assert_eq!(snapshot(&registry, session_id).await, before);

    // Restoring a representable clock lets the same reservation succeed,
    // proving the failed call did not leave an invisible attach reservation.
    clock.set(2_000_000);
    registry
        .reserve_writable_attach(session_id, generation(session_id), attach_id)
        .await
        .unwrap();
    let reserved = snapshot(&registry, session_id).await;
    clock.set(pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER + 1);
    let release_error = registry
        .release_writable_attach(
            session_id,
            generation(session_id),
            attach_id,
            WritableAttachCompletion::Unused,
        )
        .await
        .unwrap_err();
    assert_eq!(release_error.code, ErrorCode::RecoveryFailed);
    assert_eq!(release_error.details["resource"], "session_timestamp");
    assert_eq!(snapshot(&registry, session_id).await, reserved);

    clock.set(3_000_000);
    registry
        .release_writable_attach(
            session_id,
            generation(session_id),
            attach_id,
            WritableAttachCompletion::Unused,
        )
        .await
        .unwrap();
}

/// A turn whose terminal state cannot be published at commit time must be
/// poisoned like every sibling terminal path, never silently wedged.
///
/// Both completion commit paths clear `active` before publishing the
/// `Ready` transition. If that transition fails, dropping it on the floor
/// leaves the session in `Draining` with no stored terminal and no terminal
/// event: replaying the same `TurnId` answers `replayed: true` with nothing to
/// observe, and every new `TurnId` gets a *retryable* `SessionBusy` forever.
#[tokio::test(flavor = "current_thread")]
async fn commit_time_terminal_transition_failure_poisons_instead_of_wedging_the_session() {
    // Clock reads counted from the confirmation poll, which is the last actor
    // yield point before commit: 1-2 the worker's post-await and pre-result
    // deadline rechecks, 3 `turn_completed_at`, 4 the worker's final pre-send
    // recheck, 5 the actor's terminal sequence reservation, 6 the actor's
    // commit-time deadline recheck, 7 the `Ready` transition's event timestamp.
    // Read 7 is the commit-time failure under test; poisoning read 6 as well
    // sends the same failure through the timed-out sibling instead.
    let cells = [
        ("complete_active", vec![7_u64]),
        ("timeout_completed_at_commit", vec![6, 7]),
    ];
    for (index, (path, poisoned)) in cells.into_iter().enumerate() {
        let clock = Arc::new(CommitFaultClock::new(1_000_000, poisoned));
        let registry = SessionRegistry::with_clock(test_actor_config(64), clock.clone());
        let session_id = id(9_700 + index as u128);
        let turn_id = id(9_710 + index as u128);
        let terminal = Arc::new(FakeTerminal::new(
            TerminalEvidence {
                ready_prompt: true,
                quiet: true,
                ..TerminalEvidence::default()
            },
            InterruptRecovery::RecoveredToReady,
        ));
        register(
            &registry,
            session_id,
            terminal.clone(),
            Arc::new(CommitFaultTranscript {
                clock: clock.clone(),
                polls: AtomicU64::new(0),
            }),
        )
        .await;

        let request = turn(session_id, turn_id, "commit fault");
        assert!(!registry.run_turn(request.clone()).await.unwrap().replayed);

        // The caller learns the outcome instead of waiting forever. Without the
        // poison this never becomes terminal and the wait below panics.
        let StoredTurnTerminal::Failed(error) =
            wait_for_terminal(&registry, session_id, turn_id).await
        else {
            panic!("{path}: an unpublishable terminal state must store a failure")
        };
        assert_eq!(error.code, ErrorCode::RecoveryFailed, "{path}");
        assert_eq!(error.details["resource"], "event_timestamp", "{path}");
        assert_eq!(
            snapshot(&registry, session_id).await.state,
            SessionState::Tainted,
            "{path}: an unpublishable terminal state must taint the session"
        );

        // Replaying the same TurnId now republishes the terminal event.
        let replay = registry.run_turn(request).await.unwrap();
        assert!(replay.replayed, "{path}");
        let replayed = registry
            .events(SubscribeEventsRequest {
                session_id,
                generation_id: generation(session_id),
                after_sequence: replay.next_sequence.saturating_sub(1),
                wait_ms: 0,
                max_events: 1,
            })
            .await
            .unwrap();
        assert!(
            matches!(
                replayed.events.as_slice(),
                [event] if event.turn_id == Some(turn_id)
                    && matches!(event.event, EventPayload::TurnFailed(ref replayed_error)
                        if replayed_error.code == ErrorCode::RecoveryFailed)
            ),
            "{path}: replay must republish the terminal event"
        );

        // A fresh TurnId is now refused non-retryably instead of being told to
        // retry a session that can never leave `Draining`.
        let rejected = registry
            .run_turn(turn(session_id, id(9_720 + index as u128), "after poison"))
            .await
            .unwrap_err();
        assert_eq!(rejected.code, ErrorCode::RecoveryFailed, "{path}");
        assert!(!rejected.retryable, "{path}");
        assert_eq!(terminal.submissions.lock().unwrap().len(), 1, "{path}");

        registry
            .close(CloseSessionRequest {
                session_id,
                generation_id: generation(session_id),
                policy: ClosePolicy::Force,
            })
            .await
            .unwrap();
    }
}

struct CommitFaultClock {
    base: u64,
    reads: AtomicU64,
    armed: AtomicU8,
    armed_reads: AtomicU64,
    poisoned: Vec<u64>,
}

impl CommitFaultClock {
    fn new(base: u64, poisoned: Vec<u64>) -> Self {
        Self {
            base,
            reads: AtomicU64::new(0),
            armed: AtomicU8::new(0),
            armed_reads: AtomicU64::new(0),
            poisoned,
        }
    }

    fn arm(&self) {
        self.armed.store(1, Ordering::SeqCst);
    }
}

impl Clock for CommitFaultClock {
    fn now_ms(&self) -> u64 {
        let value = self.base + self.reads.fetch_add(1, Ordering::SeqCst);
        if self.armed.load(Ordering::SeqCst) == 1 {
            let armed_read = self.armed_reads.fetch_add(1, Ordering::SeqCst) + 1;
            if self.poisoned.contains(&armed_read) {
                return MAX_SAFE_JSON_INTEGER + 1;
            }
        }
        value
    }
}

struct CommitFaultTranscript {
    clock: Arc<CommitFaultClock>,
    polls: AtomicU64,
}

#[async_trait]
impl TranscriptSource for CommitFaultTranscript {
    async fn arm_at_eof(&self, _session_id: SessionId) -> DriverResult<TranscriptArm> {
        Ok(TranscriptArm::default())
    }

    async fn poll(
        &self,
        _session_id: SessionId,
        position: &TranscriptPosition,
    ) -> DriverResult<TranscriptBatch> {
        // Let the actor drain every update the worker already published so the
        // armed read counter only spans the commit path.
        tokio::time::sleep(Duration::from_millis(5)).await;
        let drain = TranscriptDrainEvidence {
            at_eof: true,
            has_partial_line: false,
            stable_for_ms: 100,
        };
        if self.polls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(TranscriptBatch {
                position: TranscriptPosition {
                    generation: 0,
                    offset: 2,
                },
                rows: simple_turn_rows("commit fault", "safe answer"),
                drain,
            });
        }
        self.clock.arm();
        Ok(TranscriptBatch {
            position: position.clone(),
            rows: Vec::new(),
            drain,
        })
    }
}

fn registry(replay_capacity: usize) -> SessionRegistry {
    registry_with_config(test_actor_config(replay_capacity))
}

fn registry_with_config(config: SessionActorConfig) -> SessionRegistry {
    SessionRegistry::with_clock(config, Arc::new(TestClock::starting_at(1_000_000)))
}

fn test_actor_config(replay_capacity: usize) -> SessionActorConfig {
    SessionActorConfig {
        replay_capacity,
        replay_byte_capacity: 16 * 1024 * 1024,
        default_event_batch_size: 128,
        poll_interval: Duration::from_millis(1),
        cancel_recovery_timeout: Duration::from_millis(100),
        attach_reconciliation_timeout: Duration::from_millis(25),
        default_turn_timeout_ms: 60_000,
        idle_ttl_ms: 30 * 60 * 1_000,
        turn_history_capacity: 128,
        turn_history_byte_capacity: 64 * 1024 * 1024,
        max_frame_bytes: pseudomux_protocol::v1::MAX_NATIVE_FRAME_BYTES,
        event_sequence_ceiling: pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER - 1,
    }
}

fn compatibility_with_drain(transcript_drain_ms: u64) -> CompatibilityReport {
    CompatibilityReport {
        claude_version: "2.1.207".to_owned(),
        os: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
        terminal_profile: TerminalProfile::Transparent,
        input_transport: InputTransport::Sdk,
        tested: true,
        transcript_drain_ms,
    }
}

async fn register(
    registry: &SessionRegistry,
    session_id: SessionId,
    terminal: Arc<FakeTerminal>,
    transcript: Arc<dyn TranscriptSource>,
) {
    let handle = register_with_initial(registry, session_id, terminal, transcript, None).await;
    assert_eq!(handle.state, SessionState::Ready);
}

async fn register_with_initial(
    registry: &SessionRegistry,
    session_id: SessionId,
    terminal: Arc<FakeTerminal>,
    transcript: Arc<dyn TranscriptSource>,
    initial_needs_input: Option<NeedsInput>,
) -> pseudomux_protocol::v1::SessionHandle {
    register_with_generation(
        registry,
        session_id,
        generation(session_id),
        terminal,
        transcript,
        initial_needs_input,
    )
    .await
}

async fn register_with_generation(
    registry: &SessionRegistry,
    session_id: SessionId,
    generation_id: SessionGenerationId,
    terminal: Arc<FakeTerminal>,
    transcript: Arc<dyn TranscriptSource>,
    initial_needs_input: Option<NeedsInput>,
) -> pseudomux_protocol::v1::SessionHandle {
    registry
        .register(SessionRegistration {
            agent: None,
            owner: pseudomux_service::v1::SessionOwner::Caller,
            session_id,
            generation_id,
            cwd: "/tmp/project".to_owned(),
            compatibility: CompatibilityReport {
                claude_version: "2.1.207".to_owned(),
                os: std::env::consts::OS.to_owned(),
                arch: std::env::consts::ARCH.to_owned(),
                terminal_profile: TerminalProfile::Transparent,
                input_transport: InputTransport::Sdk,
                tested: true,
                transcript_drain_ms: 10,
            },
            dangerous_permission_bypass: false,
            resumable: true,
            cell: SessionCell::Full,
            idle_ttl_ms: None,
            initial_needs_input,
            terminal,
            transcript,
        })
        .await
        .unwrap()
}

fn turn(session_id: SessionId, turn_id: TurnId, prompt: &str) -> RunTurnRequest {
    RunTurnRequest {
        session_id,
        generation_id: generation(session_id),
        turn: TurnRequest {
            turn_id,
            prompt: prompt.to_owned(),
            deadline_unix_ms: None,
            lease: Default::default(),
        },
    }
}

async fn snapshot(
    registry: &SessionRegistry,
    session_id: SessionId,
) -> pseudomux_protocol::v1::SessionSnapshot {
    registry
        .inspect(InspectSessionRequest {
            session_id,
            generation_id: generation(session_id),
        })
        .await
        .unwrap()
}

async fn wait_for_actor_exit(registry: &SessionRegistry, session_id: SessionId) {
    for _ in 0..500 {
        match registry
            .inspect(InspectSessionRequest {
                session_id,
                generation_id: generation(session_id),
            })
            .await
        {
            Err(error) if error.code == ErrorCode::DaemonLost => return,
            Err(error) => panic!("session {session_id} stopped with {:?}", error.code),
            Ok(_) => tokio::task::yield_now().await,
        }
    }
    panic!("session {session_id} actor did not terminate after confirmed close");
}

async fn wait_for_state(registry: &SessionRegistry, session_id: SessionId, expected: SessionState) {
    for _ in 0..500 {
        if snapshot(registry, session_id).await.state == expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("session {session_id} did not reach {expected:?}");
}

async fn wait_for_terminal(
    registry: &SessionRegistry,
    session_id: SessionId,
    turn_id: TurnId,
) -> StoredTurnTerminal {
    for _ in 0..500 {
        if let Some(terminal) = registry
            .stored_turn(session_id, generation(session_id), turn_id)
            .await
            .unwrap()
        {
            return terminal;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("turn {turn_id} did not become terminal");
}

async fn wait_for_event(
    registry: &SessionRegistry,
    session_id: SessionId,
    predicate: impl Fn(&EventPayload) -> bool,
) {
    for _ in 0..500 {
        let events = registry
            .events(SubscribeEventsRequest {
                session_id,
                generation_id: generation(session_id),
                after_sequence: 0,
                wait_ms: 0,
                max_events: 128,
            })
            .await
            .unwrap();
        if events.events.iter().any(|event| predicate(&event.event)) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("session {session_id} did not emit the expected event");
}

fn needs_input(kind: NeedsInputKind) -> NeedsInput {
    NeedsInput {
        kind,
        message: format!("{kind:?} input required"),
        details: serde_json::Value::Null,
    }
}

fn generation(session_id: SessionId) -> SessionGenerationId {
    SessionGenerationId::from_u128(session_id.as_u128() ^ (1_u128 << 127))
}

fn fixture_rows(relative: &str) -> Vec<ParsedRow> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(relative);
    let content = std::fs::read(path).unwrap();
    let parser = JsonlParser::new(ParseMode::Strict);
    content
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .enumerate()
        .map(|(index, bytes)| {
            parser
                .parse(&CompleteLine {
                    location: SourceLocation {
                        line: index as u64 + 1,
                        byte_offset: index as u64 * 1_000,
                    },
                    bytes: bytes.to_vec(),
                })
                .unwrap()
        })
        .collect()
}

fn simple_turn_rows(prompt: &str, answer: &str) -> Vec<ParsedRow> {
    let user = format!(
        r#"{{"parentUuid":null,"sessionId":"test","type":"user","message":{{"content":{prompt:?}}},"uuid":"prompt-row","promptSource":"typed","promptId":"prompt-id"}}"#
    );
    let assistant = format!(
        r#"{{"parentUuid":"prompt-row","sessionId":"test","type":"assistant","uuid":"answer-row","message":{{"id":"answer-message","model":"claude-test","content":[{{"type":"text","text":{answer:?}}}],"stop_reason":"end_turn","usage":{{"input_tokens":3,"output_tokens":2}}}}}}"#
    );
    [user, assistant]
        .into_iter()
        .enumerate()
        .map(|(index, json)| parse_line(index, json.as_bytes()))
        .collect()
}

fn unsafe_tool_input_rows(unsafe_number: &str) -> Vec<ParsedRow> {
    opaque_rows([
        typed_prompt_row(),
        format!(
            r#"{{"parentUuid":"prompt-row","sessionId":"test","type":"assistant","uuid":"tool-row","message":{{"id":"tool-message","model":"claude-test","content":[{{"type":"tool_use","id":"tool-1","name":"Read","input":{{"nested":[{unsafe_number}]}}}}],"stop_reason":"tool_use","usage":{{"input_tokens":1,"output_tokens":1}}}}}}"#
        ),
    ])
}

fn unsafe_tool_result_rows(unsafe_number: &str) -> Vec<ParsedRow> {
    opaque_rows([
        typed_prompt_row(),
        r#"{"parentUuid":"prompt-row","sessionId":"test","type":"assistant","uuid":"tool-row","message":{"id":"tool-message","model":"claude-test","content":[{"type":"tool_use","id":"tool-1","name":"Read","input":{"path":"README.md"}}],"stop_reason":"tool_use","usage":{"input_tokens":1,"output_tokens":1}}}"#.to_owned(),
        format!(
            r#"{{"parentUuid":"tool-row","sessionId":"test","type":"user","uuid":"result-row","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"tool-1","content":{{"nested":[{unsafe_number}]}}}}]}}}}"#
        ),
        terminal_answer_row("result-row"),
    ])
}

fn unsafe_unknown_content_rows(unsafe_number: &str) -> Vec<ParsedRow> {
    opaque_rows([
        typed_prompt_row(),
        format!(
            r#"{{"parentUuid":"prompt-row","sessionId":"test","type":"assistant","uuid":"answer-row","message":{{"id":"answer-message","model":"claude-test","content":[{{"type":"future-block","payload":{{"nested":[{unsafe_number}]}}}},{{"type":"text","text":"safe answer"}}],"stop_reason":"end_turn","usage":{{"input_tokens":1,"output_tokens":1}}}}}}"#
        ),
    ])
}

fn unknown_off_branch_rows(unsafe_number: &str) -> Vec<ParsedRow> {
    opaque_rows([
        typed_prompt_row(),
        format!(
            r#"{{"parentUuid":"unrelated","sessionId":"test","type":"future-event","uuid":"unknown-row","payload":{{"nested":[{unsafe_number}]}}}}"#
        ),
        terminal_answer_row("prompt-row"),
    ])
}

fn typed_prompt_row() -> String {
    r#"{"parentUuid":null,"sessionId":"test","type":"user","message":{"role":"user","content":"opaque"},"uuid":"prompt-row","promptSource":"typed","promptId":"prompt-id"}"#.to_owned()
}

fn terminal_answer_row(parent_uuid: &str) -> String {
    format!(
        r#"{{"parentUuid":{parent_uuid:?},"sessionId":"test","type":"assistant","uuid":"answer-row","message":{{"id":"answer-message","model":"claude-test","content":[{{"type":"text","text":"safe answer"}}],"stop_reason":"end_turn","usage":{{"input_tokens":1,"output_tokens":1}}}}}}"#
    )
}

fn opaque_rows<const N: usize>(rows: [String; N]) -> Vec<ParsedRow> {
    rows.into_iter()
        .enumerate()
        .map(|(index, row)| parse_line(index, row.as_bytes()))
        .collect()
}

fn parse_line(index: usize, bytes: &[u8]) -> ParsedRow {
    JsonlParser::new(ParseMode::Strict)
        .parse(&CompleteLine {
            location: SourceLocation {
                line: index as u64 + 1,
                byte_offset: index as u64 * 100,
            },
            bytes: bytes.to_vec(),
        })
        .unwrap()
}

const fn id(value: u128) -> uuid::Uuid {
    uuid::Uuid::from_u128(value)
}

// ---- transcript-drain calibration -------------------------------------------
//
// `transcript_drain_ms` is the largest single term in a real turn's latency, so
// choosing it is the one calibration worth doing. The number that supports the
// choice is *not* how long pmux waited — `completed_at_ms -
// terminal_candidate_at_ms` is the configured drain plus slack by construction,
// whatever Claude did — but how much later than the terminal candidate the
// transcript actually last changed. The two tests below pin that number at both
// ends of its range: exactly zero when the candidate row is the last row, and
// the true arrival offset when a row lands inside the drain window.

/// A simulated wall clock shared by the actor and the transcript source so the
/// published timestamps and the reported `stable_for_ms` come from one
/// timeline. It advances only where the transcript script says time passed, so
/// nothing else in the turn can perturb the arithmetic under test.
struct DrainTimelineClock(AtomicU64);

impl DrainTimelineClock {
    fn new(start_ms: u64) -> Self {
        Self(AtomicU64::new(start_ms))
    }

    fn advance(&self, ms: u64) -> u64 {
        self.0.fetch_add(ms, Ordering::SeqCst) + ms
    }
}

impl Clock for DrainTimelineClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// A transcript source with scripted arrival times.
///
/// Every poll advances the shared clock by `step_ms`; rows are appended at the
/// poll indices named in `arrivals`; and `stable_for_ms` is reported exactly as
/// the production source reports it — elapsed time since the last append
/// (`driver_io.rs` `TailState::last_change`). A mid-drain arrival is therefore
/// expressible: the drain evidence collapses to zero at the arrival and climbs
/// again, which is the case the whole calibration exists for.
struct DrainTimelineTranscript {
    clock: Arc<DrainTimelineClock>,
    step_ms: u64,
    arrivals: Mutex<VecDeque<(u64, Vec<ParsedRow>)>>,
    polls: AtomicU64,
    state: Mutex<DrainTimelineState>,
}

struct DrainTimelineState {
    offset: u64,
    last_append_at_ms: u64,
    appended_at_ms: Vec<u64>,
}

impl DrainTimelineTranscript {
    fn new(
        clock: Arc<DrainTimelineClock>,
        step_ms: u64,
        arrivals: impl IntoIterator<Item = (u64, Vec<ParsedRow>)>,
    ) -> Self {
        let start_ms = clock.now_ms();
        Self {
            clock,
            step_ms,
            arrivals: Mutex::new(arrivals.into_iter().collect()),
            polls: AtomicU64::new(0),
            state: Mutex::new(DrainTimelineState {
                offset: 0,
                last_append_at_ms: start_ms,
                appended_at_ms: Vec::new(),
            }),
        }
    }

    /// The simulated instants at which this source actually appended rows,
    /// recorded independently of anything the actor publishes.
    fn appended_at_ms(&self) -> Vec<u64> {
        self.state.lock().unwrap().appended_at_ms.clone()
    }
}

#[async_trait]
impl TranscriptSource for DrainTimelineTranscript {
    async fn arm_at_eof(&self, _session_id: SessionId) -> DriverResult<TranscriptArm> {
        Ok(TranscriptArm::default())
    }

    async fn poll(
        &self,
        _session_id: SessionId,
        _position: &TranscriptPosition,
    ) -> DriverResult<TranscriptBatch> {
        let index = self.polls.fetch_add(1, Ordering::SeqCst);
        let now_ms = self.clock.advance(self.step_ms);
        let mut state = self.state.lock().unwrap();
        let mut arrivals = self.arrivals.lock().unwrap();
        let rows = match arrivals.front() {
            Some((arrival, _)) if *arrival == index => {
                let (_, rows) = arrivals.pop_front().expect("the front was just observed");
                state.offset += rows.len() as u64;
                state.last_append_at_ms = now_ms;
                state.appended_at_ms.push(now_ms);
                rows
            }
            _ => Vec::new(),
        };
        Ok(TranscriptBatch {
            position: TranscriptPosition {
                generation: 0,
                offset: state.offset,
            },
            rows,
            drain: TranscriptDrainEvidence {
                at_eof: true,
                has_partial_line: false,
                stable_for_ms: now_ms - state.last_append_at_ms,
            },
        })
    }
}

async fn register_with_drain(
    registry: &SessionRegistry,
    session_id: SessionId,
    terminal: Arc<FakeTerminal>,
    transcript: Arc<dyn TranscriptSource>,
    transcript_drain_ms: u64,
) {
    let handle = registry
        .register(SessionRegistration {
            agent: None,
            owner: pseudomux_service::v1::SessionOwner::Caller,
            session_id,
            generation_id: generation(session_id),
            cwd: "/tmp/project".to_owned(),
            compatibility: compatibility_with_drain(transcript_drain_ms),
            dangerous_permission_bypass: false,
            resumable: true,
            cell: SessionCell::Full,
            idle_ttl_ms: None,
            initial_needs_input: None,
            terminal,
            transcript,
        })
        .await
        .unwrap();
    assert_eq!(handle.state, SessionState::Ready);
}

fn ready_and_quiet_terminal() -> Arc<FakeTerminal> {
    Arc::new(FakeTerminal::new(
        TerminalEvidence {
            ready_prompt: true,
            quiet: true,
            ..TerminalEvidence::default()
        },
        InterruptRecovery::RecoveredToReady,
    ))
}

const DRAIN_TIMELINE_STEP_MS: u64 = 10;
const DRAIN_TIMELINE_REQUIRED_MS: u64 = 40;
const DRAIN_TIMELINE_START_MS: u64 = 1_000_000;

#[tokio::test]
async fn a_transcript_quiet_at_the_candidate_publishes_a_zero_late_arrival_gap() {
    let clock = Arc::new(DrainTimelineClock::new(DRAIN_TIMELINE_START_MS));
    let registry = SessionRegistry::with_clock(test_actor_config(64), clock.clone());
    let session_id = id(910);
    let turn_id = id(9101);
    let transcript = Arc::new(DrainTimelineTranscript::new(
        clock,
        DRAIN_TIMELINE_STEP_MS,
        [(0, simple_turn_rows("quiet", "answer"))],
    ));
    register_with_drain(
        &registry,
        session_id,
        ready_and_quiet_terminal(),
        transcript.clone(),
        DRAIN_TIMELINE_REQUIRED_MS,
    )
    .await;
    registry
        .run_turn(turn(session_id, turn_id, "quiet"))
        .await
        .unwrap();

    let StoredTurnTerminal::Result(result) =
        wait_for_terminal(&registry, session_id, turn_id).await
    else {
        panic!("expected a result")
    };
    let timings = &result.timings;
    let candidate_at_ms = timings
        .terminal_candidate_at_ms
        .expect("terminal candidate");
    let last_activity_at_ms = timings
        .last_transcript_activity_at_ms
        .expect("last transcript activity");
    let drain_ms = timings.drain_ms.expect("drain evidence");

    // Provenance: the published anchor is the instant the source actually
    // appended, which the fake recorded on its own, not a reconstruction.
    assert_eq!(transcript.appended_at_ms(), vec![candidate_at_ms]);
    assert_eq!(last_activity_at_ms, transcript.appended_at_ms()[0]);
    assert_eq!(last_activity_at_ms, timings.completed_at_ms - drain_ms);

    // The calibration number. Nothing followed the terminal row, so the drain
    // window did no work: every millisecond of it was margin.
    assert_eq!(
        i128::from(last_activity_at_ms) - i128::from(candidate_at_ms),
        0,
        "a transcript that goes quiet at the candidate has no late arrival"
    );

    // And here is why the anchor had to be published. With no late rows the
    // wait and the stability duration are the same number, so `drain_ms` alone
    // is indistinguishable from `completed_at_ms - terminal_candidate_at_ms`
    // and cannot tell a caller which of the two it is looking at.
    assert!(drain_ms >= DRAIN_TIMELINE_REQUIRED_MS);
    assert_eq!(timings.completed_at_ms - candidate_at_ms, drain_ms);
}

#[tokio::test]
async fn a_row_arriving_inside_the_drain_window_moves_the_published_last_activity() {
    const LATE_ARRIVAL_POLL: u64 = 3;

    let clock = Arc::new(DrainTimelineClock::new(DRAIN_TIMELINE_START_MS));
    let registry = SessionRegistry::with_clock(test_actor_config(64), clock.clone());
    let session_id = id(911);
    let turn_id = id(9111);
    let late_row = parse_line(
        2,
        br#"{"parentUuid":"answer-row","sessionId":"test","type":"assistant","uuid":"late-row","message":{"id":"late-message","model":"claude-test","content":[{"type":"text","text":"one more block"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}}"#,
    );
    let transcript = Arc::new(DrainTimelineTranscript::new(
        clock,
        DRAIN_TIMELINE_STEP_MS,
        [
            (0, simple_turn_rows("late", "premature answer")),
            (LATE_ARRIVAL_POLL, vec![late_row]),
        ],
    ));
    register_with_drain(
        &registry,
        session_id,
        ready_and_quiet_terminal(),
        transcript.clone(),
        DRAIN_TIMELINE_REQUIRED_MS,
    )
    .await;
    registry
        .run_turn(turn(session_id, turn_id, "late"))
        .await
        .unwrap();

    let StoredTurnTerminal::Result(result) =
        wait_for_terminal(&registry, session_id, turn_id).await
    else {
        panic!("expected a result")
    };
    let timings = &result.timings;
    let candidate_at_ms = timings
        .terminal_candidate_at_ms
        .expect("terminal candidate");
    let last_activity_at_ms = timings
        .last_transcript_activity_at_ms
        .expect("last transcript activity");
    let drain_ms = timings.drain_ms.expect("drain evidence");

    let appended_at_ms = transcript.appended_at_ms();
    assert_eq!(
        appended_at_ms.len(),
        2,
        "the late row must have been served"
    );
    assert_eq!(candidate_at_ms, appended_at_ms[0]);
    assert_eq!(last_activity_at_ms, appended_at_ms[1]);
    assert_eq!(last_activity_at_ms, timings.completed_at_ms - drain_ms);

    // The calibration number, and it is the arrival offset of the late row
    // rather than anything about how long pmux chose to wait.
    let late_by_ms = last_activity_at_ms - candidate_at_ms;
    assert_eq!(late_by_ms, LATE_ARRIVAL_POLL * DRAIN_TIMELINE_STEP_MS);

    // The wait is strictly larger, because the arrival restarted the drain.
    // Reading `completed_at_ms - terminal_candidate_at_ms` as "how long Claude
    // needed" would have overstated it by the whole drain window.
    let waited_ms = timings.completed_at_ms - candidate_at_ms;
    assert!(waited_ms > late_by_ms);
    assert_eq!(waited_ms, late_by_ms + drain_ms);
}

// ---- turn_duration arrival order --------------------------------------------
//
// The drain is ~98% of pmux's own per-turn cost and has never been shown
// necessary. `turn_duration` is the candidate end-of-stream marker that could
// retire most of it, but the 82-turn survey that found it reads the timestamps
// Claude *wrote into the file*; pmux can only act on when bytes reach its
// reader. The two tests below drive the worker over a simulated timeline and
// pin the published pair against arrival instants the fake transcript recorded
// on its own: the marker's observation instant, and whether anything the
// analysis reads arrived on a strictly later read.
//
// Neither test asserts anything about completion, because nothing about
// completion changed. The measurement is observational: it installs nothing,
// writes nothing, and reads the clock only when it has a fact to stamp.

/// `simple_turn_rows` plus the marker Claude actually appends at the end of a
/// turn, parented onto the answer as the real row is.
fn turn_rows_with_marker(prompt: &str, answer: &str) -> Vec<ParsedRow> {
    let mut rows = simple_turn_rows(prompt, answer);
    rows.push(parse_line(
        2,
        br#"{"parentUuid":"answer-row","isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":4327,"messageCount":7,"timestamp":"2026-07-30T01:28:04.415Z","uuid":"turn-duration-row","sessionId":"test","version":"2.1.220"}"#,
    ));
    rows
}

#[tokio::test]
async fn a_marker_that_ends_the_transcript_publishes_its_observation_and_no_late_row() {
    let clock = Arc::new(DrainTimelineClock::new(DRAIN_TIMELINE_START_MS));
    let registry = SessionRegistry::with_clock(test_actor_config(64), clock.clone());
    let session_id = id(912);
    let turn_id = id(9121);
    let transcript = Arc::new(DrainTimelineTranscript::new(
        clock,
        DRAIN_TIMELINE_STEP_MS,
        [(0, turn_rows_with_marker("marked", "answer"))],
    ));
    register_with_drain(
        &registry,
        session_id,
        ready_and_quiet_terminal(),
        transcript.clone(),
        DRAIN_TIMELINE_REQUIRED_MS,
    )
    .await;
    registry
        .run_turn(turn(session_id, turn_id, "marked"))
        .await
        .unwrap();

    let StoredTurnTerminal::Result(result) =
        wait_for_terminal(&registry, session_id, turn_id).await
    else {
        panic!("expected a result")
    };
    let timings = &result.timings;
    let observed_at_ms = timings
        .turn_duration_observed_at_ms
        .expect("the marker was delivered, so its observation must be published");

    // Provenance: the published instant is the read that delivered the marker,
    // checked against the instant the fake source recorded for itself.
    assert_eq!(transcript.appended_at_ms(), vec![observed_at_ms]);

    // The whole point. Nothing the analysis reads arrived after the marker, so
    // completing at the marker would have published this same result sooner --
    // and the field says so by being absent rather than by carrying a zero.
    assert_eq!(timings.post_turn_duration_row_observed_at_ms, None);

    // And the drain still ran and still decided the turn: this is measurement
    // beside the existing gate, not a second authority.
    assert!(timings.drain_ms.expect("drain evidence") >= DRAIN_TIMELINE_REQUIRED_MS);
    assert_eq!(
        timings.completed_at_ms - observed_at_ms,
        timings.drain_ms.unwrap()
    );
    assert_eq!(result.outcome, TurnOutcome::Completed);
}

#[tokio::test]
async fn a_row_arriving_after_the_marker_is_published_as_the_late_observation() {
    const LATE_ARRIVAL_POLL: u64 = 3;

    let clock = Arc::new(DrainTimelineClock::new(DRAIN_TIMELINE_START_MS));
    let registry = SessionRegistry::with_clock(test_actor_config(64), clock.clone());
    let session_id = id(913);
    let turn_id = id(9131);
    // A sidechain assistant fragment: the shape a genuine post-marker row can
    // take. A semantic *main-chain* row after the marker is schema drift and
    // fails the turn, so it could never be observed on a committed result -- but
    // this one is legal there and the analysis reads it, moving sidechain and
    // combined usage.
    let late_row = parse_line(
        3,
        br#"{"parentUuid":"prompt-row","isSidechain":true,"sessionId":"test","type":"assistant","uuid":"sidechain-row","message":{"id":"sidechain-message","model":"claude-test","content":[{"type":"text","text":"sub-agent"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":7}}}"#,
    );
    let transcript = Arc::new(DrainTimelineTranscript::new(
        clock,
        DRAIN_TIMELINE_STEP_MS,
        [
            (0, turn_rows_with_marker("late", "answer")),
            (LATE_ARRIVAL_POLL, vec![late_row]),
        ],
    ));
    register_with_drain(
        &registry,
        session_id,
        ready_and_quiet_terminal(),
        transcript.clone(),
        DRAIN_TIMELINE_REQUIRED_MS,
    )
    .await;
    registry
        .run_turn(turn(session_id, turn_id, "late"))
        .await
        .unwrap();

    let StoredTurnTerminal::Result(result) =
        wait_for_terminal(&registry, session_id, turn_id).await
    else {
        panic!("expected a result")
    };
    let timings = &result.timings;
    let appended_at_ms = transcript.appended_at_ms();
    assert_eq!(
        appended_at_ms.len(),
        2,
        "the late row must have been served"
    );

    assert_eq!(
        timings.turn_duration_observed_at_ms,
        Some(appended_at_ms[0]),
        "the marker's observation is still the read that carried it"
    );
    assert_eq!(
        timings.post_turn_duration_row_observed_at_ms,
        Some(appended_at_ms[1]),
        "and the late row's observation is the strictly later read that carried it"
    );

    // The reportable quantity, and it is a real gap rather than a rounding
    // artifact: completing at the marker would have dropped this row.
    let late_by_ms = i128::from(appended_at_ms[1]) - i128::from(appended_at_ms[0]);
    assert_eq!(
        late_by_ms,
        i128::from(LATE_ARRIVAL_POLL * DRAIN_TIMELINE_STEP_MS)
    );

    // Proof that the dropped row was load-bearing: the committed result counts
    // it. A fast path that completed at the marker would have published 2.
    assert_eq!(result.usage.combined.output_tokens, 9);
    assert_eq!(result.usage.main.output_tokens, 2);
    assert_eq!(result.outcome, TurnOutcome::Completed);
}

// ---- the graduated drain ----------------------------------------------------
//
// The drain requirement is now `min(configured, floor)` once `turn_duration` is
// on the active chain, and the configured drain unchanged when it is not. These
// tests drive the worker over the same simulated timeline as the arrival-order
// tests above, with a configured drain of the size a real session carries, and
// pin what the turn actually paid.

/// Simulated milliseconds per transcript poll. Chosen so the floor lands on a
/// poll boundary: the gate is first satisfied by the poll whose reported
/// stability is exactly [`GRADUATED_REQUIRED_MS`].
const GRADUATED_STEP_MS: u64 = 50;
/// A configured drain of the size real sessions carry, so the graduated value
/// and the configured value cannot be confused for each other.
const GRADUATED_CONFIGURED_DRAIN_MS: u64 = 2_000;

/// What a marked turn on this timeline actually owes, asked of the production
/// function rather than written down here.
///
/// It is [`TURN_DURATION_DRAIN_FLOOR_MS`] whenever the floor is the smaller of
/// the two, which is the case under test. Deriving it means raising the floor --
/// strictly the safer direction -- moves every number below with it, so these
/// tests can only fail for a floor that has been *lowered* past what their
/// assertions require.
const GRADUATED_REQUIRED_MS: u64 = graduated_drain_ms(GRADUATED_CONFIGURED_DRAIN_MS, true);

/// The graduated path's premise, checked at compile time because both sides are
/// constants: a floor at or above the configured drain is not a graduated drain
/// at all, and every test below would be pinning a path that no longer exists.
const _: () = assert!(TURN_DURATION_DRAIN_FLOOR_MS < GRADUATED_CONFIGURED_DRAIN_MS);

/// Polls the gate needs before it is satisfied, plus the confirming re-poll.
const fn graduated_polls(required_stable_ms: u64, step_ms: u64) -> u64 {
    // Rows land on poll 0, so the poll at index k reports `k * step` of
    // stability; add one for that poll-0 append and one for the re-poll.
    required_stable_ms.div_ceil(step_ms) + 2
}

#[tokio::test]
async fn a_marked_turn_commits_at_the_graduated_floor_instead_of_the_configured_drain() {
    let clock = Arc::new(DrainTimelineClock::new(DRAIN_TIMELINE_START_MS));
    let registry = SessionRegistry::with_clock(test_actor_config(64), clock.clone());
    let session_id = id(914);
    let turn_id = id(9141);
    let transcript = Arc::new(DrainTimelineTranscript::new(
        clock,
        GRADUATED_STEP_MS,
        [(0, turn_rows_with_marker("graduated", "answer"))],
    ));
    register_with_drain(
        &registry,
        session_id,
        ready_and_quiet_terminal(),
        transcript.clone(),
        GRADUATED_CONFIGURED_DRAIN_MS,
    )
    .await;
    registry
        .run_turn(turn(session_id, turn_id, "graduated"))
        .await
        .unwrap();

    let StoredTurnTerminal::Result(result) =
        wait_for_terminal(&registry, session_id, turn_id).await
    else {
        panic!("expected a result")
    };
    let timings = &result.timings;
    let drain_ms = timings.drain_ms.expect("drain evidence");
    let candidate_at_ms = timings
        .terminal_candidate_at_ms
        .expect("terminal candidate");

    // The win. The turn owed the floor plus the one step the confirming re-poll
    // costs, not the configured 2000ms.
    assert_eq!(drain_ms, GRADUATED_REQUIRED_MS + GRADUATED_STEP_MS);
    assert!(drain_ms < GRADUATED_CONFIGURED_DRAIN_MS);
    assert_eq!(timings.completed_at_ms - candidate_at_ms, drain_ms);

    // And it is the same turn: the marker shortened the wait, it did not become
    // the authority. The screen was still ready+quiet, the transcript still
    // decided the content, and the answer is whole.
    assert_eq!(result.outcome, TurnOutcome::Completed);
    assert_eq!(result.completion.authority, CompletionAuthority::Transcript);
    assert!(result.completion.transcript_drained);
    assert!(result.completion.terminal_prompt_observed);
    assert!(result.completion.terminal_quiet_observed);
    assert_eq!(result.text, "answer");
    assert_eq!(
        timings.turn_duration_observed_at_ms,
        Some(transcript.appended_at_ms()[0])
    );
    assert_eq!(timings.post_turn_duration_row_observed_at_ms, None);
}

#[tokio::test]
async fn an_unmarked_turn_still_owes_the_full_configured_drain() {
    let clock = Arc::new(DrainTimelineClock::new(DRAIN_TIMELINE_START_MS));
    let registry = SessionRegistry::with_clock(test_actor_config(64), clock.clone());
    let session_id = id(915);
    let turn_id = id(9151);
    // Older Claude builds write no `turn_duration` row at all, and on builds
    // that do it is still absent from some turns. Nothing about those turns is
    // allowed to get faster: without the marker there is no in-band evidence
    // that the turn ended, so the configured drain is the whole proof.
    let transcript = Arc::new(DrainTimelineTranscript::new(
        clock,
        GRADUATED_STEP_MS,
        [(0, simple_turn_rows("unmarked", "answer"))],
    ));
    register_with_drain(
        &registry,
        session_id,
        ready_and_quiet_terminal(),
        transcript.clone(),
        GRADUATED_CONFIGURED_DRAIN_MS,
    )
    .await;
    registry
        .run_turn(turn(session_id, turn_id, "unmarked"))
        .await
        .unwrap();

    let StoredTurnTerminal::Result(result) =
        wait_for_terminal(&registry, session_id, turn_id).await
    else {
        panic!("expected a result")
    };
    let timings = &result.timings;
    let drain_ms = timings.drain_ms.expect("drain evidence");

    assert_eq!(timings.turn_duration_observed_at_ms, None);
    assert!(
        drain_ms >= GRADUATED_CONFIGURED_DRAIN_MS,
        "an unmarked turn must still pay the configured drain, paid {drain_ms}"
    );
    assert_eq!(
        drain_ms,
        GRADUATED_CONFIGURED_DRAIN_MS + GRADUATED_STEP_MS,
        "and no more than the configured drain plus the confirming re-poll"
    );
    assert_eq!(result.outcome, TurnOutcome::Completed);
}

#[tokio::test]
async fn the_confirming_repoll_honours_the_graduated_value_rather_than_the_configured_drain() {
    let clock = Arc::new(DrainTimelineClock::new(DRAIN_TIMELINE_START_MS));
    let registry = SessionRegistry::with_clock(test_actor_config(64), clock.clone());
    let session_id = id(916);
    let turn_id = id(9161);
    let transcript = Arc::new(DrainTimelineTranscript::new(
        clock,
        GRADUATED_STEP_MS,
        [(0, turn_rows_with_marker("repoll", "answer"))],
    ));
    register_with_drain(
        &registry,
        session_id,
        ready_and_quiet_terminal(),
        transcript.clone(),
        GRADUATED_CONFIGURED_DRAIN_MS,
    )
    .await;
    registry
        .run_turn(turn(session_id, turn_id, "repoll"))
        .await
        .unwrap();

    let StoredTurnTerminal::Result(result) =
        wait_for_terminal(&registry, session_id, turn_id).await
    else {
        panic!("expected a result")
    };

    // The drain is evaluated twice: at the gate, and again on the confirming
    // re-poll that exists so a row appended during the terminal-evidence window
    // cannot be omitted. A re-poll left on the configured value would reject the
    // very evidence the gate just accepted and `continue`, so the turn would
    // grind on to the full 2000ms and this count would be the counterfactual
    // below. Graduating only the gate saves nothing, and this is how that shows.
    let polls = transcript.polls.load(Ordering::SeqCst);
    assert_eq!(
        polls,
        graduated_polls(GRADUATED_REQUIRED_MS, GRADUATED_STEP_MS)
    );
    assert!(
        polls < graduated_polls(GRADUATED_CONFIGURED_DRAIN_MS, GRADUATED_STEP_MS),
        "the configured drain would have taken {} polls",
        graduated_polls(GRADUATED_CONFIGURED_DRAIN_MS, GRADUATED_STEP_MS)
    );

    // The published number is the confirming re-poll's own evidence, carried
    // verbatim by the actor, so this asserts which read was accepted and not
    // merely how long the turn took.
    assert_eq!(
        result.timings.drain_ms,
        Some(GRADUATED_REQUIRED_MS + GRADUATED_STEP_MS)
    );
}

#[tokio::test]
async fn a_semantic_row_inside_the_graduated_window_fails_the_turn_closed() {
    // Arrival at 3 * 50ms = 150ms after the marker: inside the 250ms floor, and
    // inside the empirically empty band the floor was chosen to cover. The floor
    // is the only reason pmux is still polling when this row lands.
    const LATE_ARRIVAL_POLL: u64 = 3;

    let clock = Arc::new(DrainTimelineClock::new(DRAIN_TIMELINE_START_MS));
    let registry = SessionRegistry::with_clock(test_actor_config(64), clock.clone());
    let session_id = id(917);
    let turn_id = id(9171);
    // A main-chain assistant row parented onto the marker: Claude continuing to
    // speak after announcing the turn was over. It is the shape that decides
    // whether the graduated drain is safe, because committing before it lands
    // publishes a truncated answer -- returning before the work is done, which
    // is the one outcome pmux does not accept.
    let late_row = parse_line(
        3,
        br#"{"parentUuid":"turn-duration-row","sessionId":"test","type":"assistant","uuid":"late-row","message":{"id":"late-message","model":"claude-test","content":[{"type":"text","text":"actually, one more thing"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":5}}}"#,
    );
    let transcript = Arc::new(DrainTimelineTranscript::new(
        clock,
        GRADUATED_STEP_MS,
        [
            (0, turn_rows_with_marker("fail closed", "premature answer")),
            (LATE_ARRIVAL_POLL, vec![late_row]),
        ],
    ));
    register_with_drain(
        &registry,
        session_id,
        ready_and_quiet_terminal(),
        transcript.clone(),
        GRADUATED_CONFIGURED_DRAIN_MS,
    )
    .await;
    registry
        .run_turn(turn(session_id, turn_id, "fail closed"))
        .await
        .unwrap();

    let terminal = wait_for_terminal(&registry, session_id, turn_id).await;
    let StoredTurnTerminal::Failed(error) = terminal else {
        panic!(
            "a row arriving inside the graduated window must fail the turn closed, got {terminal:#?}"
        );
    };
    // Fails closed through the trailing zone: the marker promised nothing would
    // follow it, something did, and the guarantee the graduated drain rests on
    // is therefore broken. Refusing is bad; committing the truncated answer
    // would be unacceptable.
    assert_eq!(error.code, ErrorCode::SchemaDrift);
    assert_eq!(
        transcript.appended_at_ms().len(),
        2,
        "the late row must have been served, or the test proves nothing"
    );
    assert_eq!(
        snapshot(&registry, session_id).await.state,
        SessionState::Failed
    );
}

// ---- the band the graduated floor gave away ---------------------------------
//
// Dropping the requirement from 2000ms to 250ms hands back every millisecond in
// `(250, 2000]`. Refusing to return is bad; returning before the work is done is
// unacceptable, so the only honest test of that trade is the unacceptable
// failure itself: content still arriving while the gate decides.
//
// The live campaign that measured the floor could not run that test. Across 19
// graded turns the gap between the terminal candidate and the last transcript
// activity was 0 or 1ms every time -- content arrived late exactly never -- so
// its hash matches were entailed by the workload and would have held at a drain
// of zero. The band was sampled once, at ordinal 70, and is sampled here at four
// further points on a simulated timeline where arrival instants are chosen
// rather than hoped for.
//
// Every arrival below is a *genuine* post-marker quiet gap: the transcript goes
// silent at the marker and the next byte is the late row, with nothing in
// between. That is what makes the gate reachable and the arm meaningful. Reaching
// a large gap without a keepalive drip is a matter of poll cadence rather than
// script: the gate is first satisfied by the poll reporting the requirement, the
// confirming re-poll is one poll later, so the furthest a row can lag the last
// byte and still be read is one poll past the requirement -- a distance that
// scales with the cadence. Varying the cadence walks that boundary across the
// whole band. A drip would instead keep the quiet gap below the floor forever,
// leaving the gate unsatisfiable by construction and the band untested.
//
// The four tests below say, in order: content anywhere in the band is not lost
// (and the floor re-arms from the last byte, which is *why*); the one real
// sample in the band, ordinal 70's 352ms, is still caught; the catchable window
// ends one poll past the requirement measured from that last byte, and a row
// beyond it is committed past -- which is admissible only because of what that
// row is; and the truncation-shaped row -- Claude speaking again on the main
// chain -- still fails closed at the far end of the band, not just at the 150ms
// the existing test covers.

/// Poll cadences that place the last catchable read at four points spanning the
/// band, from just past the floor to just short of the configured drain.
///
/// At the floor as it stands these are arrivals 600ms, 800ms, 1300ms and 1900ms
/// after the marker. They are stated as cadences rather than as offsets because
/// the offset a cadence can express is a function of the requirement: raise the
/// floor and every one of these points moves outward with it, which is the
/// direction a floor is allowed to move.
const GRADUATED_BAND_STEPS_MS: [u64; 4] = [200, 400, 650, 950];

/// The most a row can lag the last transcript byte and still be caught at a
/// given poll cadence.
///
/// The commit lands on the confirming re-poll, and every poll before it can
/// still carry bytes, so this is exactly one poll short of the commit. Stated in
/// terms of [`graduated_polls`] rather than re-derived, because the two numbers
/// are the same fact counted in different units and must not be able to drift.
const fn catchable_window_ms(step_ms: u64) -> u64 {
    (graduated_polls(GRADUATED_REQUIRED_MS, step_ms) - 1) * step_ms
}

/// The band's cadence arithmetic and the product's are one statement.
///
/// [`catchable_window_ms`] is what every arm in this section is written against,
/// and until `POST_MARKER_CATCH_WINDOW_FLOOR_MS` was written down it existed
/// only here -- so the guarantee those six tests pin had no name in the product,
/// and nothing outside this file could be checked against it. This is the arm
/// that says they are testing the shipped guarantee rather than a local
/// convenience, and it fails in BOTH directions: a change to the product's
/// derivation reddens it, and so does a change to this file's.
///
/// It is the link in a chain, not a fact on its own. The band tests assert that
/// the production actor's published `drain_ms` equals `catchable_window_ms`;
/// this asserts that `catchable_window_ms` is `post_marker_catch_window_ms`; so
/// together they assert that a real turn's realised catch window is the number
/// `v1::backend` states and `driver_io.rs` refuses to narrow.
///
/// NOT deletion-observable against any single mutant, and not claimed to be:
/// what it defends is an identity between two expressions of one fact.
#[test]
fn the_bands_catchable_window_is_the_products_own_derivation() {
    // Every cadence this file actually drives a turn at, not a fresh sample:
    // a cadence the suite does not use would be an agreement about nothing.
    for step_ms in GRADUATED_BAND_STEPS_MS.into_iter().chain([
        GRADUATED_STEP_MS,
        ORDINAL_70_STEP_MS,
        DRAIN_TIMELINE_STEP_MS,
    ]) {
        assert_eq!(
            catchable_window_ms(step_ms),
            post_marker_catch_window_ms(GRADUATED_REQUIRED_MS, step_ms),
            "the band's window at a {step_ms}ms cadence must be the product's own"
        );
    }

    // The one absolute in this section, checked against the floor the product
    // publishes. This is what makes 438 a floor rather than a preference: the
    // only post-marker arrival ever observed live through pmux is inside it.
    assert_eq!(
        catchable_window_ms(ORDINAL_70_STEP_MS),
        ORDINAL_70_POST_MARKER_MS,
        "the ordinal-70 cadence must still place that arrival on the last catchable read"
    );
}

/// The live sample this file pins as an absolute is inside the floor the product
/// publishes, and `v1::backend` restates the same 352 beside that floor.
///
/// `const` and not an arm of the test above: both terms are compile-time
/// constants, so a runtime assertion over them is one clippy is right to call
/// `assert!(true)`. A floor lowered past the live sample stops this test binary
/// compiling instead of reddening it.
const _: () = assert!(
    ORDINAL_70_POST_MARKER_MS < POST_MARKER_CATCH_WINDOW_FLOOR_MS,
    "a floor below the one live sample would not have caught the row that really arrived"
);

/// A post-marker sidechain assistant row.
///
/// Sidechain because it is the shape that is *legal* after the marker, so the
/// turn still commits and its timings can be read; a main-chain row is schema
/// drift and is exercised separately below. Semantic because the analysis reads
/// it -- its usage moves the committed sidechain totals, which is how the row is
/// proven ingested rather than merely observed.
fn band_sidechain_row(output_tokens: u64) -> ParsedRow {
    parse_line(
        10,
        format!(
            r#"{{"parentUuid":"prompt-row","isSidechain":true,"sessionId":"test","type":"assistant","uuid":"band-row","message":{{"id":"band-message","model":"claude-test","content":[{{"type":"text","text":"still writing"}}],"stop_reason":"end_turn","usage":{{"input_tokens":1,"output_tokens":{output_tokens}}}}}}}"#
        )
        .as_bytes(),
    )
}

/// A post-marker main-chain assistant row: Claude continuing to speak after
/// announcing the turn was over, which is what a truncated answer looks like
/// from inside pmux.
fn band_main_chain_row() -> ParsedRow {
    parse_line(
        11,
        br#"{"parentUuid":"turn-duration-row","sessionId":"test","type":"assistant","uuid":"late-row","message":{"id":"late-message","model":"claude-test","content":[{"type":"text","text":"actually, one more thing"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":5}}}"#,
    )
}

/// Runs a marked turn at poll cadence `step_ms` whose only post-marker content
/// is `late`, a row and the offset after the marker at which it arrives.
///
/// The transcript is silent between the marker and that row, so the offset is a
/// real quiet gap and the reported stability climbs monotonically across it.
/// Hands back the committed outcome, the instants the fake source recorded for
/// its own appends, and the poll count -- the three things these boundaries need
/// stated from both sides.
async fn run_band_turn(
    seed: u128,
    step_ms: u64,
    late: Option<(u64, ParsedRow)>,
) -> (StoredTurnTerminal, Vec<u64>, u64) {
    let clock = Arc::new(DrainTimelineClock::new(DRAIN_TIMELINE_START_MS));
    let registry = SessionRegistry::with_clock(test_actor_config(64), clock.clone());
    let session_id = id(930 + seed);
    let turn_id = id(9300 + seed);
    let mut arrivals = vec![(0, turn_rows_with_marker("band", "answer"))];
    if let Some((offset_ms, row)) = late {
        assert_eq!(
            offset_ms % step_ms,
            0,
            "an arrival offset must name an exact poll index at this cadence"
        );
        arrivals.push((offset_ms / step_ms, vec![row]));
    }
    let transcript = Arc::new(DrainTimelineTranscript::new(clock, step_ms, arrivals));
    register_with_drain(
        &registry,
        session_id,
        ready_and_quiet_terminal(),
        transcript.clone(),
        GRADUATED_CONFIGURED_DRAIN_MS,
    )
    .await;
    registry
        .run_turn(turn(session_id, turn_id, "band"))
        .await
        .unwrap();
    let terminal = wait_for_terminal(&registry, session_id, turn_id).await;
    let polls = transcript.polls.load(Ordering::SeqCst);
    (terminal, transcript.appended_at_ms(), polls)
}

#[tokio::test]
async fn a_row_arriving_anywhere_in_the_graduated_band_is_not_lost() {
    let mut sampled = Vec::new();
    for (index, step_ms) in GRADUATED_BAND_STEPS_MS.into_iter().enumerate() {
        let offset_ms = catchable_window_ms(step_ms);
        // A cadence whose last catchable read lands past the configured drain is
        // no longer sampling the band the floor gave away -- it is past the far
        // end of it, in margin the unmarked path never had either. That can only
        // happen once the floor has been raised until the band no longer holds a
        // whole poll of this cadence, and skipping is then the honest response.
        if offset_ms > GRADUATED_CONFIGURED_DRAIN_MS {
            continue;
        }
        // The gap is inside the band by construction: the last catchable read is
        // always at least one poll past the requirement.
        assert!(offset_ms > GRADUATED_REQUIRED_MS);
        sampled.push(offset_ms);

        let seed = u128::try_from(index).expect("index fits");
        // The row whose loss would be the unacceptable failure: it arrives
        // `offset_ms` after Claude announced the turn was over, into a transcript
        // that has been silent since the marker, which is past the floor and
        // inside what the configured drain would still have covered.
        let (terminal, appended_at_ms, polls) = run_band_turn(
            940 + seed,
            step_ms,
            Some((offset_ms, band_sidechain_row(7))),
        )
        .await;
        let StoredTurnTerminal::Result(result) = terminal else {
            panic!("expected a result at {offset_ms}ms, got {terminal:#?}")
        };
        let timings = &result.timings;

        // Nothing was skipped. Both scripted appends were actually served, so the
        // test cannot pass by the source quietly never offering the late row.
        assert_eq!(
            appended_at_ms.len(),
            2,
            "the late row must have been served at {offset_ms}ms"
        );
        let marker_at_ms = appended_at_ms[0];
        let last_byte_at_ms = appended_at_ms[1];

        // THE PROPERTY. The row landed `offset_ms` after the marker -- past the
        // floor, after an uninterrupted quiet gap of exactly that length -- and
        // the gate had not committed, so it is in the published result rather
        // than truncated out of it.
        assert_eq!(
            last_byte_at_ms - marker_at_ms,
            offset_ms,
            "the scripted offset must be the offset actually realised"
        );
        assert_eq!(result.outcome, TurnOutcome::Completed);
        assert_eq!(result.text, "answer");
        assert_eq!(
            result.usage.combined.output_tokens, 9,
            "the post-marker row must be counted, not merely observed, at {offset_ms}ms"
        );
        assert_eq!(result.usage.main.output_tokens, 2);

        // THE MECHANISM, made executable. `stable_for_ms` is quiet-since-the-last
        // -transcript-byte and re-arms on any post-marker write (`driver_io.rs`
        // sets `last_change` on every read that produced bytes and returns early,
        // without touching it, on an empty one -- pinned against the real source
        // in `transcript_filesystem_faults.rs`). So the floor is a *quiet window*,
        // not a countdown from the marker: the turn paid a full fresh window
        // measured from the last byte, and the total distance from the marker is
        // the offset plus that window rather than the window alone. If the floor
        // were ever a time-since-marker window instead, this pair would read
        // `offset_ms - something` and the row would be gone.
        assert_eq!(
            timings.completed_at_ms - last_byte_at_ms,
            catchable_window_ms(step_ms),
            "the floor re-armed from the last byte at {offset_ms}ms"
        );
        assert_eq!(
            timings.completed_at_ms - marker_at_ms,
            offset_ms + catchable_window_ms(step_ms),
            "and it is therefore not measured from the marker at {offset_ms}ms"
        );

        // The same fact counted a second way, in polls rather than milliseconds:
        // the arrival sent the loop back round for a fresh window, and the
        // control in the boundary test below shows what the count is without it.
        assert_eq!(
            polls,
            offset_ms / step_ms + graduated_polls(GRADUATED_REQUIRED_MS, step_ms)
        );

        // And the graduated path is the one under test: the marker was seen, the
        // published drain is the graduated window rather than the configured
        // 2000ms, and the post-marker row is stamped as the late observation.
        assert_eq!(timings.turn_duration_observed_at_ms, Some(marker_at_ms));
        assert_eq!(
            timings.post_turn_duration_row_observed_at_ms,
            Some(last_byte_at_ms)
        );
        assert_eq!(timings.drain_ms, Some(catchable_window_ms(step_ms)));
    }

    // The band is a range, and one point in a range is not coverage. This is the
    // arm that fails if the cadences above ever collapse onto one another.
    assert!(
        sampled.len() >= 2,
        "the band must be sampled at more than one offset, sampled {sampled:?}"
    );
}

/// The one real-world sample inside the band: on ordinal 70 of the live
/// campaign, on this build, a transcript row landed 352ms after the
/// `turn_duration` marker -- 102ms past the floor -- and the turn did not
/// truncate.
const ORDINAL_70_POST_MARKER_MS: u64 = 352;

/// A poll cadence that makes ordinal 70's arrival an exact poll index. At the
/// floor as it stands the gate is first satisfied at 264ms and the confirming
/// re-poll is at 352ms, so that arrival is the last read before the commit: the
/// live sample sits exactly on the boundary this build guarantees.
const ORDINAL_70_STEP_MS: u64 = 88;

#[tokio::test]
async fn the_live_352ms_post_marker_arrival_is_still_caught() {
    // Stated as an absolute rather than derived from the floor, on purpose, and
    // this is the only place in the band tests that does so. It is the
    // requirement: a floor too small to reach 352ms is a floor that would have
    // lost this row on a run that really happened. The failure is not a premise
    // panic either -- lower the floor and the source is simply never asked for
    // the row, so the arms below fail on observed behaviour.
    let (terminal, appended_at_ms, _) = run_band_turn(
        950,
        ORDINAL_70_STEP_MS,
        Some((ORDINAL_70_POST_MARKER_MS, band_sidechain_row(7))),
    )
    .await;
    let StoredTurnTerminal::Result(result) = terminal else {
        panic!("a row {ORDINAL_70_POST_MARKER_MS}ms after the marker must not fail the turn")
    };
    assert_eq!(
        appended_at_ms.len(),
        2,
        "the row must have been served: pmux was still polling {ORDINAL_70_POST_MARKER_MS}ms \
         after the marker, as it was on ordinal 70"
    );
    assert_eq!(
        appended_at_ms[1] - appended_at_ms[0],
        ORDINAL_70_POST_MARKER_MS,
        "and it arrived after a quiet gap of exactly the observed length"
    );
    assert_eq!(result.outcome, TurnOutcome::Completed);
    assert_eq!(
        result.usage.combined.output_tokens, 9,
        "the row was ingested before the commit rather than committed past"
    );
    assert_eq!(
        result.timings.post_turn_duration_row_observed_at_ms,
        Some(appended_at_ms[1])
    );
}

#[tokio::test]
async fn the_catchable_window_ends_one_poll_past_the_floor_measured_from_the_last_byte() {
    // CONTROL FIRST, so everything below is known to be able to fail. With no
    // post-marker content at all the gate commits promptly: the requirement and
    // one confirming re-poll, and not one poll more. A test that only ever
    // asserted "the turn did not commit" would pass against a gate that never
    // commits; this is the arm that rules that out.
    let (terminal, appended_at_ms, quiet_polls) = run_band_turn(0, GRADUATED_STEP_MS, None).await;
    let StoredTurnTerminal::Result(control) = terminal else {
        panic!("a quiet marked turn must commit")
    };
    assert_eq!(appended_at_ms.len(), 1);
    assert_eq!(control.outcome, TurnOutcome::Completed);
    assert_eq!(
        quiet_polls,
        graduated_polls(GRADUATED_REQUIRED_MS, GRADUATED_STEP_MS)
    );
    let control_commit_offset_ms = control.timings.completed_at_ms - appended_at_ms[0];
    assert_eq!(
        control_commit_offset_ms,
        catchable_window_ms(GRADUATED_STEP_MS)
    );
    assert_eq!(control.usage.combined.output_tokens, 2);

    // The row both boundary arms use, and the thing that decides what missing it
    // means. It is a sidechain assistant fragment: off the active main chain, so
    // it can move sidechain accounting and nothing else. Asserted rather than
    // assumed, because the entire justification below rests on it.
    let boundary_row = band_sidechain_row(7);
    assert_eq!(
        boundary_row.common.scope,
        RowScope::Sidechain,
        "the boundary arms are only admissible for an off-main-chain row"
    );
    assert!(matches!(boundary_row.kind, RowKind::Assistant(_)));

    // THE BOUNDARY, from inside. A row at the far edge of the catchable window
    // after a genuinely quiet marker lands on the confirming re-poll -- the last
    // read that exists before the commit -- and is caught.
    let inside_ms = catchable_window_ms(GRADUATED_STEP_MS);
    let (terminal, appended_at_ms, polls) = run_band_turn(
        1,
        GRADUATED_STEP_MS,
        Some((inside_ms, boundary_row.clone())),
    )
    .await;
    let StoredTurnTerminal::Result(caught) = terminal else {
        panic!("a row on the confirming re-poll must still be caught")
    };
    assert_eq!(
        appended_at_ms.len(),
        2,
        "the late row must have been served, or the arm proves nothing"
    );
    assert_eq!(appended_at_ms[1] - appended_at_ms[0], inside_ms);
    assert_eq!(caught.outcome, TurnOutcome::Completed);
    assert_eq!(
        caught.usage.combined.output_tokens, 9,
        "the row was ingested by the confirming re-poll before the commit"
    );
    assert_eq!(
        caught.timings.post_turn_duration_row_observed_at_ms,
        Some(appended_at_ms[1])
    );
    // It cost a further pass: the re-poll carrying rows sends the loop back
    // round, so the turn owes a fresh window from the row rather than committing
    // on the read that found it.
    assert!(polls > quiet_polls);
    assert_eq!(
        caught.timings.completed_at_ms - appended_at_ms[1],
        catchable_window_ms(GRADUATED_STEP_MS)
    );

    // AND WHAT CATCHING IT WAS WORTH -- the measurement that makes the next arm
    // admissible. Against the quiet control, ingesting this row changed only
    // sidechain accounting: the answer text and the main-chain usage are
    // identical. This row cannot change the answer, and that is established here
    // rather than asserted about it.
    assert_eq!(caught.text, control.text);
    assert_eq!(caught.usage.main, control.usage.main);
    assert_eq!(
        caught.usage.combined.output_tokens - control.usage.combined.output_tokens,
        7,
        "the only thing catching it moved was the sidechain total"
    );

    // THE BOUNDARY, from outside -- and the exposure this test exists to make
    // visible. One poll further out, the transcript has been genuinely quiet for
    // the whole requirement, the gate commits, and the row is committed past: the
    // source is never even asked for it again. The guarantee is therefore "caught
    // iff within one poll of the requirement measured from the LAST BYTE", not
    // "caught anywhere in the band" and not any fixed multiple of the floor.
    //
    // Committing past a row is acceptable here for exactly one reason, and it is
    // the reason measured immediately above: this row is off the active main
    // chain, so the answer pmux publishes is the same whether it was read or not.
    // A *main-chain* semantic row in this position is a different question, and
    // the answer to that one is not "commit past it" but "fail closed" --
    // `a_semantic_main_chain_row_late_in_the_graduated_band_still_fails_the_turn_closed`
    // pins it, at the far end of this same band.
    //
    // The bound is a function of poll cadence, which is why it is pinned. In
    // production the cadence is set by the screen-stability wait, a constant with
    // no reason to know about transcript arrival order; a latency win that
    // tightened it would silently narrow this window and buy back truncation
    // risk. This assertion is what fails when that happens.
    let outside_ms = catchable_window_ms(GRADUATED_STEP_MS) + GRADUATED_STEP_MS;
    let (terminal, appended_at_ms, polls) =
        run_band_turn(2, GRADUATED_STEP_MS, Some((outside_ms, boundary_row))).await;
    let StoredTurnTerminal::Result(missed) = terminal else {
        panic!("a turn quiet for the whole requirement must commit")
    };
    assert_eq!(
        appended_at_ms.len(),
        1,
        "the row scheduled past the window was never served: pmux had committed"
    );
    assert_eq!(missed.outcome, TurnOutcome::Completed);
    assert_eq!(missed.timings.post_turn_duration_row_observed_at_ms, None);
    assert_eq!(polls, quiet_polls);
    assert_eq!(
        missed.timings.completed_at_ms - appended_at_ms[0],
        control_commit_offset_ms,
        "and it committed at exactly the instant the quiet control did"
    );

    // The answer is whole regardless, which is the whole of the licence to
    // commit past that row: identical text and identical main-chain usage to
    // both the control and the run that did catch it.
    assert_eq!(missed.text, control.text);
    assert_eq!(missed.text, caught.text);
    assert_eq!(missed.usage.main, control.usage.main);
    assert_eq!(missed.usage.main, caught.usage.main);
    assert_eq!(missed.usage.combined.output_tokens, 2);
}

#[tokio::test]
async fn a_semantic_main_chain_row_late_in_the_graduated_band_still_fails_the_turn_closed() {
    // The far end of the band, and the shape that actually matters. Claude
    // speaking again on the main chain after announcing the turn was over is
    // what a truncated answer looks like from inside pmux, and the existing
    // 150ms case cannot discriminate the floor from a much smaller one -- an
    // arrival that near lands on the confirming re-poll whatever the floor is.
    // This one cannot: at a coarse cadence the last catchable read is most of
    // the band away, and the row is reached only because the requirement is a
    // quiet window that had not yet elapsed.
    const LATE_STEP_MS: u64 = 950;
    let late_offset_ms = catchable_window_ms(LATE_STEP_MS);

    let (terminal, appended_at_ms, _) = run_band_turn(
        3,
        LATE_STEP_MS,
        Some((late_offset_ms, band_main_chain_row())),
    )
    .await;
    let StoredTurnTerminal::Failed(error) = terminal else {
        panic!(
            "a main-chain row {late_offset_ms}ms after the marker must fail closed, got {terminal:#?}"
        );
    };
    // Fails closed through the trailing zone: the marker promised nothing would
    // follow it, something did, and the guarantee the graduated drain rests on
    // is therefore broken. Refusing is bad; committing the truncated answer
    // would be unacceptable.
    assert_eq!(error.code, ErrorCode::SchemaDrift);
    assert_eq!(
        appended_at_ms.len(),
        2,
        "the late row must have been served, or the test proves nothing"
    );
    assert_eq!(
        appended_at_ms[1] - appended_at_ms[0],
        late_offset_ms,
        "after a genuine quiet gap of that length, not a drip that kept the gate shut"
    );
    assert!(late_offset_ms > GRADUATED_REQUIRED_MS);
}

/// A transcript that never stops producing rows.
///
/// One assistant row per poll, each with its own uuid and no `stop_reason`, so
/// the turn is unmistakably ALIVE and unmistakably not complete. That is the
/// only state in which the veto's conjunction can be observed: a screen pmux
/// cannot read, over a transcript that is still moving.
#[derive(Default)]
struct AdvancingTranscript {
    offset: AtomicU64,
}

#[async_trait]
impl TranscriptSource for AdvancingTranscript {
    async fn arm_at_eof(&self, _session_id: SessionId) -> DriverResult<TranscriptArm> {
        Ok(TranscriptArm::default())
    }

    async fn poll(
        &self,
        _session_id: SessionId,
        _position: &TranscriptPosition,
    ) -> DriverResult<TranscriptBatch> {
        let index = self.offset.fetch_add(1, Ordering::SeqCst);
        let json = if index == 0 {
            r#"{"parentUuid":null,"sessionId":"test","type":"user","message":{"content":"a prompt that is being answered"},"uuid":"prompt-row","promptSource":"typed","promptId":"prompt-id"}"#.to_owned()
        } else {
            // Each row is the CHILD of the one before it. Chaining matters:
            // rows that all name one parent are two branches of the main chain,
            // and the engine refuses those as schema drift rather than treating
            // them as a stream.
            let parent = if index == 1 {
                "prompt-row".to_owned()
            } else {
                format!("answer-{}", index - 1)
            };
            format!(
                r#"{{"parentUuid":"{parent}","sessionId":"test","type":"assistant","uuid":"answer-{index}","message":{{"id":"answer-{index}","model":"claude-test","content":[{{"type":"text","text":"still writing {index}"}}],"stop_reason":null,"usage":{{"input_tokens":1,"output_tokens":1}}}}}}"#
            )
        };
        Ok(TranscriptBatch {
            position: TranscriptPosition {
                generation: 0,
                offset: index + 1,
            },
            rows: vec![parse_line(index as usize, json.as_bytes())],
            drain: TranscriptDrainEvidence {
                at_eof: true,
                has_partial_line: false,
                stable_for_ms: 0,
            },
        })
    }
}

// ---- the unrecognized-screen veto -------------------------------------------
//
// `Unknown` meant PROCEED, so a turn that landed on a screen pmux had never been
// taught -- a real "trust this directory", "not logged in", "please update" or
// "quota exceeded" modal outside `blocking_screen`'s 24 taught shapes -- sat
// there until its 600,000 ms deadline and then reported a timeout that named
// nothing. These two tests are the firing path and its conjunction.

/// A screen no rule matched, held past the window with the transcript standing
/// still, refuses the turn and NAMES what was on screen.
#[tokio::test]
async fn an_unrecognised_screen_held_past_the_window_refuses_the_turn() {
    let clock = Arc::new(TestClock::starting_at(1_000_000));
    let registry = SessionRegistry::with_clock(test_actor_config(64), clock.clone());
    let session_id = id(9_501);
    let turn_id = id(9_502);
    let terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence::default(),
        InterruptRecovery::RecoveredToReady,
    ));
    terminal.set_screen(unrecognised_screen());
    register(
        &registry,
        session_id,
        terminal,
        // No row ever arrives: the veto's other half is transcript silence, and
        // this is what silence looks like.
        Arc::new(ScriptedTranscript::stable_at_eof(Duration::ZERO)),
    )
    .await;
    let accepted = registry
        .run_turn(turn(session_id, turn_id, "a prompt nobody will answer"))
        .await
        .unwrap();

    // Before the window elapses the turn is still running: the veto is a
    // deadline, not a verdict on the first frame.
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_ne!(
        snapshot(&registry, session_id).await.state,
        SessionState::Failed,
        "an unrecognized screen must not refuse on sight"
    );

    clock.set(1_000_000 + UNRECOGNISED_SCREEN_VETO.as_millis() as u64 + 1);
    wait_for_state(&registry, session_id, SessionState::Failed).await;

    let events = registry
        .events(SubscribeEventsRequest {
            session_id,
            generation_id: generation(session_id),
            after_sequence: accepted.next_sequence.saturating_sub(1),
            wait_ms: 0,
            max_events: 16,
        })
        .await
        .unwrap();
    let failure = events
        .events
        .iter()
        .find_map(|event| match &event.event {
            EventPayload::TurnFailed(error) => Some(error.clone()),
            _ => None,
        })
        .expect("the vetoed turn publishes a failure");
    // NOT `TurnTimeout`: that is the code the silent hang already reported, and
    // a veto indistinguishable from the failure it replaces buys nothing.
    assert_eq!(failure.code, ErrorCode::NeedsInput);
    assert_eq!(failure.details["violation"], "unrecognised_screen_veto");
    assert_eq!(
        failure.details["veto_after_ms"],
        UNRECOGNISED_SCREEN_VETO.as_millis() as u64
    );
    // It names what was on screen -- structurally, and never its text.
    let screen = &failure.details["screen"];
    assert_eq!(screen["rows"], 24);
    assert_eq!(screen["cols"], 80);
    assert_eq!(screen["cursor_present"], false);
    assert_eq!(screen["contains_prompt_glyph"], false);
    assert!(
        !serde_json::to_string(&failure.details)
            .unwrap()
            .contains("a rendering pmux has no rule for"),
        "screen TEXT must never leave the terminal adapter"
    );
}

/// The conjunction: a transcript that is still arriving keeps the turn alive
/// however long the screen stays unreadable.
///
/// This is what makes the rule a liveness VETO rather than a second opinion
/// about completion. Without it, a turn whose Claude is streaming an answer
/// while rendering something pmux has no rule for would be refused at 30 s with
/// its answer half-written.
#[tokio::test]
async fn an_unrecognised_screen_does_not_refuse_a_turn_whose_transcript_is_advancing() {
    let clock = Arc::new(TestClock::starting_at(1_000_000));
    // A turn deadline far past the veto window, so that a turn surviving the
    // jump below has survived the VETO and not merely arrived before its own
    // timeout. With the 60_000 ms default this test would pass for the wrong
    // reason at 45 s and fail for the wrong reason at 120 s.
    let registry = SessionRegistry::with_clock(
        SessionActorConfig {
            default_turn_timeout_ms: UNRECOGNISED_SCREEN_VETO.as_millis() as u64 * 10,
            ..test_actor_config(64)
        },
        clock.clone(),
    );
    let session_id = id(9_601);
    let turn_id = id(9_602);
    let terminal = Arc::new(FakeTerminal::new(
        TerminalEvidence::default(),
        InterruptRecovery::RecoveredToReady,
    ));
    terminal.set_screen(unrecognised_screen());
    register(
        &registry,
        session_id,
        terminal,
        // Rows keep arriving, one per poll, for the whole test.
        Arc::new(AdvancingTranscript::default()),
    )
    .await;
    registry
        .run_turn(turn(session_id, turn_id, "a prompt that is being answered"))
        .await
        .unwrap();

    clock.set(1_000_000 + UNRECOGNISED_SCREEN_VETO.as_millis() as u64 * 4);
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_ne!(
        snapshot(&registry, session_id).await.state,
        SessionState::Failed,
        "a turn whose transcript is still advancing is alive whatever the screen renders"
    );
}
