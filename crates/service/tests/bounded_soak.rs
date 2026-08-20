#![cfg(unix)]

mod process_support;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use pseudomux_rmux::{EnvironmentSnapshot, LaunchSpec};
use pseudomux_service::runtime::{PrivateRuntime, PrivateRuntimeConfig, SessionRuntime};
use tokio::net::UnixStream;
use uuid::Uuid;

use process_support::{
    CandidateFiles, ExactProcessGuard, ProcessIdentity, ProcessResources, SocketIdentity,
    exact_open_fd_count, find_direct_child, process_resources, runtime_entries, set_owner_only,
    wait_for_pid_file, wait_for_process_absence,
};

const ITERATIONS: usize = 24;
const WARMUP_ITERATIONS: usize = 6;
const MAX_RETAINED_RSS_GROWTH_KIB: u64 = 64 * 1024;
const MAX_SIDECAR_DESCRIPTOR_GROWTH: usize = 8;
const MAX_TEST_DESCRIPTOR_GROWTH: usize = 8;

/// Repeatedly exercises the real private rmux daemon, launcher broker, PTY
/// process boundary, and Ctrl-C transport. The actor-only ownership soak
/// remains in `resource_bounds`; this target is the process/resource
/// counterpart and does not emulate rmux state.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_real_rmux_cycles_remain_resource_bounded_and_leave_no_residue() {
    let candidates = CandidateFiles::discover(&["pmux-rmuxd", "pmux-launcher"]).unwrap();
    let test_fds_before = exact_open_fd_count(std::process::id()).unwrap();
    let root = tempfile::Builder::new()
        .prefix("pmux-soak-")
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

    let runtime = PrivateRuntime::start(PrivateRuntimeConfig {
        rmuxd: candidates.path("pmux-rmuxd").to_path_buf(),
        launcher: candidates.path("pmux-launcher").to_path_buf(),
        runtime_parent: Some(runtime_parent.clone()),
        startup_timeout: Duration::from_secs(10),
        operation_timeout: Duration::from_secs(5),
        lease_ttl: Duration::from_secs(3),
    })
    .await
    .unwrap();
    let runtime_dir = runtime.runtime_dir().to_path_buf();
    let rmux_socket = runtime.rmux_socket().to_path_buf();
    let launcher_socket = runtime_dir.join("launcher.sock");
    let rmux_socket_identity = SocketIdentity::capture(&rmux_socket).unwrap();
    let launcher_socket_identity = SocketIdentity::capture(&launcher_socket).unwrap();
    let sidecar_pid = find_direct_child(
        std::process::id(),
        &[
            candidates.path("pmux-rmuxd").to_string_lossy().as_ref(),
            rmux_socket.to_string_lossy().as_ref(),
        ],
    )
    .unwrap();
    let sidecar = ProcessIdentity::capture(sidecar_pid, rmux_socket.to_string_lossy()).unwrap();
    sidecar.assert_running().unwrap();
    let baseline_entries = runtime_entries(&runtime_dir).unwrap();
    assert!(
        baseline_entries
            .iter()
            .any(|entry| entry.ends_with("rmux.sock")),
        "private runtime baseline did not contain its rmux socket: {baseline_entries:?}"
    );

    let mut resource_baseline = None;
    let mut observations = Vec::new();
    for index in 0..ITERATIONS {
        exercise_one_cycle(&runtime, &pid_files, index)
            .await
            .unwrap();
        assert_eq!(
            runtime_entries(&runtime_dir).unwrap(),
            baseline_entries,
            "cycle {index} retained a session or launch artifact"
        );
        sidecar.assert_running().unwrap();
        if index + 1 == WARMUP_ITERATIONS {
            resource_baseline = Some(process_resources(&sidecar).unwrap());
        }
        if index + 1 == ITERATIONS / 2 || index + 1 == ITERATIONS {
            observations.push((index + 1, process_resources(&sidecar).unwrap()));
        }
    }

    let baseline = resource_baseline.expect("warmup must establish a resource baseline");
    for (iteration, observation) in observations {
        assert_resource_ceiling(baseline, observation, iteration);
    }
    assert_eq!(std::fs::read_dir(&pid_files).unwrap().count(), 0);

    runtime.shutdown().await.unwrap();
    wait_for_process_absence(&sidecar, Duration::from_secs(10))
        .await
        .unwrap();
    wait_for_socket_unreachable(&rmux_socket, Duration::from_secs(5))
        .await
        .unwrap();
    assert!(
        runtime_dir.exists()
            && launcher_socket_identity
                .remains_at(&launcher_socket)
                .unwrap(),
        "graceful owner frame was misclassified as owner loss"
    );
    drop(runtime);
    assert!(
        !runtime_dir.exists(),
        "private runtime directory survived drop"
    );
    assert!(
        !rmux_socket_identity.remains_at(&rmux_socket).unwrap(),
        "the exact private rmux socket inode survived runtime drop"
    );
    assert!(
        !launcher_socket_identity
            .remains_at(&launcher_socket)
            .unwrap(),
        "the exact launcher socket inode survived runtime drop"
    );
    assert_eq!(std::fs::read_dir(&runtime_parent).unwrap().count(), 0);
    assert!(
        exact_open_fd_count(std::process::id()).unwrap()
            <= test_fds_before + MAX_TEST_DESCRIPTOR_GROWTH,
        "the soak target retained test-process descriptors"
    );
    candidates.assert_unchanged().unwrap();
}

async fn exercise_one_cycle(
    runtime: &PrivateRuntime,
    pid_files: &std::path::Path,
    index: usize,
) -> Result<()> {
    let session_id = Uuid::new_v4();
    let marker = format!("pmux-bounded-soak-{}", session_id.simple());
    let pid_file = pid_files.join(format!("{index}.pid"));
    let program = format!(
        r#"marker='{marker}'; trap 'printf "\r\nPMUX_SOAK_INTERRUPTED\r\n"' INT; printf '%s\n' "$$" > "$PMUX_SOAK_PID_FILE"; printf 'PMUX_SOAK_READY_{index}\n'; while :; do sleep 30 || :; done"#
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
                        "PMUX_SOAK_PID_FILE".to_owned(),
                        pid_file.to_string_lossy().into_owned(),
                    )],
                    [],
                ),
            },
        )
        .await?;
    terminal
        .wait_visible_text(&format!("PMUX_SOAK_READY_{index}"), Duration::from_secs(5))
        .await?;
    let pane_pid = wait_for_pid_file(&pid_file, Duration::from_secs(5)).await?;
    let pane_identity = ProcessIdentity::capture(pane_pid, &marker)?;
    ensure!(
        i32::try_from(pane_identity.pid)? == pane_identity.session_id,
        "real rmux pane was not an isolated POSIX session leader: {pane_identity:?}"
    );
    let mut pane_guard = ExactProcessGuard::new(pane_identity);

    match index % 2 {
        0 => {
            let snapshot = terminal
                .wait_quiet(Duration::from_millis(75), Duration::from_secs(3))
                .await?;
            ensure!(snapshot.visible_text.contains("PMUX_SOAK_READY_"));
        }
        1 => {
            terminal.interrupt().await?;
            terminal
                .wait_visible_text("PMUX_SOAK_INTERRUPTED", Duration::from_secs(5))
                .await?;
        }
        _ => unreachable!(),
    }

    ensure!(
        terminal.close().await?,
        "rmux did not prove pane process reaping"
    );
    wait_for_process_absence(pane_guard.identity(), Duration::from_secs(5)).await?;
    pane_guard.disarm();
    std::fs::remove_file(&pid_file)
        .with_context(|| format!("failed to remove exact pid artifact {}", pid_file.display()))?;
    Ok(())
}

fn assert_resource_ceiling(
    baseline: ProcessResources,
    observation: ProcessResources,
    iteration: usize,
) {
    assert!(
        observation.rss_kib <= baseline.rss_kib + MAX_RETAINED_RSS_GROWTH_KIB,
        "private rmux RSS exceeded the conservative retained-growth ceiling after iteration {iteration}: baseline={}KiB observed={}KiB",
        baseline.rss_kib,
        observation.rss_kib
    );
    assert!(
        observation.open_fds <= baseline.open_fds + MAX_SIDECAR_DESCRIPTOR_GROWTH,
        "private rmux descriptor count exceeded its structural ceiling after iteration {iteration}: baseline={} observed={}",
        baseline.open_fds,
        observation.open_fds
    );
}

async fn wait_for_socket_unreachable(path: &std::path::Path, timeout: Duration) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if UnixStream::connect(path).await.is_err() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("owned socket remained reachable: {}", path.display());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
