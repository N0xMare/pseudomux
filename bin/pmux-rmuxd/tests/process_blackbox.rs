#![cfg(unix)]

use std::fs;
use std::io::Write as _;
use std::net::Shutdown;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use pseudomux_rmux::OWNER_GRACEFUL_SHUTDOWN_FRAME;
use rmux_client::AttachTransition;
use rmux_sdk::{CleanupPolicy, LeaseState, Rmux, SessionName, TerminalSizeSpec};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdout, Command};

#[path = "../../../tests/support/candidate_binary.rs"]
mod candidate_binary;
use candidate_binary::CandidateBinaries;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(20);

struct Sidecar {
    child: Child,
    stdout: BufReader<ChildStdout>,
    socket: PathBuf,
}

struct SidecarOutput {
    status: ExitStatus,
    stdout_after_readiness: Vec<u8>,
    stderr: Vec<u8>,
}

impl SidecarOutput {
    fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

fn candidates() -> &'static CandidateBinaries {
    static CANDIDATES: OnceLock<CandidateBinaries> = OnceLock::new();
    CANDIDATES.get_or_init(|| {
        CandidateBinaries::discover(
            std::env::var_os("PMUX_TEST_BIN_DIR").map(PathBuf::from),
            [(
                "pmux-rmuxd".to_owned(),
                PathBuf::from(env!("CARGO_BIN_EXE_pmux-rmuxd")),
            )],
        )
        .unwrap_or_else(|error| panic!("failed to bind pmux-rmuxd candidate: {error}"))
    })
}

fn rmuxd_binary() -> PathBuf {
    candidates().path("pmux-rmuxd").to_path_buf()
}

fn assert_candidate_unchanged() {
    candidates()
        .assert_unchanged()
        .unwrap_or_else(|error| panic!("pmux-rmuxd candidate changed during test: {error}"));
}

impl Sidecar {
    async fn start(root: &Path, shutdown_timeout_ms: u64) -> Self {
        Self::start_with_launcher(root, shutdown_timeout_ms, None).await
    }

    async fn start_with_launcher(
        root: &Path,
        shutdown_timeout_ms: u64,
        launcher_socket: Option<&Path>,
    ) -> Self {
        let socket = root.join("r.sock");
        let mut command = Command::new(rmuxd_binary());
        command
            .arg("--socket")
            .arg(&socket)
            .arg("--announce-ready")
            .arg("--owner-stdin")
            .arg("--shutdown-timeout-ms")
            .arg(shutdown_timeout_ms.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(launcher_socket) = launcher_socket {
            command.arg("--launcher-socket").arg(launcher_socket);
        }
        let mut child = command.spawn().expect("failed to spawn pmux-rmuxd");
        let mut stdout = BufReader::new(child.stdout.take().expect("sidecar stdout is piped"));
        let mut line = String::new();
        let bytes = tokio::time::timeout(STARTUP_TIMEOUT, stdout.read_line(&mut line))
            .await
            .expect("sidecar readiness timed out")
            .expect("failed to read sidecar readiness");
        assert_ne!(bytes, 0, "sidecar closed stdout before readiness");
        let readiness: Value = serde_json::from_str(line.trim()).expect("invalid readiness JSON");
        assert_eq!(readiness["kind"], "pmux-rmuxd-ready");
        assert_eq!(readiness["rmux_version"], "0.9.0");
        assert_eq!(readiness["socket"], socket.to_string_lossy().as_ref());
        Self {
            child,
            stdout,
            socket,
        }
    }

    fn close_owner_pipe(&mut self) {
        drop(self.child.stdin.take());
    }

    async fn close_owner_gracefully(&mut self) {
        let mut stdin = self.child.stdin.take().expect("owner stdin is piped");
        stdin
            .write_all(OWNER_GRACEFUL_SHUTDOWN_FRAME)
            .await
            .unwrap();
        stdin.shutdown().await.unwrap();
    }

    async fn finish(mut self) -> SidecarOutput {
        let status = tokio::time::timeout(SHUTDOWN_TIMEOUT, self.child.wait())
            .await
            .expect("pmux-rmuxd exceeded its shutdown bound")
            .expect("failed to wait for pmux-rmuxd");
        let mut stdout_after_readiness = Vec::new();
        self.stdout
            .read_to_end(&mut stdout_after_readiness)
            .await
            .unwrap();
        let mut stderr = Vec::new();
        self.child
            .stderr
            .take()
            .expect("sidecar stderr is piped")
            .read_to_end(&mut stderr)
            .await
            .unwrap();
        assert_candidate_unchanged();
        SidecarOutput {
            status,
            stdout_after_readiness,
            stderr,
        }
    }
}

fn private_root(label: &str) -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix(&format!("prd-{label}-"))
        .tempdir_in("/tmp")
        .unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    root
}

async fn connect(socket: &Path) -> Rmux {
    Rmux::builder()
        .unix_socket(socket)
        .default_timeout(Duration::from_secs(5))
        .connect()
        .await
        .expect("SDK handshake with sidecar failed")
}

async fn wait_for_pid_file(path: &Path) -> (i32, i32) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(text) = fs::read_to_string(path) {
            let pids = text
                .split_whitespace()
                .map(|value| value.parse::<i32>().unwrap())
                .collect::<Vec<_>>();
            if pids.len() == 2 {
                return (pids[0], pids[1]);
            }
        }
        assert!(Instant::now() < deadline, "pane did not publish its PIDs");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn half_close_complete_attach_frame(
    socket: PathBuf,
    session_name: SessionName,
    input: &[u8],
) -> std::os::unix::net::UnixStream {
    let connection = rmux_client::connect(&socket).expect("connect raw attach client");
    let transition = connection
        .begin_attach(session_name)
        .expect("begin raw attach");
    let AttachTransition::Upgraded(upgrade) = transition else {
        panic!("private sidecar rejected the raw attach");
    };
    let (mut stream, _initial_bytes) = upgrade.into_parts();
    let mut frame = Vec::with_capacity(5 + input.len());
    frame.push(1);
    frame.extend_from_slice(
        &u32::try_from(input.len())
            .expect("bounded test input")
            .to_le_bytes(),
    );
    frame.extend_from_slice(input);
    stream
        .write_all(&frame)
        .expect("write complete attach frame");
    stream.flush().expect("flush complete attach frame");
    stream
        .shutdown(Shutdown::Write)
        .expect("half-close raw attach input");
    stream
}

fn process_command(pid: i32) -> Option<String> {
    let output = std::process::Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .unwrap();
    if !output.status.success() || output.stdout.is_empty() {
        None
    } else {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
}

async fn wait_for_process_exit(pid: i32, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if process_command(pid).is_none() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "process {pid} remained after cleanup"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

struct ExactProcessCleanup {
    pids: Vec<i32>,
    marker: String,
    armed: bool,
}

impl ExactProcessCleanup {
    fn new(pids: Vec<i32>, marker: String) -> Self {
        Self {
            pids,
            marker,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ExactProcessCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        for pid in &self.pids {
            if process_command(*pid).is_some_and(|command| command.contains(&self.marker)) {
                let _ = std::process::Command::new("/bin/kill")
                    .args(["-KILL", &pid.to_string()])
                    .status();
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readiness_socket_permissions_sdk_handshake_and_owner_eof_are_exact() {
    let root = private_root("ready");
    let mut sidecar = Sidecar::start(root.path(), 5_000).await;
    let metadata = fs::symlink_metadata(&sidecar.socket).unwrap();
    assert!(metadata.file_type().is_socket());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    assert_eq!(
        fs::metadata(root.path()).unwrap().permissions().mode() & 0o777,
        0o700
    );

    let rmux = connect(&sidecar.socket).await;
    let capabilities = rmux.capabilities().await.unwrap();
    assert!(
        !capabilities.is_empty(),
        "rmux handshake returned no capabilities"
    );
    assert!(rmux.list_sessions().await.unwrap().is_empty());

    sidecar.close_owner_pipe();
    let socket = sidecar.socket.clone();
    let output = sidecar.finish().await;
    assert!(output.status.success(), "{}", output.stderr_text());
    assert!(output.stdout_after_readiness.is_empty());
    assert!(output.stderr.is_empty(), "{}", output.stderr_text());
    assert!(!socket.exists(), "private rmux socket survived owner EOF");
    assert!(fs::read_dir(root.path()).unwrap().next().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_attach_half_close_delivers_the_final_complete_frame_exactly_once() {
    let root = private_root("attach-eof");
    let mut sidecar = Sidecar::start(root.path(), 5_000).await;
    let rmux = connect(&sidecar.socket).await;
    let session_name = SessionName::new(format!("pmux-attach-eof-{}", std::process::id())).unwrap();
    let mut owned = rmux
        .owned_session(session_name.clone())
        .cleanup_policy(CleanupPolicy::KillOnOwnerExit)
        .lease_ttl(Duration::from_secs(5))
        .await
        .unwrap();
    let pane = owned.pane(0, 0);
    pane.resize(TerminalSizeSpec::new(100, 24)).await.unwrap();
    let capture = root.path().join("attach-input.bin");
    let script = r#"/bin/stty raw -echo min 0 time 5
printf 'PMUX_ATTACH_INPUT_READY\r\n'
# VTIME makes the next empty read terminate dd, while the larger ceiling
# leaves room to expose an accidentally duplicated two-byte dispatch.
/bin/dd of="$PMUX_ATTACH_CAPTURE" bs=1 count=8 2>/dev/null
/bin/stty sane
printf 'PMUX_ATTACH_INPUT_DONE\r\n'
exec /bin/sleep 30
"#;
    pane.spawn(["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()])
        .cwd(root.path())
        .env("PMUX_ATTACH_CAPTURE", capture.to_string_lossy())
        .kill_existing(true)
        .await
        .unwrap();
    pane.expect_visible_text()
        .to_contain("PMUX_ATTACH_INPUT_READY")
        .timeout(Duration::from_secs(5))
        .await
        .unwrap();
    let done = pane
        .wait_for_text_next("PMUX_ATTACH_INPUT_DONE")
        .await
        .unwrap();

    let socket = sidecar.socket.clone();
    let input = b"y\r";
    let attach_stream = tokio::task::spawn_blocking(move || {
        half_close_complete_attach_frame(socket, session_name, input)
    })
    .await
    .expect("raw attach worker join");
    done.await.unwrap();
    assert_eq!(
        fs::read(&capture).expect("read exact attach input capture"),
        input,
        "the final complete attach frame must reach the pane exactly once"
    );

    drop(attach_stream);
    assert!(
        owned.cleanup().await.unwrap(),
        "owned session must be removed"
    );
    assert!(rmux.list_sessions().await.unwrap().is_empty());
    fs::remove_file(&capture).unwrap();
    sidecar.close_owner_pipe();
    let socket = sidecar.socket.clone();
    let output = sidecar.finish().await;
    assert!(output.status.success(), "{}", output.stderr_text());
    assert!(output.stdout_after_readiness.is_empty());
    assert!(output.stderr.is_empty(), "{}", output.stderr_text());
    assert!(!socket.exists(), "private rmux socket survived cleanup");
    assert!(fs::read_dir(root.path()).unwrap().next().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exact_graceful_owner_frame_does_not_claim_daemon_runtime_cleanup() {
    let root = private_root("graceful");
    let launcher_socket = root.path().join("launcher.sock");
    let launcher = std::os::unix::net::UnixListener::bind(&launcher_socket).unwrap();
    fs::set_permissions(&launcher_socket, fs::Permissions::from_mode(0o600)).unwrap();
    let daemon_owned = root.path().join("daemon-owned.json");
    fs::write(&daemon_owned, b"preserve-for-daemon").unwrap();
    fs::set_permissions(&daemon_owned, fs::Permissions::from_mode(0o600)).unwrap();
    let mut sidecar =
        Sidecar::start_with_launcher(root.path(), 5_000, Some(&launcher_socket)).await;

    sidecar.close_owner_gracefully().await;
    let socket = sidecar.socket.clone();
    let output = sidecar.finish().await;
    assert!(output.status.success(), "{}", output.stderr_text());
    assert!(output.stdout_after_readiness.is_empty());
    assert!(output.stderr.is_empty(), "{}", output.stderr_text());
    assert!(!socket.exists(), "rmux socket survived graceful owner EOF");
    assert_eq!(fs::read(&daemon_owned).unwrap(), b"preserve-for-daemon");
    assert!(launcher_socket.exists());
    drop(launcher);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bare_owner_loss_removes_the_exact_private_runtime_tree() {
    let root = private_root("owner-loss");
    let root_path = root.path().to_path_buf();
    let launcher_socket = root.path().join("launcher.sock");
    let launcher = std::os::unix::net::UnixListener::bind(&launcher_socket).unwrap();
    fs::set_permissions(&launcher_socket, fs::Permissions::from_mode(0o600)).unwrap();
    let private_material = root.path().join("settings.json");
    fs::write(&private_material, b"private-launch-material").unwrap();
    fs::set_permissions(&private_material, fs::Permissions::from_mode(0o600)).unwrap();
    let mut sidecar =
        Sidecar::start_with_launcher(root.path(), 5_000, Some(&launcher_socket)).await;

    drop(launcher);
    sidecar.close_owner_pipe();
    let output = sidecar.finish().await;
    assert!(output.status.success(), "{}", output.stderr_text());
    assert!(output.stdout_after_readiness.is_empty());
    assert!(output.stderr.is_empty(), "{}", output.stderr_text());
    assert!(
        !root_path.exists(),
        "owner-loss cleanup left the captured private runtime"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_eof_reaps_a_hup_term_ignoring_pane_tree_and_surfaces_lease_loss() {
    let root = private_root("tree");
    let mut sidecar = Sidecar::start(root.path(), 8_000).await;
    let rmux = connect(&sidecar.socket).await;
    let session_name = SessionName::new(format!("pmux-blackbox-{}", std::process::id())).unwrap();
    let owned = rmux
        .owned_session(session_name)
        .cleanup_policy(CleanupPolicy::KillOnOwnerExit)
        .lease_ttl(Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(owned.lease_state(), LeaseState::Active);
    let pane = owned.pane(0, 0);
    pane.resize(TerminalSizeSpec::new(100, 24)).await.unwrap();
    let pid_file = root.path().join("pids");
    let marker = format!("pmux-rmuxd-tree-{}", std::process::id());
    let script = r#"trap '' HUP TERM INT
/bin/sh -c 'trap "" HUP TERM INT; while :; do sleep 30; done' "$PMUX_PROCESS_MARKER" &
child=$!
printf '%s %s\n' "$$" "$child" > "$PMUX_PID_FILE"
printf 'PMUX_RMUXD_TREE_READY\n'
wait "$child"
"#;
    pane.spawn([
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        script.to_owned(),
        marker.clone(),
    ])
    .cwd(root.path())
    .env("PMUX_PID_FILE", pid_file.to_string_lossy())
    .env("PMUX_PROCESS_MARKER", marker.clone())
    .kill_existing(true)
    .await
    .unwrap();
    pane.expect_visible_text()
        .to_contain("PMUX_RMUXD_TREE_READY")
        .timeout(Duration::from_secs(5))
        .await
        .unwrap();
    let (root_pid, descendant_pid) = wait_for_pid_file(&pid_file).await;
    let mut cleanup = ExactProcessCleanup::new(vec![root_pid, descendant_pid], marker.clone());
    for pid in [root_pid, descendant_pid] {
        let command = process_command(pid).unwrap_or_else(|| panic!("process {pid} was absent"));
        assert!(
            command.contains(&marker),
            "unexpected process identity: {command}"
        );
    }

    sidecar.close_owner_pipe();
    let socket = sidecar.socket.clone();
    let output = sidecar.finish().await;
    assert!(output.status.success(), "{}", output.stderr_text());
    assert!(output.stdout_after_readiness.is_empty());
    let stderr = output.stderr_text();
    assert!(stderr.is_empty(), "{stderr}");
    assert!(!stderr.contains(&marker));
    wait_for_process_exit(root_pid, Duration::from_secs(5)).await;
    wait_for_process_exit(descendant_pid, Duration::from_secs(5)).await;
    cleanup.disarm();
    let lease_deadline = Instant::now() + Duration::from_secs(6);
    while !owned.lease_lost() && Instant::now() < lease_deadline {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        owned.lease_lost(),
        "SDK owner did not observe sidecar lease loss"
    );
    assert!(!socket.exists());
    drop(owned);
    fs::remove_file(&pid_file).unwrap();
    assert!(fs::read_dir(root.path()).unwrap().next().is_none());
}

async fn run_failure(args: &[&str]) -> std::process::Output {
    let mut command = Command::new(rmuxd_binary());
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(5), command.output())
        .await
        .expect("invalid pmux-rmuxd invocation hung")
        .unwrap();
    assert_candidate_unchanged();
    output
}

#[tokio::test]
async fn malformed_startup_inputs_fail_quickly_without_endpoint_damage() {
    let relative = run_failure(&["--socket", "relative.sock"]).await;
    assert!(!relative.status.success());
    assert!(relative.stdout.is_empty());
    assert!(String::from_utf8_lossy(&relative.stderr).contains("absolute"));

    let zero = private_root("zero-timeout");
    let socket = zero.path().join("r.sock");
    let output = run_failure(&[
        "--socket",
        socket.to_str().unwrap(),
        "--announce-ready",
        "--owner-stdin",
        "--shutdown-timeout-ms",
        "0",
    ])
    .await;
    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "invalid startup announced readiness: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("non-zero"));
    assert!(!socket.exists());

    let permissive = private_root("permissive");
    fs::set_permissions(permissive.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let socket = permissive.path().join("r.sock");
    let output = run_failure(&["--socket", socket.to_str().unwrap()]).await;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("mode 755"));
    assert!(!socket.exists());

    let occupied = private_root("occupied");
    let socket = occupied.path().join("r.sock");
    fs::write(&socket, b"preserve-this-file").unwrap();
    let output = run_failure(&["--socket", socket.to_str().unwrap()]).await;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(fs::read(&socket).unwrap(), b"preserve-this-file");
}
