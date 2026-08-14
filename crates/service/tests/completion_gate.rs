mod support;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pseudomux_claude::{CompleteLine, JsonlParser, ParseMode, ParsedRow, SourceLocation};
use pseudomux_protocol::v1::{
    ClosePolicy, CompatibilityReport, ErrorCode, InputTransport, NeedsInput, NeedsInputKind,
    SessionId, TerminalProfile, TurnId, TurnOutcome,
};
use pseudomux_service::v1::{
    DriverFailure, DriverResult, InterruptRecovery, SessionCell, SessionRegistration,
    SessionRegistry, StoredTurnTerminal, TerminalControl, TerminalEvidence,
    TerminalScreenObservation, TranscriptArm, TranscriptBatch, TranscriptDrainEvidence,
    TranscriptPosition, TranscriptSource,
};
use tokio::sync::Notify;

use support::{actor_config, close_and_unregister, generation, turn};

#[derive(Clone, Copy, Debug)]
enum CompletionFactor {
    PromptAcknowledgement,
    TerminalCandidate,
    StableCursor,
    AtEof,
    NoPartialLine,
    DrainElapsed,
    ReadyPrompt,
    Quiet,
    ModalAbsent,
}

struct CompletionInputs {
    prompt_acknowledgement: AtomicBool,
    terminal_candidate: AtomicBool,
    stable_cursor: AtomicBool,
    at_eof: AtomicBool,
    no_partial_line: AtomicBool,
    drain_elapsed: AtomicBool,
    ready_prompt: AtomicBool,
    quiet: AtomicBool,
    modal_absent: AtomicBool,
    lease_healthy: AtomicBool,
}

impl CompletionInputs {
    fn all_satisfied() -> Self {
        Self {
            prompt_acknowledgement: AtomicBool::new(true),
            terminal_candidate: AtomicBool::new(true),
            stable_cursor: AtomicBool::new(true),
            at_eof: AtomicBool::new(true),
            no_partial_line: AtomicBool::new(true),
            drain_elapsed: AtomicBool::new(true),
            ready_prompt: AtomicBool::new(true),
            quiet: AtomicBool::new(true),
            modal_absent: AtomicBool::new(true),
            lease_healthy: AtomicBool::new(true),
        }
    }

    fn block(&self, factor: CompletionFactor) {
        self.set(factor, false);
    }

    fn satisfy(&self, factor: CompletionFactor) {
        self.set(factor, true);
    }

    fn set(&self, factor: CompletionFactor, value: bool) {
        let input = match factor {
            CompletionFactor::PromptAcknowledgement => &self.prompt_acknowledgement,
            CompletionFactor::TerminalCandidate => &self.terminal_candidate,
            CompletionFactor::StableCursor => &self.stable_cursor,
            CompletionFactor::AtEof => &self.at_eof,
            CompletionFactor::NoPartialLine => &self.no_partial_line,
            CompletionFactor::DrainElapsed => &self.drain_elapsed,
            CompletionFactor::ReadyPrompt => &self.ready_prompt,
            CompletionFactor::Quiet => &self.quiet,
            CompletionFactor::ModalAbsent => &self.modal_absent,
        };
        input.store(value, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct TranscriptState {
    prompt_emitted: bool,
    candidate_emitted: bool,
    position: u64,
}

struct FactorTranscript {
    inputs: Arc<CompletionInputs>,
    state: Mutex<TranscriptState>,
    polls: AtomicUsize,
    poll_changed: Notify,
}

impl FactorTranscript {
    fn new(inputs: Arc<CompletionInputs>) -> Self {
        Self {
            inputs,
            state: Mutex::new(TranscriptState::default()),
            polls: AtomicUsize::new(0),
            poll_changed: Notify::new(),
        }
    }

    async fn wait_for_polls(&self, minimum: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let changed = self.poll_changed.notified();
                if self.polls.load(Ordering::SeqCst) >= minimum {
                    return;
                }
                changed.await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("transcript did not reach {minimum} polls"));
    }
}

#[async_trait]
impl TranscriptSource for FactorTranscript {
    async fn arm_at_eof(&self, _session_id: SessionId) -> DriverResult<TranscriptArm> {
        Ok(TranscriptArm::default())
    }

    async fn poll(
        &self,
        _session_id: SessionId,
        _position: &TranscriptPosition,
    ) -> DriverResult<TranscriptBatch> {
        // Real transcript polling performs filesystem I/O. Preserve that
        // scheduling boundary so a deliberately unstable cursor cannot turn
        // this mutation test into a single-threaded executor busy loop.
        tokio::task::yield_now().await;
        let mut state = self.state.lock().unwrap();
        let mut rows = Vec::new();
        if self.inputs.prompt_acknowledgement.load(Ordering::SeqCst) && !state.prompt_emitted {
            rows.push(typed_prompt_row());
            state.prompt_emitted = true;
            state.position += 1;
        }
        if state.prompt_emitted
            && self.inputs.terminal_candidate.load(Ordering::SeqCst)
            && !state.candidate_emitted
        {
            rows.push(terminal_candidate_row());
            state.candidate_emitted = true;
            state.position += 1;
        } else if state.candidate_emitted
            && rows.is_empty()
            && !self.inputs.stable_cursor.load(Ordering::SeqCst)
        {
            // Deliberately report a newer opaque position while every drain
            // flag remains positive. Keeping those inputs independent makes
            // this mutation fail only if the post-terminal cursor fence is
            // removed or weakened.
            state.position += 1;
        }
        let batch = TranscriptBatch {
            position: TranscriptPosition {
                generation: 0,
                offset: state.position,
            },
            rows,
            drain: TranscriptDrainEvidence {
                at_eof: self.inputs.at_eof.load(Ordering::SeqCst),
                has_partial_line: !self.inputs.no_partial_line.load(Ordering::SeqCst),
                stable_for_ms: if self.inputs.drain_elapsed.load(Ordering::SeqCst) {
                    100
                } else {
                    0
                },
            },
        };
        drop(state);
        self.polls.fetch_add(1, Ordering::SeqCst);
        self.poll_changed.notify_waiters();
        Ok(batch)
    }
}

struct FactorTerminal {
    inputs: Arc<CompletionInputs>,
    completion_checks: AtomicUsize,
    completion_changed: Notify,
}

impl FactorTerminal {
    fn new(inputs: Arc<CompletionInputs>) -> Self {
        Self {
            inputs,
            completion_checks: AtomicUsize::new(0),
            completion_changed: Notify::new(),
        }
    }

    async fn wait_for_completion_checks(&self, minimum: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let changed = self.completion_changed.notified();
                if self.completion_checks.load(Ordering::SeqCst) >= minimum {
                    return;
                }
                changed.await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("terminal did not reach {minimum} completion checks"));
    }
}

#[async_trait]
impl TerminalControl for FactorTerminal {
    async fn submit_prompt(
        &self,
        _session_id: SessionId,
        _turn_id: TurnId,
        _prompt: &str,
        _deadline_unix_ms: u64,
    ) -> DriverResult<()> {
        Ok(())
    }

    async fn completion_evidence(
        &self,
        _session_id: SessionId,
        _turn_id: TurnId,
    ) -> DriverResult<TerminalEvidence> {
        tokio::task::yield_now().await;
        self.completion_checks.fetch_add(1, Ordering::SeqCst);
        self.completion_changed.notify_waiters();
        if !self.inputs.lease_healthy.load(Ordering::SeqCst) {
            return Err(DriverFailure::new(
                ErrorCode::DaemonLost,
                "injected private lease loss at completion",
            )
            .retryable(true));
        }
        Ok(TerminalEvidence {
            ready_prompt: self.inputs.ready_prompt.load(Ordering::SeqCst),
            quiet: self.inputs.quiet.load(Ordering::SeqCst),
            ..TerminalEvidence::default()
        })
    }

    async fn observe_screen(
        &self,
        _session_id: SessionId,
    ) -> DriverResult<TerminalScreenObservation> {
        if self.inputs.modal_absent.load(Ordering::SeqCst) {
            Ok(TerminalScreenObservation::Ready)
        } else {
            Ok(TerminalScreenObservation::NeedsInput(NeedsInput {
                kind: NeedsInputKind::Permission,
                message: "explicit test permission is required".to_owned(),
                details: serde_json::Value::Null,
            }))
        }
    }

    async fn interrupt(
        &self,
        _session_id: SessionId,
        _turn_id: TurnId,
    ) -> DriverResult<InterruptRecovery> {
        Ok(InterruptRecovery::RecoveredToReady)
    }

    async fn close(&self, _session_id: SessionId, _policy: ClosePolicy) -> DriverResult<bool> {
        Ok(true)
    }
}

#[tokio::test]
async fn every_completion_input_independently_blocks_commit_until_satisfied() {
    let factors = [
        CompletionFactor::PromptAcknowledgement,
        CompletionFactor::TerminalCandidate,
        CompletionFactor::StableCursor,
        CompletionFactor::AtEof,
        CompletionFactor::NoPartialLine,
        CompletionFactor::DrainElapsed,
        CompletionFactor::ReadyPrompt,
        CompletionFactor::Quiet,
        CompletionFactor::ModalAbsent,
    ];

    for (index, factor) in factors.into_iter().enumerate() {
        let registry = SessionRegistry::new(actor_config());
        let session_id = SessionId::from_u128(0x7000 + index as u128);
        let turn_id = TurnId::from_u128(0x8000 + index as u128);
        let inputs = Arc::new(CompletionInputs::all_satisfied());
        inputs.block(factor);
        let terminal = Arc::new(FactorTerminal::new(Arc::clone(&inputs)));
        let transcript = Arc::new(FactorTranscript::new(Arc::clone(&inputs)));
        register(
            &registry,
            session_id,
            Arc::clone(&terminal),
            Arc::clone(&transcript),
        )
        .await;
        registry
            .run_turn(turn(session_id, turn_id, "completion gate"))
            .await
            .unwrap();

        match factor {
            CompletionFactor::PromptAcknowledgement
            | CompletionFactor::TerminalCandidate
            | CompletionFactor::ModalAbsent => transcript.wait_for_polls(5).await,
            _ => terminal.wait_for_completion_checks(3).await,
        }
        let blocked_terminal = registry
            .stored_turn(session_id, generation(session_id), turn_id)
            .await
            .unwrap();
        assert!(
            blocked_terminal.is_none(),
            "{factor:?} was not independently required: {blocked_terminal:?}"
        );

        inputs.satisfy(factor);
        let StoredTurnTerminal::Result(result) =
            wait_for_terminal(&registry, session_id, turn_id).await
        else {
            panic!("{factor:?} did not permit the positive completion baseline");
        };
        assert_eq!(result.outcome, TurnOutcome::Completed);
        assert_eq!(result.text, "answer");
        assert!(result.completion.prompt_acknowledged);
        assert!(result.completion.terminal_message_observed);
        assert!(result.completion.terminal_prompt_observed);
        assert!(result.completion.terminal_quiet_observed);
        assert!(result.completion.transcript_drained);
        close_and_unregister(&registry, session_id).await;
    }
}

#[tokio::test]
async fn completion_time_lease_loss_fails_without_a_success_commit() {
    let registry = SessionRegistry::new(actor_config());
    let session_id = SessionId::from_u128(0x9000);
    let turn_id = TurnId::from_u128(0x9001);
    let inputs = Arc::new(CompletionInputs::all_satisfied());
    inputs.lease_healthy.store(false, Ordering::SeqCst);
    let terminal = Arc::new(FactorTerminal::new(Arc::clone(&inputs)));
    let transcript = Arc::new(FactorTranscript::new(inputs));
    register(&registry, session_id, Arc::clone(&terminal), transcript).await;
    registry
        .run_turn(turn(session_id, turn_id, "completion gate"))
        .await
        .unwrap();

    let StoredTurnTerminal::Failed(error) = wait_for_terminal(&registry, session_id, turn_id).await
    else {
        panic!("lease loss must never store a successful result");
    };
    assert_eq!(error.code, ErrorCode::DaemonLost);
    assert!(error.retryable);
    close_and_unregister(&registry, session_id).await;
}

async fn register(
    registry: &SessionRegistry,
    session_id: SessionId,
    terminal: Arc<FactorTerminal>,
    transcript: Arc<FactorTranscript>,
) {
    registry
        .register(SessionRegistration {
            owner: pseudomux_service::v1::SessionOwner::Caller,
            session_id,
            generation_id: generation(session_id),
            cwd: "/completion-gate".to_owned(),
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
}

async fn wait_for_terminal(
    registry: &SessionRegistry,
    session_id: SessionId,
    turn_id: TurnId,
) -> StoredTurnTerminal {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(terminal) = registry
                .stored_turn(session_id, generation(session_id), turn_id)
                .await
                .unwrap()
            {
                return terminal;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("turn {turn_id} did not become terminal"))
}

fn typed_prompt_row() -> ParsedRow {
    parse_line(
        0,
        br#"{"parentUuid":null,"sessionId":"test","type":"user","message":{"content":"completion gate"},"uuid":"prompt-row","promptSource":"typed","promptId":"prompt-id"}"#,
    )
}

fn terminal_candidate_row() -> ParsedRow {
    parse_line(
        1,
        br#"{"parentUuid":"prompt-row","sessionId":"test","type":"assistant","uuid":"answer-row","message":{"id":"answer-message","model":"claude-test","content":[{"type":"text","text":"answer"}],"stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":2}}}"#,
    )
}

fn parse_line(index: usize, bytes: &[u8]) -> ParsedRow {
    JsonlParser::new(ParseMode::Strict)
        .parse(&CompleteLine {
            location: SourceLocation {
                line: index as u64 + 1,
                byte_offset: index as u64 * 1_000,
            },
            bytes: bytes.to_vec(),
        })
        .unwrap()
}
