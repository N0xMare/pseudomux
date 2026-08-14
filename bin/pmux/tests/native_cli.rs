#![cfg(unix)]

#[allow(dead_code)]
mod support;

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::thread;

use pseudomux_protocol::v1::{
    ErrorBody, ErrorCode, Pong, Request, RequestEnvelope, ResponseEnvelope, ResponseResult,
};

use support::{pmux_process, run};

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

/// `--agent` distinguishes a flag the caller TYPED from a variable their
/// environment exports, IN THE REAL PROCESS.
///
/// MEASURED before this split:
///
/// ```text
/// $ PMUX_MODEL=opus pmux start --agent ... --agent-version 1 --cwd /tmp
/// pmux: --agent supplies the whole launch policy, so it cannot be combined with --model. ...
/// ```
///
/// naming a flag the caller never typed, and locking anyone with `PMUX_MODEL`
/// exported in a shell rc out of `--agent` for good. `env` is clap's lowest
/// precedence source below argv; `--agent` is a higher one, so the ambient
/// value is overridden and the override is reported by VARIABLE.
///
/// It runs the binary as a subprocess deliberately: the distinction only exists
/// inside clap's `ArgMatches`, and an in-process test would have to mutate this
/// test binary's own environment out from under every other test in it.
#[test]
fn an_exported_launch_variable_is_overridden_by_agent_and_a_typed_flag_is_still_refused() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("pmuxd.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    // One connection, for the admitted case: it reaches the transport, which is
    // proof that the CLI did not refuse it.
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&mut stream);
        assert!(matches!(request.request, Request::StartSession(_)));
        write_response(
            &mut stream,
            &ResponseEnvelope::failure(
                request.request_id,
                ErrorBody::new(ErrorCode::InvalidConfig, "no such agent").retryable(false),
            ),
        );
    });

    let agent = "00000000-0000-4000-8000-000000000006";
    let start = |extra: &[&str], model: Option<&str>| {
        let mut process = pmux_process();
        process.env_remove("PMUX_MODEL");
        if let Some(model) = model {
            process.env("PMUX_MODEL", model);
        }
        process.args([
            "--socket",
            socket.to_str().unwrap(),
            "start",
            "--agent",
            agent,
            "--agent-version",
            "1",
            "--cwd",
            directory.path().to_str().unwrap(),
        ]);
        process.args(extra);
        run(process, None)
    };

    // EXPORTED: admitted, and reported by the variable's own name.
    let exported = start(&[], Some("opus"));
    let stderr = exported.stderr_text();
    assert!(
        stderr.contains("PMUX_MODEL"),
        "the note must name the variable the value came from: {stderr}"
    );
    assert!(
        !stderr.contains("cannot be combined with"),
        "an exported variable is not a flag the caller typed: {stderr}"
    );
    assert!(
        stderr.contains("no such agent"),
        "the start must have reached the daemon: {stderr}"
    );
    server.join().unwrap();

    // TYPED, with nothing exported: refused, naming the flag.
    let typed = start(&["--model", "opus"], None);
    let stderr = typed.stderr_text();
    assert!(!typed.status.success());
    assert!(
        stderr.contains("cannot be combined with --model"),
        "a typed flag beside --agent is refused by its own spelling: {stderr}"
    );

    // TYPED while the same variable is also exported: still refused, because
    // the caller did name it on this command.
    let both = start(&["--model", "opus"], Some("sonnet"));
    assert!(!both.status.success());
    assert!(
        both.stderr_text()
            .contains("cannot be combined with --model"),
        "{}",
        both.stderr_text()
    );
}
