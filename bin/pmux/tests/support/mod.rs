use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use pseudomux_protocol::v1::RequestEnvelope;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const SESSION_ID: &str = "00000000-0000-4000-8000-000000000022";
pub const GENERATION_ID: &str = "00000000-0000-4000-8000-000000000044";
pub const TURN_ID: &str = "00000000-0000-4000-8000-000000000033";
pub const PROCESS_TIMEOUT: Duration = Duration::from_secs(10);
pub const MAX_PROMPT_BYTES: usize = 1024 * 1024;

static NEXT_SANDBOX: AtomicU64 = AtomicU64::new(1);

pub struct Sandbox {
    pub root: PathBuf,
    pub socket: PathBuf,
}

impl Sandbox {
    pub fn new(label: &str) -> Self {
        let serial = NEXT_SANDBOX.fetch_add(1, Ordering::Relaxed);
        let root =
            PathBuf::from("/tmp").join(format!("pmux-cli-{}-{serial}-{label}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let socket = root.join("s");
        Self { root, socket }
    }

    pub fn bind(&self) -> UnixListener {
        let listener = UnixListener::bind(&self.socket).unwrap();
        fs::set_permissions(&self.socket, fs::Permissions::from_mode(0o600)).unwrap();
        listener
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub enum NativeReply {
    Success {
        kind: &'static str,
        data: Value,
    },
    Error {
        code: &'static str,
        message: &'static str,
        retryable: bool,
        details: Value,
    },
    MismatchedSuccess {
        kind: &'static str,
        data: Value,
    },
    Malformed(Vec<u8>),
}

pub fn success(kind: &'static str, data: Value) -> NativeReply {
    NativeReply::Success { kind, data }
}

pub fn spawn_native_server(
    listener: UnixListener,
    replies: Vec<NativeReply>,
) -> thread::JoinHandle<Vec<RequestEnvelope>> {
    thread::spawn(move || {
        let mut requests = Vec::with_capacity(replies.len());
        for reply in replies {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_native_request(&mut stream);
            let request_id = request.request_id;
            match reply {
                NativeReply::Success { kind, data } => write_native_value(
                    &mut stream,
                    &json!({
                        "version": 1,
                        "request_id": request_id,
                        "result": {"type": kind, "data": data},
                    }),
                ),
                NativeReply::Error {
                    code,
                    message,
                    retryable,
                    details,
                } => write_native_value(
                    &mut stream,
                    &json!({
                        "version": 1,
                        "request_id": request_id,
                        "error": {
                            "code": code,
                            "message": message,
                            "retryable": retryable,
                            "details": details,
                        },
                    }),
                ),
                NativeReply::MismatchedSuccess { kind, data } => write_native_value(
                    &mut stream,
                    &json!({
                        "version": 1,
                        "request_id": uuid::Uuid::nil(),
                        "result": {"type": kind, "data": data},
                    }),
                ),
                NativeReply::Malformed(payload) => write_native_payload(&mut stream, &payload),
            }
            requests.push(request);
        }
        requests
    })
}

pub fn read_native_request(stream: &mut UnixStream) -> RequestEnvelope {
    let mut header = [0_u8; 4];
    stream.read_exact(&mut header).unwrap();
    let mut payload = vec![0; u32::from_be_bytes(header) as usize];
    stream.read_exact(&mut payload).unwrap();
    serde_json::from_slice(&payload).unwrap()
}

pub fn write_native_value(stream: &mut UnixStream, value: &Value) {
    write_native_payload(stream, &serde_json::to_vec(value).unwrap());
}

fn write_native_payload(stream: &mut UnixStream, payload: &[u8]) {
    stream
        .write_all(&u32::try_from(payload.len()).unwrap().to_be_bytes())
        .unwrap();
    stream.write_all(payload).unwrap();
    stream.flush().unwrap();
}

pub struct ProcessOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl ProcessOutput {
    pub fn stdout_text(&self) -> String {
        String::from_utf8(self.stdout.clone()).unwrap()
    }

    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

struct PmuxCandidate {
    directory: PathBuf,
    directory_device: u64,
    directory_inode: u64,
    path: PathBuf,
    device: u64,
    inode: u64,
    length: u64,
    mode: u32,
    digest: [u8; 32],
}

impl PmuxCandidate {
    fn discover(exact_directory: Option<&Path>, cargo_candidate: &Path) -> Result<Self, String> {
        let (directory, path) = if let Some(directory) = exact_directory {
            if !directory.is_absolute() {
                return Err("PMUX_TEST_BIN_DIR must be absolute".to_owned());
            }
            let canonical_directory = directory.canonicalize().map_err(|error| {
                format!(
                    "PMUX_TEST_BIN_DIR cannot be canonicalized ({}): {error}",
                    directory.display()
                )
            })?;
            if canonical_directory != directory {
                return Err(format!(
                    "PMUX_TEST_BIN_DIR must name its canonical exact directory: {} != {}",
                    directory.display(),
                    canonical_directory.display()
                ));
            }
            let unresolved = canonical_directory.join("pmux");
            let candidate = unresolved.canonicalize().map_err(|error| {
                format!(
                    "required exact pmux candidate is unavailable ({}): {error}",
                    unresolved.display()
                )
            })?;
            if candidate.parent() != Some(canonical_directory.as_path()) {
                return Err(format!(
                    "pmux candidate escaped exact binary directory: {}",
                    candidate.display()
                ));
            }
            if candidate != unresolved {
                return Err(format!(
                    "pmux candidate must be a direct canonical file, not an alias: {}",
                    unresolved.display()
                ));
            }
            (canonical_directory, candidate)
        } else {
            if !cargo_candidate.is_absolute() {
                return Err(format!(
                    "Cargo pmux test candidate must be absolute: {}",
                    cargo_candidate.display()
                ));
            }
            let candidate = cargo_candidate.canonicalize().map_err(|error| {
                format!(
                    "Cargo pmux test candidate is unavailable ({}): {error}",
                    cargo_candidate.display()
                )
            })?;
            let directory = candidate
                .parent()
                .ok_or_else(|| "Cargo pmux test candidate has no parent directory".to_owned())?
                .to_path_buf();
            (directory, candidate)
        };

        let directory_metadata = fs::metadata(&directory).map_err(|error| {
            format!(
                "failed to inspect pmux candidate directory {}: {error}",
                directory.display()
            )
        })?;
        if !directory_metadata.is_dir() {
            return Err(format!(
                "pmux candidate root is not a directory: {}",
                directory.display()
            ));
        }
        let link_metadata = fs::symlink_metadata(&path).map_err(|error| {
            format!(
                "failed to inspect pmux candidate {}: {error}",
                path.display()
            )
        })?;
        if !link_metadata.file_type().is_file() {
            return Err(format!(
                "pmux candidate is not a direct regular file: {}",
                path.display()
            ));
        }
        let metadata = fs::metadata(&path).map_err(|error| {
            format!(
                "failed to inspect pmux candidate {}: {error}",
                path.display()
            )
        })?;
        let mode = metadata.permissions().mode();
        if mode & 0o111 == 0 {
            return Err(format!(
                "pmux candidate is not executable: {}",
                path.display()
            ));
        }
        let digest = digest_file(&path)?;

        Ok(Self {
            directory,
            directory_device: directory_metadata.dev(),
            directory_inode: directory_metadata.ino(),
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            mode,
            digest,
        })
    }

    fn assert_unchanged(&self) -> Result<(), String> {
        let directory_metadata = fs::metadata(&self.directory).map_err(|error| {
            format!(
                "pmux candidate directory disappeared ({}): {error}",
                self.directory.display()
            )
        })?;
        if !directory_metadata.is_dir()
            || directory_metadata.dev() != self.directory_device
            || directory_metadata.ino() != self.directory_inode
        {
            return Err("pmux candidate directory changed filesystem identity".to_owned());
        }
        let canonical = self.path.canonicalize().map_err(|error| {
            format!(
                "pmux candidate disappeared ({}): {error}",
                self.path.display()
            )
        })?;
        if canonical != self.path || canonical.parent() != Some(self.directory.as_path()) {
            return Err("pmux candidate changed canonical directory identity".to_owned());
        }
        let link_metadata = fs::symlink_metadata(&self.path).map_err(|error| {
            format!(
                "pmux candidate disappeared ({}): {error}",
                self.path.display()
            )
        })?;
        let metadata = fs::metadata(&self.path).map_err(|error| {
            format!(
                "pmux candidate disappeared ({}): {error}",
                self.path.display()
            )
        })?;
        if !link_metadata.file_type().is_file()
            || metadata.dev() != self.device
            || metadata.ino() != self.inode
            || metadata.len() != self.length
            || metadata.permissions().mode() != self.mode
            || self.mode & 0o111 == 0
            || digest_file(&self.path)? != self.digest
        {
            return Err("pmux candidate changed regular executable identity".to_owned());
        }
        Ok(())
    }
}

fn digest_file(path: &Path) -> Result<[u8; 32], String> {
    let mut file = fs::File::open(path).map_err(|error| {
        format!(
            "failed to open pmux candidate for identity hashing ({}): {error}",
            path.display()
        )
    })?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            format!(
                "failed to hash pmux candidate identity ({}): {error}",
                path.display()
            )
        })?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(digest.finalize().into())
}

static PMUX_CANDIDATE: OnceLock<PmuxCandidate> = OnceLock::new();

fn candidate() -> &'static PmuxCandidate {
    PMUX_CANDIDATE.get_or_init(|| {
        let exact_directory = std::env::var_os("PMUX_TEST_BIN_DIR").map(PathBuf::from);
        PmuxCandidate::discover(
            exact_directory.as_deref(),
            Path::new(env!("CARGO_BIN_EXE_pmux")),
        )
        .unwrap_or_else(|error| panic!("failed to bind pmux test candidate: {error}"))
    })
}

pub fn pmux_process() -> Command {
    let candidate = candidate();
    assert_pmux_candidate_unchanged();
    let mut command = Command::new(&candidate.path);
    command.env_remove("PMUX_TEST_BIN_DIR");
    command
}

#[allow(
    dead_code,
    reason = "shared integration-test support is compiled independently per test target"
)]
pub fn assert_pmux_candidate_unchanged() {
    candidate()
        .assert_unchanged()
        .unwrap_or_else(|error| panic!("pmux test candidate is no longer exact: {error}"));
}

#[doc(hidden)]
#[allow(
    dead_code,
    reason = "shared integration-test support is compiled independently per test target"
)]
pub fn resolve_pmux_candidate_for_test(
    exact_directory: Option<&Path>,
    cargo_candidate: &Path,
) -> Result<PathBuf, String> {
    PmuxCandidate::discover(exact_directory, cargo_candidate).map(|candidate| candidate.path)
}

#[doc(hidden)]
#[allow(
    dead_code,
    reason = "shared integration-test support is compiled independently per test target"
)]
pub fn bind_run_and_revalidate_pmux_candidate_for_test<F>(
    exact_directory: Option<&Path>,
    cargo_candidate: &Path,
    operation: F,
) -> Result<(), String>
where
    F: FnOnce(&Path),
{
    let candidate = PmuxCandidate::discover(exact_directory, cargo_candidate)?;
    operation(&candidate.path);
    candidate.assert_unchanged()
}

pub fn command(socket: &Path, root: &Path) -> Command {
    let mut command = pmux_process();
    command
        .env_clear()
        .env("HOME", root)
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C")
        .env("PMUX_TEST_ENV", "captured")
        .env("PMUX_TEST_SECRET", "environment-secret")
        .arg("--socket")
        .arg(socket);
    command
}

pub fn run(mut command: Command, stdin: Option<&[u8]>) -> ProcessOutput {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    if let Some(bytes) = stdin {
        let mut pipe = child.stdin.take().unwrap();
        pipe.write_all(bytes).unwrap();
    } else {
        drop(child.stdin.take());
    }

    collect_child(child)
}

pub fn wait_for_status(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            let exact_pid = child.id();
            let _ = child.kill();
            let _ = child.wait();
            panic!("pmux process {exact_pid} did not exit within {PROCESS_TIMEOUT:?}");
        }
        thread::sleep(Duration::from_millis(5));
    }
}

pub fn collect_child(mut child: Child) -> ProcessOutput {
    let status = wait_for_status(&mut child);

    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut stdout)
        .unwrap();
    let mut stderr = Vec::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_end(&mut stderr)
        .unwrap();
    assert_pmux_candidate_unchanged();
    ProcessOutput {
        status,
        stdout,
        stderr,
    }
}

pub fn compatibility() -> Value {
    json!({
        "claude_version": "9.9.9",
        "os": "test",
        "arch": "test",
        "terminal_profile": "transparent",
        "input_transport": "sdk",
        "tested": true,
        "transcript_drain_ms": 25,
    })
}

pub fn session_handle() -> Value {
    json!({
        "session_id": SESSION_ID,
        "generation_id": GENERATION_ID,
        "state": "ready",
        "compatibility": compatibility(),
        "created_at_ms": 1,
        "last_sequence": 0,
    })
}

pub fn snapshot(last_sequence: u64) -> Value {
    json!({
        "session_id": SESSION_ID,
        "generation_id": GENERATION_ID,
        "transcript_session_id": SESSION_ID,
        "cell": "full",
        "state": "ready",
        "cwd": "/work/project",
        "claude_version": "9.9.9",
        "compatibility": compatibility(),
        "created_at_ms": 1,
        "updated_at_ms": 2,
        "resumable": true,
        "last_sequence": last_sequence,
    })
}

pub fn turn_accepted(replayed: bool, next_sequence: u64) -> Value {
    json!({
        "session_id": SESSION_ID,
        "generation_id": GENERATION_ID,
        "turn_id": TURN_ID,
        "replayed": replayed,
        "state": "running",
        "next_sequence": next_sequence,
    })
}

pub fn turn_result(outcome: &str, text: &str, final_sequence: u64) -> Value {
    json!({
        "session_id": SESSION_ID,
        "generation_id": GENERATION_ID,
        "turn_id": TURN_ID,
        "outcome": outcome,
        "text": text,
        "final_blocks": [{"kind": "text", "text": text}],
        "model": "claude-test",
        "stop_reason": {"kind": "end_turn"},
        "usage": {
            "main": {
                "input_tokens": 0,
                "output_tokens": 0,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0
            },
            "sidechain": {
                "input_tokens": 0,
                "output_tokens": 0,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0
            },
            "combined": {
                "input_tokens": 0,
                "output_tokens": 0,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0
            }
        },
        "timings": {"submitted_at_ms": 1, "completed_at_ms": 2},
        "claude_version": "9.9.9",
        "compatibility": compatibility(),
        "completion": {
            "authority": "transcript",
            "prompt_acknowledged": true,
            "terminal_message_observed": true,
            "terminal_prompt_observed": true,
            "terminal_quiet_observed": true,
            "transcript_drained": true,
            "lifecycle_hook_observed": false,
        },
        "final_sequence": final_sequence,
    })
}

pub fn warning_event(sequence: u64) -> Value {
    event(
        sequence,
        Some(TURN_ID),
        json!({
            "type": "warning",
            "data": {"code": "observation", "message": "turn is still running"},
        }),
    )
}

pub fn completed_event(sequence: u64, outcome: &str, text: &str) -> Value {
    event(
        sequence,
        Some(TURN_ID),
        json!({
            "type": "turn_completed",
            "data": turn_result(outcome, text, sequence),
        }),
    )
}

pub fn failed_event(sequence: u64, message: &str, details: Value) -> Value {
    event(
        sequence,
        Some(TURN_ID),
        json!({
            "type": "turn_failed",
            "data": {
                "code": "schema_drift",
                "message": message,
                "retryable": false,
                "details": details,
            },
        }),
    )
}

fn event(sequence: u64, turn_id: Option<&str>, payload: Value) -> Value {
    json!({
        "schema_version": 1,
        "session_id": SESSION_ID,
        "generation_id": GENERATION_ID,
        "turn_id": turn_id,
        "sequence": sequence,
        "timestamp_ms": sequence,
        "event": payload,
    })
}

pub fn event_batch(events: Vec<Value>, next_sequence: u64) -> Value {
    json!({"events": events, "next_sequence": next_sequence})
}

pub fn replay_gap_batch(requested_after: u64, snapshot_sequence: u64) -> Value {
    json!({
        "next_sequence": snapshot_sequence + 1,
        "replay_gap": {
            "requested_after": requested_after,
            "oldest_available": snapshot_sequence,
            "next_sequence": snapshot_sequence + 1,
            "snapshot": snapshot(snapshot_sequence),
        },
    })
}

pub fn close_result(process_reaped: bool) -> Value {
    json!({
        "session_id": SESSION_ID,
        "generation_id": GENERATION_ID,
        "already_closed": false,
        "process_reaped": process_reaped,
    })
}

pub fn json_lines(output: &[u8]) -> Vec<Value> {
    String::from_utf8(output.to_vec())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
