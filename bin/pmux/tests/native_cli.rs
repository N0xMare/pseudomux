#![cfg(unix)]

#[allow(dead_code)]
mod support;

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::thread;

use pseudomux_protocol::v1::{
    ErrorBody, ErrorCode, Pong, Request, RequestEnvelope, ResponseEnvelope, ResponseResult,
};

use support::{GENERATION_ID, SESSION_ID, TURN_ID, pmux_process, run};

fn read_request(stream: &mut UnixStream) -> RequestEnvelope {
    let mut header = [0_u8; 4];
    stream.read_exact(&mut header).unwrap();
    let mut payload = vec![0; u32::from_be_bytes(header) as usize];
    stream.read_exact(&mut payload).unwrap();
    serde_json::from_slice(&payload).unwrap()
}

fn write_response(stream: &mut UnixStream, response: &ResponseEnvelope) {
    let payload = serde_json::to_vec(response).unwrap();
    stream
        .write_all(&u32::try_from(payload.len()).unwrap().to_be_bytes())
        .unwrap();
    stream.write_all(&payload).unwrap();
    stream.flush().unwrap();
}

#[test]
fn json_ping_keeps_protocol_data_on_stdout() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("pmuxd.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&mut stream);
        assert!(matches!(request.request, Request::Ping));
        write_response(
            &mut stream,
            &ResponseEnvelope::success(
                request.request_id,
                ResponseResult::Pong(Pong {
                    server_version: "test-server".into(),
                    protocol_version: 1,
                }),
            ),
        );
    });

    let mut process = pmux_process();
    process.args([
        "--socket",
        socket.to_str().unwrap(),
        "--output",
        "json",
        "ping",
    ]);
    let output = run(process, None);
    server.join().unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["server_version"], "test-server");
    assert_eq!(stdout["protocol_version"], 1);
}

#[test]
fn server_error_keeps_stdout_empty_and_diagnostics_on_stderr() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("pmuxd.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&mut stream);
        write_response(
            &mut stream,
            &ResponseEnvelope::failure(
                request.request_id,
                ErrorBody::new(ErrorCode::RateLimited, "quota exhausted").retryable(true),
            ),
        );
    });

    let mut process = pmux_process();
    process.args([
        "--socket",
        socket.to_str().unwrap(),
        "--output",
        "json",
        "ping",
    ]);
    let output = run(process, None);
    server.join().unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.starts_with("pmux:"));
    assert!(stderr.contains("quota exhausted"));
}

/// Former session subcommands are unknown: clap exits 2 before any socket is
/// opened.
#[test]
fn session_subcommands_are_unknown() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("pmuxd.sock");
    assert!(!socket.exists());

    let commands: &[&[&str]] = &[
        &["start"],
        &["oneshot"],
        &["turn", SESSION_ID, "--generation", GENERATION_ID],
        &["inspect", SESSION_ID, "--generation", GENERATION_ID],
        &["cancel", SESSION_ID, "--generation", GENERATION_ID, TURN_ID],
        &["close", SESSION_ID, "--generation", GENERATION_ID],
        &[
            "clear",
            SESSION_ID,
            "--generation",
            GENERATION_ID,
            "--expect-transcript",
            SESSION_ID,
        ],
        &["attach", SESSION_ID, "--generation", GENERATION_ID],
        &["probe"],
        &["agent", "list"],
    ];

    for args in commands {
        let mut process = pmux_process();
        process
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", directory.path())
            .arg("--socket")
            .arg(&socket)
            .args(*args);
        let output = run(process, None);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?} must exit 2: {}",
            output.stderr_text()
        );
        assert!(
            output.stdout.is_empty(),
            "{args:?} polluted stdout: {}",
            output.stdout_text()
        );
        let stderr = output.stderr_text();
        assert!(
            stderr.starts_with("error:"),
            "{args:?} must be a clap rejection: {stderr}"
        );
        assert!(
            stderr.contains("unrecognized subcommand"),
            "{args:?} must be unknown: {stderr}"
        );
        assert!(
            !socket.exists(),
            "{args:?} created a socket at {}",
            socket.display()
        );
    }
}

/// `pmux run` names no resource. Session launch flags are unknown arguments.
#[test]
fn run_refuses_session_launch_flags() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("pmuxd.sock");
    assert!(!socket.exists());

    let flags: &[&[&str]] = &[
        &["run", "--model", "sonnet", "--cwd", "/tmp"],
        &["run", "--model", "sonnet", "--claude", "/usr/bin/true"],
        &["run", "--model", "sonnet", "--permission-mode", "dont-ask"],
        &["run", "--model", "sonnet", "--denied-tool", "*"],
        &["run", "--model", "sonnet", "--system-prompt", "x"],
        &["run", "--model", "sonnet", "--session-id", SESSION_ID],
        &["run", "--model", "sonnet", "--generation", GENERATION_ID],
        &[
            "run",
            "--model",
            "sonnet",
            "--config-isolation-root",
            "/tmp",
        ],
    ];

    for args in flags {
        let mut process = pmux_process();
        process
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", directory.path())
            .arg("--socket")
            .arg(&socket)
            .args(*args);
        let output = run(process, None);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?} must exit 2: {}",
            output.stderr_text()
        );
        assert!(output.stdout.is_empty(), "{args:?} polluted stdout");
        let stderr = output.stderr_text();
        assert!(
            stderr.starts_with("error:"),
            "{args:?} must be a clap rejection: {stderr}"
        );
        assert!(
            stderr.contains("unexpected argument"),
            "{args:?} must reject the flag: {stderr}"
        );
        assert!(!socket.exists(), "{args:?} created a socket");
    }
}
