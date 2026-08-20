#![cfg(unix)]

mod process_support;
mod support;

use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use async_trait::async_trait;
use pseudomux_claude::{CompleteLine, JsonlParser, ParseMode, ParsedRow, SourceLocation};
use pseudomux_protocol::v1::{
    CancelOutcome, CancelTurnRequest, ClosePolicy, CompatibilityReport, InputTransport,
    InspectSessionRequest, ResponseEnvelope, ResponseResult, SessionId, SessionState,
    SubscribeEventsRequest, TerminalProfile, TurnId, TurnOutcome, TurnTimings,
};
use pseudomux_rmux::{EnvironmentSnapshot, LaunchSpec};
use pseudomux_service::runtime::{PrivateRuntime, PrivateRuntimeConfig, SessionRuntime};
use pseudomux_service::v1::{
    DriverResult, InterruptRecovery, SessionActorConfig, SessionCell, SessionRegistration,
    SessionRegistry, StoredTurnTerminal, TerminalControl, TerminalEvidence,
    TerminalScreenObservation, TranscriptArm, TranscriptBatch, TranscriptDrainEvidence,
    TranscriptPosition, TranscriptSource,
};

use process_support::{
    CandidateFiles, ExactProcessGuard, ProcessIdentity, SocketIdentity, find_direct_child,
    runtime_entries, set_owner_only, wait_for_pid_file, wait_for_process_absence,
};
use support::{
    Probe, TestTerminal, TestTranscript, actor_config, close_and_unregister, generation, id,
    register, registry_with_config, turn, wait_for_resources_released, wait_for_stored_turn,
};

const DIAGNOSTIC_SAMPLES: usize = 7;
const PARSER_ROWS: usize = 4_096;
const REPLAY_TURNS: usize = 96;
const REPLAY_PAGE_EVENTS: u32 = 64;
const ACTOR_BATCH: usize = 32;
const PRIVATE_RUNTIME_SAMPLES: usize = 5;
const TURN_PHASE_SAMPLES: usize = 9;
/// Bounded stand-in for the per-cell `transcript_drain_ms`. The production
/// fallback is 2,000 ms; the diagnostic records the configured value it used so
/// the drain constant can be separated from the observation slack around it.
const TURN_PHASE_DRAIN_MS: u64 = 150;
const TURN_PHASE_PROMPT: &str = "phase diagnostic";
const TURN_PHASE_BUDGET: Duration = Duration::from_secs(60);

/// Records host-sensitive throughput and latency without making workstation
/// speed a release gate. Correctness timeouts below are bounded liveness and
/// cleanup checks; the algorithmic scaling gate remains `claude/size_scaling`.
///
/// The `phase_breakdown` section decomposes pmux's own per-turn overhead
/// against zero model latency. It is recorded, never asserted: every phase
/// bound is either a product timestamp (`TurnTimings`) or a wall measurement
/// around a real private runtime, and nothing here compares a host number to a
/// threshold.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn records_release_diagnostics_without_host_speed_thresholds() {
    let parser = parser_diagnostic();
    let workspace = tempfile::tempdir().unwrap();
    let replay = replay_diagnostic(workspace.path()).await;
    let actors = actor_lifecycle_diagnostic(workspace.path()).await;
    let turns = turn_phase_diagnostic().await;
    let private_runtime = private_runtime_diagnostic().await.unwrap();

    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let report = serde_json::json!({
        "schema": "pmux-performance-diagnostics-v2",
        "policy": "host-sensitive-diagnostic-only",
        "profile": profile,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "samples": DIAGNOSTIC_SAMPLES,
        "parser": {
            "rows_per_sample": parser.rows,
            "bytes_per_sample": parser.bytes,
            "elapsed_ns": parser.elapsed.as_json(),
            "rows_per_second_at_median": per_second(parser.rows, parser.elapsed.median_ns),
            "bytes_per_second_at_median": per_second(parser.bytes, parser.elapsed.median_ns),
        },
        "replay": {
            "events_per_sweep": replay.events,
            "pages_per_sweep": replay.pages,
            "serialized_bytes_per_sweep": replay.bytes,
            "elapsed_ns": replay.elapsed.as_json(),
            "events_per_second_at_median": per_second(replay.events, replay.elapsed.median_ns),
            "bytes_per_second_at_median": per_second(replay.bytes, replay.elapsed.median_ns),
        },
        "actor_lifecycle": {
            "actors_per_batch": ACTOR_BATCH,
            "startup_batch_elapsed_ns": actors.startup.as_json(),
            "startup_median_ns_per_actor": actors.startup.median_ns / ACTOR_BATCH as u64,
            "cleanup_batch_elapsed_ns": actors.cleanup.as_json(),
            "cleanup_median_ns_per_actor": actors.cleanup.median_ns / ACTOR_BATCH as u64,
        },
        "private_rmux_lifecycle": {
            "samples": PRIVATE_RUNTIME_SAMPLES,
            "runtime_startup_elapsed_ns": stats_of(&private_runtime.runtime_startup).as_json(),
            "terminal_ready_elapsed_ns": stats_of(&private_runtime.terminal_ready).as_json(),
            "terminal_cleanup_elapsed_ns": stats_of(&private_runtime.terminal_cleanup).as_json(),
            "runtime_cleanup_elapsed_ns": stats_of(&private_runtime.runtime_cleanup).as_json(),
        },
        "phase_breakdown": phase_breakdown(&turns, &private_runtime),
        "algorithmic_invariant_gate": "cargo test -p pseudomux-claude --test size_scaling --release -- --nocapture",
    });
    eprintln!(
        "pmux_performance_diagnostics {}",
        serde_json::to_string(&report).unwrap()
    );
}

#[derive(Clone, Copy)]
struct DurationStats {
    min_ns: u64,
    median_ns: u64,
    max_ns: u64,
}

impl DurationStats {
    fn from_durations(samples: Vec<Duration>) -> Self {
        assert!(!samples.is_empty());
        let mut samples = samples.into_iter().map(duration_ns).collect::<Vec<_>>();
        samples.sort_unstable();
        Self {
            min_ns: samples[0],
            median_ns: samples[samples.len() / 2],
            max_ns: samples[samples.len() - 1],
        }
    }

    fn as_json(self) -> serde_json::Value {
        serde_json::json!({
            "min": self.min_ns,
            "median": self.median_ns,
            "max": self.max_ns,
        })
    }
}

fn duration_ns(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn per_second(units: usize, elapsed_ns: u64) -> u64 {
    let value = (units as u128)
        .saturating_mul(1_000_000_000)
        .checked_div(u128::from(elapsed_ns.max(1)))
        .unwrap_or(u128::MAX);
    u64::try_from(value).unwrap_or(u64::MAX)
}

struct ParserDiagnostic {
    rows: usize,
    bytes: usize,
    elapsed: DurationStats,
}

fn parser_diagnostic() -> ParserDiagnostic {
    let lines = parser_lines();
    let parser = JsonlParser::new(ParseMode::Strict);
    let bytes = lines.iter().map(|line| line.bytes.len()).sum();

    assert_eq!(parse_all(&parser, &lines), PARSER_ROWS);
    let elapsed = (0..DIAGNOSTIC_SAMPLES)
        .map(|_| {
            let started = Instant::now();
            let parsed = parse_all(&parser, &lines);
            let elapsed = started.elapsed();
            assert_eq!(parsed, PARSER_ROWS);
            elapsed
        })
        .collect();

    ParserDiagnostic {
        rows: PARSER_ROWS,
        bytes,
        elapsed: DurationStats::from_durations(elapsed),
    }
}

fn parse_all(parser: &JsonlParser, lines: &[CompleteLine]) -> usize {
    lines
        .iter()
        .map(|line| {
            black_box(parser.parse(line).unwrap());
            1
        })
        .sum()
}

fn parser_lines() -> Vec<CompleteLine> {
    (0..PARSER_ROWS)
        .map(|index| {
            let bytes = if index % 2 == 0 {
                format!(
                    r#"{{"parentUuid":null,"sessionId":"diagnostic-session","type":"user","message":{{"role":"user","content":"diagnostic prompt {index}"}},"uuid":"user-{index}","promptSource":"typed","promptId":"prompt-{index}"}}"#
                )
            } else {
                format!(
                    r#"{{"parentUuid":"user-{}","sessionId":"diagnostic-session","type":"assistant","requestId":"request-{index}","uuid":"assistant-{index}","message":{{"id":"message-{index}","model":"diagnostic-model","content":[{{"type":"text","text":"bounded diagnostic response"}}],"stop_reason":"end_turn","usage":{{"input_tokens":3,"output_tokens":4,"cache_creation_input_tokens":1,"cache_read_input_tokens":2}}}}}}"#,
                    index - 1
                )
            };
            CompleteLine {
                location: SourceLocation {
                    line: u64::try_from(index + 1).unwrap(),
                    byte_offset: 0,
                },
                bytes: bytes.into_bytes(),
            }
        })
        .collect()
}

struct ReplayDiagnostic {
    events: usize,
    pages: usize,
    bytes: usize,
    elapsed: DurationStats,
}

async fn replay_diagnostic(workspace: &Path) -> ReplayDiagnostic {
    let registry = registry_with_config(actor_config());
    let probe = Arc::new(Probe::default());
    let terminal = Arc::new(TestTerminal::new(Arc::clone(&probe)));
    let session_id = id(0xe000_0000);
    register(
        &registry,
        session_id,
        Arc::clone(&terminal),
        Arc::new(TestTranscript::pending(Arc::clone(&probe))),
        workspace,
    )
    .await;

    for index in 0..REPLAY_TURNS {
        let turn_id = id(0xe100_0000 + index as u128);
        registry
            .run_turn(turn(
                session_id,
                turn_id,
                format!("replay diagnostic {index}"),
            ))
            .await
            .unwrap();
        terminal.wait_for_submission(session_id, turn_id).await;
        let cancelled = registry
            .cancel_turn(CancelTurnRequest {
                session_id,
                generation_id: generation(session_id),
                turn_id,
            })
            .await
            .unwrap();
        assert_eq!(cancelled.outcome, CancelOutcome::Cancelled);
        let _ = wait_for_stored_turn(&registry, session_id, turn_id).await;
    }

    let final_sequence = registry
        .inspect(InspectSessionRequest {
            session_id,
            generation_id: generation(session_id),
        })
        .await
        .unwrap()
        .last_sequence;
    let (_, expected_events, expected_pages, expected_bytes) =
        replay_sweep(&registry, session_id, final_sequence).await;
    assert!(expected_events > REPLAY_TURNS);
    assert!(expected_pages > 1);

    let mut samples = Vec::with_capacity(DIAGNOSTIC_SAMPLES);
    for _ in 0..DIAGNOSTIC_SAMPLES {
        let (elapsed, events, pages, bytes) =
            replay_sweep(&registry, session_id, final_sequence).await;
        assert_eq!(events, expected_events);
        assert_eq!(pages, expected_pages);
        assert_eq!(bytes, expected_bytes);
        samples.push(elapsed);
    }

    close_and_unregister(&registry, session_id).await;
    drop(terminal);
    wait_for_resources_released(&probe).await;

    ReplayDiagnostic {
        events: expected_events,
        pages: expected_pages,
        bytes: expected_bytes,
        elapsed: DurationStats::from_durations(samples),
    }
}

async fn replay_sweep(
    registry: &SessionRegistry,
    session_id: uuid::Uuid,
    final_sequence: u64,
) -> (Duration, usize, usize, usize) {
    let started = Instant::now();
    let mut after_sequence = 0;
    let mut events = 0;
    let mut pages = 0;
    let mut bytes = 0;
    while after_sequence < final_sequence {
        let batch = registry
            .events(SubscribeEventsRequest {
                session_id,
                generation_id: generation(session_id),
                after_sequence,
                wait_ms: 0,
                max_events: REPLAY_PAGE_EVENTS,
            })
            .await
            .unwrap();
        assert!(batch.replay_gap.is_none());
        assert!(!batch.events.is_empty());
        assert_eq!(batch.events[0].sequence, after_sequence + 1);
        assert!(
            batch
                .events
                .windows(2)
                .all(|pair| pair[1].sequence == pair[0].sequence + 1)
        );
        let next_sequence = batch.next_sequence;
        events += batch.events.len();
        bytes += serde_json::to_vec(&ResponseEnvelope::success(
            id(0xe200_0000 + pages as u128),
            ResponseResult::Events(batch),
        ))
        .unwrap()
        .len();
        pages += 1;
        after_sequence = next_sequence - 1;
    }
    let elapsed = started.elapsed();
    assert_eq!(after_sequence, final_sequence);
    black_box((events, pages, bytes));
    (elapsed, events, pages, bytes)
}

struct ActorLifecycleDiagnostic {
    startup: DurationStats,
    cleanup: DurationStats,
}

async fn actor_lifecycle_diagnostic(workspace: &Path) -> ActorLifecycleDiagnostic {
    let registry = registry_with_config(actor_config());
    let probe = Arc::new(Probe::default());
    let mut startup_samples = Vec::with_capacity(DIAGNOSTIC_SAMPLES);
    let mut cleanup_samples = Vec::with_capacity(DIAGNOSTIC_SAMPLES);

    for sample in 0..DIAGNOSTIC_SAMPLES {
        let session_ids = (0..ACTOR_BATCH)
            .map(|index| id(0xf000_0000 + (sample * ACTOR_BATCH + index) as u128))
            .collect::<Vec<_>>();
        let started = Instant::now();
        for &session_id in &session_ids {
            register(
                &registry,
                session_id,
                Arc::new(TestTerminal::new(Arc::clone(&probe))),
                Arc::new(TestTranscript::pending(Arc::clone(&probe))),
                workspace,
            )
            .await;
        }
        startup_samples.push(started.elapsed());

        for &session_id in &session_ids {
            let snapshot = registry
                .inspect(InspectSessionRequest {
                    session_id,
                    generation_id: generation(session_id),
                })
                .await
                .unwrap();
            assert_eq!(snapshot.state, SessionState::Ready);
        }

        let started = Instant::now();
        for session_id in session_ids {
            close_and_unregister(&registry, session_id).await;
        }
        cleanup_samples.push(started.elapsed());
        wait_for_resources_released(&probe).await;
    }

    assert_eq!(probe.closes(), DIAGNOSTIC_SAMPLES * ACTOR_BATCH);
    ActorLifecycleDiagnostic {
        startup: DurationStats::from_durations(startup_samples),
        cleanup: DurationStats::from_durations(cleanup_samples),
    }
}

struct PrivateRuntimeDiagnostic {
    runtime_startup: Vec<Duration>,
    terminal_ready: Vec<Duration>,
    terminal_cleanup: Vec<Duration>,
    runtime_cleanup: Vec<Duration>,
}

async fn private_runtime_diagnostic() -> Result<PrivateRuntimeDiagnostic> {
    let candidates = CandidateFiles::discover(&["pmux-rmuxd", "pmux-launcher"])?;
    let root = tempfile::Builder::new()
        .prefix("pmux-performance-")
        .tempdir_in("/tmp")?;
    let root_path = root.path().canonicalize()?;
    set_owner_only(&root_path)?;
    let runtime_parent = root_path.join("runtimes");
    let pid_files = root_path.join("pid-files");
    std::fs::create_dir(&runtime_parent)?;
    std::fs::create_dir(&pid_files)?;
    set_owner_only(&runtime_parent)?;
    set_owner_only(&pid_files)?;

    let mut runtime_startup = Vec::with_capacity(PRIVATE_RUNTIME_SAMPLES);
    let mut terminal_ready = Vec::with_capacity(PRIVATE_RUNTIME_SAMPLES);
    let mut terminal_cleanup = Vec::with_capacity(PRIVATE_RUNTIME_SAMPLES);
    let mut runtime_cleanup = Vec::with_capacity(PRIVATE_RUNTIME_SAMPLES);

    for index in 0..PRIVATE_RUNTIME_SAMPLES {
        let started = Instant::now();
        let runtime = PrivateRuntime::start(PrivateRuntimeConfig {
            rmuxd: candidates.path("pmux-rmuxd").to_path_buf(),
            launcher: candidates.path("pmux-launcher").to_path_buf(),
            runtime_parent: Some(runtime_parent.clone()),
            startup_timeout: Duration::from_secs(10),
            operation_timeout: Duration::from_secs(5),
            lease_ttl: Duration::from_secs(3),
        })
        .await?;
        runtime_startup.push(started.elapsed());

        let runtime_dir = runtime.runtime_dir().to_path_buf();
        let rmux_socket = runtime.rmux_socket().to_path_buf();
        let launcher_socket = runtime_dir.join("launcher.sock");
        let rmux_socket_identity = SocketIdentity::capture(&rmux_socket)?;
        let launcher_socket_identity = SocketIdentity::capture(&launcher_socket)?;
        let baseline_entries = runtime_entries(&runtime_dir)?;
        let sidecar_pid = find_direct_child(
            std::process::id(),
            &[
                candidates.path("pmux-rmuxd").to_string_lossy().as_ref(),
                rmux_socket.to_string_lossy().as_ref(),
            ],
        )?;
        let sidecar = ProcessIdentity::capture(sidecar_pid, rmux_socket.to_string_lossy())?;
        let mut sidecar_guard = ExactProcessGuard::new(sidecar);

        let session_id = uuid::Uuid::new_v4();
        let marker = format!("pmux-performance-{}", session_id.simple());
        let ready = format!("PMUX_PERFORMANCE_READY_{index}");
        let pid_file = pid_files.join(format!("{index}.pid"));
        let program = format!(
            r#"marker='{marker}'; printf '%s\n' "$$" > "$PMUX_PERFORMANCE_PID_FILE"; printf '{ready}\n'; while :; do sleep 30 || :; done"#
        );
        let started = Instant::now();
        let mut terminal = runtime
            .create_terminal(
                session_id,
                24,
                100,
                LaunchSpec {
                    executable: PathBuf::from("/bin/sh"),
                    args: vec!["-c".into(), program],
                    cwd: std::env::current_dir()?.canonicalize()?,
                    environment: EnvironmentSnapshot::capture().patched(
                        [(
                            "PMUX_PERFORMANCE_PID_FILE".to_owned(),
                            pid_file.to_string_lossy().into_owned(),
                        )],
                        [],
                    ),
                },
            )
            .await?;
        terminal
            .wait_visible_text(&ready, Duration::from_secs(5))
            .await?;
        terminal_ready.push(started.elapsed());

        let pane_pid = wait_for_pid_file(&pid_file, Duration::from_secs(5)).await?;
        let pane = ProcessIdentity::capture(pane_pid, &marker)?;
        let mut pane_guard = ExactProcessGuard::new(pane);

        let started = Instant::now();
        ensure!(
            terminal.close().await?,
            "rmux did not prove diagnostic pane process reaping"
        );
        wait_for_process_absence(pane_guard.identity(), Duration::from_secs(5)).await?;
        terminal_cleanup.push(started.elapsed());
        pane_guard.disarm();
        std::fs::remove_file(&pid_file)?;
        ensure!(
            runtime_entries(&runtime_dir)? == baseline_entries,
            "diagnostic terminal retained a private runtime artifact"
        );

        let started = Instant::now();
        runtime.shutdown().await?;
        wait_for_process_absence(sidecar_guard.identity(), Duration::from_secs(10)).await?;
        sidecar_guard.disarm();
        drop(runtime);
        runtime_cleanup.push(started.elapsed());
        ensure!(
            !runtime_dir.exists(),
            "diagnostic private runtime directory survived cleanup"
        );
        ensure!(
            !rmux_socket_identity.remains_at(&rmux_socket)?,
            "diagnostic private rmux socket survived cleanup"
        );
        ensure!(
            !launcher_socket_identity.remains_at(&launcher_socket)?,
            "diagnostic launcher socket survived cleanup"
        );
        ensure!(
            std::fs::read_dir(&runtime_parent)?.next().is_none(),
            "diagnostic private runtime parent retained an entry"
        );
    }

    ensure!(
        std::fs::read_dir(&pid_files)?.next().is_none(),
        "diagnostic pid directory retained an artifact"
    );
    candidates.assert_unchanged()?;
    Ok(PrivateRuntimeDiagnostic {
        runtime_startup,
        terminal_ready,
        terminal_cleanup,
        runtime_cleanup,
    })
}

fn stats_of(samples: &[Duration]) -> DurationStats {
    DurationStats::from_durations(samples.to_vec())
}

// ---------------------------------------------------------------------------
// Per-turn phase decomposition.
//
// The doubles below carry zero model latency, so every millisecond a turn
// spends between `submitted_at_ms` and `completed_at_ms` is pmux's own
// overhead: actor scheduling, transcript polling, the completion gate, and the
// configured transcript drain. Phase bounds are the timestamps the product
// already publishes (`TurnTimings`); no instrumentation is added to
// `crates/service/src` and the boundaries it does not publish are recorded as
// gaps in `phase_breakdown().gaps` instead.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct TurnPhaseSample {
    admission_ms: u64,
    execution_ms: u64,
    completion_ms: u64,
    total_ms: u64,
    reported_drain_stability_ms: u64,
}

impl TurnPhaseSample {
    fn from_timings(timings: &TurnTimings) -> Self {
        let acknowledged = timings
            .prompt_acknowledged_at_ms
            .unwrap_or(timings.submitted_at_ms);
        let candidate = timings.terminal_candidate_at_ms.unwrap_or(acknowledged);
        Self {
            admission_ms: acknowledged.saturating_sub(timings.submitted_at_ms),
            execution_ms: candidate.saturating_sub(acknowledged),
            completion_ms: timings.completed_at_ms.saturating_sub(candidate),
            total_ms: timings
                .completed_at_ms
                .saturating_sub(timings.submitted_at_ms),
            reported_drain_stability_ms: timings.drain_ms.unwrap_or_default(),
        }
    }
}

/// Nearest-rank statistics in milliseconds. Deliberately has no comparison
/// operators: this record is diagnostic output, never an assertion.
#[derive(Clone, Copy)]
struct PhaseStats {
    count: usize,
    min_ms: f64,
    mean_ms: f64,
    p50_ms: f64,
    p95_ms: f64,
    max_ms: f64,
}

impl PhaseStats {
    fn from_ms(samples: &[f64]) -> Self {
        assert!(
            !samples.is_empty(),
            "a phase must record at least one sample"
        );
        let mut sorted = samples.to_vec();
        sorted.sort_by(f64::total_cmp);
        let sum: f64 = sorted.iter().sum();
        Self {
            count: sorted.len(),
            min_ms: sorted[0],
            mean_ms: sum / sorted.len() as f64,
            p50_ms: nearest_rank(&sorted, 0.50),
            p95_ms: nearest_rank(&sorted, 0.95),
            max_ms: sorted[sorted.len() - 1],
        }
    }

    fn as_json(self) -> serde_json::Value {
        serde_json::json!({
            "count": self.count,
            "min_ms": round_ms(self.min_ms),
            "mean_ms": round_ms(self.mean_ms),
            "p50_ms": round_ms(self.p50_ms),
            "p95_ms": round_ms(self.p95_ms),
            "max_ms": round_ms(self.max_ms),
        })
    }
}

fn nearest_rank(sorted: &[f64], quantile: f64) -> f64 {
    let rank = (quantile * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn round_ms(value: f64) -> f64 {
    (value * 1_000.0).round() / 1_000.0
}

fn paired_sum_ms(first: &[Duration], second: &[Duration]) -> Vec<f64> {
    first
        .iter()
        .zip(second.iter())
        .map(|(left, right)| (left.as_secs_f64() + right.as_secs_f64()) * 1_000.0)
        .collect()
}

fn phase(name: &str, boundary: &str, apparatus: &str, stats: PhaseStats) -> serde_json::Value {
    serde_json::json!({
        "phase": name,
        "boundary": boundary,
        "apparatus": apparatus,
        "stats": stats.as_json(),
    })
}

fn phase_breakdown(
    turns: &[TurnPhaseSample],
    runtime: &PrivateRuntimeDiagnostic,
) -> serde_json::Value {
    let launch = PhaseStats::from_ms(&paired_sum_ms(
        &runtime.runtime_startup,
        &runtime.terminal_ready,
    ));
    let admission = PhaseStats::from_ms(&turn_ms(turns, |sample| sample.admission_ms));
    let execution = PhaseStats::from_ms(&turn_ms(turns, |sample| sample.execution_ms));
    let completion = PhaseStats::from_ms(&turn_ms(turns, |sample| sample.completion_ms));
    let close = PhaseStats::from_ms(&paired_sum_ms(
        &runtime.terminal_cleanup,
        &runtime.runtime_cleanup,
    ));
    let turn_total = PhaseStats::from_ms(&turn_ms(turns, |sample| sample.total_ms));
    let drain_stability =
        PhaseStats::from_ms(&turn_ms(turns, |sample| sample.reported_drain_stability_ms));

    serde_json::json!({
        "unit": "ms",
        "model_latency": "none: every phase below is measured against zero-latency doubles",
        "phases": [
            phase(
                "launch",
                "PrivateRuntime::start entry -> pane ready text visible",
                "real pmux-rmuxd sidecar + real pmux-launcher + real PTY pane (private_rmux_lifecycle: runtime_startup + terminal_ready)",
                launch,
            ),
            phase(
                "admission",
                "TurnTimings.submitted_at_ms -> TurnTimings.prompt_acknowledged_at_ms",
                "production SessionRegistry with a transcript double; the real two-gate editor fence in driver_io is NOT on this path (see gaps)",
                admission,
            ),
            phase(
                "execution",
                "TurnTimings.prompt_acknowledged_at_ms -> TurnTimings.terminal_candidate_at_ms",
                "production SessionRegistry; the double answers instantly, so this is transcript-poll latency only",
                execution,
            ),
            phase(
                "completion",
                "TurnTimings.terminal_candidate_at_ms -> TurnTimings.completed_at_ms",
                "production completion gate: drain + ready/quiet evidence + confirmation re-poll + commit",
                completion,
            ),
            phase(
                "close",
                "close request -> pane process absence proven -> sidecar absence proven",
                "real process-boundary proof (private_rmux_lifecycle: terminal_cleanup + runtime_cleanup)",
                close,
            ),
        ],
        "totals": {
            "turn_total": turn_total.as_json(),
            "turn_total_boundary": "TurnTimings.submitted_at_ms -> TurnTimings.completed_at_ms",
            "composed_session_p50_ms": round_ms(
                launch.p50_ms + turn_total.p50_ms + close.p50_ms,
            ),
            "composed_from_independent_apparatus": true,
        },
        "reported_drain_stability": drain_stability.as_json(),
        "configured": {
            "transcript_drain_ms": TURN_PHASE_DRAIN_MS,
            "actor_poll_interval_ms": actor_poll_interval_ms(),
            "turns": turns.len(),
            "note": "the diagnostic configures a bounded drain; production cells configure their own, with 2000 ms as the untested fallback",
        },
        "product_constants": {
            "source": "transcribed from crates/service/src/driver_io.rs, not imported: these are private consts",
            "editor_stability_ms": 250,
            "post_paste_render_stability_ms": 250,
            "terminal_poll_interval_ms": 25,
            "evidence_timeout_ms": 400,
            "input_gate_cap_ms": 15_000,
            "recovery_timeout_ms": 5_000,
            "transcript_drain_fallback_ms": 2_000,
            // NAMES, not line numbers, and the change is the point. This map
            // published six `driver_io.rs:<line>` citations into the Gate A
            // performance receipt. Every one was correct at `405fccd` and every
            // one was wrong by 2026-08-07 -- the file grew seven thousand lines
            // around them and nothing here could notice, because a transcribed
            // line number is a claim no test can check. A name can be grepped,
            // and three of these had no name until the post-marker catch window
            // needed one (`current-state.md` 9.29).
            "citations": {
                "file": "crates/service/src/driver_io.rs",
                "editor_stability_ms": "SCREEN_QUIET_FOR_MS",
                "post_paste_render_stability_ms": "SCREEN_QUIET_FOR_MS",
                "terminal_poll_interval_ms": "TERMINAL_POLL_INTERVAL_MS",
                "evidence_timeout_ms": "evidence_timeout, in RmuxTerminalControl::new",
                "input_gate_cap_ms": "INPUT_GATE_MAX_DURATION",
                "recovery_timeout_ms": "recovery_timeout, in RmuxTerminalControl::new",
                "transcript_drain_fallback_ms": "DEFAULT_UNTESTED_TRANSCRIPT_DRAIN_MS, in crates/service/src/compatibility.rs",
            },
            // The two quantities the first three multiply out to, which is what
            // the completion phase below is actually made of. Neither is a
            // separate wait; both are consequences of the constants above, and
            // that is exactly why they are published beside them rather than
            // left to be re-derived by whoever reads the completion number.
            "derived": {
                "commit_loop_sampling_period_ms": 275,
                "post_marker_catch_window_ms": 550,
                "post_marker_catch_window_floor_ms": 438,
                "source": "COMMIT_LOOP_SAMPLING_PERIOD_MS in driver_io.rs; post_marker_catch_window_ms and POST_MARKER_CATCH_WINDOW_FLOOR_MS in crates/service/src/v1/backend.rs",
            },
        },
        "gaps": [
            {
                "boundary": "paste sent -> Enter sent",
                "observable": false,
                "why": "driver_io publishes no timestamp between the two admission gates; the only published admission bound is prompt_acknowledged_at_ms. Recorded rather than instrumented.",
            },
            {
                "boundary": "compatibility probe",
                "observable": false,
                "why": "the launch phase measured here is runtime setup + sidecar spawn + launcher exec + pane readiness; validate_v1_terminal_support runs in native.rs and emits no timestamp.",
            },
            {
                "boundary": "admission excludes the real editor fence",
                "observable": false,
                "why": "TerminalControl is a double here, so RmuxTerminalControl::submit_prompt (2 x 250 ms stability windows at 25 ms poll granularity) is not exercised. Its floor is constants-derived, in product_constants, not measured.",
            },
            {
                "boundary": "execution is not model latency",
                "observable": true,
                "why": "against a zero-latency double this phase is transcript-poll latency. Real-Claude model time belongs to Gate B, not to this record.",
            },
        ],
    })
}

fn turn_ms(turns: &[TurnPhaseSample], select: impl Fn(&TurnPhaseSample) -> u64) -> Vec<f64> {
    turns.iter().map(|sample| select(sample) as f64).collect()
}

fn actor_poll_interval_ms() -> u64 {
    u64::try_from(SessionActorConfig::default().poll_interval.as_millis()).unwrap_or(u64::MAX)
}

async fn turn_phase_diagnostic() -> Vec<TurnPhaseSample> {
    let mut samples = Vec::with_capacity(TURN_PHASE_SAMPLES);
    for index in 0..TURN_PHASE_SAMPLES {
        let registry = SessionRegistry::new(SessionActorConfig::default());
        let session_id = id(0xd000_0000 + index as u128);
        let turn_id = id(0xd100_0000 + index as u128);
        let terminal = Arc::new(PhaseTerminal);
        let transcript = Arc::new(PhaseTranscript::default());
        registry
            .register(SessionRegistration {
                agent: None,
                owner: pseudomux_service::v1::SessionOwner::Caller,
                session_id,
                generation_id: generation(session_id),
                cwd: "/pmux-phase-diagnostic".to_owned(),
                compatibility: CompatibilityReport {
                    claude_version: "diagnostic".to_owned(),
                    os: std::env::consts::OS.to_owned(),
                    arch: std::env::consts::ARCH.to_owned(),
                    terminal_profile: TerminalProfile::Transparent,
                    input_transport: InputTransport::Sdk,
                    tested: true,
                    transcript_drain_ms: TURN_PHASE_DRAIN_MS,
                },
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
        registry
            .run_turn(turn(session_id, turn_id, TURN_PHASE_PROMPT))
            .await
            .unwrap();

        let stored = tokio::time::timeout(TURN_PHASE_BUDGET, async {
            loop {
                if let Some(stored) = registry
                    .stored_turn(session_id, generation(session_id), turn_id)
                    .await
                    .unwrap()
                {
                    return stored;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("phase diagnostic turn {index} did not become terminal"));
        let StoredTurnTerminal::Result(result) = stored else {
            panic!("phase diagnostic turn {index} did not complete through the transcript");
        };
        assert_eq!(result.outcome, TurnOutcome::Completed);
        assert_eq!(result.text, "phase diagnostic answer");
        samples.push(TurnPhaseSample::from_timings(&result.timings));
        close_and_unregister(&registry, session_id).await;
    }
    samples
}

struct PhaseTerminal;

#[async_trait]
impl TerminalControl for PhaseTerminal {
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
        Ok(TerminalEvidence {
            ready_prompt: true,
            quiet: true,
            ..TerminalEvidence::default()
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
        Ok(true)
    }
}

#[derive(Default)]
struct PhaseTranscriptState {
    prompt_emitted: bool,
    candidate_emitted: bool,
    candidate_at: Option<Instant>,
    position: u64,
}

#[derive(Default)]
struct PhaseTranscript {
    state: std::sync::Mutex<PhaseTranscriptState>,
}

#[async_trait]
impl TranscriptSource for PhaseTranscript {
    async fn arm_at_eof(&self, _session_id: SessionId) -> DriverResult<TranscriptArm> {
        Ok(TranscriptArm::default())
    }

    async fn poll(
        &self,
        _session_id: SessionId,
        _position: &TranscriptPosition,
    ) -> DriverResult<TranscriptBatch> {
        // Real transcript polling performs filesystem I/O. Keep the scheduling
        // boundary so the recorded phase spans include a yield per poll.
        tokio::task::yield_now().await;
        let mut state = self.state.lock().unwrap();
        let mut rows = Vec::new();
        if !state.prompt_emitted {
            rows.push(phase_prompt_row());
            state.prompt_emitted = true;
            state.position += 1;
        } else if !state.candidate_emitted {
            rows.push(phase_candidate_row());
            state.candidate_emitted = true;
            state.candidate_at = Some(Instant::now());
            state.position += 1;
        }
        // Report real elapsed stability so the drain the actor waits out is a
        // genuine wall-clock wait against the configured cell constant.
        let stable_for_ms = state.candidate_at.map_or(0, |at| {
            u64::try_from(at.elapsed().as_millis()).unwrap_or(u64::MAX)
        });
        let batch = TranscriptBatch {
            position: TranscriptPosition {
                generation: 0,
                offset: state.position,
            },
            rows,
            drain: TranscriptDrainEvidence {
                at_eof: true,
                has_partial_line: false,
                stable_for_ms,
            },
        };
        drop(state);
        Ok(batch)
    }
}

fn phase_prompt_row() -> ParsedRow {
    phase_row(
        0,
        br#"{"parentUuid":null,"sessionId":"phase","type":"user","message":{"content":"phase diagnostic"},"uuid":"phase-prompt","promptSource":"typed","promptId":"phase-prompt-id"}"#,
    )
}

fn phase_candidate_row() -> ParsedRow {
    phase_row(
        1,
        br#"{"parentUuid":"phase-prompt","sessionId":"phase","type":"assistant","uuid":"phase-answer","message":{"id":"phase-message","model":"diagnostic-model","content":[{"type":"text","text":"phase diagnostic answer"}],"stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":2}}}"#,
    )
}

fn phase_row(index: u64, bytes: &[u8]) -> ParsedRow {
    JsonlParser::new(ParseMode::Strict)
        .parse(&CompleteLine {
            location: SourceLocation {
                line: index + 1,
                byte_offset: index * 1_000,
            },
            bytes: bytes.to_vec(),
        })
        .unwrap()
}
