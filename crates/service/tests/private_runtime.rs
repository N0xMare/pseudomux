#![cfg(unix)]

mod process_support;

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use pseudomux_rmux::{EnvironmentSnapshot, LaunchSpec, TerminalSession};
use pseudomux_service::runtime::{
    PINNED_RMUX_VERSION, PrivateRuntime, PrivateRuntimeConfig, SessionRuntime,
};
use tempfile::TempDir;
use tokio::io::AsyncReadExt;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::time::Instant;
use uuid::Uuid;

use process_support::{
    CandidateFiles, ExactProcessGuard, ProcessIdentity, SocketIdentity, find_direct_child,
    runtime_entries, set_owner_only, wait_for_pid_file, wait_for_process_absence,
};

const FAULT_ITERATIONS: usize = 4;

/// The startup reachability check, after the thing that used to carry it was
/// removed.
///
/// `RmuxBackend::connect` opened a daemon-wide transport in its constructor,
/// and that `.connect()` was silently doing a second job: it was the only place
/// `PrivateRuntime::start` ever learned that the announced private socket was
/// unusable. Per-session transports delete the daemon-wide one, so the
/// constructor became `RmuxBackend::configure` and does no I/O at all. Without a
/// deliberate replacement, an unusable socket would have moved from a failed
/// start to a failed *first session*, with a healthy readiness announcement
/// logged in between and nothing anywhere blaming the socket.
///
/// The fault is a sidecar that passes every existing readiness check --
/// announces the right record kind, the pinned rmux version, and the exact
/// socket path it was told to use -- and then never binds it. That is precisely
/// the gap `wait_for_ready` cannot see and `probe_control_plane` must.
///
/// Deliberately not `#[ignore]`d and deliberately using no candidate binary:
/// the whole point is that this check has to hold on every run, not only on the
/// ones that build a real sidecar.
#[tokio::test]
async fn private_runtime_start_refuses_a_sidecar_whose_announced_socket_is_unreachable() {
    let root = tempfile::Builder::new()
        .prefix("pmux-unreachable-socket-")
        .tempdir_in("/tmp")
        .unwrap();
    let root_path = root.path().canonicalize().unwrap();
    set_owner_only(&root_path).unwrap();
    let runtime_parent = root_path.join("runtimes");
    std::fs::create_dir(&runtime_parent).unwrap();
    set_owner_only(&runtime_parent).unwrap();

    // `--socket <path>` is the first argument pair the runtime passes, so `$2`
    // is the exact path `wait_for_ready` will compare against.
    let fake_rmuxd = root_path.join("fake-rmuxd");
    std::fs::write(
        &fake_rmuxd,
        format!(
            "#!/bin/sh\n\
             printf '{{\"kind\":\"pmux-rmuxd-ready\",\"rmux_version\":\"{PINNED_RMUX_VERSION}\",\"socket\":\"%s\"}}\\n' \"$2\"\n\
             exec sleep 60\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake_rmuxd, std::fs::Permissions::from_mode(0o700)).unwrap();

    let started = PrivateRuntime::start(PrivateRuntimeConfig {
        rmuxd: fake_rmuxd.clone(),
        launcher: fake_rmuxd.clone(),
        runtime_parent: Some(runtime_parent.clone()),
        startup_timeout: Duration::from_secs(10),
        operation_timeout: Duration::from_secs(2),
        lease_ttl: Duration::from_secs(5),
    })
    .await;
    let Err(error) = started else {
        panic!("a sidecar that never binds its announced socket must fail at start");
    };

    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("private rmux control plane is unusable"),
        "the start failure must name the control-plane probe, got: {rendered}"
    );
    let residue = runtime_entries(&runtime_parent).unwrap();
    assert!(
        residue.is_empty(),
        "a probe-rejected start left private runtime artifacts behind: {residue:?}"
    );
}

/// Run with:
/// `cargo build -p pmux-rmuxd -p pmux-launcher && cargo test -p pseudomux-service --test private_runtime -- --ignored`
#[tokio::test]
#[ignore = "spawns the private rmux sidecar and a real PTY process"]
async fn private_sidecar_launch_and_cleanup_smoke() {
    let candidates = CandidateFiles::discover(&["pmux-rmuxd", "pmux-launcher"]).unwrap();
    let root = tempfile::Builder::new()
        .prefix("pmux-private-smoke-")
        .tempdir_in("/tmp")
        .unwrap();
    let root_path = root.path().canonicalize().unwrap();
    set_owner_only(&root_path).unwrap();
    let runtime_parent = root_path.join("runtimes");
    std::fs::create_dir(&runtime_parent).unwrap();
    set_owner_only(&runtime_parent).unwrap();
    let config = PrivateRuntimeConfig {
        rmuxd: candidates.path("pmux-rmuxd").to_path_buf(),
        launcher: candidates.path("pmux-launcher").to_path_buf(),
        runtime_parent: Some(runtime_parent.clone()),
        startup_timeout: Duration::from_secs(10),
        operation_timeout: Duration::from_secs(10),
        lease_ttl: Duration::from_secs(5),
    };
    let runtime = PrivateRuntime::start(config).await.unwrap();
    let process_root = tempfile::tempdir().unwrap();
    let child_pid_file = process_root.path().join("child.pid");
    let child_marker = format!("pmux-normal-close-{}", Uuid::new_v4().simple());
    let shell_program = format!(
        "marker='{child_marker}'; (trap '' HUP TERM INT; while :; do sleep 30; done) & child=$!; printf '%s\\n' \"$child\" > \"$PMUX_CHILD_PID_FILE\"; printf 'PMUX_PRIVATE_RUNTIME_READY\\n'; wait \"$child\""
    );
    let mut terminal = runtime
        .create_terminal(
            Uuid::new_v4(),
            24,
            100,
            LaunchSpec {
                executable: PathBuf::from("/bin/sh"),
                args: vec!["-c".into(), shell_program],
                cwd: std::env::current_dir().unwrap(),
                environment: EnvironmentSnapshot::capture().patched(
                    [(
                        "PMUX_CHILD_PID_FILE".to_owned(),
                        child_pid_file.to_string_lossy().into_owned(),
                    )],
                    [],
                ),
            },
        )
        .await
        .unwrap();

    terminal
        .wait_visible_text("PMUX_PRIVATE_RUNTIME_READY", Duration::from_secs(5))
        .await
        .unwrap();
    let snapshot = terminal
        .wait_quiet(Duration::from_millis(150), Duration::from_secs(5))
        .await
        .unwrap();
    assert!(
        snapshot.visible_text.contains("PMUX_PRIVATE_RUNTIME_READY"),
        "unexpected snapshot: {:?}",
        snapshot.visible_text
    );
    let child_pid = wait_for_pid_file(&child_pid_file, Duration::from_secs(5))
        .await
        .unwrap();
    let child_identity = ProcessIdentity::capture(child_pid, child_marker).unwrap();
    let mut child_cleanup = ExactProcessGuard::new(child_identity);
    child_cleanup.identity().assert_running().unwrap();

    assert!(
        terminal.close().await.unwrap(),
        "normal close did not positively confirm process reaping"
    );
    wait_for_process_absence(child_cleanup.identity(), Duration::from_secs(5))
        .await
        .unwrap();
    child_cleanup.disarm();
    runtime.shutdown().await.unwrap();
    drop(runtime);
    assert!(runtime_entries(&runtime_parent).unwrap().is_empty());
    candidates.assert_unchanged().unwrap();
}

#[tokio::test]
#[ignore = "spawns the private rmux sidecar and validates real wait-timeout redaction"]
async fn private_wait_timeout_redacts_matcher_and_last_screen() {
    let candidates = CandidateFiles::discover(&["pmux-rmuxd", "pmux-launcher"]).unwrap();
    let root = tempfile::Builder::new()
        .prefix("pmux-private-timeout-")
        .tempdir_in("/tmp")
        .unwrap();
    let root_path = root.path().canonicalize().unwrap();
    set_owner_only(&root_path).unwrap();
    let runtime_parent = root_path.join("runtimes");
    std::fs::create_dir(&runtime_parent).unwrap();
    set_owner_only(&runtime_parent).unwrap();
    let config = PrivateRuntimeConfig {
        rmuxd: candidates.path("pmux-rmuxd").to_path_buf(),
        launcher: candidates.path("pmux-launcher").to_path_buf(),
        runtime_parent: Some(runtime_parent.clone()),
        startup_timeout: Duration::from_secs(10),
        operation_timeout: Duration::from_secs(10),
        lease_ttl: Duration::from_secs(5),
    };
    let runtime = PrivateRuntime::start(config).await.unwrap();
    let screen_secret = format!("pmux-private-screen-{}", Uuid::new_v4().simple());
    let matcher_secret = format!("pmux-private-matcher-{}", Uuid::new_v4().simple());
    let mut terminal = runtime
        .create_terminal(
            Uuid::new_v4(),
            24,
            100,
            LaunchSpec {
                executable: PathBuf::from("/bin/sh"),
                args: vec![
                    "-c".into(),
                    "printf '%s\\n' \"$PMUX_TIMEOUT_SCREEN\"; exec sleep 30".into(),
                ],
                cwd: std::env::current_dir().unwrap(),
                environment: EnvironmentSnapshot::capture().patched(
                    [("PMUX_TIMEOUT_SCREEN".to_owned(), screen_secret.clone())],
                    [],
                ),
            },
        )
        .await
        .unwrap();

    terminal
        .wait_visible_text(&screen_secret, Duration::from_secs(5))
        .await
        .unwrap();
    // The deadline must exceed one snapshot round trip, and by a real margin.
    // rmux-sdk only reports a wait as a `WaitTimeout` -- the variant pmux
    // redacts into "terminal wait timed out" -- once the wait has a *previous*
    // snapshot to attach to it (rmux-sdk wait/visible.rs:174-181, wait.rs:356).
    // Before the first snapshot completes, `last_snapshot` is still `None` and
    // the wait's shared deadline surfaces as a plain transport timeout
    // (wait.rs:333-344), which pmux correctly classifies as a lost control
    // plane. A 40 ms deadline sat close enough to one snapshot RTT that the
    // first snapshot lost that race roughly one run in four, and the test then
    // failed on the redaction assertion while nothing about redaction had gone
    // wrong. Two seconds is far above one RTT and is still nowhere near the
    // 30-second `sleep` the fixture process is sitting in, so the matcher is
    // still guaranteed to time out having never matched.
    let timed_out = terminal
        .wait_visible_text(&matcher_secret, Duration::from_secs(2))
        .await;

    assert!(
        terminal.close().await.unwrap(),
        "wait-timeout redaction fixture did not reap its process boundary"
    );
    runtime.shutdown().await.unwrap();
    drop(runtime);
    assert!(runtime_entries(&runtime_parent).unwrap().is_empty());

    let error = timed_out.expect_err("the absent secret matcher must time out");
    let rendered = format!("{error} {error:?}");
    assert!(rendered.contains("terminal wait timed out"));
    assert!(!rendered.contains(&matcher_secret));
    assert!(!rendered.contains(&screen_secret));
    candidates.assert_unchanged().unwrap();
}

/// Run with:
/// `cargo build -p pmuxd -p pmux-rmuxd -p pmux-launcher -p pmux-hook && cargo test -p pseudomux-service --test private_runtime owner_pipe_reaps_sidecar_after_pmuxd_sigkill -- --ignored --nocapture`
///
/// This test deliberately scopes its fault assertion to the direct private
/// sidecar process and socket. It does not create a pane descendant or invoke
/// any Claude executable.
#[tokio::test]
#[ignore = "SIGKILLs a dedicated pmuxd and verifies owner-pipe sidecar exit; never invokes Claude"]
async fn owner_pipe_reaps_sidecar_after_pmuxd_sigkill() {
    let candidates =
        CandidateFiles::discover(&["pmuxd", "pmux-rmuxd", "pmux-launcher", "pmux-hook"]).unwrap();
    for _ in 0..FAULT_ITERATIONS {
        owner_pipe_reaps_sidecar_after_pmuxd_sigkill_once(&candidates).await;
    }
    candidates.assert_unchanged().unwrap();
}

async fn owner_pipe_reaps_sidecar_after_pmuxd_sigkill_once(candidates: &CandidateFiles) {
    // Keep AF_UNIX endpoints below the macOS sun_path limit while retaining a
    // dedicated mode-0700 directory on both macOS and Linux.
    let root = tempfile::Builder::new()
        .prefix("pmux-kill-")
        .tempdir_in("/tmp")
        .unwrap();
    let root_path = root.path().canonicalize().unwrap();
    set_owner_only(&root_path).unwrap();
    let runtime_parent = root_path.join("private-runtimes");
    std::fs::create_dir(&runtime_parent).unwrap();
    set_owner_only(&runtime_parent).unwrap();
    let public_socket = root_path.join("pmuxd.sock");

    let mut owner = Command::new(candidates.path("pmuxd"))
        .arg("serve")
        .arg("--socket")
        .arg(&public_socket)
        .arg("--rmuxd")
        .arg(candidates.path("pmux-rmuxd"))
        .arg("--launcher")
        .arg(candidates.path("pmux-launcher"))
        .arg("--runtime-parent")
        .arg(&runtime_parent)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let owner_pid = owner.id().expect("spawned pmuxd must have a pid");
    let owner_identity = ProcessIdentity::capture(owner_pid, public_socket.to_string_lossy())
        .expect("pmuxd must retain its exact start identity");
    let mut owner_cleanup = ExactProcessGuard::new(owner_identity);

    let observed = match wait_for_private_sidecar(
        &mut owner,
        owner_pid,
        candidates.path("pmux-rmuxd"),
        &runtime_parent,
        Duration::from_secs(10),
    )
    .await
    {
        Ok(observed) => observed,
        Err(error) => {
            let _ = owner.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(2), owner.wait()).await;
            let diagnostics = read_owner_stderr(&mut owner).await;
            panic!("private sidecar did not become ready: {error:#}; pmuxd stderr: {diagnostics}");
        }
    };
    let mut cleanup = ExactProcessGuard::new(observed.process_identity.clone());

    assert!(
        socket_reachable(&observed.socket).await,
        "private sidecar socket was not reachable before the fault"
    );
    owner_cleanup
        .identity()
        .signal(libc::SIGKILL)
        .expect("failed to SIGKILL the exact dedicated pmuxd");
    let owner_status = tokio::time::timeout(Duration::from_secs(5), owner.wait())
        .await
        .expect("SIGKILLed pmuxd was not reaped in time")
        .unwrap();
    assert_eq!(
        owner_status.signal(),
        Some(libc::SIGKILL),
        "owner did not terminate from SIGKILL: {owner_status}"
    );
    owner_cleanup.disarm();

    wait_for_sidecar_exit(&observed, Duration::from_secs(10))
        .await
        .unwrap();
    let residue = runtime_entries(&runtime_parent).unwrap();
    assert!(
        residue.is_empty(),
        "owner-loss cleanup retained private runtime artifacts: {residue:?}"
    );
    cleanup.disarm();
}

/// How long a read is left outstanding against a stalled sidecar before pmux's
/// own budget drops it. Far above the microseconds a local connect and request
/// write take, and far below any configured `operation_timeout`, so the drop is
/// unambiguously pmux's rather than rmux-sdk's.
const ARMED_DROP_BUDGET: Duration = Duration::from_millis(250);

// ---------------------------------------------------------------------------
// Transport-cancellation regressions.
//
// These four tests exist because every other terminal test in the workspace
// substitutes an in-crate fake for `RmuxTerminal`, so none of them can execute
// a single line of the cancellation-safety layer. They are the only coverage of
// it, and each one is red without it. See `CancellationFixture` for the shared
// setup and the individual doc comments for what each one proves.
// ---------------------------------------------------------------------------

/// The primary guard: abandoning a terminal read must not disable anything.
///
/// The trigger is [`abandon_after_one_poll`]: exactly one poll, then an
/// unconditional drop. On the unfixed leaf that single poll is already enough to
/// arm rmux-sdk's cancellation guard -- `TransportClient::request` sends its
/// `ActorMessage` on an idle channel, ready on the first poll, and arms
/// `OrderedResponseGuard` immediately afterwards (rmux-sdk
/// transport/mod.rs:117-125) -- so the drop lands on an in-flight request and
/// `abort_with` marks the connection permanently failed
/// (transport/cancellation.rs:27-34, transport/state.rs:39-44). That is the
/// exact shape pmux produced at twelve deadline-wrapped call sites.
///
/// Three assertions, deliberately in increasing order of strength:
///
/// 1. **The same terminal still reads.** This is the one that distinguishes
///    this fix from a transport-isolation-only fix. Per-session transports
///    alone would keep the *rest* of the daemon alive while leaving the
///    cancelled session permanently unreadable, and every turn poll goes
///    through this call.
/// 2. **The backend can still create terminals.** The facade a poisoned
///    operation used must not be the facade new sessions are minted from.
/// 3. **A pre-existing sibling terminal still reads.** One session's abandoned
///    read must not be able to reach another session at all.
///
/// The second trigger is not redundant with the first. Under the fix the
/// one-poll drop lands inside the throwaway handle's `UnixStream::connect`,
/// before any request exists, so on its own it would not prove that a drop
/// *after* the request is armed is survivable. Making that second drop land
/// somewhere provable needs the sidecar stalled rather than a time budget: a
/// listening socket still completes `connect` from the kernel backlog while its
/// process is stopped, so the request is always written and always armed, and it
/// can never be answered. A plain time budget was measured doing the opposite --
/// a warm snapshot round trip finished inside 5 ms on two runs out of six -- and
/// a budget that is sometimes longer than the operation it is meant to interrupt
/// is not a trigger, it is a coin toss.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns the private rmux sidecar and abandons real in-flight terminal reads"]
async fn private_abandoned_snapshot_leaves_its_own_terminal_the_backend_and_a_sibling_usable() {
    let fixture =
        CancellationFixture::start("pmux-private-abandon-", Duration::from_secs(10)).await;
    let mut abandoned = fixture.sleeping_terminal().await;
    let mut sibling = fixture.sleeping_terminal().await;

    let unpolled = in_flight_when_dropped(abandon_after_one_poll(abandoned.snapshot()).await);
    let recovered_from_unpolled_drop = abandoned.snapshot().await;

    // Second trigger. A time budget alone cannot guarantee that the drop lands
    // after the request is armed -- a warm snapshot round trip is sometimes
    // under a millisecond -- so the sidecar is stalled first. It then provably
    // cannot answer, the read provably cannot complete, and the budget provably
    // wins with a request outstanding.
    let sidecar = fixture.sidecar();
    sidecar
        .signal(libc::SIGSTOP)
        .expect("failed to stall the exact private sidecar");
    let armed = in_flight_when_dropped(
        tokio::time::timeout(ARMED_DROP_BUDGET, abandoned.snapshot())
            .await
            .ok(),
    );
    sidecar
        .signal(libc::SIGCONT)
        .expect("failed to resume the exact private sidecar");
    let recovered_from_armed_drop = abandoned.snapshot().await;

    let created_after_cancellation = fixture
        .runtime
        .create_terminal(
            Uuid::new_v4(),
            24,
            100,
            sh_launch(&sleeping_fixture("PMUXCREATEAFTERCANCEL")),
        )
        .await;
    let sibling_snapshot = sibling.snapshot().await;

    // Close everything that can still be closed before asserting, so a red run
    // reports the defect rather than a leaked PTY on top of it.
    let (created_after_cancellation, closed_created) = match created_after_cancellation {
        Ok(mut terminal) => {
            let closed = terminal.close().await;
            (Ok(()), Some(closed))
        }
        Err(error) => (Err(format!("{error:?}")), None),
    };
    let closed_abandoned = abandoned.close().await;
    let closed_sibling = sibling.close().await;

    // Ordered so that the first assertion to fail is the strongest claim the
    // trigger before it makes possible, not the bookkeeping around it.
    assert!(
        unpolled.is_none(),
        "one poll must leave the snapshot in flight, not complete it: {unpolled:?}"
    );
    let recovered_from_unpolled_drop = recovered_from_unpolled_drop
        .map_err(|error| format!("{error:?}"))
        .expect("the abandoning terminal must still read after an unpolled drop");
    // A refusal here, rather than a completion, means the transport was already
    // dead when the second read was issued -- which is the unfixed behaviour the
    // assertion above has just ruled out.
    assert!(
        armed.is_none(),
        "a stalled sidecar must leave the read outstanding until the budget expires, not complete or refuse it: {armed:?}"
    );
    let recovered_from_armed_drop = recovered_from_armed_drop
        .map_err(|error| format!("{error:?}"))
        .expect("the abandoning terminal must still read after an armed in-flight drop");
    assert!(
        recovered_from_unpolled_drop
            .visible_text
            .contains("PMUXREADY")
            && recovered_from_armed_drop.visible_text.contains("PMUXREADY"),
        "recovered reads returned a screen from the wrong pane"
    );
    created_after_cancellation
        .expect("a cancelled read must not stop the backend from creating new terminals");
    sibling_snapshot
        .map_err(|error| format!("{error:?}"))
        .expect("a sibling terminal must be untouched by another terminal's cancelled read");
    for (label, closed) in [
        ("abandoned", Some(closed_abandoned)),
        ("sibling", Some(closed_sibling)),
        ("created-after-cancellation", closed_created),
    ]
    .into_iter()
    .filter_map(|(label, closed)| closed.map(|closed| (label, closed)))
    {
        assert!(
            closed
                .map_err(|error| format!("{error:?}"))
                .unwrap_or_else(|error| panic!("{label} terminal failed to close: {error}")),
            "{label} terminal did not positively confirm process reaping"
        );
    }

    fixture.finish().await;
}

/// Cross-session isolation, asserted on the whole terminal surface.
///
/// R1's third assertion proves a sibling can still be *read*. This one proves a
/// sibling is still fully usable -- reads, writes, and the SDK's own polling
/// waits -- after another session abandons an operation. The three go through
/// three different mechanisms in the fixed leaf (throwaway handle, detached
/// write behind the FIFO gate, throwaway handle inside the SDK wait loop), so
/// asserting only one of them would leave the other two uncovered.
///
/// The trigger is deliberately the same deterministic class as R1's rather than
/// a concurrent race: what is being tested is the blast radius, and a
/// nondeterministic trigger would only make the blast radius harder to read.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns the private rmux sidecar and proves one session's cancellation cannot reach another"]
async fn private_cancelled_operation_in_one_session_leaves_another_session_fully_usable() {
    let fixture =
        CancellationFixture::start("pmux-private-isolation-", Duration::from_secs(10)).await;
    let mut cancelling = fixture.sleeping_terminal().await;
    let bystander_ready = unique_marker("PMUXBYSTANDER");
    let mut bystander = fixture
        .terminal(
            &interruptible_fixture(&bystander_ready, "PMUXBYSTANDERINT"),
            &bystander_ready,
        )
        .await;

    // Abandon one read and one write on the cancelling session. The write is
    // included because it is the only operation that stays on the session's
    // original transport, so it is the one whose isolation is least obvious.
    // Both use the one-poll drop rather than a time budget: what this test
    // measures is the blast radius, and a trigger that sometimes lets the
    // operation finish would silently turn a red run green.
    let abandoned_read =
        in_flight_when_dropped(abandon_after_one_poll(cancelling.snapshot()).await);
    let abandoned_write = in_flight_when_dropped(
        abandon_after_one_poll(cancelling.paste("PMUXABANDONEDWRITE")).await,
    );

    let bystander_read = bystander.snapshot().await;
    let bystander_write = bystander.paste("PMUXBYSTANDERWRITE").await;
    let bystander_wait = bystander
        .wait_visible_text("PMUXBYSTANDERWRITE", Duration::from_secs(5))
        .await;
    let bystander_interrupt = bystander.interrupt().await;
    let bystander_wait_after_interrupt = bystander
        .wait_visible_text("PMUXBYSTANDERINT1", Duration::from_secs(5))
        .await;

    let closed_bystander = bystander.close().await;
    let closed_cancelling = cancelling.close().await;

    assert!(
        abandoned_read.is_none(),
        "the cancelling session's read must be abandoned in flight, not completed: {abandoned_read:?}"
    );
    bystander_read
        .map_err(|error| format!("{error:?}"))
        .expect("bystander read failed after an unrelated session cancelled an operation");
    bystander_write
        .map_err(|error| format!("{error:?}"))
        .expect("bystander write failed after an unrelated session cancelled an operation");
    let bystander_wait = bystander_wait
        .map_err(|error| format!("{error:?}"))
        .expect("bystander SDK wait failed after an unrelated session cancelled an operation");
    assert!(
        bystander_wait.visible_text.contains("PMUXBYSTANDERWRITE"),
        "bystander wait returned a screen without its own written text"
    );
    bystander_interrupt
        .map_err(|error| format!("{error:?}"))
        .expect("bystander interrupt failed after an unrelated session cancelled an operation");
    bystander_wait_after_interrupt
        .map_err(|error| format!("{error:?}"))
        .expect("bystander interrupt never reached its pane");
    // Asserted after the bystander, deliberately. A refusal here rather than a
    // completion means the read above had already destroyed the session's own
    // transport before the write was issued, which is a statement about the
    // cancelling session and not about the isolation this test is named for.
    assert!(
        abandoned_write.is_none(),
        "the cancelling session's write must be abandoned in flight, not completed or refused outright: {abandoned_write:?}"
    );
    for (label, closed) in [
        ("bystander", closed_bystander),
        ("cancelling", closed_cancelling),
    ] {
        assert!(
            closed
                .map_err(|error| format!("{error:?}"))
                .unwrap_or_else(|error| panic!("{label} terminal failed to close: {error}")),
            "{label} terminal did not positively confirm process reaping"
        );
    }

    fixture.finish().await;
}

/// Recovery from a terminal transport failure the leaf cannot prevent.
///
/// R1 and R3 abandon pmux's own futures, which is the failure the cancellation
/// layer *removes*. This one produces the failure it can only *contain*:
/// rmux-sdk aborting a transport from inside itself. `run_with_deadline`
/// (rmux-sdk transport/mod.rs:246-266) wraps every request in the configured
/// operation timeout and, when that timer wins, calls `abort_with` directly --
/// no pmux future is dropped, and no arrangement of spawns or handles on this
/// side of the API can stop it.
///
/// The seam is `PrivateRuntimeConfig::operation_timeout` plus a deterministic
/// stall: the exact private sidecar is `SIGSTOP`ped, so it provably cannot
/// answer, and the operation deadline provably expires. `SIGCONT` then removes
/// the fault, which is what makes *recovery* observable at all rather than just
/// failure. A short `operation_timeout` alone cannot do this: it was measured on
/// this tree in a debug build, `create_terminal` itself already fails with
/// `ControlPlaneLost` at 50 ms while one snapshot round trip is 24-97 ms, so no
/// value of it both admits a terminal and then fails one operation on it. The
/// stall supplies the missing dimension -- time -- instead.
///
/// What is asserted about the *stalled* terminal is the intended post-(a)
/// behaviour and nothing beyond it: the aborted read is reported as a lost
/// control plane, and the next read on the same terminal succeeds because it is
/// minted on a new connection. Its writes are deliberately not asserted to
/// recover: they ride the one connection this terminal cannot rebind, and
/// rebinding it is layer (b)'s per-session rebind, which has not landed.
///
/// ## Why the blast radius is asserted here and not in R1 or R3
///
/// R1 and R3 abandon pmux futures, and after (a) an abandoned write is
/// *completed* by its detached task -- so neither of them poisons a write
/// connection, and neither can say anything about who else that would have
/// taken down. This test is the only one in the workspace that produces a real,
/// unpreventable transport abort, so it is the only place the per-session
/// property is provable at all.
///
/// The second stall exists for exactly that. It aborts a **write**, which is
/// the operation that stays on a long-lived connection, and then asserts what a
/// shared transport could not have survived:
///
/// * a sibling session, which issued nothing during either stall, still reads,
///   still writes, and still observes its own write land;
/// * the backend still creates new terminals, so the facade sessions are minted
///   from is not the one that just latched;
/// * the stalled terminal still *reads*, because reads never touch the write
///   connection; and
/// * the stalled terminal still closes with a positive reap, because
///   `owned.cleanup()` runs on the owned session's own connection rather than
///   on the pane's.
///
/// Before per-session transports every one of those four ran through the single
/// process-wide `TransportClient` the aborted write had just latched write-once
/// (rmux-sdk transport/state.rs:39-44), so all four would have failed together.
///
/// Both stalls are bounded well under the fixture's 5 s lease TTL, and the
/// lease heartbeat gets an unstalled window between them to renew in, so the
/// `lease_lost` assertions below are about transports and never about a
/// starved heartbeat.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "SIGSTOPs the exact private sidecar so rmux-sdk aborts a transport on its own deadline"]
async fn private_terminal_read_recovers_after_the_sdk_aborts_a_transport_on_its_own_deadline() {
    // Long enough for every round trip terminal creation makes, short enough
    // that the stall below is comfortably longer than it.
    const OPERATION_TIMEOUT: Duration = Duration::from_millis(1_500);
    let fixture = CancellationFixture::start("pmux-private-abort-", OPERATION_TIMEOUT).await;
    let mut terminal = fixture.sleeping_terminal().await;
    let bystander_ready = unique_marker("PMUXABORTBYSTANDER");
    let mut bystander = fixture
        .terminal(
            &interruptible_fixture(&bystander_ready, "PMUXABORTBYSTANDERINT"),
            &bystander_ready,
        )
        .await;

    let sidecar = fixture.sidecar();
    sidecar
        .signal(libc::SIGSTOP)
        .expect("failed to stall the exact private sidecar");
    let stalled_at = Instant::now();
    // The screen is discarded rather than reported: this call is expected to
    // fail, and a red run should print the failure and not a pane dump.
    let aborted = terminal.snapshot().await.map(|_| ());
    let aborted_after = stalled_at.elapsed();
    sidecar
        .signal(libc::SIGCONT)
        .expect("failed to resume the exact private sidecar");

    let recovered = terminal.snapshot().await;
    let lease_lost_after_recovery = terminal.lease_lost();

    // Second stall, aimed at the connection reads never touch. Only the stalled
    // terminal issues anything inside this window, so every later failure is
    // attributable to the latch this write leaves behind and to nothing else.
    sidecar
        .signal(libc::SIGSTOP)
        .expect("failed to stall the exact private sidecar for the write abort");
    let write_stalled_at = Instant::now();
    let aborted_write = terminal.paste("PMUXABORTEDWRITE").await;
    let aborted_write_after = write_stalled_at.elapsed();
    sidecar
        .signal(libc::SIGCONT)
        .expect("failed to resume the exact private sidecar after the write abort");

    let bystander_read = bystander.snapshot().await;
    let bystander_write = bystander.paste("PMUXABORTBYSTANDERWRITE").await;
    let bystander_wait = bystander
        .wait_visible_text("PMUXABORTBYSTANDERWRITE", Duration::from_secs(10))
        .await;
    let created_after_abort = fixture
        .runtime
        .create_terminal(
            Uuid::new_v4(),
            24,
            100,
            sh_launch(&sleeping_fixture("PMUXCREATEAFTERABORT")),
        )
        .await;
    let read_after_write_abort = terminal.snapshot().await;

    // Close everything that can still be closed before asserting, so a red run
    // reports the defect rather than a leaked PTY on top of it.
    let (created_after_abort, closed_created) = match created_after_abort {
        Ok(mut created) => {
            let closed = created.close().await;
            (Ok(()), Some(closed))
        }
        Err(error) => (Err(format!("{error:?}")), None),
    };
    let closed = terminal.close().await;
    let closed_bystander = bystander.close().await;

    assert!(
        matches!(
            aborted,
            Err(pseudomux_rmux::TerminalBackendError::ControlPlaneLost)
        ),
        "a stalled sidecar must surface as a lost control plane, got {aborted:?}"
    );
    assert!(
        aborted_after >= OPERATION_TIMEOUT,
        "the read returned in {aborted_after:?}, before the {OPERATION_TIMEOUT:?} operation deadline could have fired"
    );
    let recovered = recovered
        .map_err(|error| format!("{error:?}"))
        .expect("the same terminal must read again once the daemon answers");
    assert!(
        recovered.visible_text.contains("PMUXREADY"),
        "the recovered read returned a screen from the wrong pane"
    );
    assert!(
        !lease_lost_after_recovery,
        "a bounded stall must not be reported as a lost session lease"
    );

    // The write abort itself, and then the four things it must not have reached.
    assert!(
        matches!(
            aborted_write,
            Err(pseudomux_rmux::TerminalBackendError::ControlPlaneLost)
        ),
        "a stalled sidecar must abort the write's own connection, got {aborted_write:?}"
    );
    assert!(
        aborted_write_after >= OPERATION_TIMEOUT,
        "the write returned in {aborted_write_after:?}, before the {OPERATION_TIMEOUT:?} operation deadline could have fired"
    );
    bystander_read.map_err(|error| format!("{error:?}")).expect(
        "a sibling session must still read after another session's write transport latched",
    );
    bystander_write
        .map_err(|error| format!("{error:?}"))
        .expect(
            "a sibling session must still write after another session's write transport latched",
        );
    let bystander_wait = bystander_wait
        .map_err(|error| format!("{error:?}"))
        .expect("a sibling session's write never reached its pane");
    assert!(
        bystander_wait
            .visible_text
            .contains("PMUXABORTBYSTANDERWRITE"),
        "the sibling wait returned a screen without its own written text"
    );
    created_after_abort
        .expect("an aborted write must not stop the backend from creating new terminals");
    let read_after_write_abort = read_after_write_abort
        .map_err(|error| format!("{error:?}"))
        .expect("reads must be independent of the write connection that latched");
    assert!(
        read_after_write_abort.visible_text.contains("PMUXREADY"),
        "the post-write-abort read returned a screen from the wrong pane"
    );

    for (label, closed) in [
        ("stalled", Some(closed)),
        ("bystander", Some(closed_bystander)),
        ("created-after-abort", closed_created),
    ]
    .into_iter()
    .filter_map(|(label, closed)| closed.map(|closed| (label, closed)))
    {
        assert!(
            closed
                .map_err(|error| format!("{error:?}"))
                .unwrap_or_else(|error| panic!("{label} terminal failed to close: {error}")),
            "{label} terminal did not positively confirm process reaping"
        );
    }

    fixture.finish().await;
}

/// The FIFO ordering gate, observed on the pane rather than in the code.
///
/// Detaching a write onto a spawned task is only half of the mechanism. Without
/// the per-terminal ordering mutex an abandoned `paste` can still be waiting to
/// run when a following `interrupt` is issued, and the prompt would then be
/// typed into the composer *after* the C-c that was meant to clear it. Nothing
/// else in the workspace observes that ordering.
///
/// The pane's own line discipline is the witness. Input is echoed in receipt
/// order, so a snapshot shows the bracketed paste, then `^C`, then whatever the
/// fixture's `SIGINT` trap printed -- and the position of `^C` relative to the
/// pasted marker is a direct reading of the order the pane received the two
/// writes in, not an inference from timing.
///
/// Each iteration abandons the paste with [`abandon_after_one_poll`] rather than
/// with a small time budget. A `send-keys` round trip is far cheaper than a
/// snapshot -- cheap enough that it can finish inside a single timer tick, which
/// is measurably enough to make a `Duration::ZERO` budget lose the race about
/// one run in eight -- while one poll is deterministic and is already enough to
/// arm the guard: rmux-sdk's `capabilities::require` asks for a named
/// capability, which never consults the per-connection cache and always issues a
/// `Handshake` request (rmux-sdk capabilities.rs:51-58), so the very first poll
/// of `send_text` has a request in flight. On the unfixed leaf that drop poisons
/// the session's transport and the following `interrupt` fails outright; with
/// the fix the write is completed by its detached task, which holds the ordering
/// permit until it is done, and the interrupt queues behind it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns the private rmux sidecar and proves abandoned writes stay ordered against a following interrupt"]
async fn private_abandoned_paste_reaches_the_pane_strictly_before_a_following_interrupt() {
    const ROUNDS: usize = 3;
    let fixture =
        CancellationFixture::start("pmux-private-ordering-", Duration::from_secs(10)).await;
    let ready = unique_marker("PMUXORDERREADY");
    let interrupt_marker = unique_marker("PMUXORDERINT");
    let mut terminal = fixture
        .terminal(&interruptible_fixture(&ready, &interrupt_marker), &ready)
        .await;

    let mut observations = Vec::new();
    for round in 1..=ROUNDS {
        let pasted = format!("PMUXORDERPASTE{round}");
        let abandoned =
            in_flight_when_dropped(abandon_after_one_poll(terminal.paste(&pasted)).await);
        let interrupted = terminal.interrupt().await;
        let interrupt_evidence = format!("{interrupt_marker}{round}");
        let settled = terminal
            .wait_visible_text(&interrupt_evidence, Duration::from_secs(10))
            .await;
        observations.push((round, pasted, abandoned, interrupted, settled));
    }
    let closed = terminal.close().await;

    for (round, pasted, abandoned, interrupted, settled) in observations {
        assert!(
            abandoned.is_none(),
            "round {round}: the paste must be abandoned in flight, not completed: {abandoned:?}"
        );
        interrupted
            .map_err(|error| format!("{error:?}"))
            .unwrap_or_else(|error| {
                panic!("round {round}: interrupt failed after an abandoned paste: {error}")
            });
        let screen = settled
            .map_err(|error| format!("{error:?}"))
            .unwrap_or_else(|error| {
                panic!("round {round}: the interrupt never reached the pane: {error}")
            })
            .visible_text;
        let pasted_at = screen.find(&pasted).unwrap_or_else(|| {
            panic!("round {round}: the abandoned paste never reached the pane: {screen:?}")
        });
        let interrupt_at = screen
            .find(&format!("{interrupt_marker}{round}"))
            .unwrap_or_else(|| {
                panic!("round {round}: the interrupt evidence is missing: {screen:?}")
            });
        assert!(
            pasted_at < interrupt_at,
            "round {round}: the interrupt was handled before the paste arrived: {screen:?}"
        );
        // The line discipline echoes `C-c` as `^C` in the same stream it echoed
        // the paste into, so this is the input order itself rather than the
        // order two independent observations happened to be made in.
        assert!(
            screen[pasted_at..interrupt_at].contains("^C"),
            "round {round}: no C-c echo between the pasted marker and its interrupt: {screen:?}"
        );
    }
    assert!(
        closed
            .map_err(|error| format!("{error:?}"))
            .expect("close failed after abandoned writes"),
        "the ordering terminal did not positively confirm process reaping"
    );

    fixture.finish().await;
}

/// Layer (b): a terminal whose write connection has latched writes again.
///
/// This is the one claim layer (c) deliberately left open and wrote down as
/// (b)'s residue. Reads were already recoverable — every read mints its own
/// throwaway connection — but `paste`, `enter` and `interrupt` rode a single
/// `Pane` handle captured at `create` for the terminal's whole life. rmux-sdk
/// binds a handle to its `TransportClient` at construction and
/// `TransportState::set_terminal_failure` is write-once and never cleared
/// (rmux-sdk transport/state.rs:39-44), so one aborted write left every later
/// write on that terminal failing forever while `map_terminal_error` kept
/// answering `DaemonLost retryable: true` — a retry the caller could not
/// possibly win.
///
/// The fault is the same one
/// [`private_terminal_read_recovers_after_the_sdk_aborts_a_transport_on_its_own_deadline`]
/// uses, and it is the only fault in the workspace that produces a real,
/// unpreventable transport abort: `SIGSTOP` the exact private sidecar, issue a
/// write, and let the SDK's own `operation_timeout` abort the in-flight
/// request. Nothing here abandons a future, so this is not the (a) property
/// restated — the write below is awaited to completion and still fails, because
/// the connection under it is gone.
///
/// What is asserted is deliberately the pane and not the return code. A
/// `paste` that answers `Ok` on a connection that never delivered would satisfy
/// a code-only assertion, so the second write is proven by
/// `wait_visible_text` finding its own unique marker in the pane, and the
/// following `interrupt` is proven by the fixture's `SIGINT` trap printing a
/// numbered marker of its own. Both are writes; both ran after the latch.
///
/// The sibling and creation checks live in the read test rather than being
/// repeated here — this test is about one terminal recovering its own write
/// path, and duplicating the isolation assertions would make a red run
/// ambiguous about which property broke.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "SIGSTOPs the exact private sidecar so rmux-sdk latches this terminal's write transport"]
async fn private_terminal_write_recovers_after_the_sdk_aborts_its_write_transport() {
    // Long enough for every round trip terminal creation makes, short enough
    // that the stall below is comfortably longer than it.
    const OPERATION_TIMEOUT: Duration = Duration::from_millis(1_500);
    let fixture = CancellationFixture::start("pmux-private-write-rebind-", OPERATION_TIMEOUT).await;
    let ready = unique_marker("PMUXWRITEREBINDREADY");
    let interrupt_marker = unique_marker("PMUXWRITEREBINDINT");
    let mut terminal = fixture
        .terminal(&interruptible_fixture(&ready, &interrupt_marker), &ready)
        .await;

    let sidecar = fixture.sidecar();
    sidecar
        .signal(libc::SIGSTOP)
        .expect("failed to stall the exact private sidecar for the write abort");
    let stalled_at = Instant::now();
    let aborted_write = terminal.paste("PMUXWRITEREBINDABORTED").await;
    let aborted_write_after = stalled_at.elapsed();
    sidecar
        .signal(libc::SIGCONT)
        .expect("failed to resume the exact private sidecar after the write abort");

    // The same terminal, the same session, the same slot: one write later.
    let recovered_marker = unique_marker("PMUXWRITEREBINDAFTER");
    let recovered_write = terminal.paste(&recovered_marker).await;
    let recovered_landed = terminal
        .wait_visible_text(&recovered_marker, Duration::from_secs(10))
        .await;
    let recovered_interrupt = terminal.interrupt().await;
    let interrupt_evidence = format!("{interrupt_marker}1");
    let interrupt_landed = terminal
        .wait_visible_text(&interrupt_evidence, Duration::from_secs(10))
        .await;

    // Close before asserting, so a red run reports the defect rather than a
    // leaked PTY on top of it.
    let closed = terminal.close().await;

    assert!(
        matches!(
            aborted_write,
            Err(pseudomux_rmux::TerminalBackendError::ControlPlaneLost)
        ),
        "a stalled sidecar must abort the write's own connection, got {aborted_write:?}"
    );
    assert!(
        aborted_write_after >= OPERATION_TIMEOUT,
        "the write returned in {aborted_write_after:?}, before the {OPERATION_TIMEOUT:?} operation deadline could have fired"
    );
    recovered_write
        .map_err(|error| format!("{error:?}"))
        .expect("the same terminal must write again once the daemon answers");
    let recovered_screen = recovered_landed
        .map_err(|error| format!("{error:?}"))
        .expect("the recovered write never reached the pane")
        .visible_text;
    assert!(
        recovered_screen.contains(&recovered_marker),
        "the recovered wait returned a screen without its own written text: {recovered_screen:?}"
    );
    recovered_interrupt
        .map_err(|error| format!("{error:?}"))
        .expect("the same terminal must interrupt again once the daemon answers");
    let interrupt_screen = interrupt_landed
        .map_err(|error| format!("{error:?}"))
        .expect("the recovered interrupt never reached the pane")
        .visible_text;
    assert!(
        interrupt_screen.contains(&interrupt_evidence),
        "the fixture's SIGINT trap left no evidence of the recovered interrupt: {interrupt_screen:?}"
    );
    assert!(
        closed
            .map_err(|error| format!("{error:?}"))
            .expect("close failed after a latched write connection"),
        "the recovered terminal did not positively confirm process reaping"
    );

    fixture.finish().await;
}

/// Shared setup for the transport-cancellation regressions above, and for the
/// geometry ones below.
///
/// Everything in here is already spelled out inline in the older tests in this
/// file; it is factored out only because several tests need the identical thing
/// and none of them are about setup. Deliberately not a count: this comment
/// said "four" while seven call sites used it, and a number nothing derives is
/// wrong the moment the next test is written. `rg -c 'CancellationFixture::start'`
/// answers it and cannot go stale.
struct CancellationFixture {
    candidates: CandidateFiles,
    /// Held for the lifetime of the fixture. Dropping it removes the runtime
    /// parent, which is what `finish` asserts is already empty.
    _root: TempDir,
    runtime_parent: PathBuf,
    runtime: PrivateRuntime,
}

impl CancellationFixture {
    async fn start(prefix: &str, operation_timeout: Duration) -> Self {
        let candidates = CandidateFiles::discover(&["pmux-rmuxd", "pmux-launcher"]).unwrap();
        // Keep AF_UNIX endpoints below the macOS sun_path limit while retaining
        // a dedicated mode-0700 directory on both macOS and Linux.
        let root = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("/tmp")
            .unwrap();
        let root_path = root.path().canonicalize().unwrap();
        set_owner_only(&root_path).unwrap();
        let runtime_parent = root_path.join("runtimes");
        std::fs::create_dir(&runtime_parent).unwrap();
        set_owner_only(&runtime_parent).unwrap();
        let runtime = PrivateRuntime::start(PrivateRuntimeConfig {
            rmuxd: candidates.path("pmux-rmuxd").to_path_buf(),
            launcher: candidates.path("pmux-launcher").to_path_buf(),
            runtime_parent: Some(runtime_parent.clone()),
            startup_timeout: Duration::from_secs(10),
            operation_timeout,
            lease_ttl: Duration::from_secs(5),
        })
        .await
        .unwrap();
        Self {
            candidates,
            _root: root,
            runtime_parent,
            runtime,
        }
    }

    /// Launches one `/bin/sh` fixture and returns only after the pane has
    /// rendered its readiness marker, so no test below races the launch.
    async fn terminal(&self, program: &str, ready_marker: &str) -> Box<dyn TerminalSession> {
        let terminal = self
            .runtime
            .create_terminal(Uuid::new_v4(), 24, 100, sh_launch(program))
            .await
            .map_err(|error| format!("{error:?}"))
            .expect("private terminal creation failed");
        terminal
            .wait_visible_text(ready_marker, Duration::from_secs(10))
            .await
            .map_err(|error| format!("{error:?}"))
            .expect("private terminal never announced readiness");
        terminal
    }

    /// This runtime's own private sidecar, as an exact start-time-fenced
    /// identity.
    ///
    /// Fenced on both the binary name and this runtime's socket path, so a
    /// concurrently running test's sidecar can never be signalled even though
    /// both are direct children of the same test process.
    fn sidecar(&self) -> ProcessIdentity {
        let socket = self.runtime.rmux_socket().to_string_lossy().into_owned();
        let pid = find_direct_child(std::process::id(), &["pmux-rmuxd", &socket])
            .expect("the private sidecar must be an exact direct child of this test process");
        ProcessIdentity::capture(pid, "pmux-rmuxd")
            .expect("the private sidecar must retain its start identity")
    }

    /// The default fixture: announces readiness once and then sits in `sleep`.
    async fn sleeping_terminal(&self) -> Box<dyn TerminalSession> {
        self.terminal(&sleeping_fixture("PMUXREADY"), "PMUXREADY")
            .await
    }

    async fn finish(self) {
        self.runtime.shutdown().await.unwrap();
        drop(self.runtime);
        assert!(runtime_entries(&self.runtime_parent).unwrap().is_empty());
        self.candidates.assert_unchanged().unwrap();
    }
}

fn sh_launch(program: &str) -> LaunchSpec {
    LaunchSpec {
        executable: PathBuf::from("/bin/sh"),
        args: vec!["-c".into(), program.to_owned()],
        cwd: std::env::current_dir().unwrap(),
        environment: EnvironmentSnapshot::capture(),
    }
}

fn sleeping_fixture(ready_marker: &str) -> String {
    format!("printf '%s\\n' '{ready_marker}'; exec sleep 30")
}

/// A fixture that stays alive across `SIGINT` and records each one it received
/// as a numbered marker line, so an interrupt leaves durable pane evidence
/// instead of only killing something.
fn interruptible_fixture(ready_marker: &str, interrupt_marker: &str) -> String {
    format!(
        "count=0; trap 'count=$((count+1)); printf \"\\n{interrupt_marker}%s\\n\" \"$count\"' INT; \
         printf '%s\\n' '{ready_marker}'; while :; do sleep 30; done"
    )
}

/// Polls a terminal call exactly once and then drops it, reporting `None` when
/// it was still in flight.
///
/// `tokio::time::timeout(Duration::ZERO, _)` is deliberately not used for this.
/// Its delay is a real timer entry, so on the first poll it can still report
/// `Pending`; the task then sleeps until the next tick and the inner future is
/// polled a *second* time up to a millisecond later. For a snapshot that is
/// immaterial, but a `send-keys` round trip can complete inside that tick --
/// measured here at one run in eight -- and the call is then not abandoned at
/// all. One poll is exactly what these regressions need: enough to arm rmux-sdk's
/// cancellation guard, never enough to finish a round trip.
async fn abandon_after_one_poll<F: std::future::Future>(future: F) -> Option<F::Output> {
    let mut future = std::pin::pin!(future);
    match std::future::poll_fn(move |context| std::task::Poll::Ready(future.as_mut().poll(context)))
        .await
    {
        std::task::Poll::Ready(output) => Some(output),
        std::task::Poll::Pending => None,
    }
}

/// Erases an abandoned terminal call's payload so it can be reported.
///
/// `None` means the call was still in flight when it was dropped, which is the
/// precondition every regression above depends on.
fn in_flight_when_dropped<T>(
    outcome: Option<Result<T, pseudomux_rmux::TerminalBackendError>>,
) -> Option<Result<(), String>> {
    outcome.map(|result| result.map(|_| ()).map_err(|error| format!("{error:?}")))
}

fn unique_marker(prefix: &str) -> String {
    format!("{prefix}{}", Uuid::new_v4().simple())
}

async fn read_owner_stderr(owner: &mut Child) -> String {
    let Some(mut stderr) = owner.stderr.take() else {
        return "<unavailable>".to_owned();
    };
    let mut bytes = Vec::new();
    match tokio::time::timeout(Duration::from_secs(1), stderr.read_to_end(&mut bytes)).await {
        Ok(Ok(_)) => String::from_utf8_lossy(&bytes).trim().to_owned(),
        Ok(Err(error)) => format!("<read failed: {error}>"),
        Err(_) => "<read timed out>".to_owned(),
    }
}

struct PrivateSidecarObservation {
    pid: u32,
    socket: PathBuf,
    process_identity: ProcessIdentity,
    socket_identity: SocketIdentity,
}

async fn wait_for_private_sidecar(
    owner: &mut Child,
    owner_pid: u32,
    expected_rmuxd: &std::path::Path,
    runtime_parent: &std::path::Path,
    timeout: Duration,
) -> Result<PrivateSidecarObservation> {
    let deadline = Instant::now() + timeout;
    let mut sidecar_pid = None;
    let mut socket = None;
    loop {
        if let Some(status) = owner.try_wait()? {
            bail!("pmuxd exited before its private sidecar was ready: {status}");
        }
        if sidecar_pid.is_none() {
            sidecar_pid = direct_sidecar_pid(owner_pid, expected_rmuxd).await?;
        }
        if socket.is_none() {
            socket = private_rmux_socket(runtime_parent)?;
        }
        if let (Some(pid), Some(socket)) = (sidecar_pid, socket.as_ref())
            && socket_reachable(socket).await
        {
            let process_identity = ProcessIdentity::capture(pid, socket.to_string_lossy())?;
            let socket_identity = SocketIdentity::capture(socket)?;
            return Ok(PrivateSidecarObservation {
                pid,
                socket: socket.clone(),
                process_identity,
                socket_identity,
            });
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for sidecar pid/socket (pid={sidecar_pid:?}, socket={socket:?})"
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_sidecar_exit(
    observed: &PrivateSidecarObservation,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        // The PID was captured as pmux-rmuxd before the fault. Do not treat a
        // zombie as success: owner-death cleanup includes collection of the
        // direct sidecar child identity.
        let process_running = observed.process_identity.is_present()?;
        let reachable = socket_reachable(&observed.socket).await;
        let exact_socket_present = observed.socket_identity.remains_at(&observed.socket)?;
        if !process_running && !reachable && !exact_socket_present {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "private sidecar survived owner SIGKILL (pid={}, start={}, pgid={}, sid={}, process_running={process_running}, socket_reachable={reachable}, exact_socket_present={exact_socket_present})",
                observed.pid,
                observed.process_identity.start_token,
                observed.process_identity.process_group_id,
                observed.process_identity.session_id
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn direct_sidecar_pid(
    owner_pid: u32,
    expected_rmuxd: &std::path::Path,
) -> Result<Option<u32>> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid=,command="])
        .output()
        .await
        .context("failed to inspect the dedicated pmuxd process tree")?;
    if !output.status.success() {
        bail!("ps failed while locating the private sidecar");
    }
    let expected_rmuxd = expected_rmuxd.to_string_lossy();
    let mut matches = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.split_whitespace();
        let Some(pid) = fields.next().and_then(|value| value.parse::<u32>().ok()) else {
            continue;
        };
        let Some(parent) = fields.next().and_then(|value| value.parse::<u32>().ok()) else {
            continue;
        };
        let command = fields.collect::<Vec<_>>().join(" ");
        if parent == owner_pid && command.contains(expected_rmuxd.as_ref()) {
            matches.push(pid);
        }
    }
    if matches.len() > 1 {
        bail!(
            "dedicated pmuxd had multiple direct children matching exact candidate {expected_rmuxd}: {matches:?}"
        );
    }
    Ok(matches.pop())
}

fn private_rmux_socket(runtime_parent: &std::path::Path) -> Result<Option<PathBuf>> {
    let mut sockets = Vec::new();
    for entry in std::fs::read_dir(runtime_parent)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && entry.file_name().to_string_lossy().starts_with("pmux-") {
            let candidate = entry.path().join("rmux.sock");
            if candidate.exists() {
                sockets.push(candidate);
            }
        }
    }
    if sockets.len() > 1 {
        bail!("dedicated runtime parent contained multiple private rmux sockets");
    }
    Ok(sockets.pop())
}

async fn socket_reachable(path: &std::path::Path) -> bool {
    matches!(
        tokio::time::timeout(Duration::from_millis(250), UnixStream::connect(path)).await,
        Ok(Ok(_))
    )
}

/// The geometry a caller asks for is the geometry the child is launched into.
///
/// This is a screen-shape guarantee before it is a comfort feature. Every
/// minified-cell screen predicate -- the composer border, the tool-permission
/// prompt, the `assert_empty_after_clear` frame -- is calibrated against a real
/// pane, and a pane that is not the size pmux asked for makes every one of
/// those calibrations a measurement of something else. A REQUESTED value that
/// is fiction is worse than a small pane, because it is the number the next
/// person calibrates against.
///
/// WHAT WAS WRONG. `bin/pmux/src/cli.rs` has requested `DEFAULT_COLS: u16 = 120`
/// since it was written, and every measured pane rendered 24x80. The request
/// was never refused and never warned about -- it was silently clamped, two
/// layers down:
///
/// * `RmuxBackend::create` asked for the size with `pane.resize(...)`, which the
///   SDK turns into `resize-pane -x <cols>` (rmux-sdk pane/input.rs:42-70).
/// * rmux creates the owned session with no size, so it gets
///   `DEFAULT_SESSION_SIZE = 80x24` (vendor/rmux-server/src/handler.rs:188).
/// * For a single-pane window `Window::resize_pane_width` records a
///   `requested_main_width` and rebuilds the layout tree against the WINDOW's
///   size (rmux-core window/layout_ops.rs:96-115, :285-295). A lone pane fills
///   its window and cannot exceed it, so 120 collapsed back to 80 with no
///   error -- `resize-pane` returned success.
///
/// The pane was never the thing to resize. `create` now resizes the WINDOW,
/// which is the resource the pane's size is derived from.
///
/// Asserted on the SNAPSHOT rather than on the request pmux sent, because the
/// defect was precisely that the request was accepted and discarded. Both
/// dimensions and a non-default width, so a regression that reinstated the
/// clamp cannot pass by coincidence: 24 is `DEFAULT_SESSION_SIZE`'s height, so
/// a rows-only assertion would have passed throughout the whole defect.
#[tokio::test]
async fn a_terminal_is_created_at_the_requested_geometry_and_not_the_rmux_default() {
    const REQUESTED_ROWS: u16 = 40;
    const REQUESTED_COLS: u16 = 120;

    let fixture = CancellationFixture::start("pmux-geometry", Duration::from_secs(10)).await;
    let terminal = fixture
        .runtime
        .create_terminal(
            Uuid::new_v4(),
            REQUESTED_ROWS,
            REQUESTED_COLS,
            sh_launch(&sleeping_fixture("GEOMETRY-READY")),
        )
        .await
        .map_err(|error| format!("{error:?}"))
        .expect("private terminal creation failed");
    terminal
        .wait_visible_text("GEOMETRY-READY", Duration::from_secs(10))
        .await
        .map_err(|error| format!("{error:?}"))
        .expect("private terminal never announced readiness");

    let snapshot = terminal
        .snapshot()
        .await
        .expect("snapshot must be readable");
    assert_eq!(
        (snapshot.rows, snapshot.cols),
        (REQUESTED_ROWS, REQUESTED_COLS),
        "the delivered pane geometry must be the requested one; 80 columns means the rmux \
         session default silently clamped the request again"
    );

    drop(terminal);
    fixture.finish().await;
}

/// A LATER resize is delivered too, not only the one at creation.
///
/// THE SAME DEFECT, ONE ENTRY POINT LATER. When the creation path was moved
/// off `Pane::resize` and onto the window,
/// `<RmuxTerminal as TerminalSession>::resize` was left behind on
/// `pane.resize(TerminalSizeSpec::new(cols, rows))`. That call is the silently
/// clamped one: the SDK turns it into `resize-pane -x/-y`, and for a
/// single-pane window rmux records a `requested_main_width` and rebuilds the
/// layout tree against the WINDOW's size, so a lone pane cannot exceed the
/// window it is in -- AND `resize-pane` RETURNS SUCCESS. Every resize after
/// creation was therefore accepted, discarded, and reported as done, which is
/// exactly the shape the creation fix was written against.
///
/// STARTS AT THE RMUX DEFAULT AND ASKS FOR MORE IN BOTH DIMENSIONS, ON PURPOSE.
/// The clamp is an upper bound at the window's size, so a resize DOWN would
/// land even through the broken call and prove nothing; and 24x80 is
/// `DEFAULT_SESSION_SIZE` itself, so a test that started larger could pass by
/// keeping a size it never had to change. Growing from the default in both
/// dimensions is the only shape the old call cannot satisfy.
///
/// ASSERTED ON THE SNAPSHOT, never on the request pmux sent, for the reason the
/// whole defect existed: the request was accepted and thrown away three layers
/// down, so only the delivered screen is evidence.
#[tokio::test]
async fn a_terminal_resize_after_creation_is_delivered_and_not_silently_clamped() {
    const CREATED_ROWS: u16 = 24;
    const CREATED_COLS: u16 = 80;
    const RESIZED_ROWS: u16 = 40;
    const RESIZED_COLS: u16 = 132;

    let fixture = CancellationFixture::start("pmux-resize", Duration::from_secs(10)).await;
    let mut terminal = fixture
        .runtime
        .create_terminal(
            Uuid::new_v4(),
            CREATED_ROWS,
            CREATED_COLS,
            sh_launch(&sleeping_fixture("RESIZE-READY")),
        )
        .await
        .map_err(|error| format!("{error:?}"))
        .expect("private terminal creation failed");
    terminal
        .wait_visible_text("RESIZE-READY", Duration::from_secs(10))
        .await
        .map_err(|error| format!("{error:?}"))
        .expect("private terminal never announced readiness");

    let before = terminal
        .snapshot()
        .await
        .expect("snapshot must be readable");
    assert_eq!(
        (before.rows, before.cols),
        (CREATED_ROWS, CREATED_COLS),
        "this test's premise is that the terminal starts at the rmux default and has to grow \
         out of it; it did not start there"
    );

    terminal
        .resize(RESIZED_ROWS, RESIZED_COLS)
        .await
        .map_err(|error| format!("{error:?}"))
        .expect("resize must be accepted");

    let after = terminal
        .snapshot()
        .await
        .expect("snapshot must be readable");
    assert_eq!(
        (after.rows, after.cols),
        (RESIZED_ROWS, RESIZED_COLS),
        "the resize returned success and did not land; 80 columns means `resize` went back to \
         `Pane::resize`, which a single-pane window silently clamps to the window's own size"
    );

    drop(terminal);
    fixture.finish().await;
}
