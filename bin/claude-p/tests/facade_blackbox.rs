#![cfg(unix)]

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use pseudomux_protocol::v1::{
    AuthPolicy, CompatibilityPolicy, EffortLevel, LifecycleMode, Request, RequestEnvelope,
    RetentionPolicy, RunOnceRequest, SessionIdentity, TurnOutcome,
};
use serde_json::{Value, json};

#[path = "../../../tests/support/candidate_binary.rs"]
mod candidate_binary;
use candidate_binary::CandidateBinaries;

const SESSION_ID: &str = "00000000-0000-4000-8000-000000000022";
const GENERATION_ID: &str = "00000000-0000-4000-8000-000000000044";
const PROCESS_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PROMPT_BYTES: usize = 1024 * 1024;

static NEXT_SANDBOX: AtomicU64 = AtomicU64::new(1);

struct Sandbox {
    root: PathBuf,
    socket: PathBuf,
}

impl Sandbox {
    fn new(label: &str) -> Self {
        let serial = NEXT_SANDBOX.fetch_add(1, Ordering::Relaxed);
        let root =
            PathBuf::from("/tmp").join(format!("clp-{}-{serial}-{label}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let socket = root.join("s");
        Self { root, socket }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn candidates() -> &'static CandidateBinaries {
    static CANDIDATES: OnceLock<CandidateBinaries> = OnceLock::new();
    CANDIDATES.get_or_init(|| {
        CandidateBinaries::discover(
            std::env::var_os("PMUX_TEST_BIN_DIR").map(PathBuf::from),
            [(
                "claude-p".to_owned(),
                PathBuf::from(env!("CARGO_BIN_EXE_claude-p")),
            )],
        )
        .unwrap_or_else(|error| panic!("failed to bind claude-p candidate: {error}"))
    })
}

fn claude_p_binary() -> PathBuf {
    candidates().path("claude-p").to_path_buf()
}

fn assert_candidate_unchanged() {
    candidates()
        .assert_unchanged()
        .unwrap_or_else(|error| panic!("claude-p candidate changed during test: {error}"));
}

enum NativeReply {
    Turn {
        outcome: &'static str,
        text: &'static str,
    },
    Error,
    Malformed,
    WrongResult,
}

fn spawn_native_server(
    listener: UnixListener,
    replies: Vec<NativeReply>,
) -> thread::JoinHandle<Vec<RequestEnvelope>> {
    thread::spawn(move || {
        let mut captured = Vec::with_capacity(replies.len());
        for reply in replies {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_native_request(&mut stream);
            let request_id = request.request_id;
            match reply {
                NativeReply::Turn { outcome, text } => write_native_value(
                    &mut stream,
                    &json!({
                        "version": 1,
                        "request_id": request_id,
                        "result": {
                            "type": "turn_result",
                            "data": turn_result(&request, outcome, text),
                        },
                    }),
                ),
                NativeReply::Error => write_native_value(
                    &mut stream,
                    &json!({
                        "version": 1,
                        "request_id": request_id,
                        "error": {
                            "code": "schema_drift",
                            "message": "daemon rejected the reconstructed transcript",
                            "retryable": false,
                            "details": {
                                "field": "message.content",
                                "private": "daemon-error-details-secret",
                            },
                        },
                    }),
                ),
                NativeReply::Malformed => write_native_payload(&mut stream, b"{not-json"),
                NativeReply::WrongResult => write_native_value(
                    &mut stream,
                    &json!({
                        "version": 1,
                        "request_id": request_id,
                        "result": {
                            "type": "pong",
                            "data": {"server_version": "test", "protocol_version": 1},
                        },
                    }),
                ),
            }
            captured.push(request);
        }
        captured
    })
}

fn read_native_request(stream: &mut UnixStream) -> RequestEnvelope {
    let mut header = [0_u8; 4];
    stream.read_exact(&mut header).unwrap();
    let mut payload = vec![0; u32::from_be_bytes(header) as usize];
    stream.read_exact(&mut payload).unwrap();
    serde_json::from_slice(&payload).unwrap()
}

fn write_native_value(stream: &mut UnixStream, value: &Value) {
    write_native_payload(stream, &serde_json::to_vec(value).unwrap());
}

fn write_native_payload(stream: &mut UnixStream, payload: &[u8]) {
    stream
        .write_all(&u32::try_from(payload.len()).unwrap().to_be_bytes())
        .unwrap();
    stream.write_all(payload).unwrap();
    stream.flush().unwrap();
}

fn turn_result(request: &RequestEnvelope, outcome: &str, text: &str) -> Value {
    let Request::RunOnce(run_once) = &request.request else {
        panic!("turn-result fixture received a non-run_once request")
    };
    let session_id = match &run_once.session.identity {
        SessionIdentity::New { session_id } => session_id
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| SESSION_ID.to_owned()),
        SessionIdentity::Resume { session_id } => session_id.to_string(),
    };
    json!({
        "session_id": session_id,
        "generation_id": GENERATION_ID,
        "turn_id": run_once.turn.turn_id,
        "outcome": outcome,
        "text": text,
        "final_blocks": [{"kind": "text", "text": text}],
        "model": "claude-test",
        "stop_reason": {"kind": "end_turn", "raw": "end_turn"},
        "usage": {
            "main": {
                "input_tokens": 11,
                "output_tokens": 7,
                "cache_creation_input_tokens": 2,
                "cache_read_input_tokens": 3,
            },
            "sidechain": {
                "input_tokens": 0,
                "output_tokens": 0,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
            },
            "combined": {
                "input_tokens": 11,
                "output_tokens": 7,
                "cache_creation_input_tokens": 2,
                "cache_read_input_tokens": 3,
            },
        },
        "timings": {"submitted_at_ms": 1, "completed_at_ms": 2},
        "claude_version": "9.9.9",
        "compatibility": {
            "claude_version": "9.9.9",
            "os": "test",
            "arch": "test",
            "terminal_profile": "transparent",
            "input_transport": "sdk",
            "tested": true,
            "transcript_drain_ms": 25,
        },
        "completion": {
            "authority": "transcript",
            "prompt_acknowledged": true,
            "terminal_message_observed": true,
            "terminal_prompt_observed": true,
            "terminal_quiet_observed": true,
            "transcript_drained": true,
            "lifecycle_hook_observed": false,
        },
        "final_sequence": 3,
    })
}

struct ProcessOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl ProcessOutput {
    fn stdout_text(&self) -> String {
        String::from_utf8(self.stdout.clone()).unwrap()
    }

    fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

fn base_command(socket: &Path, cwd: &Path) -> Command {
    let mut command = Command::new(claude_p_binary());
    command
        .env_clear()
        .env("HOME", cwd)
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C")
        .env("PMUX_TEST_ENV", "captured")
        .arg("--socket")
        .arg(socket)
        .arg("--claude-bin")
        .arg("/bin/sh")
        .arg("--cwd")
        .arg(cwd);
    command
}

fn env_socket_command(socket: &Path, cwd: &Path) -> Command {
    let mut command = Command::new(claude_p_binary());
    command
        .env_clear()
        .env("HOME", cwd)
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C")
        .env("PMUX_TEST_ENV", "captured")
        .env("PSEUDOMUX_SOCKET", socket)
        .arg("--claude-bin")
        .arg("/bin/sh")
        .arg("--cwd")
        .arg(cwd);
    command
}

fn run(mut command: Command, stdin: Option<&[u8]>) -> ProcessOutput {
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

    let deadline = Instant::now() + PROCESS_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("claude-p did not exit within {PROCESS_TIMEOUT:?}");
        }
        thread::sleep(Duration::from_millis(5));
    };
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
    assert_candidate_unchanged();
    ProcessOutput {
        status,
        stdout,
        stderr,
    }
}

/// Every facade invocation is one `run_once`; a test that reads a mapped field
/// asserts that shape first so a future non-`run_once` request cannot be read as
/// a field mismatch.
fn run_once_of(request: &RequestEnvelope) -> &RunOnceRequest {
    assert_eq!(request.version, 1);
    let Request::RunOnce(request) = &request.request else {
        panic!("facade sent a non-run_once native request");
    };
    request
}

fn completed_replies(count: usize) -> Vec<NativeReply> {
    (0..count)
        .map(|_| NativeReply::Turn {
            outcome: "completed",
            text: "ok",
        })
        .collect()
}

fn assert_run_once(request: &RequestEnvelope, expected_prompt: &str, cwd: &Path) {
    assert_eq!(request.version, 1);
    let Request::RunOnce(request) = &request.request else {
        panic!("facade sent a non-run_once native request");
    };
    assert_eq!(request.turn.prompt, expected_prompt);
    assert!(request.turn.deadline_unix_ms.is_some());
    assert_eq!(
        request.session.cwd,
        cwd.canonicalize().unwrap().to_string_lossy()
    );
    assert_eq!(
        request
            .session
            .claude
            .as_ref()
            .expect("inline launch")
            .executable,
        fs::canonicalize("/bin/sh").unwrap().to_string_lossy()
    );
    assert!(
        request
            .session
            .claude
            .as_ref()
            .expect("inline launch")
            .extra_args
            .is_empty()
    );
    assert!(matches!(
        request.session.identity,
        SessionIdentity::New { .. }
    ));
    assert_eq!(request.session.auth_policy, AuthPolicy::Subscription);
    assert_eq!(request.session.lifecycle, LifecycleMode::Transcript);
    assert_eq!(request.session.retention, RetentionPolicy::OneShot);
    assert_eq!(
        request.session.compatibility,
        CompatibilityPolicy::RequireTested
    );
    assert_eq!(
        request
            .session
            .environment
            .snapshot
            .get("PMUX_TEST_ENV")
            .map(String::as_str),
        Some("captured")
    );
    let encoded = serde_json::to_string(request).unwrap();
    assert!(!encoded.contains("--print"));
    assert!(!encoded.contains("\"-p\""));
}

#[test]
fn positional_and_stdin_prompts_consume_print_markers_without_forwarding_them() {
    let sandbox = Sandbox::new("input");
    let listener = UnixListener::bind(&sandbox.socket).unwrap();
    let server = spawn_native_server(
        listener,
        vec![
            NativeReply::Turn {
                outcome: "completed",
                text: "positional result",
            },
            NativeReply::Turn {
                outcome: "completed",
                text: "stdin result",
            },
        ],
    );

    let mut positional = base_command(&sandbox.socket, &sandbox.root);
    positional.args(["-p", "--output-format", "text", "positional prompt"]);
    let positional = run(positional, None);
    assert!(positional.status.success());
    assert_eq!(positional.stdout_text(), "positional result");
    assert!(positional.stderr.is_empty());

    let mut piped = env_socket_command(&sandbox.socket, &sandbox.root);
    piped.args(["--print", "--output-format", "json"]);
    let piped = run(piped, Some(b"first\r\nsecond\rthird"));
    assert!(piped.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&piped.stdout).unwrap()["text"],
        "stdin result"
    );
    assert!(piped.stderr.is_empty());

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert_run_once(&requests[0], "positional prompt", &sandbox.root);
    assert_run_once(&requests[1], "first\nsecond\nthird", &sandbox.root);
}

#[test]
fn text_json_and_stream_json_are_stdout_pure_and_label_reconstruction() {
    let sandbox = Sandbox::new("formats");
    let listener = UnixListener::bind(&sandbox.socket).unwrap();
    let server = spawn_native_server(
        listener,
        vec![
            NativeReply::Turn {
                outcome: "completed",
                text: "plain Ω",
            },
            NativeReply::Turn {
                outcome: "completed",
                text: "json Ω",
            },
            NativeReply::Turn {
                outcome: "completed",
                text: "stream Ω",
            },
        ],
    );

    let mut text = base_command(&sandbox.socket, &sandbox.root);
    text.args(["--output-format", "text", "text prompt"]);
    let text = run(text, None);
    assert!(text.status.success());
    assert_eq!(text.stdout_text(), "plain Ω");
    assert!(text.stderr.is_empty());

    let mut json_command = base_command(&sandbox.socket, &sandbox.root);
    json_command.args(["--output-format", "json", "json prompt"]);
    let json_output = run(json_command, None);
    assert!(json_output.status.success());
    let decoded: Value = serde_json::from_slice(&json_output.stdout).unwrap();
    assert_eq!(decoded["outcome"], "completed");
    assert_eq!(decoded["text"], "json Ω");
    assert!(json_output.stdout.ends_with(b"\n"));
    assert!(json_output.stderr.is_empty());

    let mut stream_command = base_command(&sandbox.socket, &sandbox.root);
    stream_command.args(["--output-format", "stream-json", "stream prompt"]);
    let stream = run(stream_command, None);
    assert!(stream.status.success());
    assert!(stream.stderr.is_empty());
    let records = stream
        .stdout_text()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0]["type"], "system");
    assert_eq!(records[0]["subtype"], "init");
    assert_eq!(
        records[0]["provenance"],
        "pmux_interactive_transcript_reconstruction"
    );
    assert_eq!(records[1]["type"], "assistant");
    assert_eq!(records[1]["message"]["model"], "claude-test");
    assert_eq!(records[1]["message"]["content"][0]["text"], "stream Ω");
    assert_eq!(records[2]["type"], "result");
    assert_eq!(records[2]["subtype"], "success");
    assert_eq!(records[2]["is_error"], false);
    assert_eq!(records[2]["result"], "stream Ω");
    assert_eq!(
        records[2]["provenance"],
        "pmux_interactive_transcript_reconstruction"
    );
    assert_eq!(records[2]["token_deltas"], false);

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 3);
    assert_run_once(&requests[0], "text prompt", &sandbox.root);
    assert_run_once(&requests[1], "json prompt", &sandbox.root);
    assert_run_once(&requests[2], "stream prompt", &sandbox.root);
}

#[test]
fn terminal_outcomes_are_emitted_but_only_completed_exits_successfully() {
    let sandbox = Sandbox::new("outcomes");
    let listener = UnixListener::bind(&sandbox.socket).unwrap();
    let server = spawn_native_server(
        listener,
        vec![
            NativeReply::Turn {
                outcome: "completed",
                text: "done",
            },
            NativeReply::Turn {
                outcome: "cancelled",
                text: "cancelled result",
            },
            NativeReply::Turn {
                outcome: "failed",
                text: "failed result",
            },
        ],
    );

    for (prompt, outcome, succeeds) in [
        ("completed prompt", TurnOutcome::Completed, true),
        ("cancelled prompt", TurnOutcome::Cancelled, false),
        ("failed prompt", TurnOutcome::Failed, false),
    ] {
        let mut command = base_command(&sandbox.socket, &sandbox.root);
        command.args(["--output-format", "json", prompt]);
        let output = run(command, None);
        assert_eq!(output.status.success(), succeeds);
        let decoded: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(decoded["outcome"], serde_json::to_value(outcome).unwrap());
        if succeeds {
            assert!(output.stderr.is_empty());
        } else {
            let stderr = output.stderr_text();
            assert!(stderr.contains("Claude turn ended with"));
            assert!(stderr.contains(match outcome {
                TurnOutcome::Completed => unreachable!(),
                TurnOutcome::Cancelled => "Cancelled",
                TurnOutcome::Failed => "Failed",
            }));
        }
    }

    assert_eq!(server.join().unwrap().len(), 3);
}

#[test]
fn facade_rejects_invalid_prompt_socket_timeout_and_flag_surfaces() {
    let sandbox = Sandbox::new("reject");
    let cases: Vec<(Vec<&str>, Option<Vec<u8>>)> = vec![
        (vec!["/compact"], None),
        (vec!["   \t\r\n"], None),
        (vec![], Some(b"unsafe\x1bprompt".to_vec())),
        (vec![], Some(b"unsafe\0prompt".to_vec())),
        (vec![], Some(b"unsafe\x01prompt".to_vec())),
        (vec![], Some(b"unsafe\x7fprompt".to_vec())),
        (vec![], Some(vec![0xff, 0xfe])),
        (vec![], Some(vec![b'x'; MAX_PROMPT_BYTES + 1])),
        (vec!["--timeout-seconds", "0", "prompt"], None),
        (vec!["--input-format", "stream-json", "prompt"], None),
        (vec!["--definitely-unknown", "prompt"], None),
    ];
    for (args, input) in cases {
        let mut command = base_command(&sandbox.socket, &sandbox.root);
        command.args(args);
        let output = run(command, input.as_deref());
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }

    let mut relative = base_command(Path::new("relative.sock"), &sandbox.root);
    relative.arg("prompt");
    let relative = run(relative, None);
    assert!(!relative.status.success());
    assert!(relative.stdout.is_empty());
    assert!(
        relative
            .stderr_text()
            .contains("socket_path must be absolute")
    );

    let mut missing_socket = Command::new(claude_p_binary());
    missing_socket
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .args(["--claude-bin", "/bin/sh", "--cwd"])
        .arg(&sandbox.root)
        .arg("prompt");
    let missing_socket = run(missing_socket, None);
    assert_eq!(missing_socket.status.code(), Some(2));
    assert!(missing_socket.stdout.is_empty());

    // The facade intentionally uses PSEUDOMUX_SOCKET, not the native clients'
    // PMUX_SOCKET variable.
    let mut wrong_environment = Command::new(claude_p_binary());
    wrong_environment
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("PMUX_SOCKET", &sandbox.socket)
        .args(["--claude-bin", "/bin/sh", "--cwd"])
        .arg(&sandbox.root)
        .arg("prompt");
    let wrong_environment = run(wrong_environment, None);
    assert_eq!(wrong_environment.status.code(), Some(2));
    assert!(wrong_environment.stdout.is_empty());
}

#[test]
fn malformed_rejected_unexpected_and_unavailable_daemons_fail_without_stdout() {
    let sandbox = Sandbox::new("daemon");
    let listener = UnixListener::bind(&sandbox.socket).unwrap();
    let server = spawn_native_server(
        listener,
        vec![
            NativeReply::Malformed,
            NativeReply::Error,
            NativeReply::WrongResult,
        ],
    );

    for (index, expected_stderr) in ["invalid JSON frame", "pmuxd error", "expected turn_result"]
        .into_iter()
        .enumerate()
    {
        let mut command = base_command(&sandbox.socket, &sandbox.root);
        command.arg(format!("daemon prompt {index}"));
        let output = run(command, None);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(
            output.stderr_text().contains(expected_stderr),
            "stderr did not contain {expected_stderr:?}: {}",
            output.stderr_text()
        );
        assert!(!output.stderr_text().contains("daemon-error-details-secret"));
    }
    let captured = server.join().unwrap();
    assert_eq!(captured.len(), 3);
    assert!(
        captured
            .iter()
            .all(|request| matches!(request.request, Request::RunOnce(_)))
    );

    let unavailable_sandbox = Sandbox::new("unavailable");
    let mut unavailable = base_command(&unavailable_sandbox.socket, &unavailable_sandbox.root);
    unavailable.arg("unavailable prompt");
    let unavailable = run(unavailable, None);
    assert_eq!(unavailable.status.code(), Some(1));
    assert!(unavailable.stdout.is_empty());
    assert!(unavailable.stderr_text().contains("I/O error"));
}

#[test]
fn forced_new_and_resume_identity_are_preserved_in_the_native_request() {
    let sandbox = Sandbox::new("identity");
    let listener = UnixListener::bind(&sandbox.socket).unwrap();
    let server = spawn_native_server(
        listener,
        vec![
            NativeReply::Turn {
                outcome: "completed",
                text: "new",
            },
            NativeReply::Turn {
                outcome: "completed",
                text: "resume",
            },
        ],
    );

    let mut new = base_command(&sandbox.socket, &sandbox.root);
    new.args(["--session-id", SESSION_ID, "new prompt"]);
    assert!(run(new, None).status.success());
    let mut resume = base_command(&sandbox.socket, &sandbox.root);
    resume.args(["--resume", SESSION_ID, "resume prompt"]);
    assert!(run(resume, None).status.success());

    let requests = server.join().unwrap();
    let Request::RunOnce(new) = &requests[0].request else {
        panic!("expected run_once");
    };
    assert_eq!(
        new.session.identity,
        SessionIdentity::New {
            session_id: Some(SESSION_ID.parse().unwrap())
        }
    );
    let Request::RunOnce(resume) = &requests[1].request else {
        panic!("expected run_once");
    };
    assert_eq!(
        resume.session.identity,
        SessionIdentity::Resume {
            session_id: SESSION_ID.parse().unwrap()
        }
    );
}

#[test]
fn executable_configuration_flags_map_to_bounded_native_fields() {
    let sandbox = Sandbox::new("mapping");
    let settings = sandbox.root.join("settings.json");
    let mcp = sandbox.root.join("mcp.json");
    let plugin = sandbox.root.join("plugin");
    fs::write(&settings, b"{}").unwrap();
    fs::write(&mcp, b"{}").unwrap();
    fs::create_dir(&plugin).unwrap();
    let listener = UnixListener::bind(&sandbox.socket).unwrap();
    let server = spawn_native_server(
        listener,
        vec![NativeReply::Turn {
            outcome: "completed",
            text: "mapped",
        }],
    );

    let mut command = base_command(&sandbox.socket, &sandbox.root);
    command
        .args([
            "--model",
            "claude-test",
            "--effort",
            "xhigh",
            "--permission-mode",
            "plan",
            "--allowedTools",
            "Read",
            "--disallowedTools",
            "Bash",
            "--settings",
        ])
        .arg(&settings)
        .arg("--mcp-config")
        .arg(&mcp)
        .arg("--plugin-dir")
        .arg(&plugin)
        .args(["--append-system-prompt", "bounded system", "mapping prompt"]);
    let output = run(command, None);
    assert!(output.status.success(), "{}", output.stderr_text());

    let requests = server.join().unwrap();
    let Request::RunOnce(request) = &requests[0].request else {
        panic!("expected run_once");
    };
    let claude = request.session.claude.as_ref().expect("inline launch");
    assert_eq!(claude.model.as_deref(), Some("claude-test"));
    assert_eq!(serde_json::to_value(claude.effort).unwrap(), "xhigh");
    assert_eq!(
        serde_json::to_value(claude.permission_mode).unwrap(),
        "plan"
    );
    assert_eq!(claude.allowed_tools, ["Read"]);
    assert_eq!(claude.denied_tools, ["Bash"]);
    assert_eq!(claude.settings.len(), 1);
    assert_eq!(claude.mcp_configs.len(), 1);
    assert_eq!(
        claude.plugin_dirs,
        [plugin.canonicalize().unwrap().to_string_lossy()]
    );
    assert_eq!(
        serde_json::to_value(&claude.system_prompt).unwrap(),
        json!({"mode": "append", "prompt": "bounded system"})
    );
    assert!(claude.extra_args.is_empty());
}

#[test]
fn clap_help_and_version_are_protocol_free_stdout_only_commands() {
    for flag in ["--help", "--version"] {
        let mut command = Command::new(claude_p_binary());
        command.env_clear().arg(flag);
        let output = run(command, None);
        assert!(output.status.success());
        assert!(!output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
}

/// `--claude-bin` and `PATH` are left to the caller so the executable-resolution
/// tests can control both; everything else matches `base_command`.
fn command_without_claude_bin(socket: &Path, cwd: &Path) -> Command {
    let mut command = Command::new(claude_p_binary());
    command
        .env_clear()
        .env("HOME", cwd)
        .env("LANG", "C")
        .env("PMUX_TEST_ENV", "captured")
        .arg("--socket")
        .arg(socket)
        .arg("--cwd")
        .arg(cwd);
    command
}

fn write_executable(path: &Path) {
    fs::write(path, b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// `tools/phase0/phase0_lib.py:1499` refuses a `claude-p-one-shot`
/// campaign whose cell is not "24x120 transparent/auto/transcript", because the
/// facade has no flag for any of it and sends `TerminalSpec::default()`. Nothing
/// else asserted that the default *is* that cell, so the rejection was a claim
/// about a constant three crates away. This is the assertion that makes it true.
#[test]
fn the_facade_sends_one_fixed_cell_and_leaves_every_optional_launch_field_unset() {
    let sandbox = Sandbox::new("cell");
    let listener = UnixListener::bind(&sandbox.socket).unwrap();
    let server = spawn_native_server(listener, completed_replies(1));

    let mut command = base_command(&sandbox.socket, &sandbox.root);
    command.args(["-p", "--output-format", "json", "cell prompt"]);
    let output = run(command, None);
    assert!(output.status.success(), "{}", output.stderr_text());

    let requests = server.join().unwrap();
    assert_run_once(&requests[0], "cell prompt", &sandbox.root);
    let request = run_once_of(&requests[0]);
    assert_eq!(
        serde_json::to_value(&request.session.terminal).unwrap(),
        json!({
            "rows": 24,
            "cols": 120,
            "profile": "transparent",
            "input_transport": "auto",
        }),
        "the facade's fixed cell drifted from the campaign contract phase0 enforces"
    );

    let claude = request.session.claude.as_ref().expect("inline launch");
    assert_eq!(claude.model, None);
    assert_eq!(claude.effort, None);
    assert_eq!(claude.permission_mode, None);
    assert!(claude.allowed_tools.is_empty());
    assert!(claude.denied_tools.is_empty());
    assert!(claude.settings.is_empty());
    assert!(claude.mcp_configs.is_empty());
    assert!(claude.plugin_dirs.is_empty());
    assert_eq!(
        serde_json::to_value(&claude.system_prompt).unwrap(),
        json!({"mode": "default"})
    );
    assert!(claude.extra_args.is_empty());
    assert!(request.session.environment.set.is_empty());
    assert!(request.session.environment.unset.is_empty());
}

/// A silently mis-mapped permission mode is the facade's worst failure: the
/// caller asks for `plan` and Claude is launched able to edit. Only `plan` was
/// pinned before, so five of the six spellings were unverified. The rejection
/// cases are the other half of the same claim, and they are what
/// `tools/phase0/phase0_lib.py:1423-1429` and `:1453-1454` assert from outside:
/// the facade exposes six modes, and has neither
/// `--dangerously-skip-permissions` nor `--agent`.
#[test]
fn every_permission_mode_spelling_maps_to_its_protocol_name_and_the_bypass_surfaces_are_absent() {
    const MODES: [(&str, &str); 6] = [
        ("default", "default"),
        ("accept-edits", "accept_edits"),
        ("plan", "plan"),
        ("auto", "auto"),
        ("bypass-permissions", "bypass_permissions"),
        ("dont-ask", "dont_ask"),
    ];

    let sandbox = Sandbox::new("permission");
    let listener = UnixListener::bind(&sandbox.socket).unwrap();
    let server = spawn_native_server(listener, completed_replies(MODES.len()));

    for (flag_value, _) in MODES {
        let mut command = base_command(&sandbox.socket, &sandbox.root);
        command.args(["--permission-mode", flag_value, "permission prompt"]);
        let output = run(command, None);
        assert!(
            output.status.success(),
            "--permission-mode {flag_value} was refused: {}",
            output.stderr_text()
        );
    }

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), MODES.len());
    for (request, (flag_value, wire_value)) in requests.iter().zip(MODES) {
        assert_eq!(
            serde_json::to_value(
                run_once_of(request)
                    .session
                    .claude
                    .as_ref()
                    .expect("inline launch")
                    .permission_mode
            )
            .unwrap(),
            wire_value,
            "--permission-mode {flag_value} reached the daemon as something else"
        );
    }

    for arguments in [
        vec!["--permission-mode", "dangerously-skip-permissions"],
        vec!["--dangerously-skip-permissions"],
        vec!["--agent", "reviewer"],
        vec!["--agent-file", "/dev/null"],
        vec!["--permission-mode", "bypass"],
    ] {
        let mut command = base_command(&sandbox.socket, &sandbox.root);
        command.args(&arguments).arg("rejected prompt");
        let output = run(command, None);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{arguments:?} was not refused by the parser"
        );
        assert!(output.stdout.is_empty());
    }
}

/// Every variant of a plain enum as an array, kept complete by a wildcard-free
/// `match`.
///
/// A hand-written variant list is not a guard: the assertions run over the
/// array, so a variant nobody added to the array is a variant nobody asserts
/// about, and the test passes while claiming the coverage it lost. The
/// `exhaustive` function is the mechanism -- a new enum variant stops this
/// compiling until it is listed, and listing it puts it through the loop below.
/// Same shape and same reason as `wire_values!` at
/// `crates/protocol/tests/v1_conformance_vectors.rs:387`.
macro_rules! every_variant {
    ($ty:ty, [$($variant:path),+ $(,)?]) => {{
        fn exhaustive(value: $ty) {
            match value {
                $($variant => ()),+
            }
        }
        [$({ exhaustive($variant); $variant }),+]
    }};
}

/// The `--effort` word a caller types is the word that reaches the daemon, for
/// EVERY level the protocol has.
///
/// `xhigh` is the only level `EffortLevel` renames, so a wrong mapping here buys
/// a different amount of thinking than the caller paid for with no diagnostic
/// anywhere: an effort value Claude does not recognise is warned about on the
/// child's stderr and silently replaced by the default, and no pmux layer reads
/// that stderr.
///
/// THE LIST IS DERIVED, NOT TYPED. It used to be `const LEVELS: [(&str, &str); 5]`
/// written out by hand, which meant a new `EffortLevel` variant was simply
/// absent from the array and therefore never launched, never sent, and never
/// asserted about -- the check passed by having no case for it. `every_variant!`
/// makes that a compile error, and the pinned five-element literal below makes
/// the reviewer move the spellings deliberately rather than by adding a line.
///
/// The flag value and the wire value are the same string BY DESIGN, and that is
/// the property under test rather than a shortcut: `claude-p`'s clap value names
/// are the protocol's own spellings, so a rename on either side shows up here as
/// either a refused `--effort` (clap no longer takes the wire word) or a
/// mismatched envelope (the word reached the daemon as a different variant).
#[test]
fn every_effort_level_spelling_maps_to_its_protocol_name() {
    let levels = every_variant!(
        EffortLevel,
        [
            EffortLevel::Low,
            EffortLevel::Medium,
            EffortLevel::High,
            EffortLevel::XHigh,
            EffortLevel::Max,
        ]
    );
    let spellings = levels.map(wire_spelling);
    assert_eq!(spellings, ["low", "medium", "high", "xhigh", "max"]);

    let sandbox = Sandbox::new("effort");
    let listener = UnixListener::bind(&sandbox.socket).unwrap();
    let server = spawn_native_server(listener, completed_replies(levels.len()));

    for flag_value in &spellings {
        let mut command = base_command(&sandbox.socket, &sandbox.root);
        command.args(["--effort", flag_value.as_str(), "effort prompt"]);
        let output = run(command, None);
        assert!(
            output.status.success(),
            "--effort {flag_value} was refused: {}",
            output.stderr_text()
        );
    }

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), levels.len());
    for (request, flag_value) in requests.iter().zip(&spellings) {
        assert_eq!(
            serde_json::to_value(
                run_once_of(request)
                    .session
                    .claude
                    .as_ref()
                    .expect("inline launch")
                    .effort
            )
            .unwrap(),
            *flag_value,
            "--effort {flag_value} reached the daemon as something else"
        );
    }
}

/// The plain string one v1 effort level serialises as, which is also the word
/// `claude-p` accepts on the command line.
fn wire_spelling(level: EffortLevel) -> String {
    serde_json::to_value(level)
        .expect("EffortLevel serialises")
        .as_str()
        .expect("v1 value enums serialize as plain strings")
        .to_owned()
}

/// Replace and append differ only by the variant name, and confusing them is
/// silent: Claude answers with the wrong system prompt and every completion
/// signal still looks healthy. Only append was covered.
#[test]
fn replace_and_append_system_prompts_are_distinct_on_the_wire_and_mutually_exclusive() {
    let sandbox = Sandbox::new("system");
    let listener = UnixListener::bind(&sandbox.socket).unwrap();
    let server = spawn_native_server(listener, completed_replies(2));

    let mut replace = base_command(&sandbox.socket, &sandbox.root);
    replace.args(["--system-prompt", "replacement text", "replace prompt"]);
    let replace = run(replace, None);
    assert!(replace.status.success(), "{}", replace.stderr_text());

    let mut append = base_command(&sandbox.socket, &sandbox.root);
    append.args(["--append-system-prompt", "appended text", "append prompt"]);
    let append = run(append, None);
    assert!(append.status.success(), "{}", append.stderr_text());

    let requests = server.join().unwrap();
    assert_eq!(
        serde_json::to_value(
            &run_once_of(&requests[0])
                .session
                .claude
                .as_ref()
                .expect("inline launch")
                .system_prompt
        )
        .unwrap(),
        json!({"mode": "replace", "prompt": "replacement text"})
    );
    assert_eq!(
        serde_json::to_value(
            &run_once_of(&requests[1])
                .session
                .claude
                .as_ref()
                .expect("inline launch")
                .system_prompt
        )
        .unwrap(),
        json!({"mode": "append", "prompt": "appended text"})
    );

    for arguments in [
        vec![
            "--system-prompt",
            "one",
            "--append-system-prompt",
            "two",
            "conflict prompt",
        ],
        vec!["--session-id", SESSION_ID, "--resume", SESSION_ID, "prompt"],
    ] {
        let mut command = base_command(&sandbox.socket, &sandbox.root);
        command.args(&arguments);
        let output = run(command, None);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{arguments:?} was not refused by the parser"
        );
        assert!(output.stdout.is_empty());
    }
}

/// The facade's own prompt handling was only ever exercised with ASCII. A
/// prompt that is silently re-wrapped or trimmed produces a confident answer to
/// a question the caller did not ask, which is the one failure mode this
/// project treats as unacceptable.
///
/// The transformations the facade is allowed to make are exactly the ones
/// `pseudomux_client::normalize_cli_prompt` names, and there are now two:
/// `\r` folding, and Unicode canonical composition. NFC was added because it
/// was MEASURED at Claude Code 2.1.226 to be what the composer records — pmux
/// typed `e` + U+0301 and the child wrote U+00E9 — so the pre-NFC behaviour
/// this test used to pin was not "untransformed", it was "transformed by
/// Claude instead of by pmux, one step too late to be acknowledged".
/// Canonical composition changes the bytes and not the text; every other
/// character below still has to arrive exactly as it was written.
#[test]
fn unicode_multiline_and_padded_prompts_reach_the_daemon_untransformed() {
    // A combining acute that NFC fuses, a wide CJK pair, an astral codepoint, a
    // tab, an interior line that starts with `/`, LEADING padding that nothing
    // is allowed to touch, and TRAILING padding that the composer removes.
    const PROMPT: &str = "  he\u{301}llo 世界 🌍\tfirst\n/interior-slash-is-not-a-command\nlast  ";

    let sandbox = Sandbox::new("unicode");
    let listener = UnixListener::bind(&sandbox.socket).unwrap();
    let server = spawn_native_server(listener, completed_replies(2));

    let mut positional = base_command(&sandbox.socket, &sandbox.root);
    positional.args(["--output-format", "text", PROMPT]);
    let positional = run(positional, None);
    assert!(positional.status.success(), "{}", positional.stderr_text());

    let piped = format!("{PROMPT}\n");
    let mut stdin_command = base_command(&sandbox.socket, &sandbox.root);
    stdin_command.args(["--output-format", "text"]);
    let stdin_output = run(stdin_command, Some(piped.as_bytes()));
    assert!(
        stdin_output.status.success(),
        "{}",
        stdin_output.stderr_text()
    );

    // The expectation is DERIVED from the shipped rule rather than typed out,
    // so a third transformation cannot be added to `normalize_cli_prompt`
    // without this test asking what it is.
    let expected = pseudomux_client::normalize_cli_prompt(PROMPT);
    assert_eq!(
        expected,
        PROMPT
            .replace("e\u{301}", "\u{e9}")
            .trim_end_matches(pseudomux_client::prompt::is_trimmed_from_the_end),
        "canonical composition and the composer's own trailing trim are the only \
         differences this test tolerates"
    );
    assert!(
        expected.starts_with("  h"),
        "leading padding is not the composer's to remove and must survive"
    );
    let requests = server.join().unwrap();
    assert_eq!(run_once_of(&requests[0]).turn.prompt, expected);
    assert_eq!(
        run_once_of(&requests[1]).turn.prompt,
        expected,
        "a piped prompt was rewritten beyond dropping the trailing run a composer \
         cannot hold"
    );
}

/// `echo q | claude-p` is the invocation this facade exists for, and every
/// conventional producer of piped text ends it with the POSIX terminator. A
/// composer cannot hold trailing whitespace, so Claude records the typed prompt
/// without it; arming the turn with it therefore makes `expected` unequal to
/// `actual` and the turn dies in `UnexpectedTypedPrompt`
/// (`crates/claude/src/engine.rs:127`). `bin/pmux/src/cli.rs` measured that
/// death and dropped exactly one terminator; the facade must apply the same rule
/// to the same bytes, so this asserts the rule and not just the absence of a
/// newline.
///
/// The rule stopped being "exactly one terminator" on 2026-08-09. At 2.1.226 the
/// composer removes its whole trailing run of whitespace, MEASURED over spaces,
/// `\n`, U+FEFF and U+3000 (`docs/path-b-adversarial.md` sec. 11), so case three
/// -- a deliberate trailing blank line -- was armed as `"poem\n"`, recorded as
/// `"poem"`, and cost the pooled instance. It is here as the case that changed.
#[test]
fn a_piped_prompt_arrives_without_the_trailing_run_a_composer_cannot_hold() {
    // Each pair is (what a producer writes to the pipe, what the composer can
    // hold). Case four is a CRLF file, folded before the trim runs; case five
    // is the one invisible character the composer was MEASURED to KEEP, so the
    // rule cannot be widened to "invisible characters" without failing here.
    const CASES: [(&str, &str); 6] = [
        ("Reply with exactly: ok\n", "Reply with exactly: ok"),
        ("Reply with exactly: ok", "Reply with exactly: ok"),
        ("poem\n\n", "poem"),
        ("line one\r\nline two\r\n", "line one\nline two"),
        ("ok\u{200b}", "ok\u{200b}"),
        ("  padded  \n", "  padded"),
    ];

    let sandbox = Sandbox::new("terminator");
    let listener = UnixListener::bind(&sandbox.socket).unwrap();
    let server = spawn_native_server(listener, completed_replies(CASES.len()));

    for (piped, _) in CASES {
        let mut command = base_command(&sandbox.socket, &sandbox.root);
        command.args(["--output-format", "text"]);
        let output = run(command, Some(piped.as_bytes()));
        assert!(output.status.success(), "{}", output.stderr_text());
    }

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), CASES.len());
    for (request, (piped, armed)) in requests.iter().zip(CASES) {
        assert_eq!(
            run_once_of(request).turn.prompt,
            armed,
            "{piped:?} armed a turn the composer cannot acknowledge"
        );
    }
}

/// `read_prompt` prefers the positional argument and never reads stdin in that
/// case. Untested, and getting it backwards would send a prompt the caller did
/// not mean to send.
#[test]
fn a_positional_prompt_wins_over_piped_stdin() {
    let sandbox = Sandbox::new("precedence");
    let listener = UnixListener::bind(&sandbox.socket).unwrap();
    let server = spawn_native_server(listener, completed_replies(1));

    let mut command = base_command(&sandbox.socket, &sandbox.root);
    command.args(["--output-format", "text", "positional wins"]);
    let output = run(command, Some(b"ignored stdin prompt"));
    assert!(output.status.success(), "{}", output.stderr_text());

    let requests = server.join().unwrap();
    assert_eq!(run_once_of(&requests[0]).turn.prompt, "positional wins");
}

/// One byte under the limit and exactly at it are the two sides of the only
/// prompt bound the facade owns; only the rejecting side was covered, so an
/// off-by-one that refused a legal 1 MiB prompt would have gone unnoticed.
#[test]
fn a_prompt_of_exactly_the_maximum_size_is_accepted() {
    let sandbox = Sandbox::new("boundary");
    let listener = UnixListener::bind(&sandbox.socket).unwrap();
    let server = spawn_native_server(listener, completed_replies(1));

    let mut command = base_command(&sandbox.socket, &sandbox.root);
    command.args(["--output-format", "text"]);
    let output = run(command, Some(&vec![b'x'; MAX_PROMPT_BYTES]));
    assert!(output.status.success(), "{}", output.stderr_text());

    let requests = server.join().unwrap();
    assert_eq!(
        run_once_of(&requests[0]).turn.prompt.len(),
        MAX_PROMPT_BYTES
    );
}

/// The unit test drives `environment_patch` with an injected lookup, so nothing
/// proved the patch survives into the native request. `--unset` is the subtle
/// half: the name stays in `snapshot` and is carried as a separate `unset` term
/// that the daemon applies, so a test that expected removal from `snapshot`
/// would be wrong about the contract.
#[test]
fn the_environment_patch_reaches_the_native_request_beside_the_exact_snapshot() {
    let sandbox = Sandbox::new("environment");
    let listener = UnixListener::bind(&sandbox.socket).unwrap();
    let server = spawn_native_server(listener, completed_replies(1));

    let mut command = base_command(&sandbox.socket, &sandbox.root);
    command.args([
        "--env",
        "EXPLICIT_NAME=explicit=value",
        "--env-passthrough",
        "PMUX_TEST_ENV",
        "--unset",
        "LANG",
        "environment prompt",
    ]);
    let output = run(command, None);
    assert!(output.status.success(), "{}", output.stderr_text());

    let requests = server.join().unwrap();
    let environment = &run_once_of(&requests[0]).session.environment;
    assert_eq!(
        environment.set.get("EXPLICIT_NAME").map(String::as_str),
        Some("explicit=value")
    );
    assert_eq!(
        environment.set.get("PMUX_TEST_ENV").map(String::as_str),
        Some("captured"),
        "--env-passthrough did not carry the caller's value into `set`"
    );
    assert_eq!(environment.unset, ["LANG".to_owned()].into_iter().collect());
    assert_eq!(
        environment.snapshot.get("LANG").map(String::as_str),
        Some("C"),
        "the exact snapshot is reported verbatim; `unset` is the daemon's instruction"
    );
}

/// `--timeout-seconds` is the facade's only deadline surface and it is seconds
/// on the outside, absolute milliseconds on the wire. The existing black-box
/// assertion was `is_some()`, which a seconds-for-milliseconds mix-up passes.
#[test]
fn timeout_seconds_becomes_an_absolute_unix_millisecond_deadline() {
    const TIMEOUT_SECONDS: u64 = 7;

    let sandbox = Sandbox::new("deadline");
    let listener = UnixListener::bind(&sandbox.socket).unwrap();
    let server = spawn_native_server(listener, completed_replies(1));

    let before = unix_millis();
    let mut command = base_command(&sandbox.socket, &sandbox.root);
    command.args([
        "--timeout-seconds",
        &TIMEOUT_SECONDS.to_string(),
        "deadline prompt",
    ]);
    let output = run(command, None);
    assert!(output.status.success(), "{}", output.stderr_text());
    let after = unix_millis();

    let requests = server.join().unwrap();
    let deadline = run_once_of(&requests[0]).turn.deadline_unix_ms.unwrap();
    let window = (before + TIMEOUT_SECONDS * 1_000)..=(after + TIMEOUT_SECONDS * 1_000);
    assert!(
        window.contains(&deadline),
        "deadline {deadline} is outside {window:?}: the seconds-to-millisecond \
         conversion or the epoch base is wrong"
    );
}

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap()
}

/// Every other test passes an absolute `--claude-bin`, so the `PATH` search --
/// which decides *which* Claude gets launched, and is what the defaulted
/// `claude` always takes -- was never executed. A directory that shares the
/// name must not win: `resolve_executable` filters on `is_file`.
#[test]
fn a_bare_claude_bin_is_resolved_through_path_skipping_same_named_directories() {
    let sandbox = Sandbox::new("path");
    let shadow = sandbox.root.join("shadow");
    let real = sandbox.root.join("real");
    fs::create_dir(&shadow).unwrap();
    fs::create_dir(&real).unwrap();
    fs::create_dir(shadow.join("faux-claude")).unwrap();
    let executable = real.join("faux-claude");
    write_executable(&executable);
    let search_path = format!("{}:{}:/usr/bin:/bin", shadow.display(), real.display());

    let listener = UnixListener::bind(&sandbox.socket).unwrap();
    let server = spawn_native_server(listener, completed_replies(2));

    let mut flag = command_without_claude_bin(&sandbox.socket, &sandbox.root);
    flag.env("PATH", &search_path)
        .args(["--claude-bin", "faux-claude", "path prompt"]);
    let flag = run(flag, None);
    assert!(flag.status.success(), "{}", flag.stderr_text());

    // `PMUX_CLAUDE_BIN` is the documented environment channel for the same
    // argument and had no coverage at all.
    let mut from_environment = command_without_claude_bin(&sandbox.socket, &sandbox.root);
    from_environment
        .env("PATH", &search_path)
        .env("PMUX_CLAUDE_BIN", "faux-claude")
        .arg("environment prompt");
    let from_environment = run(from_environment, None);
    assert!(
        from_environment.status.success(),
        "{}",
        from_environment.stderr_text()
    );

    let canonical = executable.canonicalize().unwrap();
    let expected = canonical.to_string_lossy();
    let requests = server.join().unwrap();
    for request in &requests {
        assert_eq!(
            run_once_of(request)
                .session
                .claude
                .as_ref()
                .expect("inline launch")
                .executable,
            expected,
            "the facade resolved a bare --claude-bin to the wrong path"
        );
    }

    let mut missing = command_without_claude_bin(&sandbox.socket, &sandbox.root);
    missing.env("PATH", &search_path).args([
        "--claude-bin",
        "definitely-not-on-path",
        "missing prompt",
    ]);
    let missing = run(missing, None);
    assert_eq!(missing.status.code(), Some(1));
    assert!(missing.stdout.is_empty());
    assert!(
        missing.stderr_text().contains("was not found in PATH"),
        "{}",
        missing.stderr_text()
    );
}

/// Each path flag is canonicalized before it is sent, so a symlinked `--cwd`
/// must arrive as its target: the daemon binds the session to that path, and
/// two names for one directory would be two sessions. A missing path must fail
/// before any prompt is sent rather than reaching the daemon as a bad launch.
#[test]
fn path_arguments_are_canonicalized_and_missing_paths_fail_without_stdout() {
    let sandbox = Sandbox::new("paths");
    let target = sandbox.root.join("target");
    fs::create_dir(&target).unwrap();
    let link = sandbox.root.join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let listener = UnixListener::bind(&sandbox.socket).unwrap();
    let server = spawn_native_server(listener, completed_replies(1));

    let mut command = base_command(&sandbox.socket, &link);
    command.args(["--output-format", "text", "symlink prompt"]);
    let output = run(command, None);
    assert!(output.status.success(), "{}", output.stderr_text());

    let requests = server.join().unwrap();
    assert_eq!(
        run_once_of(&requests[0]).session.cwd,
        target.canonicalize().unwrap().to_string_lossy()
    );

    let absent = sandbox.root.join("absent");
    for flag in ["--settings", "--mcp-config", "--plugin-dir"] {
        let mut command = base_command(&sandbox.socket, &sandbox.root);
        command.arg(flag).arg(&absent).arg("missing path prompt");
        let output = run(command, None);
        assert_eq!(
            output.status.code(),
            Some(1),
            "{flag} accepted a path that does not exist"
        );
        assert!(output.stdout.is_empty());
        assert!(
            output.stderr_text().contains(flag),
            "{flag} was not named in its own diagnostic: {}",
            output.stderr_text()
        );
    }

    // `--cwd` is set once by `base_command`, and clap refuses a second
    // occurrence rather than letting the last one win, so this case builds its
    // own argv.
    let mut missing_cwd = command_without_claude_bin(&sandbox.socket, &absent);
    missing_cwd.env("PATH", "/usr/bin:/bin").args([
        "--claude-bin",
        "/bin/sh",
        "missing cwd prompt",
    ]);
    let missing_cwd = run(missing_cwd, None);
    assert_eq!(missing_cwd.status.code(), Some(1));
    assert!(missing_cwd.stdout.is_empty());
    assert!(
        missing_cwd.stderr_text().contains("invalid --cwd"),
        "{}",
        missing_cwd.stderr_text()
    );

    let mut repeated = base_command(&sandbox.socket, &sandbox.root);
    repeated.arg("--cwd").arg(&sandbox.root).arg("prompt");
    let repeated = run(repeated, None);
    assert_eq!(
        repeated.status.code(),
        Some(2),
        "a repeated --cwd must be refused rather than silently resolved to one of them"
    );
    assert!(repeated.stdout.is_empty());
}

/// `stream-json` is the surface a migrating `claude -p` caller parses, and the
/// non-completed outcomes were only ever checked through `--output-format json`.
/// The claim under test is the one that matters: a turn that did not complete is
/// never labeled `success` on this surface, and the process still exits
/// non-zero.
///
/// `is_error` is deliberately not asserted for `Cancelled`. The facade currently
/// emits `false` there (`bin/claude-p/src/main.rs:459` tests only
/// `TurnOutcome::Failed`), and pinning that would defend it: a cancelled turn's
/// `result` text is partial, so a caller that branches on `is_error` alone would
/// read a truncated answer as a complete one. That decision belongs to a change
/// to the facade, not to this test.
#[test]
fn stream_json_never_labels_a_non_completed_outcome_success() {
    let sandbox = Sandbox::new("stream-outcomes");
    let listener = UnixListener::bind(&sandbox.socket).unwrap();
    let server = spawn_native_server(
        listener,
        vec![
            NativeReply::Turn {
                outcome: "cancelled",
                text: "partial cancelled text",
            },
            NativeReply::Turn {
                outcome: "failed",
                text: "failed text",
            },
        ],
    );

    for (prompt, subtype, text) in [
        ("cancelled prompt", "cancelled", "partial cancelled text"),
        ("failed prompt", "error", "failed text"),
    ] {
        let mut command = base_command(&sandbox.socket, &sandbox.root);
        command.args(["--output-format", "stream-json", prompt]);
        let output = run(command, None);
        assert_eq!(
            output.status.code(),
            Some(1),
            "{prompt} exited successfully"
        );
        let records = output
            .stdout_text()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 3);
        let result = records.last().unwrap();
        assert_eq!(result["type"], "result");
        assert_eq!(result["subtype"], subtype);
        assert_ne!(result["subtype"], "success");
        assert_eq!(result["result"], text);
        assert!(output.stderr_text().contains("Claude turn ended with"));
    }

    let failed = server.join().unwrap();
    assert_eq!(failed.len(), 2);
}
