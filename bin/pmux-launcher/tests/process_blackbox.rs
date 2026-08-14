#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use pseudomux_rmux::{
    EnvironmentSnapshot, LAUNCHER_PROTOCOL_VERSION, LaunchSpec, LauncherRequest, LauncherResponse,
    MAX_LAUNCHER_FRAME_BYTES,
};

#[path = "../../../tests/support/candidate_binary.rs"]
mod candidate_binary;
use candidate_binary::CandidateBinaries;

const PROCESS_TIMEOUT: Duration = Duration::from_secs(13);
const TOKEN: &str = "01020304-0506-4708-890a-0b0c0d0e0f10";
const TOKEN_COMPACT: &str = "0102030405064708890a0b0c0d0e0f10";
/// Separates a refusal that never reached the broker from one that did: the
/// shipped read deadline is ten seconds (`bin/pmux-launcher/src/main.rs:48`),
/// so anything that got as far as a silent broker cannot land under this.
/// Measured on macOS/aarch64, the three refusals below take 4-5 ms.
const PRE_BROKER_REFUSAL_BOUND: Duration = Duration::from_secs(2);

struct Output {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    /// The moment the child had exited and its pipes were drained, taken
    /// *before* the candidate is re-hashed. `CandidateBinaries::path` and
    /// `assert_candidate_unchanged` each sha256 the whole binary — 174 ms
    /// apiece for the 4.3 MB debug build on macOS/aarch64, which is 40x the
    /// launcher's own refusal — so a stopwatch that spans them is reading this
    /// harness, not the launcher. Callers that assert on wall-clock read this
    /// instead of calling `Instant::elapsed` after `wait` returns.
    completed: Instant,
}

impl Output {
    fn stdout_text(&self) -> String {
        String::from_utf8(self.stdout.clone()).expect("launcher stdout must be UTF-8")
    }

    fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self(Some(child))
    }

    fn id(&self) -> u32 {
        self.0.as_ref().expect("child is present").id()
    }

    fn wait(mut self, timeout: Duration) -> Output {
        let mut child = self.0.take().expect("child is present");
        let deadline = Instant::now() + timeout;
        let status = loop {
            if let Some(status) = child.try_wait().expect("launcher wait failed") {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("pmux-launcher exceeded its {timeout:?} process bound");
            }
            thread::sleep(Duration::from_millis(5));
        };
        let mut stdout = Vec::new();
        child
            .stdout
            .take()
            .expect("captured stdout")
            .read_to_end(&mut stdout)
            .unwrap();
        let mut stderr = Vec::new();
        child
            .stderr
            .take()
            .expect("captured stderr")
            .read_to_end(&mut stderr)
            .unwrap();
        let completed = Instant::now();
        assert_candidate_unchanged();
        Output {
            status,
            stdout,
            stderr,
            completed,
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn candidates() -> &'static CandidateBinaries {
    static CANDIDATES: OnceLock<CandidateBinaries> = OnceLock::new();
    CANDIDATES.get_or_init(|| {
        CandidateBinaries::discover(
            std::env::var_os("PMUX_TEST_BIN_DIR").map(PathBuf::from),
            [(
                "pmux-launcher".to_owned(),
                PathBuf::from(env!("CARGO_BIN_EXE_pmux-launcher")),
            )],
        )
        .unwrap_or_else(|error| panic!("failed to bind pmux-launcher candidate: {error}"))
    })
}

fn launcher_binary() -> PathBuf {
    candidates().path("pmux-launcher").to_path_buf()
}

fn assert_candidate_unchanged() {
    candidates()
        .assert_unchanged()
        .unwrap_or_else(|error| panic!("pmux-launcher candidate changed during test: {error}"));
}

fn spawn_launcher(socket: &Path) -> ChildGuard {
    spawn_launcher_at(&launcher_binary(), socket)
}

/// `spawn_launcher` with the candidate resolved by the caller, so that a caller
/// timing the child does not also time `launcher_binary`'s sha256 of it.
fn spawn_launcher_at(binary: &Path, socket: &Path) -> ChildGuard {
    let child = Command::new(binary)
        .env_clear()
        .env("PMUX_AMBIENT_LEAK", "ambient-launch-secret")
        .arg("--socket")
        .arg(socket)
        .arg("--token")
        .arg(TOKEN)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn pmux-launcher");
    ChildGuard::new(child)
}

fn private_socket(label: &str) -> (tempfile::TempDir, PathBuf, UnixListener) {
    let root = tempfile::Builder::new()
        .prefix(&format!("pl-{label}-"))
        .tempdir_in("/tmp")
        .unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let socket = root.path().join("b.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    (root, socket, listener)
}

fn read_request(stream: &mut UnixStream) -> LauncherRequest {
    let mut header = [0_u8; 4];
    stream.read_exact(&mut header).unwrap();
    let length = u32::from_be_bytes(header) as usize;
    assert!(length <= MAX_LAUNCHER_FRAME_BYTES);
    let mut payload = vec![0; length];
    stream.read_exact(&mut payload).unwrap();
    serde_json::from_slice(&payload).unwrap()
}

fn write_payload(stream: &mut UnixStream, payload: &[u8]) {
    stream
        .write_all(&u32::try_from(payload.len()).unwrap().to_be_bytes())
        .unwrap();
    stream.write_all(payload).unwrap();
    stream.flush().unwrap();
}

fn write_response(stream: &mut UnixStream, response: &LauncherResponse) {
    write_payload(stream, &serde_json::to_vec(response).unwrap());
}

fn assert_exact_request(request: &LauncherRequest) {
    assert_eq!(request.version, LAUNCHER_PROTOCOL_VERSION);
    assert_eq!(request.token.expose(), TOKEN_COMPACT);
}

#[test]
fn exact_broker_request_exec_replaces_pid_and_applies_cwd_argv_and_environment() {
    let (root, socket, listener) = private_socket("exec");
    let cwd = root.path().join("cwd");
    fs::create_dir(&cwd).unwrap();
    let script = root.path().join("attest.sh");
    fs::write(
        &script,
        r#"printf 'pid=%s\n' "$$"
printf 'cwd=%s\n' "$PWD"
printf 'arg0=%s\n' "$0"
printf 'arg1=%s\n' "$1"
printf 'arg2=%s\n' "$2"
printf 'selected=%s\n' "$PMUX_SELECTED"
printf 'ambient=%s\n' "${PMUX_AMBIENT_LEAK-unset}"
"#,
    )
    .unwrap();
    let expected_script = script.to_string_lossy().into_owned();
    let server_script = script.clone();
    let server_cwd = cwd.clone();
    let broker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        assert_exact_request(&read_request(&mut stream));
        write_response(
            &mut stream,
            &LauncherResponse::Ready {
                version: LAUNCHER_PROTOCOL_VERSION,
                spec: LaunchSpec {
                    executable: PathBuf::from("/bin/sh"),
                    args: vec![
                        server_script.to_string_lossy().into_owned(),
                        "alpha".into(),
                        "two words".into(),
                    ],
                    cwd: server_cwd,
                    environment: EnvironmentSnapshot {
                        variables: BTreeMap::from([
                            ("PATH".into(), "/usr/bin:/bin".into()),
                            ("PMUX_SELECTED".into(), "exact-value".into()),
                        ]),
                    },
                },
            },
        );
    });

    let launcher = spawn_launcher(&socket);
    let launcher_pid = launcher.id();
    let output = launcher.wait(PROCESS_TIMEOUT);
    broker.join().unwrap();

    assert!(output.status.success(), "{}", output.stderr_text());
    assert!(output.stderr.is_empty(), "{}", output.stderr_text());
    let fields = output
        .stdout_text()
        .lines()
        .map(|line| line.split_once('=').expect("attestation is key=value"))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect::<BTreeMap<_, _>>();
    let expected_pid = launcher_pid.to_string();
    let canonical_cwd = fs::canonicalize(&cwd).unwrap();
    let expected_cwd = canonical_cwd.to_string_lossy();
    assert_eq!(
        fields.get("pid").map(String::as_str),
        Some(expected_pid.as_str())
    );
    assert_eq!(
        fields.get("cwd").map(String::as_str),
        Some(expected_cwd.as_ref())
    );
    assert_eq!(
        fields.get("arg0").map(String::as_str),
        Some(expected_script.as_str())
    );
    assert_eq!(fields.get("arg1").map(String::as_str), Some("alpha"));
    assert_eq!(fields.get("arg2").map(String::as_str), Some("two words"));
    assert_eq!(
        fields.get("selected").map(String::as_str),
        Some("exact-value")
    );
    assert_eq!(fields.get("ambient").map(String::as_str), Some("unset"));
}

#[test]
fn rejected_invalid_and_oversized_broker_responses_fail_closed_without_secrets() {
    enum Reply {
        Rejected,
        InvalidSpec,
        MissingExecutable,
        WrongVersion,
        Malformed,
        Oversized,
    }

    for (label, reply, expected) in [
        ("rejected", Reply::Rejected, "rejected request"),
        ("invalid", Reply::InvalidSpec, "executable must be absolute"),
        ("exec", Reply::MissingExecutable, "failed to exec"),
        ("version", Reply::WrongVersion, "unsupported version"),
        (
            "malformed",
            Reply::Malformed,
            "invalid launch broker response",
        ),
        ("oversized", Reply::Oversized, "maximum frame size"),
    ] {
        let (_root, socket, listener) = private_socket(label);
        let broker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            assert_exact_request(&read_request(&mut stream));
            match reply {
                Reply::Rejected => write_response(
                    &mut stream,
                    &LauncherResponse::Rejected {
                        version: LAUNCHER_PROTOCOL_VERSION,
                        code: "token_expired".into(),
                    },
                ),
                Reply::InvalidSpec => write_response(
                    &mut stream,
                    &LauncherResponse::Ready {
                        version: LAUNCHER_PROTOCOL_VERSION,
                        spec: LaunchSpec {
                            executable: PathBuf::from("relative-claude"),
                            args: vec!["argument-launch-secret".into()],
                            cwd: PathBuf::from("/tmp"),
                            environment: EnvironmentSnapshot {
                                variables: BTreeMap::from([(
                                    "PMUX_SECRET".into(),
                                    "environment-launch-secret".into(),
                                )]),
                            },
                        },
                    },
                ),
                Reply::MissingExecutable => write_response(
                    &mut stream,
                    &LauncherResponse::Ready {
                        version: LAUNCHER_PROTOCOL_VERSION,
                        spec: LaunchSpec {
                            executable: PathBuf::from("/definitely/missing/claude"),
                            args: vec!["argument-launch-secret".into()],
                            cwd: PathBuf::from("/tmp"),
                            environment: EnvironmentSnapshot {
                                variables: BTreeMap::from([(
                                    "PMUX_SECRET".into(),
                                    "environment-launch-secret".into(),
                                )]),
                            },
                        },
                    },
                ),
                Reply::WrongVersion => write_response(
                    &mut stream,
                    &LauncherResponse::Ready {
                        version: LAUNCHER_PROTOCOL_VERSION + 1,
                        spec: LaunchSpec {
                            executable: PathBuf::from("/bin/sh"),
                            args: vec!["argument-launch-secret".into()],
                            cwd: PathBuf::from("/tmp"),
                            environment: EnvironmentSnapshot {
                                variables: BTreeMap::from([(
                                    "PMUX_SECRET".into(),
                                    "environment-launch-secret".into(),
                                )]),
                            },
                        },
                    },
                ),
                Reply::Malformed => write_payload(&mut stream, b"{malformed-launch-secret"),
                Reply::Oversized => stream
                    .write_all(&((MAX_LAUNCHER_FRAME_BYTES as u32) + 1).to_be_bytes())
                    .unwrap(),
            }
        });
        let output = spawn_launcher(&socket).wait(PROCESS_TIMEOUT);
        broker.join().unwrap();
        let stderr = output.stderr_text();
        assert!(!output.status.success(), "{label} unexpectedly succeeded");
        assert!(output.stdout.is_empty(), "{label}: unexpected stdout");
        assert!(
            stderr.to_ascii_lowercase().contains(expected),
            "{label}: {stderr}"
        );
        for secret in [
            TOKEN,
            TOKEN_COMPACT,
            "argument-launch-secret",
            "environment-launch-secret",
            "malformed-launch-secret",
            "ambient-launch-secret",
        ] {
            assert!(
                !stderr.contains(secret),
                "{label} exposed {secret:?}: {stderr}"
            );
        }
    }
}

/// Runs the launcher once and returns how long *the launcher* took.
///
/// The candidate arrives already resolved and nothing in here touches
/// `CandidateBinaries`, so there is no harness sha256 for the clock to span;
/// callers verify the candidate on either side of this call instead.
fn timed_refusal(binary: &Path, args: &[&str]) -> (std::process::Output, Duration) {
    let started = Instant::now();
    let output = Command::new(binary).args(args).output().unwrap();
    let elapsed = started.elapsed();
    (output, elapsed)
}

#[test]
fn socket_and_token_validation_fail_before_broker_use_and_are_bounded() {
    // A broker that accepts and answers nothing. The token case points at it
    // because that is the one case here whose socket a launcher validating in
    // the wrong order could actually reach: a launcher that connected before
    // parsing the token would sit on its ten-second read deadline and show up
    // both in the bound below and in the accept check after the loop. The other
    // two name a relative and an absent path, which no broker can listen on.
    let (_root, silent_socket, listener) = private_socket("prevalidation");
    listener.set_nonblocking(true).unwrap();
    let silent = silent_socket.to_string_lossy().into_owned();
    let binary = launcher_binary();

    for args in [
        vec!["--socket", "relative.sock", "--token", TOKEN],
        vec![
            "--socket",
            "/definitely/missing/pmux.sock",
            "--token",
            TOKEN,
        ],
        vec!["--socket", silent.as_str(), "--token", "not-a-token"],
    ] {
        let (output, elapsed) = timed_refusal(&binary, &args);
        assert_candidate_unchanged();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(
            elapsed < PRE_BROKER_REFUSAL_BOUND,
            "{args:?} refused in {elapsed:?}, over the {PRE_BROKER_REFUSAL_BOUND:?} bound"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains(TOKEN));
        assert!(!stderr.contains(TOKEN_COMPACT));
    }

    match listener.accept() {
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
        other => panic!("a refusal reached the broker socket: {other:?}"),
    }
}

#[test]
fn stalled_broker_read_uses_the_shipped_ten_second_deadline_and_redacts_token() {
    let (_root, socket, listener) = private_socket("timeout");
    let broker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        assert_exact_request(&read_request(&mut stream));
        thread::sleep(Duration::from_secs(11));
    });
    let binary = launcher_binary();
    let started = Instant::now();
    let output = spawn_launcher_at(&binary, &socket).wait(PROCESS_TIMEOUT);
    let elapsed = output.completed.duration_since(started);
    broker.join().unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(elapsed >= Duration::from_secs(9));
    assert!(elapsed < PROCESS_TIMEOUT);
    let stderr = output.stderr_text();
    assert!(!stderr.contains(TOKEN));
    assert!(!stderr.contains(TOKEN_COMPACT));
}
