#![allow(dead_code)]

use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pseudomux_protocol::v1::{
    ClosePolicy, CloseSessionRequest, CompatibilityReport, ErrorCode, InputTransport,
    InspectSessionRequest, RunTurnRequest, SessionGenerationId, SessionHandle, SessionId,
    TerminalProfile, TurnId, TurnRequest,
};
use pseudomux_service::v1::{
    Clock, DriverFailure, DriverResult, InterruptRecovery, SessionActorConfig, SessionCell,
    SessionRegistration, SessionRegistry, StoredTurnTerminal, TerminalControl, TerminalEvidence,
    TerminalScreenObservation, TranscriptArm, TranscriptBatch, TranscriptDrainEvidence,
    TranscriptPosition, TranscriptSource,
};
use tokio::sync::Notify;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Submission {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub prompt: String,
}

#[derive(Default)]
pub struct Probe {
    live_terminals: AtomicUsize,
    live_transcripts: AtomicUsize,
    submissions: Mutex<Vec<Submission>>,
    closes: AtomicUsize,
    interrupts: AtomicUsize,
}

impl Probe {
    pub fn live_terminals(&self) -> usize {
        self.live_terminals.load(Ordering::SeqCst)
    }

    pub fn live_transcripts(&self) -> usize {
        self.live_transcripts.load(Ordering::SeqCst)
    }

    pub fn submissions(&self) -> Vec<Submission> {
        self.submissions.lock().unwrap().clone()
    }

    pub fn closes(&self) -> usize {
        self.closes.load(Ordering::SeqCst)
    }

    pub fn interrupts(&self) -> usize {
        self.interrupts.load(Ordering::SeqCst)
    }
}

pub struct TestClock(AtomicU64);

impl TestClock {
    pub const fn starting_at(value: u64) -> Self {
        Self(AtomicU64::new(value))
    }
}

impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.0.fetch_add(1, Ordering::SeqCst)
    }
}

pub struct TestTerminal {
    probe: Arc<Probe>,
    submit_failure: Mutex<Option<DriverFailure>>,
    interrupt_recovery: InterruptRecovery,
    evidence: AtomicUsize,
    /// `TerminalEvidence::lifecycle_hook_at_ms`, kept beside the packed
    /// booleans; `0` reproduces the absent instant.
    evidence_hook_at_ms: AtomicU64,
    close_reaped: AtomicUsize,
    screen: Mutex<TerminalScreenObservation>,
    submission_notify: Notify,
}

impl TestTerminal {
    pub fn new(probe: Arc<Probe>) -> Self {
        Self::with_behavior(probe, None, InterruptRecovery::RecoveredToReady)
    }

    pub fn failing_submit(probe: Arc<Probe>, code: ErrorCode) -> Self {
        Self::with_behavior(
            probe,
            Some(DriverFailure::new(code, "injected submit failure")),
            InterruptRecovery::RecoveredToReady,
        )
    }

    pub fn with_behavior(
        probe: Arc<Probe>,
        submit_failure: Option<DriverFailure>,
        interrupt_recovery: InterruptRecovery,
    ) -> Self {
        probe.live_terminals.fetch_add(1, Ordering::SeqCst);
        Self {
            probe,
            submit_failure: Mutex::new(submit_failure),
            interrupt_recovery,
            evidence: AtomicUsize::new(0),
            evidence_hook_at_ms: AtomicU64::new(0),
            close_reaped: AtomicUsize::new(1),
            screen: Mutex::new(TerminalScreenObservation::Ready),
            submission_notify: Notify::new(),
        }
    }

    pub fn set_submit_failure(&self, failure: Option<DriverFailure>) {
        *self.submit_failure.lock().unwrap() = failure;
    }

    pub fn set_close_reaped(&self, process_reaped: bool) {
        self.close_reaped
            .store(usize::from(process_reaped), Ordering::SeqCst);
    }

    pub fn set_evidence(&self, evidence: TerminalEvidence) {
        let bits = usize::from(evidence.ready_prompt)
            | (usize::from(evidence.quiet) << 1)
            | (usize::from(evidence.lifecycle_expected) << 2)
            | (usize::from(evidence.lifecycle_hook_observed) << 3);
        self.evidence.store(bits, Ordering::SeqCst);
        self.evidence_hook_at_ms
            .store(evidence.lifecycle_hook_at_ms.unwrap_or(0), Ordering::SeqCst);
    }

    pub fn set_screen(&self, screen: TerminalScreenObservation) {
        *self.screen.lock().unwrap() = screen;
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

    pub async fn wait_for_submission(&self, session_id: SessionId, turn_id: TurnId) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let notified = self.submission_notify.notified();
                if self
                    .probe
                    .submissions
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|submission| {
                        submission.session_id == session_id && submission.turn_id == turn_id
                    })
                {
                    return;
                }
                notified.await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("turn {turn_id} in session {session_id} was never submitted"));
    }
}

impl Drop for TestTerminal {
    fn drop(&mut self) {
        self.probe.live_terminals.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl TerminalControl for TestTerminal {
    async fn submit_prompt(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        prompt: &str,
        _deadline_unix_ms: u64,
    ) -> DriverResult<()> {
        self.probe.submissions.lock().unwrap().push(Submission {
            session_id,
            turn_id,
            prompt: prompt.to_owned(),
        });
        self.submission_notify.notify_waiters();
        self.submit_failure
            .lock()
            .unwrap()
            .clone()
            .map_or(Ok(()), Err)
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
        self.probe.interrupts.fetch_add(1, Ordering::SeqCst);
        Ok(self.interrupt_recovery)
    }

    async fn close(&self, _session_id: SessionId, _policy: ClosePolicy) -> DriverResult<bool> {
        self.probe.closes.fetch_add(1, Ordering::SeqCst);
        Ok(self.close_reaped.load(Ordering::SeqCst) != 0)
    }
}

pub struct TestTranscript {
    probe: Arc<Probe>,
    arm_failure: Option<DriverFailure>,
    tempdir: Option<tempfile::TempDir>,
}

impl TestTranscript {
    pub fn pending(probe: Arc<Probe>) -> Self {
        Self::with_behavior(probe, None, None)
    }

    pub fn with_tempdir(
        probe: Arc<Probe>,
        arm_failure: Option<DriverFailure>,
        tempdir: tempfile::TempDir,
    ) -> Self {
        Self::with_behavior(probe, arm_failure, Some(tempdir))
    }

    fn with_behavior(
        probe: Arc<Probe>,
        arm_failure: Option<DriverFailure>,
        tempdir: Option<tempfile::TempDir>,
    ) -> Self {
        probe.live_transcripts.fetch_add(1, Ordering::SeqCst);
        Self {
            probe,
            arm_failure,
            tempdir,
        }
    }
}

impl Drop for TestTranscript {
    fn drop(&mut self) {
        drop(self.tempdir.take());
        self.probe.live_transcripts.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl TranscriptSource for TestTranscript {
    async fn arm_at_eof(&self, _session_id: SessionId) -> DriverResult<TranscriptArm> {
        self.arm_failure
            .clone()
            .map_or_else(|| Ok(TranscriptArm::default()), Err)
    }

    async fn poll(
        &self,
        _session_id: SessionId,
        position: &TranscriptPosition,
    ) -> DriverResult<TranscriptBatch> {
        Ok(TranscriptBatch {
            position: position.clone(),
            rows: Vec::new(),
            drain: TranscriptDrainEvidence {
                at_eof: true,
                has_partial_line: false,
                stable_for_ms: u64::MAX,
            },
        })
    }
}

pub fn actor_config() -> SessionActorConfig {
    SessionActorConfig {
        replay_capacity: 1_024,
        replay_byte_capacity: 16 * 1_024 * 1_024,
        default_event_batch_size: 128,
        poll_interval: Duration::from_millis(1),
        cancel_recovery_timeout: Duration::from_millis(50),
        attach_reconciliation_timeout: Duration::from_millis(50),
        default_turn_timeout_ms: 60_000,
        idle_ttl_ms: 30 * 60 * 1_000,
        turn_history_capacity: 128,
        turn_history_byte_capacity: 64 * 1_024 * 1_024,
        max_frame_bytes: pseudomux_protocol::v1::MAX_NATIVE_FRAME_BYTES,
        event_sequence_ceiling: pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER - 1,
    }
}

pub fn registry_with_config(config: SessionActorConfig) -> SessionRegistry {
    SessionRegistry::with_clock(config, Arc::new(TestClock::starting_at(1_000_000)))
}

pub async fn register(
    registry: &SessionRegistry,
    session_id: SessionId,
    terminal: Arc<TestTerminal>,
    transcript: Arc<TestTranscript>,
    cwd: &Path,
) -> SessionHandle {
    register_owned(
        registry,
        pseudomux_service::v1::SessionOwner::Caller,
        session_id,
        terminal,
        transcript,
        cwd,
    )
    .await
}

pub async fn register_owned(
    registry: &SessionRegistry,
    owner: pseudomux_service::v1::SessionOwner,
    session_id: SessionId,
    terminal: Arc<TestTerminal>,
    transcript: Arc<TestTranscript>,
    cwd: &Path,
) -> SessionHandle {
    registry
        .register(SessionRegistration {
            owner,
            session_id,
            generation_id: generation(session_id),
            cwd: cwd.to_string_lossy().into_owned(),
            compatibility: CompatibilityReport {
                claude_version: "test".to_owned(),
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
            agent: None,
            idle_ttl_ms: None,
            initial_needs_input: None,
            terminal,
            transcript,
        })
        .await
        .unwrap()
}

pub fn turn(session_id: SessionId, turn_id: TurnId, prompt: impl Into<String>) -> RunTurnRequest {
    RunTurnRequest {
        session_id,
        generation_id: generation(session_id),
        turn: TurnRequest {
            turn_id,
            prompt: prompt.into(),
            deadline_unix_ms: None,
            lease: Default::default(),
        },
    }
}

pub async fn wait_for_stored_turn(
    registry: &SessionRegistry,
    session_id: SessionId,
    turn_id: TurnId,
) -> StoredTurnTerminal {
    for _ in 0..2_048 {
        if let Some(terminal) = registry
            .stored_turn(session_id, generation(session_id), turn_id)
            .await
            .unwrap()
        {
            return terminal;
        }
        tokio::task::yield_now().await;
    }
    panic!("turn {turn_id} did not terminate within the bounded scheduler budget");
}

pub async fn close_and_unregister(registry: &SessionRegistry, session_id: SessionId) {
    let closed = registry
        .close(CloseSessionRequest {
            session_id,
            generation_id: generation(session_id),
            policy: ClosePolicy::Force,
        })
        .await
        .unwrap();
    assert!(closed.process_reaped);

    unregister_after_exit(registry, session_id).await;
}

pub async fn unregister_after_exit(registry: &SessionRegistry, session_id: SessionId) {
    let mut exited = false;

    for _ in 0..2_048 {
        match registry
            .inspect(InspectSessionRequest {
                session_id,
                generation_id: generation(session_id),
            })
            .await
        {
            Err(error) if error.code == ErrorCode::DaemonLost => {
                exited = true;
                break;
            }
            Ok(_) => tokio::task::yield_now().await,
            Err(error) => panic!("unexpected actor-exit error: {:?}", error.code),
        }
    }
    assert!(
        exited,
        "actor did not exit within the bounded scheduler budget"
    );
    registry
        .unregister(session_id, generation(session_id))
        .await
        .unwrap();
    let error = match registry.actor(session_id, generation(session_id)).await {
        Ok(_) => panic!("unregistered actor remained addressable"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::SessionNotFound);
}

pub async fn wait_for_resources_released(probe: &Probe) {
    for _ in 0..4_096 {
        if probe.live_terminals() == 0 && probe.live_transcripts() == 0 {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!(
        "owned resources remained live: terminals={}, transcripts={}",
        probe.live_terminals(),
        probe.live_transcripts()
    );
}

pub const fn generation(session_id: SessionId) -> SessionGenerationId {
    SessionGenerationId::from_u128(session_id.as_u128() ^ (1_u128 << 127))
}

pub const fn id(value: u128) -> uuid::Uuid {
    uuid::Uuid::from_u128(value)
}
