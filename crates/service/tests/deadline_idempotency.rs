use std::collections::VecDeque;
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pseudomux_claude::{CompleteLine, JsonlParser, ParseMode, SourceLocation};
use pseudomux_protocol::v1::{
    ClosePolicy, CompatibilityReport, ErrorCode, EventPayload, InputTransport, RunTurnRequest,
    SessionGenerationId, SessionId, SessionState, SubscribeEventsRequest, TerminalProfile, TurnId,
    TurnRequest,
};
use pseudomux_service::v1::{
    Clock, DriverFailure, DriverResult, InterruptRecovery, SessionActorConfig, SessionCell,
    SessionRegistration, SessionRegistry, StoredTurnTerminal, TerminalControl, TerminalEvidence,
    TerminalScreenObservation, TranscriptArm, TranscriptBatch, TranscriptDrainEvidence,
    TranscriptPosition, TranscriptSource,
};
use tokio::sync::Notify;
use uuid::Uuid;

const BEFORE_DEADLINE_MS: u64 = 9_000;
const DEADLINE_MS: u64 = 10_000;
const MODE_NORMAL: u8 = 0;
const MODE_EXPIRE_SUBMIT: u8 = 1;
const MODE_EXPIRE_EVIDENCE: u8 = 2;
const MODE_COMMIT_RACE: u8 = 3;

/// Controllable wall clock with an optional call-count transition. The latter
/// makes the worker/actor commit race deterministic without sleeping.
struct ManualClock {
    now_ms: AtomicU64,
    race_after_calls: AtomicUsize,
    race_calls: AtomicUsize,
    race_value_ms: AtomicU64,
}

impl ManualClock {
    fn new(now_ms: u64) -> Self {
        Self {
            now_ms: AtomicU64::new(now_ms),
            race_after_calls: AtomicUsize::new(usize::MAX),
            race_calls: AtomicUsize::new(0),
            race_value_ms: AtomicU64::new(now_ms),
        }
    }

    fn set(&self, now_ms: u64) {
        self.now_ms.store(now_ms, Ordering::SeqCst);
    }

    fn expire_after_calls(&self, calls_before_expiry: usize, value_ms: u64) {
        self.race_calls.store(0, Ordering::SeqCst);
        self.race_value_ms.store(value_ms, Ordering::SeqCst);
        self.race_after_calls
            .store(calls_before_expiry, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        let threshold = self.race_after_calls.load(Ordering::SeqCst);
        if threshold != usize::MAX {
            let call = self.race_calls.fetch_add(1, Ordering::SeqCst);
            if call >= threshold {
                self.now_ms
                    .store(self.race_value_ms.load(Ordering::SeqCst), Ordering::SeqCst);
            }
        }
        self.now_ms.load(Ordering::SeqCst)
    }
}

struct ProbeTerminal {
    clock: Arc<ManualClock>,
    mode: AtomicU8,
    submissions: AtomicUsize,
    closes: AtomicUsize,
    evidence_gate: Arc<Notify>,
}

impl ProbeTerminal {
    fn new(clock: Arc<ManualClock>, mode: u8) -> Self {
        Self {
            clock,
            mode: AtomicU8::new(mode),
            submissions: AtomicUsize::new(0),
            closes: AtomicUsize::new(0),
            evidence_gate: Arc::new(Notify::new()),
        }
    }
}

#[async_trait]
impl TerminalControl for ProbeTerminal {
    async fn submit_prompt(
        &self,
        _session_id: SessionId,
        _turn_id: TurnId,
        _prompt: &str,
        deadline_unix_ms: u64,
    ) -> DriverResult<()> {
        if self.mode.load(Ordering::SeqCst) == MODE_EXPIRE_SUBMIT {
            self.clock.set(deadline_unix_ms);
        }
        // Model the production adapter's last-boundary check: mutation is
        // counted only when Enter would still be legal.
        if self.clock.now_ms() >= deadline_unix_ms {
            return Err(DriverFailure::new(
                ErrorCode::TurnTimeout,
                "deadline elapsed at fake submission boundary",
            ));
        }
        self.submissions.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn completion_evidence(
        &self,
        _session_id: SessionId,
        _turn_id: TurnId,
    ) -> DriverResult<TerminalEvidence> {
        match self.mode.load(Ordering::SeqCst) {
            MODE_EXPIRE_EVIDENCE => self.clock.set(DEADLINE_MS),
            MODE_COMMIT_RACE => {
                self.evidence_gate.notified().await;
                // After evidence returns, the worker performs six clock reads:
                // evidence postcheck, confirmation pre/postchecks, pre-build,
                // completed_at, and pre-send. The seventh is the actor's
                // terminal commit check and must observe expiry.
                self.clock.expire_after_calls(6, DEADLINE_MS);
            }
            _ => {}
        }
        Ok(TerminalEvidence {
            ready_prompt: true,
            quiet: true,
            lifecycle_expected: false,
            lifecycle_hook_observed: false,
            lifecycle_hook_at_ms: None,
        })
    }

    async fn observe_screen(
        &self,
        _session_id: SessionId,
    ) -> DriverResult<TerminalScreenObservation> {
        Ok(TerminalScreenObservation::Ready)
    }

    async fn interrupt(
        &self,
        _session_id: SessionId,
        _turn_id: TurnId,
    ) -> DriverResult<InterruptRecovery> {
        Ok(InterruptRecovery::RecoveredToReady)
    }

    async fn close(&self, _session_id: SessionId, _policy: ClosePolicy) -> DriverResult<bool> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        Ok(true)
    }
}

struct ProbeTranscript {
    clock: Arc<ManualClock>,
    expire_during_arm: bool,
    polls: Mutex<VecDeque<TranscriptBatch>>,
    fallback: TranscriptBatch,
}

impl ProbeTranscript {
    fn pending(clock: Arc<ManualClock>, expire_during_arm: bool) -> Self {
        Self {
            clock,
            expire_during_arm,
            polls: Mutex::new(VecDeque::new()),
            fallback: TranscriptBatch::default(),
        }
    }

    fn terminal(clock: Arc<ManualClock>, prompt: &str, answer: &str) -> Self {
        let rows = terminal_rows(prompt, answer);
        let position = TranscriptPosition {
            generation: 0,
            offset: rows.len() as u64,
        };
        let drained = TranscriptDrainEvidence {
            at_eof: true,
            has_partial_line: false,
            stable_for_ms: 100,
        };
        Self {
            clock,
            expire_during_arm: false,
            polls: Mutex::new(VecDeque::from([TranscriptBatch {
                position: position.clone(),
                rows,
                drain: drained,
            }])),
            fallback: TranscriptBatch {
                position,
                rows: Vec::new(),
                drain: drained,
            },
        }
    }
}

#[async_trait]
impl TranscriptSource for ProbeTranscript {
    async fn arm_at_eof(&self, _session_id: SessionId) -> DriverResult<TranscriptArm> {
        if self.expire_during_arm {
            self.clock.set(DEADLINE_MS);
        }
        Ok(TranscriptArm::default())
    }

    async fn poll(
        &self,
        _session_id: SessionId,
        _position: &TranscriptPosition,
    ) -> DriverResult<TranscriptBatch> {
        Ok(self
            .polls
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| self.fallback.clone()))
    }
}

#[tokio::test]
async fn deadline_expires_during_arm_before_any_prompt_injection() {
    let clock = Arc::new(ManualClock::new(BEFORE_DEADLINE_MS));
    let terminal = Arc::new(ProbeTerminal::new(Arc::clone(&clock), MODE_NORMAL));
    let (registry, generation_id) = register(
        Arc::clone(&clock),
        terminal.clone(),
        Arc::new(ProbeTranscript::pending(Arc::clone(&clock), true)),
        id(1),
    )
    .await;

    let terminal_outcome = submit_and_wait(&registry, id(1), generation_id, id(11), "arm").await;
    assert_timeout(&terminal_outcome);
    assert_eq!(terminal.submissions.load(Ordering::SeqCst), 0);
    assert_eq!(terminal.closes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn deadline_expires_at_submit_boundary_without_prompt_injection() {
    let clock = Arc::new(ManualClock::new(BEFORE_DEADLINE_MS));
    let terminal = Arc::new(ProbeTerminal::new(Arc::clone(&clock), MODE_EXPIRE_SUBMIT));
    let (registry, generation_id) = register(
        Arc::clone(&clock),
        terminal.clone(),
        Arc::new(ProbeTranscript::pending(Arc::clone(&clock), false)),
        id(2),
    )
    .await;

    let terminal_outcome = submit_and_wait(&registry, id(2), generation_id, id(12), "submit").await;
    assert_timeout(&terminal_outcome);
    assert_eq!(terminal.submissions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn deadline_expires_during_completion_evidence() {
    let clock = Arc::new(ManualClock::new(BEFORE_DEADLINE_MS));
    let terminal = Arc::new(ProbeTerminal::new(Arc::clone(&clock), MODE_EXPIRE_EVIDENCE));
    let (registry, generation_id) = register(
        Arc::clone(&clock),
        terminal.clone(),
        Arc::new(ProbeTranscript::terminal(
            Arc::clone(&clock),
            "evidence",
            "too late",
        )),
        id(3),
    )
    .await;

    let terminal_outcome =
        submit_and_wait(&registry, id(3), generation_id, id(13), "evidence").await;
    assert_timeout(&terminal_outcome);
    assert_eq!(terminal.submissions.load(Ordering::SeqCst), 1);
    assert_eq!(terminal.closes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn actor_rejects_terminal_success_that_races_the_deadline_commit() {
    let clock = Arc::new(ManualClock::new(BEFORE_DEADLINE_MS));
    let terminal = Arc::new(ProbeTerminal::new(Arc::clone(&clock), MODE_COMMIT_RACE));
    let session_id = id(4);
    let turn_id = id(14);
    let (registry, generation_id) = register(
        Arc::clone(&clock),
        terminal.clone(),
        Arc::new(ProbeTranscript::terminal(
            Arc::clone(&clock),
            "commit",
            "discarded success",
        )),
        session_id,
    )
    .await;
    registry
        .run_turn(turn_request(session_id, generation_id, turn_id, "commit"))
        .await
        .unwrap();
    wait_for_state(&registry, session_id, generation_id, SessionState::Draining).await;
    terminal.evidence_gate.notify_one();

    let terminal_outcome = wait_for_terminal(&registry, session_id, generation_id, turn_id).await;
    assert_timeout(&terminal_outcome);
    assert_eq!(terminal.submissions.load(Ordering::SeqCst), 1);
    assert_eq!(terminal.closes.load(Ordering::SeqCst), 0);
    assert_eq!(
        registry
            .inspect(pseudomux_protocol::v1::InspectSessionRequest {
                session_id,
                generation_id,
            })
            .await
            .unwrap()
            .state,
        SessionState::Ready
    );
    assert_terminal_event_kind(&registry, session_id, generation_id, turn_id, false).await;
}

#[tokio::test]
async fn duplicate_replay_preserves_timeout_or_success_without_reinjection() {
    let timeout_clock = Arc::new(ManualClock::new(BEFORE_DEADLINE_MS));
    let timeout_terminal = Arc::new(ProbeTerminal::new(
        Arc::clone(&timeout_clock),
        MODE_EXPIRE_SUBMIT,
    ));
    let timeout_session = id(5);
    let timeout_turn = id(15);
    let (timeout_registry, timeout_generation) = register(
        Arc::clone(&timeout_clock),
        timeout_terminal.clone(),
        Arc::new(ProbeTranscript::pending(Arc::clone(&timeout_clock), false)),
        timeout_session,
    )
    .await;
    let timeout_request = turn_request(
        timeout_session,
        timeout_generation,
        timeout_turn,
        "stable timeout",
    );
    timeout_registry
        .run_turn(timeout_request.clone())
        .await
        .unwrap();
    let first_timeout = wait_for_terminal(
        &timeout_registry,
        timeout_session,
        timeout_generation,
        timeout_turn,
    )
    .await;
    assert_timeout(&first_timeout);
    let timeout_replay = timeout_registry.run_turn(timeout_request).await.unwrap();
    assert!(timeout_replay.replayed);
    let replayed_timeout = timeout_registry
        .stored_turn(timeout_session, timeout_generation, timeout_turn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(replayed_timeout, first_timeout);
    assert_eq!(timeout_terminal.submissions.load(Ordering::SeqCst), 0);
    assert_replay_event(
        &timeout_registry,
        timeout_session,
        timeout_generation,
        timeout_replay.next_sequence,
        false,
    )
    .await;

    let success_clock = Arc::new(ManualClock::new(BEFORE_DEADLINE_MS));
    let success_terminal = Arc::new(ProbeTerminal::new(Arc::clone(&success_clock), MODE_NORMAL));
    let success_session = id(6);
    let success_turn = id(16);
    let (success_registry, success_generation) = register(
        Arc::clone(&success_clock),
        success_terminal.clone(),
        Arc::new(ProbeTranscript::terminal(
            Arc::clone(&success_clock),
            "stable success",
            "done",
        )),
        success_session,
    )
    .await;
    let success_request = turn_request(
        success_session,
        success_generation,
        success_turn,
        "stable success",
    );
    success_registry
        .run_turn(success_request.clone())
        .await
        .unwrap();
    let first_success = wait_for_terminal(
        &success_registry,
        success_session,
        success_generation,
        success_turn,
    )
    .await;
    assert!(matches!(first_success, StoredTurnTerminal::Result(_)));
    let success_replay = success_registry.run_turn(success_request).await.unwrap();
    assert!(success_replay.replayed);
    let replayed_success = success_registry
        .stored_turn(success_session, success_generation, success_turn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(replayed_success, first_success);
    assert_eq!(success_terminal.submissions.load(Ordering::SeqCst), 1);
    assert_replay_event(
        &success_registry,
        success_session,
        success_generation,
        success_replay.next_sequence,
        true,
    )
    .await;
}

async fn register(
    clock: Arc<ManualClock>,
    terminal: Arc<ProbeTerminal>,
    transcript: Arc<ProbeTranscript>,
    session_id: SessionId,
) -> (SessionRegistry, SessionGenerationId) {
    let config = SessionActorConfig {
        poll_interval: Duration::from_millis(1),
        cancel_recovery_timeout: Duration::from_millis(100),
        default_turn_timeout_ms: 1_000,
        ..SessionActorConfig::default()
    };
    let registry = SessionRegistry::with_clock(config, clock);
    let generation_id = SessionGenerationId::new();
    let handle = registry
        .register(SessionRegistration {
            owner: pseudomux_service::v1::SessionOwner::Caller,
            session_id,
            generation_id,
            cwd: "/tmp/deadline-test".to_owned(),
            compatibility: CompatibilityReport {
                claude_version: "test".to_owned(),
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
            agent: None,
            idle_ttl_ms: None,
            initial_needs_input: None,
            terminal,
            transcript,
        })
        .await
        .unwrap();
    assert_eq!(handle.generation_id, generation_id);
    (registry, handle.generation_id)
}

fn turn_request(
    session_id: SessionId,
    generation_id: SessionGenerationId,
    turn_id: TurnId,
    prompt: &str,
) -> RunTurnRequest {
    RunTurnRequest {
        session_id,
        generation_id,
        turn: TurnRequest {
            turn_id,
            prompt: prompt.to_owned(),
            deadline_unix_ms: Some(DEADLINE_MS),
            lease: Default::default(),
        },
    }
}

async fn submit_and_wait(
    registry: &SessionRegistry,
    session_id: SessionId,
    generation_id: SessionGenerationId,
    turn_id: TurnId,
    prompt: &str,
) -> StoredTurnTerminal {
    registry
        .run_turn(turn_request(session_id, generation_id, turn_id, prompt))
        .await
        .unwrap();
    wait_for_terminal(registry, session_id, generation_id, turn_id).await
}

async fn wait_for_terminal(
    registry: &SessionRegistry,
    session_id: SessionId,
    generation_id: SessionGenerationId,
    turn_id: TurnId,
) -> StoredTurnTerminal {
    for _ in 0..500 {
        if let Some(terminal) = registry
            .stored_turn(session_id, generation_id, turn_id)
            .await
            .unwrap()
        {
            return terminal;
        }
        tokio::task::yield_now().await;
    }
    panic!("turn did not publish a terminal outcome");
}

async fn wait_for_state(
    registry: &SessionRegistry,
    session_id: SessionId,
    generation_id: SessionGenerationId,
    state: SessionState,
) {
    for _ in 0..500 {
        if registry
            .inspect(pseudomux_protocol::v1::InspectSessionRequest {
                session_id,
                generation_id,
            })
            .await
            .unwrap()
            .state
            == state
        {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("session did not reach {state:?}");
}

fn assert_timeout(terminal: &StoredTurnTerminal) {
    assert!(matches!(
        terminal,
        StoredTurnTerminal::Failed(error) if error.code == ErrorCode::TurnTimeout
    ));
}

async fn assert_terminal_event_kind(
    registry: &SessionRegistry,
    session_id: SessionId,
    generation_id: SessionGenerationId,
    turn_id: TurnId,
    success: bool,
) {
    let batch = registry
        .events(SubscribeEventsRequest {
            session_id,
            generation_id,
            after_sequence: 0,
            wait_ms: 0,
            max_events: 128,
        })
        .await
        .unwrap();
    let completed = batch.events.iter().filter(|event| {
        event.turn_id == Some(turn_id) && matches!(event.event, EventPayload::TurnCompleted(_))
    });
    let failed = batch.events.iter().filter(|event| {
        event.turn_id == Some(turn_id) && matches!(event.event, EventPayload::TurnFailed(_))
    });
    assert_eq!(completed.count(), usize::from(success));
    assert_eq!(failed.count(), usize::from(!success));
}

async fn assert_replay_event(
    registry: &SessionRegistry,
    session_id: SessionId,
    generation_id: SessionGenerationId,
    next_sequence: u64,
    success: bool,
) {
    let batch = registry
        .events(SubscribeEventsRequest {
            session_id,
            generation_id,
            after_sequence: next_sequence.saturating_sub(1),
            wait_ms: 0,
            max_events: 1,
        })
        .await
        .unwrap();
    assert_eq!(batch.events.len(), 1);
    assert_eq!(
        matches!(batch.events[0].event, EventPayload::TurnCompleted(_)),
        success
    );
    assert_eq!(
        matches!(batch.events[0].event, EventPayload::TurnFailed(_)),
        !success
    );
}

fn terminal_rows(prompt: &str, answer: &str) -> Vec<pseudomux_claude::ParsedRow> {
    let user = format!(
        r#"{{"parentUuid":null,"sessionId":"test","type":"user","message":{{"content":{prompt:?}}},"uuid":"prompt-row","promptSource":"typed","promptId":"prompt-id"}}"#
    );
    let assistant = format!(
        r#"{{"parentUuid":"prompt-row","sessionId":"test","type":"assistant","uuid":"answer-row","message":{{"id":"answer-message","model":"claude-test","content":[{{"type":"text","text":{answer:?}}}],"stop_reason":"end_turn","usage":{{"input_tokens":3,"output_tokens":2}}}}}}"#
    );
    [user, assistant]
        .into_iter()
        .enumerate()
        .map(|(index, json)| {
            JsonlParser::new(ParseMode::Strict)
                .parse(&CompleteLine {
                    location: SourceLocation {
                        line: index as u64 + 1,
                        byte_offset: index as u64 * 1_000,
                    },
                    bytes: json.into_bytes(),
                })
                .unwrap()
        })
        .collect()
}

fn id(value: u128) -> Uuid {
    Uuid::from_u128(value)
}
