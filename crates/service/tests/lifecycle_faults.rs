#![cfg(unix)]

mod process_support;

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use pseudomux_rmux::{EnvironmentSnapshot, LaunchSpec};
use pseudomux_service::runtime::{PrivateRuntime, PrivateRuntimeConfig, SessionRuntime};
use tokio::net::UnixStream;
use uuid::Uuid;

use process_support::{
    CandidateFiles, ExactProcessGuard, ProcessIdentity, SocketIdentity, find_direct_child,
    runtime_entries, set_owner_only, wait_for_pid_file, wait_for_process_absence,
};

const FAULT_ITERATIONS: usize = 4;

/// Proves that the terminal's locally retained POSIX process boundary remains
/// sufficient when the private rmux control plane is killed. Owner-pipe loss
/// from the opposite direction (pmuxd SIGKILL -> sidecar and pane cleanup) is
/// separately exercised by `private_runtime`; this target adds the distinct
/// sidecar-loss direction and repeats it as a bounded fault loop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_exact_sidecar_loss_reaps_active_descendants_and_runtime_artifacts() {
    let candidates = CandidateFiles::discover(&["pmux-rmuxd", "pmux-launcher"]).unwrap();
    let root = tempfile::Builder::new()
        .prefix("pmux-sidecar-loss-")
        .tempdir_in("/tmp")
        .unwrap();
    let root_path = root.path().canonicalize().unwrap();
    set_owner_only(&root_path).unwrap();
    let runtime_parent = root_path.join("runtimes");
    let pid_files = root_path.join("pid-files");
    std::fs::create_dir(&runtime_parent).unwrap();
    std::fs::create_dir(&pid_files).unwrap();
    set_owner_only(&runtime_parent).unwrap();
    set_owner_only(&pid_files).unwrap();

    for index in 0..FAULT_ITERATIONS {
        exercise_sidecar_loss(&candidates, &runtime_parent, &pid_files, index)
            .await
            .unwrap();
        assert_eq!(
            runtime_entries(&runtime_parent).unwrap().len(),
            0,
            "fault iteration {index} retained a runtime directory or socket"
        );
    }

    assert_eq!(std::fs::read_dir(&pid_files).unwrap().count(), 0);
    candidates.assert_unchanged().unwrap();
}

/// Exercises the real OS/rmux boundary for the fail-closed escape rule. The
/// pane helper starts a descendant inside its isolated POSIX session, then the
/// descendant calls `setsid(2)` while it is still directly observable below
/// the pane leader. Cleanup may reap the original session, but both the first
/// close and an exact retry must remain unconfirmed permanently. The escaped
/// process is fenced by PID plus kernel start identity before this test alone
/// terminates it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn observed_descendant_escape_keeps_real_close_unconfirmed_across_retry() {
    let candidates = CandidateFiles::discover(&["pmux-rmuxd", "pmux-launcher"]).unwrap();
    let root = tempfile::Builder::new()
        .prefix("pmux-observed-escape-")
        .tempdir_in("/tmp")
        .unwrap();
    let root_path = root.path().canonicalize().unwrap();
    set_owner_only(&root_path).unwrap();
    let runtime_parent = root_path.join("runtimes");
    std::fs::create_dir(&runtime_parent).unwrap();
    set_owner_only(&runtime_parent).unwrap();
    let pid_file = root_path.join("descendant.pid");
    let ready_file = root_path.join("descendant.ready");
    let trigger_file = root_path.join("escape.trigger");
    let escaped_file = root_path.join("descendant.escaped");
    let helper_executable = std::env::current_exe().unwrap().canonicalize().unwrap();

    let runtime = PrivateRuntime::start(PrivateRuntimeConfig {
        rmuxd: candidates.path("pmux-rmuxd").to_path_buf(),
        launcher: candidates.path("pmux-launcher").to_path_buf(),
        runtime_parent: Some(runtime_parent.clone()),
        startup_timeout: Duration::from_secs(10),
        operation_timeout: Duration::from_secs(3),
        lease_ttl: Duration::from_secs(5),
    })
    .await
    .unwrap();
    let mut terminal = runtime
        .create_terminal(
            Uuid::new_v4(),
            24,
            100,
            LaunchSpec {
                executable: helper_executable,
                args: vec![
                    "--ignored".into(),
                    "--exact".into(),
                    "observed_escape_pane_helper".into(),
                    "--nocapture".into(),
                    "--test-threads=1".into(),
                ],
                cwd: std::env::current_dir().unwrap().canonicalize().unwrap(),
                environment: EnvironmentSnapshot::capture().patched(
                    [
                        ("PMUX_ESCAPE_HELPER_ROLE".to_owned(), "pane".to_owned()),
                        (
                            "PMUX_ESCAPE_PID_FILE".to_owned(),
                            pid_file.to_string_lossy().into_owned(),
                        ),
                        (
                            "PMUX_ESCAPE_READY_FILE".to_owned(),
                            ready_file.to_string_lossy().into_owned(),
                        ),
                        (
                            "PMUX_ESCAPE_TRIGGER_FILE".to_owned(),
                            trigger_file.to_string_lossy().into_owned(),
                        ),
                        (
                            "PMUX_ESCAPE_ESCAPED_FILE".to_owned(),
                            escaped_file.to_string_lossy().into_owned(),
                        ),
                    ],
                    [],
                ),
            },
        )
        .await
        .unwrap();

    terminal
        .wait_visible_text("PMUX_ESCAPE_PANE_READY", Duration::from_secs(5))
        .await
        .unwrap();
    let descendant_pid = wait_for_pid_file(&pid_file, Duration::from_secs(5))
        .await
        .unwrap();
    let before_escape =
        ProcessIdentity::capture(descendant_pid, "observed_escape_descendant_helper").unwrap();
    assert_ne!(
        before_escape.session_id,
        i32::try_from(descendant_pid).unwrap(),
        "descendant escaped before its original boundary identity was captured"
    );

    std::fs::write(&trigger_file, b"escape\n").unwrap();
    wait_for_file(&escaped_file, Duration::from_secs(5))
        .await
        .unwrap();
    let after_escape =
        ProcessIdentity::capture(descendant_pid, "observed_escape_descendant_helper").unwrap();
    assert_eq!(before_escape.start_token, after_escape.start_token);
    assert_ne!(before_escape.session_id, after_escape.session_id);
    assert_eq!(
        after_escape.session_id,
        i32::try_from(descendant_pid).unwrap()
    );
    assert_eq!(
        after_escape.process_group_id,
        i32::try_from(descendant_pid).unwrap()
    );
    let mut escaped_cleanup = ExactProcessGuard::new(after_escape);

    assert!(
        !terminal.close().await.unwrap(),
        "an observed descendant escape must invalidate positive cleanup proof"
    );
    escaped_cleanup.identity().assert_running().unwrap();
    assert!(
        !terminal.close().await.unwrap(),
        "retry must not forget a previously observed descendant escape"
    );
    escaped_cleanup.identity().assert_running().unwrap();

    escaped_cleanup.identity().signal(libc::SIGKILL).unwrap();
    wait_for_process_absence(escaped_cleanup.identity(), Duration::from_secs(5))
        .await
        .unwrap();
    escaped_cleanup.disarm();
    drop(terminal);
    runtime.shutdown().await.unwrap();
    drop(runtime);
    assert!(runtime_entries(&runtime_parent).unwrap().is_empty());
    candidates.assert_unchanged().unwrap();
}

/// Subprocess-only fixture for `observed_descendant_escape_keeps_real_close_unconfirmed_across_retry`.
#[test]
#[ignore = "invoked only as the controlled pane process for the observed-escape regression"]
fn observed_escape_pane_helper() {
    if std::env::var("PMUX_ESCAPE_HELPER_ROLE").as_deref() != Ok("pane") {
        return;
    }
    let executable = std::env::current_exe().unwrap();
    let mut descendant = Command::new(executable)
        .args([
            "--ignored",
            "--exact",
            "observed_escape_descendant_helper",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("PMUX_ESCAPE_HELPER_ROLE", "descendant")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let ready_file = PathBuf::from(std::env::var_os("PMUX_ESCAPE_READY_FILE").unwrap());
    wait_for_file_blocking(&ready_file, Duration::from_secs(5)).unwrap();
    println!("PMUX_ESCAPE_PANE_READY");
    let _ = descendant.wait();
}

/// Controlled descendant which changes only its own POSIX session and then
/// waits to be terminated through its exact post-escape identity.
#[test]
#[ignore = "invoked only as the controlled escaping descendant regression fixture"]
#[allow(
    unsafe_code,
    reason = "the controlled helper changes only its own POSIX session"
)]
fn observed_escape_descendant_helper() {
    if std::env::var("PMUX_ESCAPE_HELPER_ROLE").as_deref() != Ok("descendant") {
        return;
    }
    ignore_cleanup_signals();
    let pid_file = PathBuf::from(std::env::var_os("PMUX_ESCAPE_PID_FILE").unwrap());
    let ready_file = PathBuf::from(std::env::var_os("PMUX_ESCAPE_READY_FILE").unwrap());
    let trigger_file = PathBuf::from(std::env::var_os("PMUX_ESCAPE_TRIGGER_FILE").unwrap());
    let escaped_file = PathBuf::from(std::env::var_os("PMUX_ESCAPE_ESCAPED_FILE").unwrap());
    std::fs::write(&pid_file, format!("{}\n", std::process::id())).unwrap();
    std::fs::write(&ready_file, b"ready\n").unwrap();
    wait_for_file_blocking(&trigger_file, Duration::from_secs(10)).unwrap();
    let pid = i32::try_from(std::process::id()).unwrap();
    let session_id = unsafe {
        // SAFETY: `setsid` changes only this controlled fixture process.
        libc::setsid()
    };
    assert_eq!(
        session_id, pid,
        "controlled descendant could not escape via setsid"
    );
    std::fs::write(&escaped_file, b"escaped\n").unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(30));
    }
}

async fn exercise_sidecar_loss(
    candidates: &CandidateFiles,
    runtime_parent: &std::path::Path,
    pid_files: &std::path::Path,
    index: usize,
) -> Result<()> {
    let runtime = PrivateRuntime::start(PrivateRuntimeConfig {
        rmuxd: candidates.path("pmux-rmuxd").to_path_buf(),
        launcher: candidates.path("pmux-launcher").to_path_buf(),
        runtime_parent: Some(runtime_parent.to_path_buf()),
        startup_timeout: Duration::from_secs(10),
        operation_timeout: Duration::from_secs(3),
        lease_ttl: Duration::from_secs(2),
    })
    .await?;
    let runtime_dir = runtime.runtime_dir().to_path_buf();
    let rmux_socket = runtime.rmux_socket().to_path_buf();
    let socket_identity = SocketIdentity::capture(&rmux_socket)?;
    let sidecar_pid = find_direct_child(
        std::process::id(),
        &[
            candidates.path("pmux-rmuxd").to_string_lossy().as_ref(),
            rmux_socket.to_string_lossy().as_ref(),
        ],
    )?;
    let sidecar_identity = ProcessIdentity::capture(sidecar_pid, rmux_socket.to_string_lossy())?;
    let mut sidecar_guard = ExactProcessGuard::new(sidecar_identity);

    let session_id = Uuid::new_v4();
    let marker = format!("pmux-sidecar-loss-{}", session_id.simple());
    let pid_file = pid_files.join(format!("{index}.pid"));
    let program = format!(
        "marker='{marker}'; (trap '' HUP TERM INT; while :; do sleep 30; done) & child=$!; printf '%s\\n' \"$child\" > \"$PMUX_FAULT_PID_FILE\"; printf 'PMUX_FAULT_READY_{index}\\n'; trap '' HUP TERM INT; wait \"$child\""
    );
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
                        "PMUX_FAULT_PID_FILE".to_owned(),
                        pid_file.to_string_lossy().into_owned(),
                    )],
                    [],
                ),
            },
        )
        .await?;
    terminal
        .wait_visible_text(&format!("PMUX_FAULT_READY_{index}"), Duration::from_secs(5))
        .await?;
    let descendant_pid = wait_for_pid_file(&pid_file, Duration::from_secs(5)).await?;
    let descendant_identity = ProcessIdentity::capture(descendant_pid, &marker)?;
    ensure!(
        descendant_identity.session_id > 0,
        "descendant had no observable POSIX session identity"
    );
    let mut descendant_guard = ExactProcessGuard::new(descendant_identity);

    sidecar_guard.identity().signal(libc::SIGKILL)?;
    wait_for_socket_unreachable(&rmux_socket, Duration::from_secs(3)).await?;

    ensure!(
        terminal.close().await?,
        "local process-boundary fallback did not prove cleanup after sidecar loss"
    );
    wait_for_process_absence(descendant_guard.identity(), Duration::from_secs(5)).await?;
    descendant_guard.disarm();
    std::fs::remove_file(&pid_file)
        .with_context(|| format!("failed to remove exact pid artifact {}", pid_file.display()))?;

    let shutdown = runtime
        .shutdown()
        .await
        .expect_err("a SIGKILLed sidecar must not be reported as a clean shutdown");
    ensure!(
        shutdown.to_string().contains("exited unsuccessfully"),
        "sidecar-loss shutdown returned an unexpected error: {shutdown:#}"
    );
    wait_for_process_absence(sidecar_guard.identity(), Duration::from_secs(5)).await?;
    sidecar_guard.disarm();
    drop(runtime);
    ensure!(
        !runtime_dir.exists(),
        "faulted private runtime survived drop"
    );
    ensure!(
        !socket_identity.remains_at(&rmux_socket)?,
        "faulted private rmux socket inode survived runtime drop"
    );
    Ok(())
}

async fn wait_for_file(path: &std::path::Path, timeout: Duration) -> Result<()> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || wait_for_file_blocking(&path, timeout))
        .await
        .context("file-wait observer task failed")?
}

fn wait_for_file_blocking(path: &std::path::Path, timeout: Duration) -> Result<()> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if path.is_file() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!(
                "timed out waiting for exact fixture file {}",
                path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[allow(
    unsafe_code,
    reason = "the controlled helper changes only its own signal dispositions"
)]
fn ignore_cleanup_signals() {
    for signal in [libc::SIGHUP, libc::SIGTERM, libc::SIGINT] {
        let previous = unsafe {
            // SAFETY: `signal` changes only this controlled fixture process.
            libc::signal(signal, libc::SIG_IGN)
        };
        assert_ne!(previous, libc::SIG_ERR, "could not ignore signal {signal}");
    }
}

async fn wait_for_socket_unreachable(path: &std::path::Path, timeout: Duration) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if UnixStream::connect(path).await.is_err() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("faulted socket remained reachable: {}", path.display());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
