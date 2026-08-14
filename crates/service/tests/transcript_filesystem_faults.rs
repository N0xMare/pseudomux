#![cfg(unix)]

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use pseudomux_protocol::v1::{
    ClosePolicy, CloseSessionRequest, CompatibilityReport, ErrorCode, InputTransport,
    InspectSessionRequest, RunTurnRequest, SessionGenerationId, SessionId, SessionState,
    TerminalProfile, TurnId, TurnRequest,
};
use pseudomux_service::driver_io::FileTranscriptSource;
use pseudomux_service::v1::{
    Clock, DriverResult, InterruptRecovery, SessionActorConfig, SessionCell, SessionRegistration,
    SessionRegistry, StoredTurnTerminal, TerminalControl, TerminalEvidence,
    TerminalScreenObservation, TranscriptSource,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::Notify;

const CLOCK_MS: u64 = 1_000_000;
const PROMPT: &str = "filesystem boundary prompt";

#[derive(Debug)]
struct FixedClock;

impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        CLOCK_MS
    }
}

#[derive(Debug, Default)]
struct ReadyTerminal {
    submissions: Mutex<Vec<(SessionId, TurnId, String)>>,
    submission_changed: Notify,
    closes: AtomicUsize,
    close_changed: Notify,
}

impl ReadyTerminal {
    async fn wait_for_submission(&self, session_id: SessionId, turn_id: TurnId) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let changed = self.submission_changed.notified();
                if self
                    .submissions
                    .lock()
                    .expect("submission mutex")
                    .iter()
                    .any(|(actual_session, actual_turn, prompt)| {
                        *actual_session == session_id && *actual_turn == turn_id && prompt == PROMPT
                    })
                {
                    return;
                }
                changed.await;
            }
        })
        .await
        .expect("the actor did not reach terminal submission");
    }

    fn close_count(&self) -> usize {
        self.closes.load(Ordering::SeqCst)
    }

    async fn wait_for_close_count(&self, expected: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let changed = self.close_changed.notified();
                if self.close_count() >= expected {
                    return;
                }
                changed.await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("terminal did not reach {expected} close attempts"));
    }
}

#[async_trait]
impl TerminalControl for ReadyTerminal {
    async fn submit_prompt(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        prompt: &str,
        _deadline_unix_ms: u64,
    ) -> DriverResult<()> {
        self.submissions.lock().expect("submission mutex").push((
            session_id,
            turn_id,
            prompt.to_owned(),
        ));
        self.submission_changed.notify_waiters();
        Ok(())
    }

    async fn completion_evidence(
        &self,
        _session_id: SessionId,
        _turn_id: TurnId,
    ) -> DriverResult<TerminalEvidence> {
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
        self.close_changed.notify_waiters();
        Ok(true)
    }
}

struct Fixture {
    _config_home: TempDir,
    _cwd: TempDir,
    transcript: PathBuf,
    canonical_cwd: PathBuf,
    registry: SessionRegistry,
    terminal: Arc<ReadyTerminal>,
    session_id: SessionId,
    generation_id: SessionGenerationId,
}

impl Fixture {
    async fn new() -> Self {
        let config_home = TempDir::new().expect("config tempdir");
        let cwd = TempDir::new().expect("cwd tempdir");
        let canonical_cwd = cwd.path().canonicalize().expect("canonical cwd");
        let project = config_home.path().join("projects/project");
        std::fs::create_dir_all(&project).expect("project directory");
        let session_id = SessionId::new_v4();
        let generation_id = SessionGenerationId::new();
        let transcript = project.join(format!("{session_id}.jsonl"));
        write_values(
            &transcript,
            &[json!({
                "type": "file-history-snapshot",
                "sessionId": session_id,
                "cwd": canonical_cwd,
            })],
        );

        let transcript_source = Arc::new(
            FileTranscriptSource::new(config_home.path(), &canonical_cwd, session_id)
                .expect("filesystem transcript source"),
        );
        let terminal = Arc::new(ReadyTerminal::default());
        let registry = SessionRegistry::with_clock(
            SessionActorConfig {
                poll_interval: Duration::from_millis(2),
                cancel_recovery_timeout: Duration::from_millis(250),
                default_turn_timeout_ms: 5_000,
                ..SessionActorConfig::default()
            },
            Arc::new(FixedClock),
        );
        registry
            .register(SessionRegistration {
                owner: pseudomux_service::v1::SessionOwner::Caller,
                session_id,
                generation_id,
                cwd: canonical_cwd.to_string_lossy().into_owned(),
                compatibility: CompatibilityReport {
                    claude_version: "filesystem-test".to_owned(),
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
                terminal: Arc::clone(&terminal) as Arc<dyn TerminalControl>,
                transcript: transcript_source,
            })
            .await
            .expect("register filesystem-backed actor");

        Self {
            _config_home: config_home,
            _cwd: cwd,
            transcript,
            canonical_cwd,
            registry,
            terminal,
            session_id,
            generation_id,
        }
    }

    async fn start_turn(&self, timeout_ms: Option<u64>) -> TurnId {
        let turn_id = TurnId::new_v4();
        self.registry
            .run_turn(RunTurnRequest {
                session_id: self.session_id,
                generation_id: self.generation_id,
                turn: TurnRequest {
                    turn_id,
                    prompt: PROMPT.to_owned(),
                    deadline_unix_ms: timeout_ms.map(|timeout| CLOCK_MS + timeout),
                    lease: Default::default(),
                },
            })
            .await
            .expect("turn acceptance");
        self.terminal
            .wait_for_submission(self.session_id, turn_id)
            .await;
        turn_id
    }

    fn append_value(&self, value: &Value) {
        let mut file = OpenOptions::new()
            .append(true)
            .open(&self.transcript)
            .expect("open transcript for append");
        writeln!(file, "{value}").expect("append complete JSONL record");
        file.flush().expect("flush transcript append");
    }

    fn append_bytes(&self, bytes: &[u8]) {
        let mut file = OpenOptions::new()
            .append(true)
            .open(&self.transcript)
            .expect("open transcript for byte append");
        file.write_all(bytes).expect("append transcript bytes");
        file.flush().expect("flush transcript bytes");
    }

    async fn acknowledge(&self) {
        self.append_value(&typed_user(self.session_id, &self.canonical_cwd));
        self.wait_for_state(SessionState::Running).await;
    }

    async fn wait_for_state(&self, expected: SessionState) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let snapshot = self
                    .registry
                    .inspect(InspectSessionRequest {
                        session_id: self.session_id,
                        generation_id: self.generation_id,
                    })
                    .await
                    .expect("inspect filesystem-backed actor");
                if snapshot.state == expected {
                    return;
                }
                assert_ne!(
                    snapshot.state,
                    SessionState::Failed,
                    "actor failed before reaching {expected:?}"
                );
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("actor did not reach {expected:?}"));
    }

    async fn wait_for_terminal(&self, turn_id: TurnId) -> StoredTurnTerminal {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(terminal) = self
                    .registry
                    .stored_turn(self.session_id, self.generation_id, turn_id)
                    .await
                    .expect("read stored turn")
                {
                    return terminal;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("turn did not reach a terminal actor outcome")
    }

    async fn assert_not_terminal(&self, turn_id: TurnId) {
        assert!(
            self.registry
                .stored_turn(self.session_id, self.generation_id, turn_id)
                .await
                .expect("read stored turn")
                .is_none(),
            "an incomplete transcript record must not commit a turn"
        );
    }

    async fn cleanup(&self) {
        let closed = self
            .registry
            .close(CloseSessionRequest {
                session_id: self.session_id,
                generation_id: self.generation_id,
                policy: ClosePolicy::Force,
            })
            .await
            .expect("close filesystem-backed actor");
        assert!(closed.process_reaped);
        self.registry
            .unregister(self.session_id, self.generation_id)
            .await
            .expect("unregister filesystem-backed actor");
    }
}

#[tokio::test]
async fn complete_line_framing_blocks_a_terminal_row_until_its_newline() {
    let fixture = Fixture::new().await;
    let turn_id = fixture.start_turn(None).await;
    let assistant = assistant(
        fixture.session_id,
        &fixture.canonical_cwd,
        "complete answer",
    );
    fixture.append_bytes(
        format!(
            "{}\n{assistant}",
            typed_user(fixture.session_id, &fixture.canonical_cwd)
        )
        .as_bytes(),
    );
    // Reaching Running proves the real source consumed the complete typed row
    // from this append while retaining the adjacent assistant suffix.
    fixture.wait_for_state(SessionState::Running).await;
    fixture.assert_not_terminal(turn_id).await;

    fixture.append_bytes(b"\n");
    let StoredTurnTerminal::Result(result) = fixture.wait_for_terminal(turn_id).await else {
        panic!("a newline-terminated terminal row must produce a successful result");
    };
    assert_eq!(result.text, "complete answer");
    assert!(result.completion.prompt_acknowledged);
    assert!(result.completion.terminal_message_observed);
    assert!(result.completion.transcript_drained);

    fixture.cleanup().await;
    fixture.terminal.wait_for_close_count(1).await;
    assert_eq!(fixture.terminal.close_count(), 1);
}

#[tokio::test]
async fn unterminated_terminal_row_times_out_and_reaps_instead_of_committing() {
    let fixture = Fixture::new().await;
    let turn_id = fixture.start_turn(Some(500)).await;
    let assistant = assistant(
        fixture.session_id,
        &fixture.canonical_cwd,
        "must not commit",
    );
    fixture.append_bytes(
        format!(
            "{}\n{assistant}",
            typed_user(fixture.session_id, &fixture.canonical_cwd)
        )
        .as_bytes(),
    );
    fixture.wait_for_state(SessionState::Running).await;

    let StoredTurnTerminal::Failed(error) = fixture.wait_for_terminal(turn_id).await else {
        panic!("an unterminated terminal row must never become a result");
    };
    assert_eq!(error.code, ErrorCode::TurnTimeout);
    fixture.terminal.wait_for_close_count(1).await;
    assert_eq!(fixture.terminal.close_count(), 1);

    fixture.cleanup().await;
    fixture.terminal.wait_for_close_count(2).await;
    assert_eq!(
        fixture.terminal.close_count(),
        2,
        "timeout publishes before its best-effort reap, so explicit close must independently re-prove cleanup"
    );
}

#[tokio::test]
async fn active_turn_truncation_fails_closed_and_reaps() {
    let fixture = Fixture::new().await;
    let turn_id = fixture.start_turn(None).await;
    fixture.acknowledge().await;

    File::create(&fixture.transcript).expect("truncate active transcript");
    let StoredTurnTerminal::Failed(error) = fixture.wait_for_terminal(turn_id).await else {
        panic!("active transcript truncation must never become a result");
    };
    assert_eq!(error.code, ErrorCode::TranscriptUnavailable);
    fixture.terminal.wait_for_close_count(1).await;
    assert_eq!(fixture.terminal.close_count(), 1);

    fixture.cleanup().await;
    assert_eq!(fixture.terminal.close_count(), 1);
}

#[tokio::test]
async fn active_turn_file_generation_replacement_fails_before_replacement_content_can_commit() {
    let fixture = Fixture::new().await;
    let turn_id = fixture.start_turn(None).await;
    fixture.acknowledge().await;

    let replacement = fixture.transcript.with_extension("replacement");
    write_values(
        &replacement,
        &[
            json!({
                "type": "file-history-snapshot",
                "sessionId": fixture.session_id,
                "cwd": fixture.canonical_cwd,
            }),
            typed_user(fixture.session_id, &fixture.canonical_cwd),
            assistant(
                fixture.session_id,
                &fixture.canonical_cwd,
                "replacement content must not commit",
            ),
        ],
    );
    let old_identity = file_identity(&fixture.transcript);
    let replacement_identity = file_identity(&replacement);
    assert_ne!(old_identity, replacement_identity);
    std::fs::rename(&replacement, &fixture.transcript).expect("replace active transcript");

    let StoredTurnTerminal::Failed(error) = fixture.wait_for_terminal(turn_id).await else {
        panic!("active transcript replacement must never become a result");
    };
    assert_eq!(error.code, ErrorCode::TranscriptUnavailable);
    fixture.terminal.wait_for_close_count(1).await;
    assert_eq!(fixture.terminal.close_count(), 1);

    fixture.cleanup().await;
    assert_eq!(fixture.terminal.close_count(), 1);
}

#[tokio::test]
async fn malformed_complete_json_fails_closed_through_the_actor() {
    let fixture = Fixture::new().await;
    let turn_id = fixture.start_turn(None).await;
    fixture.acknowledge().await;
    fixture.append_bytes(b"{\"type\":\"assistant\"\n");

    let StoredTurnTerminal::Failed(error) = fixture.wait_for_terminal(turn_id).await else {
        panic!("malformed complete JSON must never become a result");
    };
    assert_eq!(error.code, ErrorCode::SchemaDrift);
    fixture.terminal.wait_for_close_count(1).await;
    assert_eq!(fixture.terminal.close_count(), 1);

    fixture.cleanup().await;
    assert_eq!(fixture.terminal.close_count(), 1);
}

#[tokio::test]
async fn malformed_complete_utf8_fails_closed_through_the_actor() {
    let fixture = Fixture::new().await;
    let turn_id = fixture.start_turn(None).await;
    fixture.acknowledge().await;
    fixture.append_bytes(&[b'{', 0xff, b'}', b'\n']);

    let StoredTurnTerminal::Failed(error) = fixture.wait_for_terminal(turn_id).await else {
        panic!("malformed complete UTF-8 must never become a result");
    };
    assert_eq!(error.code, ErrorCode::SchemaDrift);
    fixture.terminal.wait_for_close_count(1).await;
    assert_eq!(fixture.terminal.close_count(), 1);

    fixture.cleanup().await;
    assert_eq!(fixture.terminal.close_count(), 1);
}

// ---- the byte that re-arms the drain ----------------------------------------
//
// `stable_for_ms` is the drain's entire proof of absence, and it measures quiet
// since the last transcript BYTE rather than since any other event. Two lines of
// `driver_io.rs` make that true and nothing else does: `read_available` returns
// early on a read of length zero without touching `TailState::last_change`, and
// `read_observed_range` assigns `last_change` on every read that produced bytes.
//
// That asymmetry is why a row landing after the end-of-turn marker is not lost:
// the graduated floor restarts from the arriving byte instead of counting down
// from the marker. A fake source can only assert its own arithmetic, so both
// halves are pinned here against the real filesystem source, driven directly
// over the `TranscriptSource` boundary the actor uses.

/// Real milliseconds of quiet between polls in the re-arm test.
///
/// Nothing is asserted against this number directly -- every stability claim
/// below is checked against a wall-clock interval the test measured itself -- so
/// it only has to be long enough that a re-armed window and a window that was
/// never re-armed cannot be confused on a loaded machine.
const REARM_QUIET_MS: u64 = 100;

#[tokio::test]
async fn a_read_that_produced_bytes_rearms_stability_and_an_empty_read_does_not() {
    let config_home = TempDir::new().expect("config tempdir");
    let cwd = TempDir::new().expect("cwd tempdir");
    let canonical_cwd = cwd.path().canonicalize().expect("canonical cwd");
    let project = config_home.path().join("projects/project");
    std::fs::create_dir_all(&project).expect("project directory");
    let session_id = SessionId::new_v4();
    let transcript = project.join(format!("{session_id}.jsonl"));
    write_values(
        &transcript,
        &[json!({
            "type": "file-history-snapshot",
            "sessionId": session_id,
            "cwd": canonical_cwd,
        })],
    );
    let source = FileTranscriptSource::new(config_home.path(), &canonical_cwd, session_id)
        .expect("filesystem transcript source");

    let armed = source.arm_at_eof(session_id).await.expect("arm at eof");
    let mut position = armed.position;

    // Two empty reads, each after a measured interval of real quiet. The
    // reported stability is at least the age of the arm every time and keeps
    // climbing, so neither empty read moved the origin: observing the file is
    // not activity.
    let mut quiet_since = Instant::now();
    tokio::time::sleep(Duration::from_millis(REARM_QUIET_MS)).await;
    let first_idle = source
        .poll(session_id, &position)
        .await
        .expect("first idle poll");
    let first_quiet_ms = elapsed_ms(quiet_since);
    assert!(first_idle.rows.is_empty(), "nothing was appended");
    assert!(
        first_idle.drain.stable_for_ms >= first_quiet_ms,
        "the arm point is the origin until a byte arrives: reported {}ms of quiet \
         over a measured {first_quiet_ms}ms",
        first_idle.drain.stable_for_ms
    );
    position = first_idle.position;

    quiet_since = Instant::now();
    tokio::time::sleep(Duration::from_millis(REARM_QUIET_MS)).await;
    let second_idle = source
        .poll(session_id, &position)
        .await
        .expect("second idle poll");
    let second_quiet_ms = elapsed_ms(quiet_since);
    assert!(second_idle.rows.is_empty(), "nothing was appended");
    assert!(
        second_idle.drain.stable_for_ms >= first_idle.drain.stable_for_ms + second_quiet_ms,
        "an empty read must not re-arm the window: it reported {}ms of quiet after \
         a previous read reported {}ms and a further {second_quiet_ms}ms passed",
        second_idle.drain.stable_for_ms,
        first_idle.drain.stable_for_ms
    );
    position = second_idle.position;

    // THE BYTE. One appended row, read on the very next poll, and the window the
    // drain measures collapses to the age of *this* read rather than the age of
    // the arm. Without the re-arm the source would report the transcript as
    // having been quiet for the whole run above, and a drain floor would be
    // satisfiable while content was still arriving -- returning before the work
    // is done, the one outcome pmux does not accept.
    append_value_to(&transcript, &typed_user(session_id, &canonical_cwd));
    let carrying = source
        .poll(session_id, &position)
        .await
        .expect("poll carrying the appended row");
    assert_eq!(
        carrying.rows.len(),
        1,
        "the appended row must have been read"
    );
    assert!(
        carrying.drain.stable_for_ms < second_idle.drain.stable_for_ms,
        "a read that produced bytes must re-arm the window: it reported {}ms of \
         quiet, no less than the {}ms the previous empty read reported",
        carrying.drain.stable_for_ms,
        second_idle.drain.stable_for_ms
    );
    assert!(
        carrying.drain.stable_for_ms < REARM_QUIET_MS,
        "and the re-armed window starts at the read, not somewhere in the {REARM_QUIET_MS}ms \
         of quiet before it: reported {}ms",
        carrying.drain.stable_for_ms
    );
    position = carrying.position;

    // And it climbs again from the new origin, so the arriving byte moved the
    // origin rather than stopping the clock.
    quiet_since = Instant::now();
    tokio::time::sleep(Duration::from_millis(REARM_QUIET_MS)).await;
    let after = source
        .poll(session_id, &position)
        .await
        .expect("idle poll after the append");
    let after_quiet_ms = elapsed_ms(quiet_since);
    assert!(after.rows.is_empty(), "nothing further was appended");
    assert!(
        after.drain.stable_for_ms >= after_quiet_ms,
        "quiet after the append still accumulates: reported {}ms over a measured \
         {after_quiet_ms}ms",
        after.drain.stable_for_ms
    );
    assert!(
        after.drain.stable_for_ms < second_idle.drain.stable_for_ms,
        "but it is measured from the byte, not the arm: reported {}ms where the \
         un-re-armed window had already reached {}ms one poll earlier",
        after.drain.stable_for_ms,
        second_idle.drain.stable_for_ms
    );
}

fn elapsed_ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).expect("a test interval fits in u64")
}

fn append_value_to(path: &Path, value: &Value) {
    let mut file = OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open transcript for append");
    writeln!(file, "{value}").expect("append complete JSONL record");
    file.flush().expect("flush transcript append");
}

fn typed_user(session_id: SessionId, cwd: &Path) -> Value {
    json!({
        "type": "user",
        "uuid": "filesystem-typed-user",
        "parentUuid": null,
        "sessionId": session_id,
        "cwd": cwd,
        "promptSource": "typed",
        "message": {"content": PROMPT},
    })
}

fn assistant(session_id: SessionId, cwd: &Path, text: &str) -> Value {
    json!({
        "type": "assistant",
        "uuid": "filesystem-assistant",
        "parentUuid": "filesystem-typed-user",
        "sessionId": session_id,
        "cwd": cwd,
        "message": {
            "id": "filesystem-message",
            "model": "filesystem-test-model",
            "content": [{"type": "text", "text": text}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1},
        },
    })
}

fn write_values(path: &Path, values: &[Value]) {
    let mut file = File::create(path).expect("create transcript");
    for value in values {
        writeln!(file, "{value}").expect("write JSONL record");
    }
    file.flush().expect("flush JSONL transcript");
}

fn file_identity(path: &Path) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path).expect("transcript metadata");
    (metadata.dev(), metadata.ino())
}
