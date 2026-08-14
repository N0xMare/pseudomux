#![cfg(unix)]

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use pseudomux_protocol::v1::{
    ErrorCode, MAX_NATIVE_FRAME_BYTES, PROTOCOL_VERSION, Request, RequestEnvelope, RequestId,
    ResponseEnvelope, ResponsePayload, ResponseResult,
};
use serde_json::{Value, json};

#[path = "../../../tests/support/candidate_binary.rs"]
mod candidate_binary;
use candidate_binary::CandidateBinaries;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(12);
const PROCESS_TIMEOUT: Duration = Duration::from_secs(20);
const REQUEST_SECRET: &str = "pmux-daemon-request-secret-must-not-be-logged";

struct ProcessOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl ProcessOutput {
    fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

struct Daemon(Option<Child>);

impl Daemon {
    fn spawn(command: &mut Command) -> Self {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Self(Some(command.spawn().expect("failed to spawn pmuxd")))
    }

    fn id(&self) -> u32 {
        self.0.as_ref().expect("daemon is present").id()
    }

    fn try_status(&mut self) -> Option<ExitStatus> {
        self.0
            .as_mut()
            .expect("daemon is present")
            .try_wait()
            .unwrap()
    }

    #[allow(unsafe_code)]
    fn signal(&self, signal: i32) {
        let pid = i32::try_from(self.id()).unwrap();
        // SAFETY: the PID is the exact child owned by this guard and `kill`
        // does not dereference pointers.
        let result = unsafe { libc::kill(pid, signal) };
        assert_eq!(
            result,
            0,
            "failed to signal pmuxd: {}",
            io::Error::last_os_error()
        );
    }

    fn wait(mut self, timeout: Duration) -> ProcessOutput {
        let mut child = self.0.take().expect("daemon is present");
        let deadline = Instant::now() + timeout;
        let status = loop {
            if let Some(status) = child.try_wait().expect("pmuxd wait failed") {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("pmuxd exceeded its {timeout:?} process bound");
            }
            thread::sleep(Duration::from_millis(10));
        };
        let mut stdout = Vec::new();
        child
            .stdout
            .take()
            .expect("daemon stdout is piped")
            .read_to_end(&mut stdout)
            .unwrap();
        let mut stderr = Vec::new();
        child
            .stderr
            .take()
            .expect("daemon stderr is piped")
            .read_to_end(&mut stderr)
            .unwrap();
        assert_candidates_unchanged();
        ProcessOutput {
            status,
            stdout,
            stderr,
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn private_root(label: &str) -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix(&format!("pmd-{label}-"))
        .tempdir_in("/tmp")
        .unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn configured_bin_dir() -> Option<PathBuf> {
    let preferred = std::env::var_os("PMUX_TEST_BIN_DIR").map(PathBuf::from);
    let legacy = std::env::var_os("PMUX_PROCESS_BIN_DIR").map(PathBuf::from);
    let directory = match (preferred, legacy) {
        (Some(preferred), Some(legacy)) => {
            assert_eq!(
                preferred, legacy,
                "PMUX_TEST_BIN_DIR and PMUX_PROCESS_BIN_DIR must identify one exact candidate"
            );
            preferred
        }
        (Some(directory), None) | (None, Some(directory)) => directory,
        (None, None) => return None,
    };
    Some(directory)
}

fn cargo_companion(name: &str) -> PathBuf {
    let test_built = Path::new(env!("CARGO_BIN_EXE_pmuxd"))
        .parent()
        .unwrap()
        .join(name);
    if test_built.is_file() {
        return test_built;
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/release")
        .join(name)
}

fn candidates() -> &'static CandidateBinaries {
    static CANDIDATES: OnceLock<CandidateBinaries> = OnceLock::new();
    CANDIDATES.get_or_init(|| {
        CandidateBinaries::discover(
            configured_bin_dir(),
            [
                (
                    "pmuxd".to_owned(),
                    PathBuf::from(env!("CARGO_BIN_EXE_pmuxd")),
                ),
                ("pmux-rmuxd".to_owned(), cargo_companion("pmux-rmuxd")),
                ("pmux-launcher".to_owned(), cargo_companion("pmux-launcher")),
                ("pmux-hook".to_owned(), cargo_companion("pmux-hook")),
            ],
        )
        .unwrap_or_else(|error| panic!("failed to bind pmuxd process candidates: {error}"))
    })
}

fn assert_candidates_unchanged() {
    candidates()
        .assert_unchanged()
        .unwrap_or_else(|error| panic!("pmuxd process candidate changed during test: {error}"));
}

fn pmuxd_binary() -> PathBuf {
    candidates().path("pmuxd").to_path_buf()
}

fn packaged_companion(name: &str) -> PathBuf {
    candidates().path(name).to_path_buf()
}

#[test]
fn explicit_candidate_directory_never_falls_back_to_cargo_binaries() {
    let root = private_root("candidate-missing");
    let exact = fs::canonicalize(root.path()).unwrap();
    let result = CandidateBinaries::discover(
        Some(exact),
        [(
            "pmuxd".to_owned(),
            PathBuf::from(env!("CARGO_BIN_EXE_pmuxd")),
        )],
    );
    let Err(error) = result else {
        panic!("an explicit candidate directory silently used the Cargo fallback")
    };
    assert!(error.contains("required candidate pmuxd is unavailable"));
}

#[test]
fn candidate_hash_fence_detects_same_length_in_place_mutation() {
    let root = private_root("candidate-mutated");
    let exact = fs::canonicalize(root.path()).unwrap();
    let binary = exact.join("mini-bin");
    fs::write(&binary, b"first").unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    let candidates =
        CandidateBinaries::discover(Some(exact), [("mini-bin".to_owned(), binary.clone())])
            .unwrap();

    fs::write(&binary, b"other").unwrap();
    let error = candidates.assert_unchanged().unwrap_err();
    assert!(error.contains("changed content or filesystem identity"));
}

fn base_serve_command(socket: &Path, runtime_parent: &Path) -> Command {
    let rmuxd = packaged_companion("pmux-rmuxd");
    let launcher = packaged_companion("pmux-launcher");
    let hook = packaged_companion("pmux-hook");
    assert!(hook.is_file(), "packaged pmux-hook is unavailable");
    let mut command = Command::new(pmuxd_binary());
    command
        .arg("serve")
        .arg("--socket")
        .arg(socket)
        .arg("--rmuxd")
        .arg(rmuxd)
        .arg("--launcher")
        .arg(launcher)
        .arg("--runtime-parent")
        .arg(runtime_parent)
        .arg("--shutdown-grace-ms")
        .arg("250");
    command
}

fn write_payload(stream: &mut UnixStream, payload: &[u8]) -> io::Result<()> {
    stream.write_all(&u32::try_from(payload.len()).unwrap().to_be_bytes())?;
    stream.write_all(payload)?;
    stream.flush()
}

fn read_value(stream: &mut UnixStream) -> io::Result<Value> {
    let mut header = [0_u8; 4];
    stream.read_exact(&mut header)?;
    let length = u32::from_be_bytes(header) as usize;
    assert!(length <= MAX_NATIVE_FRAME_BYTES);
    let mut payload = vec![0; length];
    stream.read_exact(&mut payload)?;
    serde_json::from_slice(&payload).map_err(io::Error::other)
}

fn connect_with_timeout(socket: &Path, timeout: Duration) -> io::Result<UnixStream> {
    let stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    Ok(stream)
}

fn ping(socket: &Path, request_number: u128, timeout: Duration) -> io::Result<ResponseEnvelope> {
    let mut stream = connect_with_timeout(socket, timeout)?;
    let request = RequestEnvelope::new(RequestId::from_u128(request_number), Request::Ping);
    write_payload(&mut stream, &serde_json::to_vec(&request).unwrap())?;
    serde_json::from_value(read_value(&mut stream)?).map_err(io::Error::other)
}

fn wait_until_ready(daemon: &mut Daemon, socket: &Path) {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        if let Some(status) = daemon.try_status() {
            panic!("pmuxd exited before readiness: {status}");
        }
        if let Ok(response) = ping(socket, 1, Duration::from_millis(300)) {
            if matches!(
                response.payload,
                ResponsePayload::Success(result) if matches!(*result, ResponseResult::Pong(_))
            ) {
                return;
            }
        }
        assert!(Instant::now() < deadline, "pmuxd did not become ready");
        thread::sleep(Duration::from_millis(20));
    }
}

fn direct_child_pids(parent: u32) -> Vec<i32> {
    let output = Command::new("/bin/ps")
        .args(["-axo", "pid=,ppid="])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse::<i32>().ok()?;
            let ppid = fields.next()?.parse::<u32>().ok()?;
            (ppid == parent).then_some(pid)
        })
        .collect()
}

fn wait_for_direct_child(parent: u32) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let children = direct_child_pids(parent);
        if children.len() == 1 {
            return children[0];
        }
        assert!(
            Instant::now() < deadline,
            "expected one pmuxd sidecar child, observed {children:?}"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn process_exists(pid: i32) -> bool {
    Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "pid="])
        .output()
        .is_ok_and(|output| output.status.success() && !output.stdout.is_empty())
}

fn wait_for_process_exit(pid: i32) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while process_exists(pid) {
        assert!(
            Instant::now() < deadline,
            "owned sidecar PID {pid} survived shutdown"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn raw_uds_modes_framing_redaction_signal_and_cleanup_cross_the_real_daemon() {
    let root = private_root("serve");
    let socket = root.path().join("p.sock");
    let runtime_parent = root.path().join("runtime");
    fs::create_dir(&runtime_parent).unwrap();
    fs::set_permissions(&runtime_parent, fs::Permissions::from_mode(0o700)).unwrap();
    let mut command = base_serve_command(&socket, &runtime_parent);
    let mut daemon = Daemon::spawn(&mut command);
    wait_until_ready(&mut daemon, &socket);
    let daemon_pid = daemon.id();
    let sidecar_pid = wait_for_direct_child(daemon_pid);

    let socket_metadata = fs::symlink_metadata(&socket).unwrap();
    assert!(socket_metadata.file_type().is_socket());
    assert_eq!(socket_metadata.permissions().mode() & 0o777, 0o600);
    let logs = root.path().join("logs");
    let log = logs.join("pmuxd.log");
    assert_eq!(
        fs::metadata(&logs).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&log).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let runtime_dirs = fs::read_dir(&runtime_parent)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(runtime_dirs.len(), 1);
    assert_eq!(
        fs::metadata(&runtime_dirs[0]).unwrap().permissions().mode() & 0o777,
        0o700
    );

    let mut stream = connect_with_timeout(&socket, Duration::from_secs(2)).unwrap();
    let invalid_id = RequestId::from_u128(2);
    write_payload(
        &mut stream,
        serde_json::to_string(&json!({
            "version": PROTOCOL_VERSION,
            "request_id": invalid_id,
            "method": "not_a_method",
            "private": REQUEST_SECRET,
        }))
        .unwrap()
        .as_bytes(),
    )
    .unwrap();
    let invalid: ResponseEnvelope =
        serde_json::from_value(read_value(&mut stream).unwrap()).unwrap();
    assert_eq!(invalid.request_id, invalid_id);
    assert!(matches!(
        invalid.payload,
        ResponsePayload::Failure(ref error) if error.code == ErrorCode::InvalidConfig
    ));

    let recovered_id = RequestId::from_u128(3);
    write_payload(
        &mut stream,
        &serde_json::to_vec(&RequestEnvelope::new(recovered_id, Request::Ping)).unwrap(),
    )
    .unwrap();
    let recovered: ResponseEnvelope =
        serde_json::from_value(read_value(&mut stream).unwrap()).unwrap();
    assert_eq!(recovered.request_id, recovered_id);
    assert!(matches!(recovered.payload, ResponsePayload::Success(_)));

    let mut malformed = connect_with_timeout(&socket, Duration::from_secs(2)).unwrap();
    write_payload(
        &mut malformed,
        format!("{{not-json-{REQUEST_SECRET}").as_bytes(),
    )
    .unwrap();
    let malformed: ResponseEnvelope =
        serde_json::from_value(read_value(&mut malformed).unwrap()).unwrap();
    assert_eq!(malformed.request_id, RequestId::nil());
    assert!(matches!(malformed.payload, ResponsePayload::Failure(_)));

    let mut oversized = connect_with_timeout(&socket, Duration::from_secs(2)).unwrap();
    oversized
        .write_all(&((MAX_NATIVE_FRAME_BYTES as u32) + 1).to_be_bytes())
        .unwrap();
    let oversized_response: ResponseEnvelope =
        serde_json::from_value(read_value(&mut oversized).unwrap()).unwrap();
    assert_eq!(oversized_response.request_id, RequestId::nil());
    let mut byte = [0_u8];
    assert_eq!(oversized.read(&mut byte).unwrap(), 0);

    let mut partial = connect_with_timeout(&socket, Duration::from_secs(12)).unwrap();
    partial.write_all(&[0_u8]).unwrap();
    let partial_started = Instant::now();
    assert_eq!(partial.read(&mut byte).unwrap(), 0);
    let partial_elapsed = partial_started.elapsed();
    assert!(partial_elapsed >= Duration::from_secs(9));
    assert!(partial_elapsed < Duration::from_secs(12));

    daemon.signal(libc::SIGTERM);
    let output = daemon.wait(PROCESS_TIMEOUT);
    assert!(output.status.success(), "{}", output.stderr_text());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty(), "{}", output.stderr_text());
    wait_for_process_exit(sidecar_pid);
    assert!(
        !socket.exists(),
        "public socket survived graceful signal shutdown"
    );
    assert!(fs::read_dir(&runtime_parent).unwrap().next().is_none());
    let log_text = fs::read_to_string(&log).unwrap();
    assert!(log_text.contains("startup"));
    assert!(log_text.contains("protocol v1 listening"));
    assert!(log_text.contains("pmuxd stopped"));
    assert!(!log_text.contains(REQUEST_SECRET));
    assert!(fs::metadata(&log).unwrap().len() <= 16 * 1024 * 1024);
}

#[test]
fn sigint_uses_the_same_bounded_sidecar_and_socket_cleanup_path() {
    let root = private_root("sigint");
    let socket = root.path().join("p.sock");
    let runtime_parent = root.path().join("runtime");
    fs::create_dir(&runtime_parent).unwrap();
    fs::set_permissions(&runtime_parent, fs::Permissions::from_mode(0o700)).unwrap();
    let mut command = base_serve_command(&socket, &runtime_parent);
    let mut daemon = Daemon::spawn(&mut command);
    wait_until_ready(&mut daemon, &socket);
    let sidecar_pid = wait_for_direct_child(daemon.id());

    daemon.signal(libc::SIGINT);
    let output = daemon.wait(PROCESS_TIMEOUT);
    assert!(output.status.success(), "{}", output.stderr_text());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty(), "{}", output.stderr_text());
    wait_for_process_exit(sidecar_pid);
    assert!(!socket.exists());
    assert!(fs::read_dir(&runtime_parent).unwrap().next().is_none());
}

#[test]
fn signal_shutdown_preserves_a_replacement_at_the_public_socket_path() {
    let root = private_root("socket-replaced");
    let socket = root.path().join("p.sock");
    let runtime_parent = root.path().join("runtime");
    fs::create_dir(&runtime_parent).unwrap();
    fs::set_permissions(&runtime_parent, fs::Permissions::from_mode(0o700)).unwrap();
    let stale = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    drop(stale);
    assert!(
        fs::symlink_metadata(&socket)
            .unwrap()
            .file_type()
            .is_socket(),
        "test precondition did not leave an owned stale socket"
    );
    let mut command = base_serve_command(&socket, &runtime_parent);
    let mut daemon = Daemon::spawn(&mut command);
    wait_until_ready(&mut daemon, &socket);
    let sidecar_pid = wait_for_direct_child(daemon.id());

    fs::remove_file(&socket).unwrap();
    let replacement = b"replacement-must-survive";
    fs::write(&socket, replacement).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();

    daemon.signal(libc::SIGTERM);
    let output = daemon.wait(PROCESS_TIMEOUT);
    assert!(output.status.success(), "{}", output.stderr_text());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty(), "{}", output.stderr_text());
    wait_for_process_exit(sidecar_pid);
    assert_eq!(
        fs::read(&socket).unwrap(),
        replacement,
        "pmuxd removed or changed a path whose socket identity was replaced"
    );
    assert!(fs::read_dir(&runtime_parent).unwrap().next().is_none());
}

fn run_bounded(command: &mut Command, timeout: Duration) -> ProcessOutput {
    Daemon::spawn(command).wait(timeout)
}

#[test]
fn startup_failures_are_bounded_preserve_existing_paths_and_redact_config_values() {
    let mut relative = Command::new(pmuxd_binary());
    relative.args(["serve", "--socket", "relative.sock"]);
    let output = run_bounded(&mut relative, Duration::from_secs(5));
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(output.stderr_text().contains("absolute"));

    let permissive = private_root("permissive");
    fs::set_permissions(permissive.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let socket = permissive.path().join("p.sock");
    let mut command = Command::new(pmuxd_binary());
    command.arg("serve").arg("--socket").arg(&socket);
    let output = run_bounded(&mut command, Duration::from_secs(5));
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(output.stderr_text().contains("owner-only"));
    assert!(!socket.exists());

    let occupied = private_root("occupied");
    let socket = occupied.path().join("p.sock");
    fs::write(&socket, b"preserve-this-file").unwrap();
    let mut command = Command::new(pmuxd_binary());
    command.arg("serve").arg("--socket").arg(&socket);
    let output = run_bounded(&mut command, Duration::from_secs(5));
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(fs::read(&socket).unwrap(), b"preserve-this-file");

    let invalid = private_root("profile");
    let runtime_parent = invalid.path().join("runtime");
    fs::create_dir(&runtime_parent).unwrap();
    fs::set_permissions(&runtime_parent, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = invalid.path().join("p.sock");
    let profile_secret = "pmux-profile-value-secret";
    let invalid_profile = json!({
        "claude_version": "9.9.9",
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "terminal_profile": "transparent",
        "input_transport": "sdk",
        "transcript_drain_ms": 25,
        "credential": profile_secret,
    })
    .to_string();
    let mut command = base_serve_command(&socket, &runtime_parent);
    command.arg("--tested-claude-profile").arg(&invalid_profile);
    let output = run_bounded(&mut command, Duration::from_secs(5));
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(!output.stderr_text().contains(profile_secret));
    assert!(!socket.exists(), "startup error left a public socket");
    assert!(fs::read_dir(&runtime_parent).unwrap().next().is_none());
    let log = fs::read_to_string(invalid.path().join("logs/pmuxd.log")).unwrap();
    assert!(!log.contains(profile_secret));

    let missing = private_root("companion");
    let runtime_parent = missing.path().join("runtime");
    fs::create_dir(&runtime_parent).unwrap();
    fs::set_permissions(&runtime_parent, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = missing.path().join("p.sock");
    let mut command = Command::new(pmuxd_binary());
    command
        .arg("serve")
        .arg("--socket")
        .arg(&socket)
        .arg("--rmuxd")
        .arg(missing.path().join("missing-rmuxd"))
        .arg("--launcher")
        .arg(packaged_companion("pmux-launcher"))
        .arg("--runtime-parent")
        .arg(&runtime_parent);
    let output = run_bounded(&mut command, Duration::from_secs(5));
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(
        output
            .stderr_text()
            .contains("pmux-rmuxd binary is unavailable")
    );
    assert!(!socket.exists());
    assert!(fs::read_dir(&runtime_parent).unwrap().next().is_none());
}
