//! The two Path B capabilities, exercised through the actor that owns them.
//!
//! Both are about one asymmetry: returning before the work is done is
//! unacceptable, refusing to return is merely bad. So the interesting assertions
//! here are the negative ones -- a turn that did NOT complete on the shorter
//! proof, a session that will NOT accept a turn after a failed rebind -- and
//! each of them is anchored against a positive baseline on the same fixture, so
//! a test that stops meaning anything fails rather than silently passing.

mod support;

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pseudomux_claude::{CompleteLine, JsonlParser, ParseMode, ParsedRow, SourceLocation};
use pseudomux_protocol::v1::{
    CompatibilityReport, ErrorCode, InputTransport, InspectSessionRequest, NeedsInput,
    NeedsInputKind, SessionId, SessionState, TerminalProfile, TurnId, TurnOutcome,
};
use pseudomux_service::v1::{
    ClearRebind, DriverFailure, DriverResult, MINIFIED_FAST_PATH_DRAIN_FLOOR_MS, SessionCell,
    SessionRegistration, SessionRegistry, StoredTurnTerminal, TURN_DURATION_DRAIN_FLOOR_MS,
    TerminalEvidence, TerminalScreenObservation, TranscriptArm, TranscriptBatch,
    TranscriptDrainEvidence, TranscriptPosition, TranscriptSource, WritableAttachCompletion,
};
use tokio::sync::Notify;

use support::{Probe, TestTerminal, actor_config, generation, turn};

/// The Full cell's configured drain. Above the marker floor, so a turn carrying
/// `turn_duration` owes that floor rather than this.
const FULL_DRAIN_MS: u64 = 400;
/// Observed transcript stability that clears the minified floor and not the
/// Full cell's marker floor. The whole suite turns on this band.
const SHORT_STABILITY_MS: u64 = 100;
/// Observed stability that clears every requirement.
const LONG_STABILITY_MS: u64 = 300;

// The band these tests distinguish, asserted rather than assumed. Every timing
// case below is a comparison between the three windows, so a constant that
// moved far enough to collapse two of them would turn this suite into a set of
// tests that pass without separating anything. Stated here, that shows up as a
// compile error naming the collapse instead of a timeout somewhere else.
const _: () = assert!(MINIFIED_FAST_PATH_DRAIN_FLOOR_MS <= SHORT_STABILITY_MS);
const _: () = assert!(SHORT_STABILITY_MS < TURN_DURATION_DRAIN_FLOOR_MS);
const _: () = assert!(TURN_DURATION_DRAIN_FLOOR_MS <= LONG_STABILITY_MS);
const _: () = assert!(TURN_DURATION_DRAIN_FLOOR_MS < FULL_DRAIN_MS);

const PROMPT: &str = "minified cell";

// -- fixtures ---------------------------------------------------------------

/// The calibrated Path B turn: one text-only answer that stopped with
/// `end_turn`, followed by the `turn_duration` marker.
fn calibrated_rows() -> Vec<ParsedRow> {
    vec![
        parse_line(0, prompt_row()),
        parse_line(
            1,
            r#"{"parentUuid":"prompt-row","sessionId":"s","type":"assistant","uuid":"answer-row","message":{"model":"claude-test","id":"answer-message","content":[{"type":"text","text":"answer"}],"stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":2}}}"#.to_owned(),
        ),
        parse_line(2, turn_duration_row("answer-row", "duration")),
    ]
}

/// The same turn with a tool actually run. On a cell launched with
/// `--disallowedTools "*"` this cannot happen, which is why checks 1 and 2 read
/// it as the launch bundle having failed to take effect.
fn tool_using_rows() -> Vec<ParsedRow> {
    vec![
        parse_line(0, prompt_row()),
        parse_line(
            1,
            r#"{"parentUuid":"prompt-row","sessionId":"s","type":"assistant","uuid":"tool-row","message":{"model":"claude-test","id":"tool-message","content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"README.md"}}],"stop_reason":"tool_use","usage":{"input_tokens":3,"output_tokens":2}}}"#.to_owned(),
        ),
        parse_line(
            2,
            r##"{"parentUuid":"tool-row","sessionId":"s","type":"user","uuid":"tool-result-row","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"# Project"}]}}"##.to_owned(),
        ),
        parse_line(
            3,
            r#"{"parentUuid":"tool-result-row","sessionId":"s","type":"assistant","uuid":"answer-row","message":{"model":"claude-test","id":"answer-message","content":[{"type":"text","text":"answer"}],"stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":2}}}"#.to_owned(),
        ),
        parse_line(4, turn_duration_row("answer-row", "duration")),
    ]
}

fn prompt_row() -> String {
    format!(
        r#"{{"parentUuid":null,"sessionId":"s","type":"user","message":{{"role":"user","content":{PROMPT:?}}},"uuid":"prompt-row","promptSource":"typed","promptId":"prompt-id"}}"#
    )
}

fn turn_duration_row(parent: &str, uuid: &str) -> String {
    format!(
        r#"{{"parentUuid":"{parent}","sessionId":"s","type":"system","subtype":"turn_duration","uuid":"{uuid}","durationMs":42,"messageCount":1}}"#
    )
}

fn parse_line(index: usize, bytes: String) -> ParsedRow {
    JsonlParser::new(ParseMode::Strict)
        .parse(&CompleteLine {
            location: SourceLocation {
                line: index as u64 + 1,
                byte_offset: index as u64 * 1_000,
            },
            bytes: bytes.into_bytes(),
        })
        .expect("fixture rows are valid strict-mode JSONL")
}

// -- doubles ----------------------------------------------------------------

#[derive(Clone, Debug, Eq, PartialEq)]
enum TailEvent {
    Armed(SessionId),
    Polled(SessionId),
}

/// A transcript tail that enforces the same authority boundary the real one
/// does: a poll under an id the tail is not armed on REFUSES rather than
/// following the rotation.
///
/// That refusal is the reason this double exists. If the actor ever polled a
/// rotated id without re-arming, a permissive double would let the turn pass and
/// the regression would only appear against a real Claude.
struct ScriptedTranscript {
    rows: Vec<ParsedRow>,
    armed: Mutex<Option<SessionId>>,
    events: Mutex<Vec<TailEvent>>,
    emitted: AtomicBool,
    offset: AtomicU64,
    stable_for_ms: AtomicU64,
    polls: AtomicUsize,
    poll_changed: Notify,
    arm_failure: Mutex<Option<DriverFailure>>,
    /// What the launch half of assert-empty answers for this source. `Ok` is the
    /// clean launch transcript every other test in this file assumes; a fixture
    /// sets the refusal to stand in for a transcript that already served work.
    launch_proof: Mutex<DriverResult<()>>,
    launch_proofs: AtomicUsize,
}

impl ScriptedTranscript {
    fn new(rows: Vec<ParsedRow>) -> Self {
        Self {
            rows,
            armed: Mutex::new(None),
            events: Mutex::new(Vec::new()),
            emitted: AtomicBool::new(false),
            offset: AtomicU64::new(0),
            stable_for_ms: AtomicU64::new(SHORT_STABILITY_MS),
            polls: AtomicUsize::new(0),
            poll_changed: Notify::new(),
            arm_failure: Mutex::new(None),
            launch_proof: Mutex::new(Ok(())),
            launch_proofs: AtomicUsize::new(0),
        }
    }

    fn refuse_launch_proof(&self, failure: DriverFailure) {
        *self.launch_proof.lock().unwrap() = Err(failure);
    }

    fn launch_proofs(&self) -> usize {
        self.launch_proofs.load(Ordering::SeqCst)
    }

    fn set_stability(&self, stable_for_ms: u64) {
        self.stable_for_ms.store(stable_for_ms, Ordering::SeqCst);
    }

    fn fail_next_arm(&self, failure: DriverFailure) {
        *self.arm_failure.lock().unwrap() = Some(failure);
    }

    fn events(&self) -> Vec<TailEvent> {
        self.events.lock().unwrap().clone()
    }

    fn polls(&self) -> usize {
        self.polls.load(Ordering::SeqCst)
    }

    async fn wait_for_polls(&self, minimum: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
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
impl TranscriptSource for ScriptedTranscript {
    async fn arm_at_eof(&self, session_id: SessionId) -> DriverResult<TranscriptArm> {
        if let Some(failure) = self.arm_failure.lock().unwrap().take() {
            return Err(failure);
        }
        self.events
            .lock()
            .unwrap()
            .push(TailEvent::Armed(session_id));
        *self.armed.lock().unwrap() = Some(session_id);
        self.emitted.store(false, Ordering::SeqCst);
        self.offset.store(0, Ordering::SeqCst);
        Ok(TranscriptArm::default())
    }

    async fn poll(
        &self,
        session_id: SessionId,
        position: &TranscriptPosition,
    ) -> DriverResult<TranscriptBatch> {
        // Real polling performs filesystem I/O; keep the scheduling boundary so
        // a busy gate cannot starve the runtime.
        tokio::task::yield_now().await;
        self.events
            .lock()
            .unwrap()
            .push(TailEvent::Polled(session_id));
        if *self.armed.lock().unwrap() != Some(session_id) {
            return Err(DriverFailure::new(
                ErrorCode::SchemaDrift,
                "transcript tail is not armed on the polled session",
            )
            .with_details(serde_json::json!({
                "field": "session_id",
                "violation": "rebind_requires_rearm",
            })));
        }
        let rows = if self.emitted.swap(true, Ordering::SeqCst) {
            Vec::new()
        } else {
            self.offset.store(self.rows.len() as u64, Ordering::SeqCst);
            self.rows.clone()
        };
        let batch = TranscriptBatch {
            position: TranscriptPosition {
                generation: 0,
                offset: self.offset.load(Ordering::SeqCst).max(position.offset),
            },
            rows,
            drain: TranscriptDrainEvidence {
                at_eof: true,
                has_partial_line: false,
                stable_for_ms: self.stable_for_ms.load(Ordering::SeqCst),
            },
        };
        self.polls.fetch_add(1, Ordering::SeqCst);
        self.poll_changed.notify_waiters();
        Ok(batch)
    }

    async fn assert_empty_at_launch(&self, _session_id: SessionId) -> DriverResult<()> {
        self.launch_proofs.fetch_add(1, Ordering::SeqCst);
        self.launch_proof.lock().unwrap().clone()
    }
}

/// A `/clear` that answers exactly what the test says it answers.
struct ScriptedClear {
    outcome: Mutex<Result<SessionId, DriverFailure>>,
    calls: Mutex<Vec<SessionId>>,
}

impl ScriptedClear {
    fn rotating_to(successor: SessionId) -> Self {
        Self {
            outcome: Mutex::new(Ok(successor)),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn failing(failure: DriverFailure) -> Self {
        Self {
            outcome: Mutex::new(Err(failure)),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<SessionId> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl ClearRebind for ScriptedClear {
    async fn clear_and_rebind(
        &self,
        session_id: SessionId,
        _deadline_unix_ms: u64,
    ) -> DriverResult<SessionId> {
        self.calls.lock().unwrap().push(session_id);
        self.outcome.lock().unwrap().clone()
    }
}

// -- harness ----------------------------------------------------------------

struct Cell {
    registry: SessionRegistry,
    session_id: SessionId,
    terminal: Arc<TestTerminal>,
    transcript: Arc<ScriptedTranscript>,
}

impl Cell {
    async fn start(seed: u128, rows: Vec<ParsedRow>, tested: bool) -> Self {
        Self::start_with(seed, rows, tested, SessionCell::Full)
            .await
            .expect("registration")
    }

    async fn start_with(
        seed: u128,
        rows: Vec<ParsedRow>,
        tested: bool,
        cell: SessionCell,
    ) -> Result<Self, pseudomux_protocol::v1::ErrorBody> {
        Self::start_with_transcript(seed, rows, tested, cell, |_| {}).await
    }

    /// The same registration with one hook: `prepare` runs on the transcript
    /// double before it is handed to the registry, which is the only window in
    /// which a launch-time property of the transcript can be set.
    async fn start_with_transcript(
        seed: u128,
        rows: Vec<ParsedRow>,
        tested: bool,
        cell: SessionCell,
        prepare: impl FnOnce(&Arc<ScriptedTranscript>),
    ) -> Result<Self, pseudomux_protocol::v1::ErrorBody> {
        let registry = SessionRegistry::new(actor_config());
        let session_id = SessionId::from_u128(seed);
        let terminal = Arc::new(TestTerminal::new(Arc::new(Probe::default())));
        terminal.set_evidence(TerminalEvidence {
            ready_prompt: true,
            quiet: true,
            ..TerminalEvidence::default()
        });
        let transcript = Arc::new(ScriptedTranscript::new(rows));
        prepare(&transcript);
        registry
            .register(SessionRegistration {
                agent: None,
                owner: pseudomux_service::v1::SessionOwner::Caller,
                session_id,
                generation_id: generation(session_id),
                cwd: "/minified-cell".to_owned(),
                compatibility: CompatibilityReport {
                    claude_version: "test".to_owned(),
                    os: std::env::consts::OS.to_owned(),
                    arch: std::env::consts::ARCH.to_owned(),
                    terminal_profile: TerminalProfile::Transparent,
                    input_transport: InputTransport::Sdk,
                    tested,
                    transcript_drain_ms: FULL_DRAIN_MS,
                },
                dangerous_permission_bypass: false,
                resumable: true,
                cell,
                idle_ttl_ms: None,
                initial_needs_input: None,
                terminal: Arc::clone(&terminal) as Arc<_>,
                transcript: Arc::clone(&transcript) as Arc<_>,
            })
            .await?;
        Ok(Self {
            registry,
            session_id,
            terminal,
            transcript,
        })
    }

    /// Registers a session that is a minified cell from birth -- the shape the
    /// wire path produces, where the cell is a `start_session` field and never a
    /// later mutation.
    async fn start_as_minified(
        seed: u128,
        rows: Vec<ParsedRow>,
        tested: bool,
    ) -> Result<Self, pseudomux_protocol::v1::ErrorBody> {
        let cell = Self::start_with(seed, rows, tested, SessionCell::Minified).await?;
        Ok(cell)
    }

    async fn select_minified(&self) -> Result<(), pseudomux_protocol::v1::ErrorBody> {
        self.actor().await.select_minified_cell().await
    }

    async fn reserve_writable_attach(
        &self,
        attach_id: uuid::Uuid,
    ) -> Result<(), pseudomux_protocol::v1::ErrorBody> {
        self.registry
            .reserve_writable_attach(self.session_id, generation(self.session_id), attach_id)
            .await
    }

    async fn clear_session(
        &self,
        expected: SessionId,
        rotated: SessionId,
    ) -> Result<pseudomux_protocol::v1::ClearSessionResult, pseudomux_protocol::v1::ErrorBody> {
        self.actor()
            .await
            .clear_session(
                Arc::new(ScriptedClear::rotating_to(rotated)) as Arc<dyn ClearRebind>,
                expected,
                1,
            )
            .await
    }

    async fn actor(&self) -> pseudomux_service::v1::SessionActorHandle {
        self.registry
            .actor(self.session_id, generation(self.session_id))
            .await
            .expect("actor is addressable")
    }

    async fn submit(&self, turn_id: TurnId) {
        self.registry
            .run_turn(turn(self.session_id, turn_id, PROMPT))
            .await
            .expect("turn accepted");
    }

    async fn stored(&self, turn_id: TurnId) -> Option<StoredTurnTerminal> {
        self.registry
            .stored_turn(self.session_id, generation(self.session_id), turn_id)
            .await
            .expect("stored turn lookup")
    }

    async fn await_completion(&self, turn_id: TurnId) -> StoredTurnTerminal {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(terminal) = self.stored(turn_id).await {
                    return terminal;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("turn {turn_id} did not become terminal"))
    }

    /// Proves a turn did NOT commit while only the minified floor was
    /// satisfied, by checking for a stored outcome on every one of the first
    /// `polls` gate evaluations. A fast-path commit lands on the first
    /// evaluation, so a turn that survives several of them owed a longer
    /// window.
    ///
    /// Written as one loop rather than "wait for polls, then assert" so that a
    /// regression reports the commit it saw instead of a poll count it never
    /// reached.
    async fn assert_owes_more_than_the_minified_floor(&self, turn_id: TurnId, polls: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while self.transcript.polls() < polls {
                if let Some(terminal) = self.stored(turn_id).await {
                    panic!(
                        "turn committed on the minified floor rather than the Full cell's drain: \
                         {terminal:?}"
                    );
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("transcript did not reach {polls} polls"));
        assert!(
            self.stored(turn_id).await.is_none(),
            "turn committed on the minified floor rather than the Full cell's drain"
        );
    }

    async fn snapshot(&self) -> pseudomux_protocol::v1::SessionSnapshot {
        self.registry
            .inspect(InspectSessionRequest {
                session_id: self.session_id,
                generation_id: generation(self.session_id),
            })
            .await
            .expect("snapshot")
    }

    async fn state(&self) -> SessionState {
        self.snapshot().await.state
    }

    async fn await_state(&self, expected: SessionState) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while self.state().await != expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("session never reached {expected:?}"));
    }
}

fn completed(terminal: StoredTurnTerminal) -> Box<pseudomux_protocol::v1::TurnResult> {
    match terminal {
        StoredTurnTerminal::Result(result) => result,
        StoredTurnTerminal::Failed(error) => {
            panic!("turn failed instead of completing: {error:?}")
        }
    }
}

// -- the fast path ----------------------------------------------------------

/// The positive baseline, and the reason every negative assertion below means
/// something: on identical evidence the minified cell commits at a stability the
/// Full cell still refuses.
#[tokio::test]
async fn the_minified_cell_commits_on_a_stability_the_full_cell_refuses() {
    let fast = Cell::start(0xB000, calibrated_rows(), true).await;
    let slow = Cell::start(0xB001, calibrated_rows(), true).await;
    fast.select_minified().await.expect("cell selected");

    let turn_id = TurnId::from_u128(0xB100);
    fast.submit(turn_id).await;
    slow.submit(turn_id).await;

    let result = completed(fast.await_completion(turn_id).await);
    assert_eq!(result.outcome, TurnOutcome::Completed);
    assert_eq!(result.text, "answer");
    assert_eq!(result.timings.drain_ms, Some(SHORT_STABILITY_MS));

    // The Full cell has been polling the same evidence for at least as long,
    // and still owes its marker floor at this stability.
    slow.assert_owes_more_than_the_minified_floor(turn_id, 5)
        .await;

    slow.transcript.set_stability(LONG_STABILITY_MS);
    let result = completed(slow.await_completion(turn_id).await);
    assert_eq!(result.outcome, TurnOutcome::Completed);
    assert_eq!(result.text, "answer");
}

/// A turn that fails a check takes the slow path and still completes correctly.
/// A refusal is a statement about which proof may finish the turn, never about
/// whether the turn succeeded.
#[tokio::test]
async fn a_turn_that_fails_a_check_falls_back_to_the_full_proof_and_still_completes() {
    let cell = Cell::start(0xB010, tool_using_rows(), true).await;
    cell.select_minified().await.expect("cell selected");

    let turn_id = TurnId::from_u128(0xB110);
    cell.submit(turn_id).await;

    // Checks 1 and 2 both fire on this turn, so the shorter proof is refused
    // and the turn owes exactly what the Full cell owed.
    cell.assert_owes_more_than_the_minified_floor(turn_id, 5)
        .await;

    cell.transcript.set_stability(LONG_STABILITY_MS);
    let result = completed(cell.await_completion(turn_id).await);
    assert_eq!(result.outcome, TurnOutcome::Completed);
    assert_eq!(result.text, "answer");
    assert_eq!(result.tools.len(), 1);
    assert_eq!(result.tools[0].name, "Read");
    assert!(result.completion.prompt_acknowledged);
    assert!(result.completion.transcript_drained);
}

/// Check 9's stickiness, which is created at the call site and cannot be
/// recovered from live state: by the time this turn commits, the modal is gone.
/// A non-sticky reading would say "no modal" and admit the fast path.
#[tokio::test]
async fn a_modal_that_appeared_and_was_dismissed_still_refuses_the_fast_path() {
    let cell = Cell::start(0xB020, calibrated_rows(), true).await;
    cell.select_minified().await.expect("cell selected");
    cell.terminal
        .set_screen(TerminalScreenObservation::NeedsInput(NeedsInput {
            kind: NeedsInputKind::Permission,
            message: "explicit test permission is required".to_owned(),
            details: serde_json::Value::Null,
        }));

    let turn_id = TurnId::from_u128(0xB120);
    cell.submit(turn_id).await;
    cell.await_state(SessionState::NeedsInput).await;
    cell.terminal.set_screen(TerminalScreenObservation::Ready);

    cell.assert_owes_more_than_the_minified_floor(turn_id, 5)
        .await;

    cell.transcript.set_stability(LONG_STABILITY_MS);
    let result = completed(cell.await_completion(turn_id).await);
    assert_eq!(result.outcome, TurnOutcome::Completed);
    assert_eq!(result.text, "answer");
}

/// Off unless selected. A caller that never asked for Path B runs exactly the
/// proof it ran before, on the same evidence that admits the fast path above.
#[tokio::test]
async fn an_unselected_session_never_takes_the_fast_path() {
    let cell = Cell::start(0xB030, calibrated_rows(), true).await;

    let turn_id = TurnId::from_u128(0xB130);
    cell.submit(turn_id).await;
    cell.assert_owes_more_than_the_minified_floor(turn_id, 5)
        .await;

    cell.transcript.set_stability(LONG_STABILITY_MS);
    assert_eq!(
        completed(cell.await_completion(turn_id).await).text,
        "answer"
    );
}

/// Unselectable on an uncalibrated host, under the same require-tested rule
/// every other compatibility decision uses. An untested cell reached this far
/// only through an explicit `allow_untested` request.
#[tokio::test]
async fn an_untested_compatibility_cell_cannot_select_the_minified_cell() {
    let cell = Cell::start(0xB040, calibrated_rows(), false).await;

    let error = cell.select_minified().await.expect_err("must be refused");
    assert_eq!(error.code, ErrorCode::UnsupportedClaudeVersion);

    // And the capability it gates is refused with it.
    let clear = Arc::new(ScriptedClear::rotating_to(SessionId::from_u128(0xB041)));
    let error = cell
        .actor()
        .await
        .clear_and_rebind(clear, 1)
        .await
        .expect_err("clearing an unselected session must be refused");
    assert_eq!(error.code, ErrorCode::UnsupportedFeature);
}

// -- clear and rebind -------------------------------------------------------

/// The rebind's new session id becomes the id subsequent turns are armed and
/// polled under -- and the re-arm comes first. The tail refuses a poll under an
/// id it is not armed on, so a rebind that only rewrote the id would fail this
/// turn rather than silently following the rotation.
#[tokio::test]
async fn a_rebind_rearms_before_any_poll_and_later_turns_run_under_the_rotated_id() {
    let cell = Cell::start(0xB050, calibrated_rows(), true).await;
    cell.select_minified().await.expect("cell selected");
    let rotated = SessionId::from_u128(0xB051);

    let first = TurnId::from_u128(0xB150);
    cell.submit(first).await;
    assert_eq!(completed(cell.await_completion(first).await).text, "answer");

    let clear = Arc::new(ScriptedClear::rotating_to(rotated));
    let rebound = cell
        .actor()
        .await
        .clear_and_rebind(Arc::clone(&clear) as Arc<dyn ClearRebind>, 1)
        .await
        .expect("rebind");
    assert_eq!(rebound, rotated);
    assert_eq!(clear.calls(), vec![cell.session_id]);

    let second = TurnId::from_u128(0xB151);
    cell.submit(second).await;
    let result = completed(cell.await_completion(second).await).clone();
    assert_eq!(result.outcome, TurnOutcome::Completed);
    // The caller's handle did not rotate with Claude's id.
    assert_eq!(result.session_id, cell.session_id);

    let events = cell.transcript.events();
    let first_rotated = events
        .iter()
        .position(|event| {
            matches!(
                event,
                TailEvent::Armed(id) | TailEvent::Polled(id) if *id == rotated
            )
        })
        .expect("the rotated id must reach the tail");
    assert_eq!(
        events[first_rotated],
        TailEvent::Armed(rotated),
        "the first thing done under a rotated id must be an arm: {events:?}"
    );
    assert!(
        events[first_rotated..].contains(&TailEvent::Polled(rotated)),
        "later turns must poll under the rotated id: {events:?}"
    );
}

/// Fail closed. A clear whose rebind fails leaves a session that may be tailing
/// an abandoned transcript, so it must refuse every later turn immediately
/// rather than time one out ten minutes later with nothing to pull on.
#[tokio::test]
async fn a_clear_whose_rebind_fails_cannot_complete_a_later_turn() {
    let cell = Cell::start(0xB060, calibrated_rows(), true).await;
    cell.select_minified().await.expect("cell selected");

    let clear = Arc::new(ScriptedClear::failing(
        DriverFailure::new(
            ErrorCode::TranscriptUnavailable,
            "/clear abandoned the bound transcript and no replacement transcript could be identified",
        )
        .with_details(serde_json::json!({
            "field": "session_id",
            "violation": "clear_rebind_failed",
        })),
    ));
    let error = cell
        .actor()
        .await
        .clear_and_rebind(clear, 1)
        .await
        .expect_err("a failed rebind must be reported");
    assert_eq!(error.code, ErrorCode::TranscriptUnavailable);
    assert_eq!(error.details["violation"], "clear_rebind_failed");
    assert_eq!(cell.state().await, SessionState::Tainted);

    let turn_id = TurnId::from_u128(0xB160);
    let refused = cell
        .registry
        .run_turn(turn(cell.session_id, turn_id, PROMPT))
        .await
        .expect_err("a tainted session must not accept a turn");
    assert_eq!(refused.code, ErrorCode::RecoveryFailed);
    assert!(!refused.retryable);
    assert!(
        cell.stored(turn_id).await.is_none(),
        "a refused turn must leave no terminal outcome"
    );
}

/// The other side of the same rule, and the one the actor used to get wrong: a
/// refusal that provably typed nothing must leave the session exactly as it was.
///
/// The driver already draws this line -- it returns early WITHOUT poisoning its
/// tail when Enter was never attempted -- and the actor discarded it by sending
/// every `Err` through `poison_after_failed_rebind`. Two triggers need no
/// malformed input: a `clear_session` whose deadline has already passed, and a
/// `clear_session` issued before the session's first turn, where there is no
/// transcript yet for the rotation watch to snapshot. Both left a permanently
/// `Tainted` cell holding a Claude process that only `close_session` reclaims.
///
/// The proof is not the error code, which is the same either way. It is that the
/// next turn still completes.
#[tokio::test]
async fn a_clear_that_provably_typed_nothing_leaves_the_session_usable() {
    let cell = Cell::start_as_minified(0xB0A8, calibrated_rows(), true)
        .await
        .expect("registration");

    // The shape `driver_io::clear_and_rebind` produces when it refuses before
    // submitting the command: the failure carries its own proof that the bound
    // transcript was never abandoned.
    let refused = Arc::new(ScriptedClear::failing(
        DriverFailure::new(
            ErrorCode::TranscriptUnavailable,
            "no transcript exists for this session yet, so no rotation can be observed",
        )
        .with_details(serde_json::json!({
            "field": "session_id",
            "violation": "clear_rebind_failed",
            "clear_not_submitted": true,
        })),
    ));
    let error = cell
        .actor()
        .await
        .clear_session(
            Arc::clone(&refused) as Arc<dyn ClearRebind>,
            cell.session_id,
            1,
        )
        .await
        .expect_err("the refusal is still a refusal");
    assert_eq!(error.code, ErrorCode::TranscriptUnavailable);
    assert_eq!(
        cell.state().await,
        SessionState::Ready,
        "a clear that typed nothing must not quarantine the cell"
    );

    // The session still owns its transcript authority, and proves it.
    let turn_id = TurnId::from_u128(0xB1A8);
    cell.submit(turn_id).await;
    assert_eq!(
        completed(cell.await_completion(turn_id).await).text,
        "answer"
    );

    // And it is still clearable: nothing about the refusal consumed the
    // capability.
    let cleared = cell
        .clear_session(cell.session_id, SessionId::from_u128(0xB0A9))
        .await
        .expect("a session that was never poisoned can still be cleared");
    assert!(cleared.rotated);
}

/// The same fail-closed rule when the clear itself succeeded and only the
/// re-arm did not. This is the worse half: the old transcript is already
/// abandoned, and the tail is armed on nothing that will grow.
#[tokio::test]
async fn a_rebind_whose_rearm_fails_is_as_closed_as_a_failed_clear() {
    let cell = Cell::start(0xB070, calibrated_rows(), true).await;
    cell.select_minified().await.expect("cell selected");
    cell.transcript.fail_next_arm(DriverFailure::new(
        ErrorCode::TranscriptUnavailable,
        "no transcript for the rotated session",
    ));

    let clear = Arc::new(ScriptedClear::rotating_to(SessionId::from_u128(0xB071)));
    let error = cell
        .actor()
        .await
        .clear_and_rebind(clear, 1)
        .await
        .expect_err("an unarmed rebind must be reported");
    assert_eq!(error.code, ErrorCode::TranscriptUnavailable);
    assert_eq!(cell.state().await, SessionState::Tainted);

    let turn_id = TurnId::from_u128(0xB170);
    let refused = cell
        .registry
        .run_turn(turn(cell.session_id, turn_id, PROMPT))
        .await
        .expect_err("a tainted session must not accept a turn");
    assert_eq!(refused.code, ErrorCode::RecoveryFailed);
    assert!(cell.stored(turn_id).await.is_none());
}

/// A clear must not abandon the transcript a running turn is being proven from.
/// Refusing is retryable, and the turn it protected still completes.
#[tokio::test]
async fn a_clear_is_refused_while_a_turn_is_active() {
    let cell = Cell::start(0xB080, calibrated_rows(), true).await;
    cell.select_minified().await.expect("cell selected");
    cell.transcript.set_stability(0);
    cell.terminal.set_evidence(TerminalEvidence::default());

    let turn_id = TurnId::from_u128(0xB180);
    cell.submit(turn_id).await;
    cell.transcript.wait_for_polls(2).await;

    let clear = Arc::new(ScriptedClear::rotating_to(SessionId::from_u128(0xB081)));
    let error = cell
        .actor()
        .await
        .clear_and_rebind(Arc::clone(&clear) as Arc<dyn ClearRebind>, 1)
        .await
        .expect_err("a clear must not run under an active turn");
    assert_eq!(error.code, ErrorCode::SessionBusy);
    assert!(error.retryable);
    assert!(clear.calls().is_empty(), "nothing may have been typed");

    cell.terminal.set_evidence(TerminalEvidence {
        ready_prompt: true,
        quiet: true,
        ..TerminalEvidence::default()
    });
    cell.transcript.set_stability(LONG_STABILITY_MS);
    assert_eq!(
        completed(cell.await_completion(turn_id).await).text,
        "answer"
    );
}

// -- the cell as a start-time property --------------------------------------

/// The wire shape: a session that is a minified cell from birth needs no later
/// mutation, and no `select_minified_cell` call, to be cleared.
#[tokio::test]
async fn a_session_registered_as_a_minified_cell_can_be_cleared_without_selection() {
    let cell = Cell::start_as_minified(0xB090, calibrated_rows(), true)
        .await
        .expect("a tested profile admits the minified cell at registration");
    let rotated = SessionId::from_u128(0xB091);

    let result = cell
        .clear_session(cell.session_id, rotated)
        .await
        .expect("a fenced clear on the bound transcript rotates");
    assert!(result.rotated);
    assert_eq!(result.transcript_session_id, rotated);
    assert_eq!(result.session_id, cell.session_id);
    assert_eq!(result.state, SessionState::Ready);
}

/// The registry is `pub`, so the require-tested rule cannot live only in the
/// wire path. Refusing at spawn means no actor is created and no state is
/// published, which is what keeps the two escape hatches from composing.
#[tokio::test]
async fn an_untested_profile_cannot_register_a_minified_cell_at_all() {
    let error = Cell::start_as_minified(0xB0A0, calibrated_rows(), false)
        .await
        .err()
        .expect("an untested profile must not produce a minified-cell actor");
    assert_eq!(error.code, ErrorCode::UnsupportedClaudeVersion);
    assert_eq!(
        error.message,
        "the minified cell requires a tested compatibility profile"
    );
}

/// The launch half of assert-empty, at the boundary that owns it.
///
/// A stateless cell must not silently be a resumed one: `SessionIdentity::Resume`
/// names a transcript that already holds a prior caller's context, and a
/// caller-chosen `New` id can collide with one, so the question is asked of the
/// FILE and not of the request. This lived in `NativeService::start_session`,
/// where the only thing that could reach it was an `#[ignore]`d end-to-end test
/// that builds real binaries and a real PTY -- setting the guard to `if false`
/// left the entire default suite green. Registration is the boundary every route
/// to a minified cell passes through, wire and embedder alike, and it is
/// reachable without a Claude process.
#[tokio::test]
async fn a_minified_cell_cannot_be_registered_over_a_transcript_that_served_work() {
    let observed = Arc::new(Mutex::new(None));
    let captured = Arc::clone(&observed);
    let error = Cell::start_with_transcript(
        0xB0A4,
        calibrated_rows(),
        true,
        SessionCell::Minified,
        move |transcript| {
            *captured.lock().unwrap() = Some(Arc::clone(transcript));
            transcript.refuse_launch_proof(
                DriverFailure::new(
                    ErrorCode::SchemaDrift,
                    "a transcript claimed to have served no work was refused: semantic_row_present",
                )
                .with_details(serde_json::json!({
                    "field": "session_id",
                    "violation": "assert_empty_refused",
                    "reason": "semantic_row_present",
                })),
            );
        },
    )
    .await
    .err()
    .expect("a transcript that already served work must not back a minified cell");
    assert_eq!(error.code, ErrorCode::SchemaDrift);
    assert_eq!(error.details["violation"], "assert_empty_refused");
    assert_eq!(error.details["reason"], "semantic_row_present");
    let transcript = observed.lock().unwrap().take().expect("transcript double");
    assert_eq!(
        transcript.launch_proofs(),
        1,
        "the launch proof must be demanded exactly once, before any actor exists"
    );

    // The rule is scoped to the cell that needs it: the same dirty transcript
    // backs a Full cell without complaint, so this is an admission rule and not
    // a new precondition on every session.
    let full = Cell::start_with_transcript(
        0xB0A5,
        calibrated_rows(),
        true,
        SessionCell::Full,
        |transcript| {
            transcript.refuse_launch_proof(DriverFailure::new(
                ErrorCode::SchemaDrift,
                "a transcript claimed to have served no work was refused: semantic_row_present",
            ));
        },
    )
    .await
    .expect("a Full cell makes no emptiness claim and must not be asked to prove one");
    assert_eq!(
        full.transcript.launch_proofs(),
        0,
        "a Full cell must not pay for a proof it does not make"
    );
}

/// The POLARITY of the default, which is the whole of the rule and which nothing
/// else in the workspace can see.
///
/// `TranscriptSource::assert_empty_at_launch` has a default body that REFUSES.
/// Only `FileTranscriptSource` and the double above override it; every other
/// implementor in the workspace takes the default and none of them is ever
/// registered as `Minified`, so replacing that body with `Ok(())` leaves the
/// entire suite green while silently admitting every future source -- including
/// every embedder's -- to a cell whose product claim is an emptiness it never
/// proved. A predicate that passes by omission is not a predicate.
///
/// So this registers a source that deliberately does NOT override it, and
/// asserts the refusal by its message. It fails on the flip, which is the only
/// assertion here that matters.
#[tokio::test]
async fn a_transcript_source_that_cannot_prove_emptiness_may_not_back_a_minified_cell() {
    struct Unproving;

    #[async_trait]
    impl TranscriptSource for Unproving {
        async fn arm_at_eof(&self, _session_id: SessionId) -> DriverResult<TranscriptArm> {
            Ok(TranscriptArm::default())
        }

        async fn poll(
            &self,
            _session_id: SessionId,
            _position: &TranscriptPosition,
        ) -> DriverResult<TranscriptBatch> {
            unreachable!("no turn runs: registration is refused before an actor exists")
        }

        // `assert_empty_at_launch` is deliberately NOT implemented.
    }

    let registry = SessionRegistry::new(actor_config());
    let session_id = SessionId::from_u128(0xB0A6);
    let error = registry
        .register(SessionRegistration {
            agent: None,
            owner: pseudomux_service::v1::SessionOwner::Caller,
            session_id,
            generation_id: generation(session_id),
            cwd: "/minified-cell".to_owned(),
            compatibility: CompatibilityReport {
                claude_version: "test".to_owned(),
                os: std::env::consts::OS.to_owned(),
                arch: std::env::consts::ARCH.to_owned(),
                terminal_profile: TerminalProfile::Transparent,
                input_transport: InputTransport::Sdk,
                tested: true,
                transcript_drain_ms: FULL_DRAIN_MS,
            },
            dangerous_permission_bypass: false,
            resumable: true,
            cell: SessionCell::Minified,
            idle_ttl_ms: None,
            initial_needs_input: None,
            terminal: Arc::new(TestTerminal::new(Arc::new(Probe::default()))) as Arc<_>,
            transcript: Arc::new(Unproving) as Arc<_>,
        })
        .await
        .expect_err(
            "a source that does not implement the emptiness proof must not back a minified cell",
        );
    assert_eq!(error.code, ErrorCode::UnsupportedFeature);
    assert_eq!(
        error.message,
        "this transcript source cannot prove a launch transcript has served no work, so it may not back a minified cell",
        "the default must refuse; a passing `Ok(())` default would admit every source ever written"
    );

    // Same source, Full cell: admitted. So the default is a rule about the
    // minified cell's claim and not a new requirement on every backend.
    let full_id = SessionId::from_u128(0xB0A7);
    registry
        .register(SessionRegistration {
            agent: None,
            owner: pseudomux_service::v1::SessionOwner::Caller,
            session_id: full_id,
            generation_id: generation(full_id),
            cwd: "/minified-cell".to_owned(),
            compatibility: CompatibilityReport {
                claude_version: "test".to_owned(),
                os: std::env::consts::OS.to_owned(),
                arch: std::env::consts::ARCH.to_owned(),
                terminal_profile: TerminalProfile::Transparent,
                input_transport: InputTransport::Sdk,
                tested: true,
                transcript_drain_ms: FULL_DRAIN_MS,
            },
            dangerous_permission_bypass: false,
            resumable: true,
            cell: SessionCell::Full,
            idle_ttl_ms: None,
            initial_needs_input: None,
            terminal: Arc::new(TestTerminal::new(Arc::new(Probe::default()))) as Arc<_>,
            transcript: Arc::new(Unproving) as Arc<_>,
        })
        .await
        .expect("a Full cell makes no emptiness claim");
}

// -- the clear fence --------------------------------------------------------

/// One rotation stale is refused like any other stale value, and this is the
/// case the refusal exists for.
///
/// It was twice implemented as an idempotent no-op, on the reasoning that a
/// retry of a lost response is exactly one rotation behind. The reasoning is
/// sound about the retry and useless as a RULE, because the bytes a retry
/// carries are indistinguishable from the fence a session starts with: at start
/// `expected_transcript_session_id == session_id`, so after one clear the
/// one-behind value is what a restarted client, or a second caller that never
/// saw the first clear, presents. Both attempts to bound the window by session
/// state -- the abandoned id, then the event sequence -- leaked, and the second
/// leaked through `reserve_writable_attach`, which mutates a session without
/// emitting anything. So there is no window.
///
/// What the caller gets instead is on the next two assertions: the refusal names
/// the field, and `session_snapshot` publishes the transcript the caller needs
/// in order to find out whether its clear landed. That is one round trip, and it
/// cannot be wrong.
#[tokio::test]
async fn a_clear_fenced_one_rotation_behind_is_refused_rather_than_answered() {
    let cell = Cell::start_as_minified(0xB0B0, calibrated_rows(), true)
        .await
        .expect("registration");
    let first = SessionId::from_u128(0xB0B1);
    let second = SessionId::from_u128(0xB0B2);

    let rotated = cell
        .clear_session(cell.session_id, first)
        .await
        .expect("the first clear rotates");
    assert!(rotated.rotated);

    // The same request bytes again. Nothing may be typed and nothing may move.
    let replay = Arc::new(ScriptedClear::rotating_to(second));
    let error = cell
        .actor()
        .await
        .clear_session(
            Arc::clone(&replay) as Arc<dyn ClearRebind>,
            cell.session_id,
            1,
        )
        .await
        .expect_err("a one-behind fence must be refused, not answered as already cleared");
    assert_eq!(error.code, ErrorCode::IdConflict);
    assert!(!error.retryable);
    assert_eq!(error.details["field"], "expected_transcript_session_id");
    assert_eq!(error.details["violation"], "stale_transcript_fence");
    assert!(
        replay.calls().is_empty(),
        "a refused clear must type nothing: {:?}",
        replay.calls()
    );

    // The recovery the refusal leaves open, and the reason it costs a caller
    // nothing it used to have: the fence is readable, so "did my clear land"
    // is answered by the transcript id rather than inferred from a `rotated`
    // flag that could only ever have described SOME clear, never the caller's.
    let snapshot = cell.snapshot().await;
    assert_eq!(snapshot.transcript_session_id, first);
    assert_eq!(snapshot.state, SessionState::Ready);
    let recovered = cell
        .clear_session(snapshot.transcript_session_id, second)
        .await
        .expect("a clear fenced on the published id runs");
    assert!(
        recovered.rotated,
        "every clear that runs rotated; there is no other kind of success"
    );
    assert_eq!(recovered.transcript_session_id, second);
}

/// Two steps stale is a caller model that is simply wrong. Clearing anyway would
/// leave a caller believing every turn was stateless while its fence never
/// advanced -- context accumulating silently under a session that was asked to
/// be empty.
#[tokio::test]
async fn a_clear_fenced_two_rotations_behind_is_refused() {
    let cell = Cell::start_as_minified(0xB0C0, calibrated_rows(), true)
        .await
        .expect("registration");
    let first = SessionId::from_u128(0xB0C1);
    let second = SessionId::from_u128(0xB0C2);
    cell.clear_session(cell.session_id, first)
        .await
        .expect("first clear");
    cell.clear_session(first, second)
        .await
        .expect("second clear");

    let stale = Arc::new(ScriptedClear::rotating_to(SessionId::from_u128(0xB0C3)));
    let error = cell
        .actor()
        .await
        .clear_session(
            Arc::clone(&stale) as Arc<dyn ClearRebind>,
            cell.session_id,
            1,
        )
        .await
        .expect_err("a two-step-stale fence must be refused");
    assert_eq!(error.code, ErrorCode::IdConflict);
    assert!(!error.retryable);
    assert_eq!(error.details["field"], "expected_transcript_session_id");
    assert!(stale.calls().is_empty(), "nothing may have been typed");
}

/// The refusal is Step 0, ahead of the busy guard, and it still types nothing
/// while a turn is being proven.
///
/// This is the ordering the deleted retry window got wrong twice: it answered
/// `Ok` from the same position, ahead of the guards that know a turn is running
/// or that another party holds writable input. A refusal is safe there because
/// it moves nothing; an `Ok` is not, because it describes a transcript those
/// guards exist to say is in use.
#[tokio::test]
async fn a_stale_fence_is_refused_without_disturbing_a_turn_in_flight() {
    let cell = Cell::start_as_minified(0xB0D0, calibrated_rows(), true)
        .await
        .expect("registration");
    let rotated = SessionId::from_u128(0xB0D1);
    cell.clear_session(cell.session_id, rotated)
        .await
        .expect("first clear");

    cell.transcript.set_stability(0);
    cell.terminal.set_evidence(TerminalEvidence::default());
    let turn_id = TurnId::from_u128(0xB1D0);
    cell.submit(turn_id).await;
    cell.transcript.wait_for_polls(2).await;

    let replay = Arc::new(ScriptedClear::rotating_to(SessionId::from_u128(0xB0D2)));
    let error = cell
        .actor()
        .await
        .clear_session(
            Arc::clone(&replay) as Arc<dyn ClearRebind>,
            cell.session_id,
            1,
        )
        .await
        .expect_err("a stale fence must be refused even while a turn is in flight");
    assert_eq!(error.code, ErrorCode::IdConflict);
    assert_eq!(error.details["field"], "expected_transcript_session_id");
    assert_eq!(error.details["violation"], "stale_transcript_fence");
    assert!(
        replay.calls().is_empty(),
        "a refused fence must type nothing while a turn is being proven: {:?}",
        replay.calls()
    );

    // The turn it did not disturb still completes.
    cell.terminal.set_evidence(TerminalEvidence {
        ready_prompt: true,
        quiet: true,
        ..TerminalEvidence::default()
    });
    cell.transcript.set_stability(LONG_STABILITY_MS);
    assert_eq!(
        completed(cell.await_completion(turn_id).await).text,
        "answer"
    );
}

/// The state leak itself, end to end, in the shape a pool reaches it.
///
/// Caller A clears (fence `S` -> `N1`) and runs a turn carrying a secret. Caller
/// B -- a restarted client, or a second holder of the same handle -- still holds
/// `S`, which is what `ClearSessionRequest::expected_transcript_session_id`
/// documents the fence as being "at start". Under the unbounded window, B's
/// clear was answered "already cleared, nothing to do", typed nothing, opened no
/// transcript, and B's turn then landed in A's transcript with A's secret still
/// in front of it.
///
/// The assertion is about the transcript boundary, not the error code: after the
/// refusal the session must still be bound to `N1` and must NOT have been told
/// it is on a fresh one.
#[tokio::test]
async fn a_stale_fence_cannot_claim_a_transcript_that_has_since_served_a_turn() {
    let cell = Cell::start_as_minified(0xB0F0, calibrated_rows(), true)
        .await
        .expect("registration");
    let first = SessionId::from_u128(0xB0F1);

    let rotated = cell
        .clear_session(cell.session_id, first)
        .await
        .expect("caller A's clear rotates");
    assert!(rotated.rotated);
    assert_eq!(rotated.transcript_session_id, first);

    // Caller A's turn. Everything it said now lives in `first`.
    let a_turn = TurnId::from_u128(0xB1F0);
    cell.submit(a_turn).await;
    assert_eq!(
        completed(cell.await_completion(a_turn).await).text,
        "answer"
    );

    // Caller B, holding the fence a session starts with.
    let b_clear = Arc::new(ScriptedClear::rotating_to(SessionId::from_u128(0xB0F2)));
    let error = cell
        .actor()
        .await
        .clear_session(
            Arc::clone(&b_clear) as Arc<dyn ClearRebind>,
            cell.session_id,
            1,
        )
        .await
        .expect_err("a fence from before another caller's turn must not be answered as done");
    assert_eq!(error.code, ErrorCode::IdConflict);
    assert_eq!(error.details["violation"], "stale_transcript_fence");
    assert!(b_clear.calls().is_empty(), "nothing may have been typed");

    // And the session is still bound to the transcript caller A's turn used --
    // no second caller was handed a "you are on a clean transcript" answer.
    let snapshot = cell.snapshot().await;
    assert_eq!(snapshot.transcript_session_id, first);
    assert_eq!(snapshot.cell, SessionCell::Minified);
    assert_eq!(snapshot.state, SessionState::Ready);
}

/// A stale fence is refused in a state where no turn is running either, and the
/// refusal is a statement about the fence rather than a quarantine.
///
/// `NeedsInput` is the interesting one because a session reaches it without any
/// turn completing, so it is where a rule keyed on "has a turn run" and a rule
/// keyed on "is this fence current" give different answers.
#[tokio::test]
async fn a_stale_fence_is_refused_while_the_session_waits_on_a_modal() {
    let cell = Cell::start_as_minified(0xB0F8, calibrated_rows(), true)
        .await
        .expect("registration");
    let first = SessionId::from_u128(0xB0F9);
    cell.clear_session(cell.session_id, first)
        .await
        .expect("the clear rotates");

    // A modal appears: a state change with no turn behind it.
    cell.terminal
        .set_screen(TerminalScreenObservation::NeedsInput(NeedsInput {
            kind: NeedsInputKind::Permission,
            message: "explicit test permission is required".to_owned(),
            details: serde_json::Value::Null,
        }));
    let turn_id = TurnId::from_u128(0xB1F8);
    cell.submit(turn_id).await;
    cell.await_state(SessionState::NeedsInput).await;

    let replay = Arc::new(ScriptedClear::rotating_to(SessionId::from_u128(0xB0FA)));
    let error = cell
        .actor()
        .await
        .clear_session(
            Arc::clone(&replay) as Arc<dyn ClearRebind>,
            cell.session_id,
            1,
        )
        .await
        .expect_err("a stale fence must be refused whatever the session is doing");
    assert_eq!(error.code, ErrorCode::IdConflict);
    assert_eq!(error.details["violation"], "stale_transcript_fence");
    assert!(replay.calls().is_empty());

    // Left usable, so the refusal is a statement about the fence and not a
    // quarantine.
    cell.terminal.set_screen(TerminalScreenObservation::Ready);
    cell.transcript.set_stability(LONG_STABILITY_MS);
    assert_eq!(
        completed(cell.await_completion(turn_id).await).text,
        "answer"
    );
}

// -- the writable-attach channel --------------------------------------------

/// The mutation channel that broke the retry window, closed at its source.
///
/// A writable attach hands a second party an rmux grant; their keystrokes go
/// client -> rmux socket -> PTY and the actor sees the reservation lifecycle and
/// none of the bytes. `reserve_writable_attach`, `release_writable_attach` and
/// attach reconciliation all mutate this session and emit nothing, which is what
/// made "no event has been recorded" a false proxy for "nothing has happened".
///
/// On a minified cell the capability itself is refused, because the things it
/// enables are open-ended and it is one place: composer text that PREFIXES the
/// next caller's prompt, up-arrow recall out of the instance's own
/// `history.jsonl` -- which `/clear` does not truncate and which recall scopes to
/// the cwd rather than the session, so it spans every rotation -- and a
/// hand-typed `/clear` or `/model` that rotates Claude's id underneath the one
/// pmux has bound.
#[tokio::test]
async fn a_minified_cell_refuses_a_writable_terminal_attachment() {
    let cell = Cell::start_as_minified(0xB0FC, calibrated_rows(), true)
        .await
        .expect("registration");
    let error = cell
        .reserve_writable_attach(uuid::Uuid::from_u128(0xA77A))
        .await
        .expect_err("a minified cell must not grant writable input to a second party");
    assert_eq!(error.code, ErrorCode::UnsupportedFeature);
    assert!(!error.retryable);
    assert_eq!(
        error.details["violation"],
        "writable_attach_forbidden_on_minified_cell"
    );

    // The refusal is about the cell and not about attachment: the same
    // reservation succeeds on a Full session, so this is a narrowing of Path B
    // rather than a new global restriction.
    let full = Cell::start(0xB0FD, calibrated_rows(), true).await;
    full.reserve_writable_attach(uuid::Uuid::from_u128(0xA77B))
        .await
        .expect("a Full cell still grants writable attachments");
}

/// The other half of the same rule. Without it a session could acquire, by
/// conversion, exactly the reservation the reservation path refuses -- and then
/// every statement that reads "a minified cell holds no writable attachment"
/// would be false for the one session that got there sideways.
#[tokio::test]
async fn a_session_holding_a_writable_attachment_cannot_become_a_minified_cell() {
    let cell = Cell::start(0xB0FE, calibrated_rows(), true).await;
    cell.reserve_writable_attach(uuid::Uuid::from_u128(0xA77C))
        .await
        .expect("a Full cell grants the reservation");

    let error = cell
        .select_minified()
        .await
        .expect_err("the cell must not change under a held writable attachment");
    assert_eq!(error.code, ErrorCode::SessionBusy);
    assert!(error.retryable);

    // Released, and then the conversion is admissible -- so the refusal names a
    // condition rather than banning the conversion.
    cell.registry
        .release_writable_attach(
            cell.session_id,
            generation(cell.session_id),
            uuid::Uuid::from_u128(0xA77C),
            WritableAttachCompletion::Unused,
        )
        .await
        .expect("release");
    cell.select_minified()
        .await
        .expect("a released attachment leaves the conversion admissible");
}

/// A clear that landed but whose result was not empty is quarantined exactly as
/// a clear that failed outright. The predicate itself is proven over real bytes
/// in `driver_io`; this is the mapping from its refusal to the terminal state.
#[tokio::test]
async fn a_clear_whose_result_was_not_empty_taints_the_session() {
    let cell = Cell::start_as_minified(0xB0E0, calibrated_rows(), true)
        .await
        .expect("registration");
    let clear = Arc::new(ScriptedClear::failing(
        DriverFailure::new(
            ErrorCode::SchemaDrift,
            "a transcript claimed to have served no work was refused: wrong_local_command",
        )
        .with_details(serde_json::json!({
            "field": "session_id",
            "violation": "assert_empty_refused",
            "reason": "wrong_local_command",
        })),
    ));
    let error = cell
        .actor()
        .await
        .clear_session(clear, cell.session_id, 1)
        .await
        .expect_err("an assert-empty refusal must be reported");
    assert_eq!(error.code, ErrorCode::SchemaDrift);
    assert_eq!(error.details["violation"], "assert_empty_refused");
    assert_eq!(error.details["reason"], "wrong_local_command");
    cell.await_state(SessionState::Tainted).await;

    let turn_id = TurnId::from_u128(0xB1E0);
    let refused = cell
        .registry
        .run_turn(turn(cell.session_id, turn_id, PROMPT))
        .await
        .expect_err("a quarantined session must not accept a turn");
    assert_eq!(refused.code, ErrorCode::RecoveryFailed);
    assert!(!refused.retryable, "quarantine must not be retryable");

    // And it must not be clearable again either: "clear it and try once more"
    // is exactly the move that trades a guarantee for an instance.
    let retry = cell
        .clear_session(cell.session_id, SessionId::from_u128(0xB0E1))
        .await
        .expect_err("a tainted session must refuse a second clear");
    assert_eq!(retry.code, ErrorCode::SessionBusy);
    assert!(!retry.retryable);
}
