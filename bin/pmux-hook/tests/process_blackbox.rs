#![cfg(unix)]

use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use pseudomux_protocol::v1::LifecycleMode;
use pseudomux_service::hybrid_hooks::{
    LifecycleEventKind, MAX_HOOK_FRAME_BYTES, PreparedLifecycle, prepare_lifecycle,
};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use uuid::Uuid;

#[path = "../../../tests/support/candidate_binary.rs"]
mod candidate_binary;
use candidate_binary::CandidateBinaries;

/// The hook's own self-imposed deadline, `HOOK_CLIENT_IO_TIMEOUT` in
/// `bin/pmux-hook/src/main.rs`. Tests derive their expectations from it; none
/// of them asserts that this host finished the work inside it.
const HOOK_SELF_DEADLINE: Duration = Duration::from_secs(5);

/// Harness liveness budget.
///
/// This is **not** a performance bound and no assertion in this file may treat
/// it as one. It exists only so that a hook which never returns *at all* fails
/// in finite time instead of hanging the suite forever. At 24x
/// `HOOK_SELF_DEADLINE` it is out of reach of host contention; anything that
/// does reach it is a process that did not terminate, not a machine that was
/// busy. The only cost of its size is how long a genuinely broken product
/// takes to fail, and that is the cheap direction.
///
/// History: every wall-clock *upper* bound in this file used to be 8 s, one
/// second above a 5 s product deadline, which made an ordinary scheduling
/// delay indistinguishable from an unbounded relay (debt row C9 in
/// `docs/current-state.md`: 1 failure in 3 consecutive runs, under load). That
/// margin is measurably too thin: on a 10-core host at load average ~100 the
/// hook still bounded itself correctly but took 21.1 s of wall clock to do it.
const LIVENESS_BUDGET: Duration = Duration::from_secs(120);

struct Output {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Output {
    fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

fn private_runtime() -> tempfile::TempDir {
    let runtime = tempfile::Builder::new()
        .prefix("ph-")
        .tempdir_in("/tmp")
        .unwrap();
    fs::set_permissions(runtime.path(), fs::Permissions::from_mode(0o700)).unwrap();
    runtime
}

fn candidates() -> &'static CandidateBinaries {
    static CANDIDATES: OnceLock<CandidateBinaries> = OnceLock::new();
    CANDIDATES.get_or_init(|| {
        CandidateBinaries::discover(
            std::env::var_os("PMUX_TEST_BIN_DIR").map(PathBuf::from),
            [(
                "pmux-hook".to_owned(),
                PathBuf::from(env!("CARGO_BIN_EXE_pmux-hook")),
            )],
        )
        .unwrap_or_else(|error| panic!("failed to bind pmux-hook candidate: {error}"))
    })
}

fn hook_binary() -> PathBuf {
    candidates().path("pmux-hook").to_path_buf()
}

fn assert_candidate_unchanged() {
    candidates()
        .assert_unchanged()
        .unwrap_or_else(|error| panic!("pmux-hook candidate changed during test: {error}"));
}

async fn run_hook(socket: &Path, session_id: Uuid, event: &str, stdin: &[u8]) -> Output {
    let mut command = Command::new(hook_binary());
    command
        .arg("--socket")
        .arg(socket)
        .arg("--session-id")
        .arg(session_id.to_string())
        .arg("--event")
        .arg(event)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().expect("failed to spawn pmux-hook");
    let mut child_stdin = child.stdin.take().expect("hook stdin is piped");
    if let Err(error) = child_stdin.write_all(stdin).await {
        assert_eq!(
            error.kind(),
            io::ErrorKind::BrokenPipe,
            "hook stdin: {error}"
        );
    }
    drop(child_stdin);
    // Liveness fallback, not a stopwatch: reaching this means the process never
    // terminated on its own. `kill_on_drop` reaps the child as the panic unwinds.
    let output = tokio::time::timeout(LIVENESS_BUDGET, child.wait_with_output())
        .await
        .expect("pmux-hook never terminated on its own; the relay is unbounded")
        .expect("failed to wait for pmux-hook");
    assert_candidate_unchanged();
    Output {
        status: output.status,
        stdout: output.stdout,
        stderr: output.stderr,
    }
}

async fn prepare(runtime: &Path, session_id: Uuid) -> PreparedLifecycle {
    prepare_lifecycle(
        &LifecycleMode::Hybrid {
            hook_timeout_ms: 2_000,
        },
        runtime,
        session_id,
        &hook_binary(),
        &[],
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn all_three_events_cross_the_real_stdio_and_private_relay_boundary() {
    let runtime = private_runtime();
    let session_id = Uuid::parse_str("12345678-1234-4234-8234-123456789abc").unwrap();
    let mut prepared = prepare(runtime.path(), session_id).await;
    let hybrid = prepared.hybrid().expect("Hybrid lifecycle was prepared");
    let socket = hybrid.socket_path().to_path_buf();
    let settings = hybrid.settings_path().to_path_buf();
    let socket_metadata = fs::symlink_metadata(&socket).unwrap();
    assert!(socket_metadata.file_type().is_socket());
    assert_eq!(socket_metadata.permissions().mode() & 0o777, 0o600);
    assert_eq!(
        fs::metadata(runtime.path()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&settings).unwrap().permissions().mode() & 0o777,
        0o600
    );

    for (sequence, event_name, event_kind, failure) in [
        (1, "SessionStart", LifecycleEventKind::SessionStart, false),
        (2, "Stop", LifecycleEventKind::Stop, false),
        (3, "StopFailure", LifecycleEventKind::StopFailure, true),
    ] {
        let transcript = runtime.path().join(format!("{event_name}.jsonl"));
        let output_secret = format!("private-output-{event_name}");
        let usage_secret = format!("private-usage-{event_name}");
        let payload = serde_json::to_vec(&json!({
            "session_id": session_id,
            "hook_event_name": event_name,
            "transcript_path": transcript,
            "output": output_secret,
            "usage": {"secret": usage_secret},
        }))
        .unwrap();
        let output = run_hook(&socket, session_id, event_name, &payload).await;
        assert!(output.status.success(), "{}", output.stderr_text());
        assert!(output.stdout.is_empty(), "hook protocol leaked onto stdout");
        assert!(output.stderr.is_empty(), "{}", output.stderr_text());

        let observation =
            tokio::time::timeout(LIVENESS_BUDGET, prepared.hybrid_mut().unwrap().recv())
                .await
                .expect("relay observation timed out")
                .expect("relay closed before observation");
        assert_eq!(observation.sequence(), sequence);
        assert_eq!(observation.session_id(), session_id);
        assert_eq!(observation.event(), event_kind);
        assert_eq!(observation.transcript_path(), Some(transcript.as_path()));
        assert_eq!(observation.failure_observed(), failure);
        let debug = format!("{observation:?}");
        assert!(!debug.contains(&output_secret));
        assert!(!debug.contains(&usage_secret));
    }

    let settings_text = fs::read_to_string(&settings).unwrap();
    assert!(!settings_text.contains("private-output-"));
    assert!(!settings_text.contains("private-usage-"));
    drop(prepared);
    assert!(
        !socket.exists(),
        "relay socket remained after lifecycle drop"
    );
    assert!(
        !settings.exists(),
        "generated settings remained after lifecycle drop"
    );
}

#[tokio::test]
async fn stdin_bounds_and_json_validation_fail_before_connecting_without_echo() {
    let runtime = private_runtime();
    let missing_socket = runtime.path().join("missing.sock");
    let session_id = Uuid::parse_str("22345678-1234-4234-8234-123456789abc").unwrap();
    let invalid_secret = b"{invalid-hook-payload-secret";
    let oversized = vec![b'x'; MAX_HOOK_FRAME_BYTES + 1];

    for (label, payload, expected) in [
        ("empty", &b""[..], "payload is empty"),
        ("invalid", &invalid_secret[..], "not valid JSON"),
        ("oversized", oversized.as_slice(), "size limit"),
    ] {
        let output = run_hook(&missing_socket, session_id, "Stop", payload).await;
        let stderr = output.stderr_text();
        assert_eq!(output.status.code(), Some(1), "{label}: {stderr}");
        assert!(output.stdout.is_empty(), "{label}: stdout was not empty");
        assert!(stderr.contains(expected), "{label}: {stderr}");
        assert!(!stderr.contains("invalid-hook-payload-secret"));
        assert!(!stderr.contains(&"x".repeat(128)));
    }
}

#[tokio::test]
async fn relay_rejection_is_nonzero_redacted_and_does_not_create_an_observation() {
    let runtime = private_runtime();
    let session_id = Uuid::parse_str("32345678-1234-4234-8234-123456789abc").unwrap();
    let other_session = Uuid::parse_str("42345678-1234-4234-8234-123456789abc").unwrap();
    let mut prepared = prepare(runtime.path(), session_id).await;
    let socket = prepared.hybrid().unwrap().socket_path().to_path_buf();
    let payload = serde_json::to_vec(&json!({
        "session_id": other_session,
        "hook_event_name": "Stop",
        "output": "rejected-hook-output-secret",
    }))
    .unwrap();
    let output = run_hook(&socket, session_id, "Stop", &payload).await;
    let stderr = output.stderr_text();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(stderr.contains("rejected the event"), "{stderr}");
    assert!(!stderr.contains("rejected-hook-output-secret"));
    assert!(prepared.hybrid_mut().unwrap().try_recv().is_err());
}

/// The relay bound is a property of the hook, not of the host clock.
///
/// WHAT THIS ASSERTS. A hook whose relay peer accepts the connection and then
/// never reads it terminates *of its own accord* while that peer is still
/// stalled: it dies by exiting, not by a signal, with the hook's failure
/// status, and it says its own deadline expired. Every one of those facts is
/// produced by the product, so together they are exactly the claim "the relay
/// is bounded" and nothing else.
///
/// WHAT THIS DOES NOT ASSERT. How many milliseconds of host wall-clock the
/// exit took. This test used to assert `elapsed < 8 s` and failed 1 run in 3
/// under load (debt row C9): an *upper* bound on elapsed time gates on the
/// machine, not on the product, so widening the gate past the claim could only
/// add false failures — a busy host was never part of "the relay is bounded".
/// Elapsed time is now recorded and printed as an observation.
///
/// WHY IT STILL CATCHES A GENUINELY UNBOUNDED RELAY. Three distinct exits:
/// a hook that never returns is not reaped inside `LIVENESS_BUDGET` and
/// `run_hook` panics; a hook that had to be killed carries a signal and no
/// exit code; a hook that gave up without ever waiting trips the lower bound.
#[tokio::test]
async fn stalled_relay_is_bounded_and_does_not_echo_private_input() {
    let runtime = private_runtime();
    let socket = runtime.path().join("stalled.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
    let stall = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let _ = accepted_tx.send(());
        // Hold the accepted connection open and unread for longer than any
        // budget in this test, so the peer can never be the one that gives up.
        tokio::time::sleep(LIVENESS_BUDGET * 2).await;
        drop(stream);
    });
    let session_id = Uuid::parse_str("62345678-1234-4234-8234-123456789abc").unwrap();
    let private = "stalled-hook-private-output";
    let payload = serde_json::to_vec(&json!({
        "session_id": session_id,
        "hook_event_name": "Stop",
        "output": private,
    }))
    .unwrap();

    let started = Instant::now();
    let output = run_hook(&socket, session_id, "Stop", &payload).await;
    // Observation, never a gate: printed for forensics under `--nocapture`.
    let elapsed = started.elapsed();
    println!("stalled relay self-terminated after {elapsed:?} (observation only)");
    let stderr = output.stderr_text();
    assert!(
        output.status.signal().is_none(),
        "the hook was terminated by a signal rather than bounding itself: {:?}",
        output.status
    );
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(output.stdout.is_empty());
    assert!(stderr.contains("timed out"), "{stderr}");
    assert!(!stderr.contains(private));
    // Lower bound only. Load can delay an exit but cannot hurry one, so this
    // direction is not load-sensitive; it catches a hook that reports a
    // timeout it never actually waited out.
    assert!(
        elapsed >= HOOK_SELF_DEADLINE - Duration::from_secs(1),
        "returned before it could have waited on the relay: {elapsed:?}"
    );

    // Confirmed LAST, deliberately. `accepted_tx` is owned by the `stall` task,
    // which is parked in `accept()`; a `oneshot::Receiver` only errors when its
    // sender is dropped. So a hook that exits BEFORE connecting -- anything
    // failing in path validation, the stdin read, the size limit or the JSON
    // decode, all of which run pre-connect in main.rs -- leaves this await
    // pending forever. Sitting ahead of the assertions, it preempted them and
    // hung the whole gate command instead of failing it, producing no verdict at
    // all. Moved here, that same regression trips the lower bound above in under
    // two seconds, which is what this test's doc comment already promised.
    accepted_rx
        .await
        .expect("the stalled relay never accepted the hook connection");
    assert!(!stall.is_finished(), "the stalled peer stopped stalling");
    stall.abort();
    let _ = stall.await;
}

/// Same disposition as `stalled_relay_is_bounded_and_does_not_echo_private_input`,
/// applied to the other half of the deadline: a writer that opens stdin and
/// never closes it. The gate is that the hook exits by itself, unsignalled,
/// reporting its own timeout — not that it did so within N milliseconds of
/// this host's wall-clock.
#[tokio::test]
async fn partial_open_stdin_is_covered_by_the_same_process_deadline() {
    let runtime = private_runtime();
    let socket = runtime.path().join("unused.sock");
    let session_id = Uuid::parse_str("72345678-1234-4234-8234-123456789abc").unwrap();
    let private = "partial-hook-private-input";
    let mut command = Command::new(hook_binary());
    command
        .arg("--socket")
        .arg(&socket)
        .arg("--session-id")
        .arg(session_id.to_string())
        .arg("--event")
        .arg("Stop")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().expect("failed to spawn pmux-hook");
    let mut child_stdin = child.stdin.take().expect("hook stdin is piped");
    child_stdin
        .write_all(format!(r#"{{"output":"{private}""#).as_bytes())
        .await
        .unwrap();
    child_stdin.flush().await.unwrap();

    let started = Instant::now();
    let status = match tokio::time::timeout(LIVENESS_BUDGET, child.wait()).await {
        Ok(status) => status.expect("failed to wait for pmux-hook"),
        Err(_) => {
            child.start_kill().expect("failed to kill stuck pmux-hook");
            tokio::time::timeout(LIVENESS_BUDGET, child.wait())
                .await
                .expect("killed pmux-hook was not reaped")
                .expect("failed to reap killed pmux-hook");
            panic!("pmux-hook held a partial open stdin forever; it never bounded itself");
        }
    };
    let elapsed = started.elapsed();
    println!("partial-open stdin self-terminated after {elapsed:?} (observation only)");
    drop(child_stdin);
    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut stdout)
        .await
        .unwrap();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .await
        .unwrap();
    assert_candidate_unchanged();

    assert!(
        status.signal().is_none(),
        "the hook was terminated by a signal rather than bounding itself: {status:?}"
    );
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(stdout.is_empty());
    assert!(stderr.contains("timed out"), "{stderr}");
    assert!(!stderr.contains(private));
    // Lower bound only; see the note in the stalled-relay test above.
    assert!(
        elapsed >= HOOK_SELF_DEADLINE - Duration::from_secs(1),
        "returned before it could have waited on stdin: {elapsed:?}"
    );
}

#[tokio::test]
async fn clap_and_socket_validation_use_exit_two_and_one_respectively() {
    let session_id = Uuid::parse_str("52345678-1234-4234-8234-123456789abc").unwrap();
    let clap = run_hook(Path::new("relative.sock"), session_id, "stop", b"{}").await;
    assert_eq!(clap.status.code(), Some(2));
    assert!(clap.stdout.is_empty());

    let relative = run_hook(Path::new("relative.sock"), session_id, "Stop", b"{}").await;
    assert_eq!(relative.status.code(), Some(1));
    assert!(relative.stdout.is_empty());
    assert!(relative.stderr_text().contains("absolute"));
}
