#![cfg(unix)]

mod support;

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use pseudomux_protocol::v1::{
    AuthPolicy, ClosePolicy, CompatibilityPolicy, ConfigSource, DisconnectAction, HealthLayer,
    HealthLayerName, InputTransport, LayerFinding, LifecycleMode, Request, RequestEnvelope,
    RetentionPolicy, SessionIdentity, SessionProbe, SystemPromptPolicy, TerminalProfile,
};
use serde_json::{Value, json};

use support::{
    GENERATION_ID, MAX_PROMPT_BYTES, NativeReply, PROCESS_TIMEOUT, ProcessOutput, SESSION_ID,
    Sandbox, TURN_ID, assert_pmux_candidate_unchanged, close_result, collect_child, command,
    completed_event, event_batch, failed_event, json_lines, pmux_process, read_native_request,
    replay_gap_batch, run, session_handle, snapshot, spawn_native_server, success, turn_accepted,
    wait_for_status, warning_event, write_native_value,
};

fn ping_reply() -> NativeReply {
    success(
        "pong",
        json!({"server_version": "test-server", "protocol_version": 1}),
    )
}

fn add_launch_args(command: &mut std::process::Command, root: &Path) {
    command
        .arg("--claude")
        .arg("/bin/sh")
        .arg("--cwd")
        .arg(root);
}

fn add_turn_target(command: &mut std::process::Command) {
    command
        .arg("turn")
        .arg(SESSION_ID)
        .arg("--generation")
        .arg(GENERATION_ID)
        .arg("--turn-id")
        .arg(TURN_ID);
}

fn completion_replies(outcome: &'static str, text: &'static str) -> Vec<NativeReply> {
    vec![
        success("turn_accepted", turn_accepted(false, 1)),
        success(
            "events",
            event_batch(vec![completed_event(1, outcome, text)], 2),
        ),
    ]
}

fn accept_stream(listener: &UnixListener) -> UnixStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                // Darwin may propagate nonblocking status from the listener
                // to the accepted descriptor. The protocol peers below use
                // bounded blocking I/O via explicit socket deadlines.
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(PROCESS_TIMEOUT)).unwrap();
                stream.set_write_timeout(Some(PROCESS_TIMEOUT)).unwrap();
                return stream;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for an exact pmux test connection"
                );
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("failed to accept pmux test connection: {error}"),
        }
    }
}

fn accept_native_request(listener: &UnixListener) -> (UnixStream, RequestEnvelope) {
    let mut stream = accept_stream(listener);
    let request = read_native_request(&mut stream);
    (stream, request)
}

fn write_success(
    stream: &mut UnixStream,
    request: &RequestEnvelope,
    kind: &'static str,
    data: Value,
) {
    write_native_value(
        stream,
        &json!({
            "version": 1,
            "request_id": request.request_id,
            "result": {"type": kind, "data": data},
        }),
    );
}

fn spawn_interrupt_server(
    listener: UnixListener,
    include_start_and_close: bool,
    waiting: mpsc::Sender<()>,
) -> thread::JoinHandle<Vec<RequestEnvelope>> {
    thread::spawn(move || {
        let mut requests = Vec::new();
        if include_start_and_close {
            let (mut stream, request) = accept_native_request(&listener);
            write_success(&mut stream, &request, "session_started", session_handle());
            requests.push(request);
        }

        let (mut stream, request) = accept_native_request(&listener);
        write_success(
            &mut stream,
            &request,
            "turn_accepted",
            turn_accepted(false, 1),
        );
        requests.push(request);

        // Deliberately retain the exact long-poll connection without a
        // response. This proves the real CLI handles SIGINT while blocked on
        // its public event boundary rather than after a synthetic failure.
        let (subscription, request) = accept_native_request(&listener);
        requests.push(request);
        waiting.send(()).unwrap();

        let (mut stream, request) = accept_native_request(&listener);
        write_success(
            &mut stream,
            &request,
            "turn_cancelled",
            json!({
                "session_id": SESSION_ID,
                "generation_id": GENERATION_ID,
                "turn_id": TURN_ID,
                "outcome": "cancelled",
                "session_state": "ready",
            }),
        );
        requests.push(request);

        if include_start_and_close {
            let (mut stream, request) = accept_native_request(&listener);
            write_success(&mut stream, &request, "session_closed", close_result(true));
            requests.push(request);
        }

        drop(subscription);
        requests
    })
}

struct ExactChild {
    child: Option<Child>,
}

impl ExactChild {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    fn child(&self) -> &Child {
        self.child.as_ref().unwrap()
    }

    fn into_child(mut self) -> Child {
        self.child.take().unwrap()
    }
}

impl Drop for ExactChild {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn spawn_piped(mut command: Command) -> ExactChild {
    ExactChild::new(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    )
}

#[allow(unsafe_code)]
fn interrupt_exact_child(child: &Child) {
    let pid = i32::try_from(child.id()).unwrap();
    // SAFETY: `pid` is the positive PID retained directly from this test's
    // still-owned `Child`; no process group, wildcard, or discovered PID is
    // used. The child is reaped through the same handle below.
    let result = unsafe { libc::kill(pid, libc::SIGINT) };
    assert_eq!(
        result,
        0,
        "failed to signal exact pmux child {pid}: {}",
        std::io::Error::last_os_error()
    );
}

#[derive(Debug, PartialEq, Eq)]
struct TermiosSnapshot {
    input_flags: libc::tcflag_t,
    output_flags: libc::tcflag_t,
    control_flags: libc::tcflag_t,
    local_flags: libc::tcflag_t,
    control_characters: Vec<libc::cc_t>,
    input_speed: libc::speed_t,
    output_speed: libc::speed_t,
}

#[allow(unsafe_code)]
fn termios_snapshot(terminal: &File) -> TermiosSnapshot {
    let mut attributes = MaybeUninit::<libc::termios>::uninit();
    // SAFETY: `attributes` points to writable storage for one termios value,
    // and `terminal` is a live slave PTY descriptor owned by the test.
    let result = unsafe { libc::tcgetattr(terminal.as_raw_fd(), attributes.as_mut_ptr()) };
    assert_eq!(
        result,
        0,
        "failed to inspect test PTY termios: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: tcgetattr returned success and initialized the complete value.
    let attributes = unsafe { attributes.assume_init() };
    // SAFETY: both functions only inspect the initialized termios value.
    let input_speed = unsafe { libc::cfgetispeed(&attributes) };
    // SAFETY: both functions only inspect the initialized termios value.
    let output_speed = unsafe { libc::cfgetospeed(&attributes) };
    TermiosSnapshot {
        input_flags: attributes.c_iflag,
        output_flags: attributes.c_oflag,
        control_flags: attributes.c_cflag,
        local_flags: attributes.c_lflag,
        control_characters: attributes.c_cc.to_vec(),
        input_speed,
        output_speed,
    }
}

fn assert_terminal_restored(restored: &TermiosSnapshot, original: &TermiosSnapshot) {
    assert_eq!(restored.input_flags, original.input_flags);
    assert_eq!(restored.output_flags, original.output_flags);
    assert_eq!(restored.control_flags, original.control_flags);
    // PENDIN is a kernel-maintained status bit, not a requested terminal
    // configuration. Darwin may set it when switching back to canonical mode
    // after raw input even though tcsetattr restored every configured bit.
    // Compare every stable local-mode bit and all remaining termios state.
    assert_eq!(
        restored.local_flags & !libc::PENDIN,
        original.local_flags & !libc::PENDIN
    );
    assert_eq!(restored.control_characters, original.control_characters);
    assert_eq!(restored.input_speed, original.input_speed);
    assert_eq!(restored.output_speed, original.output_speed);
}

#[allow(unsafe_code)]
fn open_test_pty(rows: u16, cols: u16) -> (File, File) {
    let mut master_fd = -1;
    let mut slave_fd = -1;
    let mut size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: all output pointers target initialized writable storage; the
    // optional name and termios pointers are null; `size` is initialized.
    let result = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    assert_eq!(
        result,
        0,
        "failed to create test PTY: {}",
        std::io::Error::last_os_error()
    );
    assert!(master_fd >= 0 && slave_fd >= 0);
    // SAFETY: openpty returned two fresh owned descriptors on success.
    let master = unsafe { File::from_raw_fd(master_fd) };
    // SAFETY: openpty returned two fresh owned descriptors on success.
    let slave = unsafe { File::from_raw_fd(slave_fd) };
    (master, slave)
}

#[allow(unsafe_code)]
fn set_nonblocking(file: &File) {
    // SAFETY: F_GETFL only reads flags from this live owned descriptor.
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    assert!(
        flags >= 0,
        "failed to read PTY flags: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: F_SETFL updates only status flags on this live owned descriptor.
    let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) };
    assert_eq!(
        result,
        0,
        "failed to make PTY nonblocking: {}",
        std::io::Error::last_os_error()
    );
}

fn spawn_pty_reader(mut reader: File, completed: Arc<AtomicBool>) -> thread::JoinHandle<Vec<u8>> {
    set_nonblocking(&reader);
    thread::spawn(move || {
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        let mut idle_after_completion = None;
        let mut output = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => return output,
                Ok(length) => {
                    output.extend_from_slice(&buffer[..length]);
                    idle_after_completion = None;
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if completed.load(Ordering::SeqCst) {
                        let idle = idle_after_completion.get_or_insert_with(Instant::now);
                        if idle.elapsed() >= Duration::from_millis(100) {
                            return output;
                        }
                    }
                    assert!(
                        Instant::now() < deadline,
                        "timed out draining exact test PTY output"
                    );
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) if error.raw_os_error() == Some(libc::EIO) => return output,
                Err(error) => panic!("failed to read test PTY output: {error}"),
            }
        }
    })
}

fn write_pty_input(master: &mut File, mut input: &[u8]) {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    while !input.is_empty() {
        match master.write(input) {
            Ok(0) => panic!("test PTY accepted zero input bytes"),
            Ok(length) => input = &input[length..],
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "timed out writing exact test PTY input"
                );
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("failed to write test PTY input: {error}"),
        }
    }
}

fn write_attach_data(stream: &mut UnixStream, payload: &[u8]) -> io::Result<()> {
    // Preserve the three legal Unix-stream fragments that originally exposed
    // the attach failure.
    stream.write_all(&[1])?;
    stream.write_all(
        &u32::try_from(payload.len())
            .expect("attach test payload length fits u32")
            .to_le_bytes(),
    )?;
    stream.write_all(payload)?;
    stream.flush()?;
    Ok(())
}

fn encoded_attach_data(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(1);
    frame.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_le_bytes());
    frame.extend_from_slice(payload);
    frame
}

#[derive(Debug)]
enum ClientAttachFrame {
    Data(Vec<u8>),
    Resize { rows: u16, cols: u16 },
}

fn read_client_attach_frame(stream: &mut UnixStream) -> io::Result<ClientAttachFrame> {
    let mut tag = [0_u8; 1];
    stream.read_exact(&mut tag)?;
    match tag[0] {
        1 => {
            let mut length = [0_u8; 4];
            stream.read_exact(&mut length)?;
            let mut payload = vec![0_u8; u32::from_le_bytes(length) as usize];
            stream.read_exact(&mut payload)?;
            Ok(ClientAttachFrame::Data(payload))
        }
        2 => {
            let mut size = [0_u8; 4];
            stream.read_exact(&mut size)?;
            Ok(ClientAttachFrame::Resize {
                cols: u16::from_le_bytes([size[0], size[1]]),
                rows: u16::from_le_bytes([size[2], size[3]]),
            })
        }
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected attach frame tag from CLI: {other}"),
        )),
    }
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Debug)]
struct AttachObservation {
    authenticated_token: Vec<u8>,
    resize: (u16, u16),
    input: Vec<u8>,
    extra_connection: bool,
}

fn spawn_attach_endpoint(
    listener: UnixListener,
    expected_input: &'static [u8],
    ready: mpsc::Sender<()>,
) -> thread::JoinHandle<Result<AttachObservation, String>> {
    thread::spawn(move || {
        let mut stream = accept_stream(&listener);
        let mut length = [0_u8; 4];
        stream
            .read_exact(&mut length)
            .map_err(|error| format!("failed to read capability token length: {error}"))?;
        let mut authenticated_token = vec![0_u8; u32::from_be_bytes(length) as usize];
        stream
            .read_exact(&mut authenticated_token)
            .map_err(|error| format!("failed to read capability token: {error}"))?;

        const INITIAL_PAYLOAD: &[u8] = b"proxy-output\r\n";
        write_attach_data(&mut stream, INITIAL_PAYLOAD)
            .map_err(|error| format!("failed to write initial attach frame: {error}"))?;
        let resize = match read_client_attach_frame(&mut stream).map_err(|error| {
            format!(
                "failed before initial client resize: {error}; sent_initial_frame={}",
                hex_bytes(&encoded_attach_data(INITIAL_PAYLOAD))
            )
        })? {
            ClientAttachFrame::Resize { rows, cols } => (rows, cols),
            ClientAttachFrame::Data(data) => {
                return Err(format!(
                    "received terminal input before raw attach readiness: {data:?}; sent_initial_frame={}",
                    hex_bytes(&encoded_attach_data(INITIAL_PAYLOAD))
                ));
            }
        };
        ready
            .send(())
            .map_err(|_| "attach readiness receiver disappeared".to_owned())?;

        let mut input = Vec::new();
        while input.len() < expected_input.len() {
            match read_client_attach_frame(&mut stream).map_err(|error| {
                format!(
                    "failed while reading client input: {error}; sent_initial_frame={}",
                    hex_bytes(&encoded_attach_data(INITIAL_PAYLOAD))
                )
            })? {
                ClientAttachFrame::Data(data) => input.extend_from_slice(&data),
                ClientAttachFrame::Resize { .. } => {}
            }
        }
        if input != expected_input {
            return Err(format!(
                "client input differed: expected={expected_input:?} observed={input:?}"
            ));
        }
        const FINAL_PAYLOAD: &[u8] = b"proxy-finished\r\n\x1b[?1049l";
        write_attach_data(&mut stream, FINAL_PAYLOAD)
            .map_err(|error| format!("failed to write final attach frame: {error}"))?;
        drop(stream);

        let extra_connection = match listener.accept() {
            Ok((extra, _)) => {
                drop(extra);
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => false,
            Err(error) => {
                return Err(format!(
                    "failed to verify one-use attach endpoint: {error}; sent_final_frame={}",
                    hex_bytes(&encoded_attach_data(FINAL_PAYLOAD))
                ));
            }
        };
        Ok(AttachObservation {
            authenticated_token,
            resize,
            input,
            extra_connection,
        })
    })
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

#[test]
fn turn_sigint_cancels_the_exact_active_turn_and_emits_no_success() {
    let sandbox = Sandbox::new("turn-sigint");
    let (waiting_tx, waiting_rx) = mpsc::channel();
    let server = spawn_interrupt_server(sandbox.bind(), false, waiting_tx);

    let mut command_line = command(&sandbox.socket, &sandbox.root);
    command_line.args(["--output", "ndjson"]);
    add_turn_target(&mut command_line);
    command_line.arg("interrupt-only-prompt");
    let child = spawn_piped(command_line);

    waiting_rx
        .recv_timeout(PROCESS_TIMEOUT)
        .expect("CLI never reached its retained subscribe_events wait");
    interrupt_exact_child(child.child());
    let exact_pid = child.child().id();
    let output = collect_child(child.into_child());

    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stdout.is_empty(),
        "unexpected success bytes on stdout"
    );
    let stderr = output.stderr_text();
    assert!(stderr.contains("interrupt received; cancelling turn"));
    assert!(stderr.contains("turn interrupted by user"));
    assert!(!stderr.contains("interrupt-only-prompt"));
    assert!(!stderr.contains("environment-secret"));

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 3);
    let Request::RunTurn(turn) = &requests[0].request else {
        panic!("CLI process {exact_pid} did not issue run_turn first");
    };
    assert_eq!(turn.session_id.to_string(), SESSION_ID);
    assert_eq!(turn.generation_id.to_string(), GENERATION_ID);
    assert_eq!(turn.turn.turn_id.to_string(), TURN_ID);
    assert_eq!(turn.turn.prompt, "interrupt-only-prompt");

    let Request::SubscribeEvents(subscription) = &requests[1].request else {
        panic!("CLI process {exact_pid} did not retain subscribe_events");
    };
    assert_eq!(subscription.session_id.to_string(), SESSION_ID);
    assert_eq!(subscription.generation_id.to_string(), GENERATION_ID);
    assert_eq!(subscription.after_sequence, 0);

    let Request::CancelTurn(cancel) = &requests[2].request else {
        panic!("SIGINT did not issue cancel_turn from CLI process {exact_pid}");
    };
    assert_eq!(cancel.session_id.to_string(), SESSION_ID);
    assert_eq!(cancel.generation_id.to_string(), GENERATION_ID);
    assert_eq!(cancel.turn_id.to_string(), TURN_ID);
}

#[test]
fn run_sigint_cancels_then_force_closes_and_withholds_success() {
    let sandbox = Sandbox::new("run-sigint");
    let (waiting_tx, waiting_rx) = mpsc::channel();
    let server = spawn_interrupt_server(sandbox.bind(), true, waiting_tx);

    let mut command_line = command(&sandbox.socket, &sandbox.root);
    command_line.args([
        "--output",
        "ndjson",
        "oneshot",
        "--session-id",
        SESSION_ID,
        "--turn-id",
        TURN_ID,
    ]);
    add_launch_args(&mut command_line, &sandbox.root);
    command_line.arg("interrupt-and-close-prompt");
    let child = spawn_piped(command_line);

    waiting_rx
        .recv_timeout(PROCESS_TIMEOUT)
        .expect("run never reached its retained subscribe_events wait");
    interrupt_exact_child(child.child());
    let exact_pid = child.child().id();
    let output = collect_child(child.into_child());

    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stdout.is_empty(),
        "run emitted a false result marker"
    );
    let stderr = output.stderr_text();
    assert!(stderr.contains("interrupt received; cancelling turn"));
    assert!(stderr.contains("turn interrupted by user"));
    assert!(!stderr.contains("interrupt-and-close-prompt"));
    assert!(!stderr.contains("environment-secret"));

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 5);
    assert!(matches!(requests[0].request, Request::StartSession(_)));
    assert!(matches!(requests[1].request, Request::RunTurn(_)));
    assert!(matches!(requests[2].request, Request::SubscribeEvents(_)));
    let Request::CancelTurn(cancel) = &requests[3].request else {
        panic!("SIGINT did not issue cancel_turn from CLI process {exact_pid}");
    };
    assert_eq!(cancel.session_id.to_string(), SESSION_ID);
    assert_eq!(cancel.generation_id.to_string(), GENERATION_ID);
    assert_eq!(cancel.turn_id.to_string(), TURN_ID);
    let Request::CloseSession(close) = &requests[4].request else {
        panic!("interrupted run from CLI process {exact_pid} did not close");
    };
    assert_eq!(close.session_id.to_string(), SESSION_ID);
    assert_eq!(close.generation_id.to_string(), GENERATION_ID);
    assert_eq!(close.policy, ClosePolicy::Force);
}

#[test]
fn text_attach_proxies_bytes_restores_terminal_and_never_leaks_capability() {
    const ROWS: u16 = 37;
    const COLS: u16 = 111;
    const TOKEN: &str = "one-use-attach-token-never-print";
    const CLIENT_INPUT: &[u8] = b"exact-client-input";

    let sandbox = Sandbox::new("text-attach-pty");
    let endpoint = sandbox.root.join("attach.sock");
    let attach_listener = UnixListener::bind(&endpoint).unwrap();
    fs::set_permissions(&endpoint, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        fs::metadata(&endpoint).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let (ready_tx, ready_rx) = mpsc::channel();
    let attach_server = spawn_attach_endpoint(attach_listener, CLIENT_INPUT, ready_tx);

    let public_server = spawn_native_server(
        sandbox.bind(),
        vec![success(
            "attach_capability",
            json!({
                "session_id": SESSION_ID,
                "generation_id": GENERATION_ID,
                "token": TOKEN,
                "endpoint": endpoint,
                "expires_at_ms": 9_999_999_999_u64,
                "read_only": false,
            }),
        )],
    );

    let (mut master, slave) = open_test_pty(ROWS, COLS);
    let observer = slave.try_clone().unwrap();
    let original_termios = termios_snapshot(&observer);
    assert_ne!(original_termios.local_flags & libc::ECHO, 0);
    assert_ne!(original_termios.local_flags & libc::ICANON, 0);
    let reader_complete = Arc::new(AtomicBool::new(false));
    let pty_reader = spawn_pty_reader(master.try_clone().unwrap(), Arc::clone(&reader_complete));

    let mut attach = command(&sandbox.socket, &sandbox.root);
    attach
        .env("TERM", "xterm-256color")
        .args([
            "--output",
            "text",
            "attach",
            SESSION_ID,
            "--generation",
            GENERATION_ID,
            "--rows",
            &ROWS.to_string(),
            "--cols",
            &COLS.to_string(),
        ])
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::piped());
    let child = ExactChild::new(attach.spawn().unwrap());
    drop(slave);

    ready_rx
        .recv_timeout(PROCESS_TIMEOUT)
        .expect("text attach never entered raw mode and sent its initial resize");
    let active_termios = termios_snapshot(&observer);
    assert_ne!(active_termios, original_termios);
    assert_eq!(active_termios.local_flags & (libc::ECHO | libc::ICANON), 0);
    write_pty_input(&mut master, CLIENT_INPUT);

    let mut child = child.into_child();
    let status = wait_for_status(&mut child);
    let restored_termios = termios_snapshot(&observer);
    let mut stderr = Vec::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_end(&mut stderr)
        .unwrap();
    assert_pmux_candidate_unchanged();
    reader_complete.store(true, Ordering::SeqCst);
    drop(observer);
    drop(master);
    let terminal_output = pty_reader.join().unwrap();

    let attach_observation = attach_server.join().unwrap();
    let requests = public_server.join().unwrap();
    let exact_request_order = requests
        .iter()
        .map(|request| match &request.request {
            Request::AttachSession(_) => "attach_session",
            _ => "unexpected",
        })
        .collect::<Vec<_>>();

    assert!(
        status.success(),
        "{}; attach_server={attach_observation:?}; public_request_order={exact_request_order:?}",
        String::from_utf8_lossy(&stderr)
    );
    assert_terminal_restored(&restored_termios, &original_termios);
    assert!(contains_bytes(&terminal_output, b"proxy-output"));
    assert!(contains_bytes(&terminal_output, b"proxy-finished"));
    assert!(contains_bytes(&terminal_output, b"\x1b[?1049l"));
    assert!(!contains_bytes(&terminal_output, CLIENT_INPUT));
    assert!(!contains_bytes(&terminal_output, TOKEN.as_bytes()));
    assert!(!contains_bytes(
        &terminal_output,
        endpoint.as_os_str().as_encoded_bytes()
    ));
    assert!(!contains_bytes(&stderr, TOKEN.as_bytes()));
    assert!(!contains_bytes(
        &stderr,
        endpoint.as_os_str().as_encoded_bytes()
    ));
    assert!(stderr.is_empty(), "{}", String::from_utf8_lossy(&stderr));

    let observation = attach_observation.unwrap();
    assert_eq!(observation.authenticated_token, TOKEN.as_bytes());
    assert_eq!(observation.resize, (ROWS, COLS));
    assert_eq!(observation.input, CLIENT_INPUT);
    assert!(!observation.extra_connection);

    assert_eq!(requests.len(), 1);
    let Request::AttachSession(request) = &requests[0].request else {
        panic!("text attach did not request a capability");
    };
    assert_eq!(request.session_id.to_string(), SESSION_ID);
    assert_eq!(request.generation_id.to_string(), GENERATION_ID);
    assert!(!request.read_only);
    let size = request.size.as_ref().unwrap();
    assert_eq!((size.rows, size.cols), (ROWS, COLS));

    fs::remove_file(&endpoint).unwrap();
    assert!(!endpoint.exists());
}

#[test]
fn ping_covers_text_json_ndjson_and_exact_native_requests() {
    let sandbox = Sandbox::new("ping");
    let server = spawn_native_server(
        sandbox.bind(),
        vec![ping_reply(), ping_reply(), ping_reply(), ping_reply()],
    );

    let mut text = command(&sandbox.socket, &sandbox.root);
    text.args(["--output", "text", "ping"]);
    let text = run(text, None);
    assert!(text.status.success());
    assert_eq!(text.stdout_text(), "pong server=test-server protocol=1\n");
    assert!(text.stderr.is_empty());

    let mut json_command = command(&sandbox.socket, &sandbox.root);
    json_command.args(["--output", "json", "ping"]);
    let json_output = run(json_command, None);
    assert!(json_output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&json_output.stdout).unwrap(),
        json!({"server_version": "test-server", "protocol_version": 1})
    );
    assert!(json_output.stderr.is_empty());

    let mut ndjson = command(&sandbox.socket, &sandbox.root);
    ndjson.args(["--output", "ndjson", "ping"]);
    let ndjson = run(ndjson, None);
    assert!(ndjson.status.success());
    assert_eq!(
        json_lines(&ndjson.stdout),
        [json!({
            "type": "pong",
            "data": {"server_version": "test-server", "protocol_version": 1},
        })]
    );
    assert!(ndjson.stderr.is_empty());

    let mut environment = pmux_process();
    environment
        .env_clear()
        .env("PMUX_SOCKET", &sandbox.socket)
        .args(["--output", "json", "ping"]);
    let environment = run(environment, None);
    assert!(environment.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&environment.stdout).unwrap()["server_version"],
        "test-server"
    );
    assert!(environment.stderr.is_empty());

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(
        requests
            .iter()
            .all(|request| request.version == 1 && matches!(request.request, Request::Ping))
    );
}

#[test]
fn start_maps_the_complete_launch_dto_and_output_shapes() {
    let sandbox = Sandbox::new("start");
    let settings = sandbox.root.join("settings.json");
    let plugin = sandbox.root.join("plugin");
    fs::write(&settings, b"{}").unwrap();
    fs::create_dir(&plugin).unwrap();
    let server = spawn_native_server(
        sandbox.bind(),
        vec![
            success("session_started", session_handle()),
            success("session_started", session_handle()),
            success("session_started", session_handle()),
        ],
    );

    let mut json_command = command(&sandbox.socket, &sandbox.root);
    json_command.args(["--output", "json", "start", "--session-id", SESSION_ID]);
    add_launch_args(&mut json_command, &sandbox.root);
    json_command
        .args([
            "--model",
            "claude-test",
            "--effort",
            "xhigh",
            "--permission-mode",
            "plan",
            "--allowed-tool",
            "Read,Glob",
            "--denied-tool",
            "Bash",
            "--settings",
        ])
        .arg(&settings)
        .args([
            "--settings-json",
            r#"{"hooks":{"Stop":[]}}"#,
            "--mcp-json",
            r#"{"servers":{}}"#,
            "--plugin-dir",
        ])
        .arg(&plugin)
        .args([
            "--append-system-prompt",
            "stay bounded",
            "--auth",
            "inherit",
            "--rows",
            "30",
            "--cols",
            "100",
            "--terminal-profile",
            "rmux-standard",
            "--input-transport",
            "attached-stream",
            "--lifecycle",
            "hybrid",
            "--hook-timeout-ms",
            "3210",
            "--retention",
            "one-shot",
            "--compatibility",
            "allow-untested",
        ]);
    let json_output = run(json_command, None);
    assert!(
        json_output.status.success(),
        "{}",
        json_output.stderr_text()
    );
    let handle: Value = serde_json::from_slice(&json_output.stdout).unwrap();
    assert_eq!(handle["session_id"], SESSION_ID);
    assert_eq!(handle["compatibility"]["tested"], true);
    assert!(json_output.stderr.is_empty());

    let mut text = command(&sandbox.socket, &sandbox.root);
    text.args(["--output", "text", "start", "--session-id", SESSION_ID]);
    add_launch_args(&mut text, &sandbox.root);
    let text = run(text, None);
    assert!(text.status.success());
    assert_eq!(
        text.stdout_text(),
        format!("session_id={SESSION_ID}\ngeneration_id={GENERATION_ID}\n")
    );
    assert!(text.stderr.is_empty());

    let mut ndjson = command(&sandbox.socket, &sandbox.root);
    ndjson.args(["--output", "ndjson", "start", "--session-id", SESSION_ID]);
    add_launch_args(&mut ndjson, &sandbox.root);
    let ndjson = run(ndjson, None);
    assert!(ndjson.status.success());
    assert_eq!(json_lines(&ndjson.stdout)[0]["type"], "session_started");
    assert!(ndjson.stderr.is_empty());

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 3);
    let Request::StartSession(start) = &requests[0].request else {
        panic!("expected start_session");
    };
    assert_eq!(
        start.identity,
        SessionIdentity::New {
            session_id: Some(SESSION_ID.parse().unwrap())
        }
    );
    assert_eq!(
        start.cwd,
        sandbox.root.canonicalize().unwrap().to_string_lossy()
    );
    assert_eq!(
        start.claude.as_ref().expect("inline launch").executable,
        fs::canonicalize("/bin/sh").unwrap().to_string_lossy()
    );
    assert_eq!(
        start
            .claude
            .as_ref()
            .expect("inline launch")
            .model
            .as_deref(),
        Some("claude-test")
    );
    assert_eq!(
        serde_json::to_value(start.claude.as_ref().expect("inline launch").effort).unwrap(),
        "xhigh"
    );
    assert_eq!(
        serde_json::to_value(
            start
                .claude
                .as_ref()
                .expect("inline launch")
                .permission_mode
        )
        .unwrap(),
        "plan"
    );
    assert_eq!(
        start.claude.as_ref().expect("inline launch").allowed_tools,
        ["Read", "Glob"]
    );
    assert_eq!(
        start.claude.as_ref().expect("inline launch").denied_tools,
        ["Bash"]
    );
    assert_eq!(
        start.claude.as_ref().expect("inline launch").settings.len(),
        2
    );
    assert!(matches!(
        start.claude.as_ref().expect("inline launch").settings[0],
        ConfigSource::File { .. }
    ));
    assert!(matches!(
        start.claude.as_ref().expect("inline launch").settings[1],
        ConfigSource::Inline { .. }
    ));
    assert_eq!(
        start
            .claude
            .as_ref()
            .expect("inline launch")
            .mcp_configs
            .len(),
        1
    );
    assert_eq!(
        start.claude.as_ref().expect("inline launch").plugin_dirs,
        [plugin.canonicalize().unwrap().to_string_lossy()]
    );
    assert_eq!(
        start.claude.as_ref().expect("inline launch").system_prompt,
        SystemPromptPolicy::Append {
            prompt: "stay bounded".into()
        }
    );
    assert!(
        start
            .claude
            .as_ref()
            .expect("inline launch")
            .extra_args
            .is_empty()
    );
    assert_eq!(start.auth_policy, AuthPolicy::Inherit);
    assert_eq!(start.terminal.rows, 30);
    assert_eq!(start.terminal.cols, 100);
    assert_eq!(start.terminal.profile, TerminalProfile::RmuxStandard);
    assert_eq!(
        start.terminal.input_transport,
        InputTransport::AttachedStream
    );
    assert_eq!(
        start.lifecycle,
        LifecycleMode::Hybrid {
            hook_timeout_ms: 3210
        }
    );
    assert_eq!(start.retention, RetentionPolicy::OneShot);
    assert_eq!(start.compatibility, CompatibilityPolicy::AllowUntested);
    assert_eq!(
        start
            .environment
            .snapshot
            .get("PMUX_TEST_ENV")
            .map(String::as_str),
        Some("captured")
    );
    assert!(
        requests[1..]
            .iter()
            .all(|request| matches!(request.request, Request::StartSession(_)))
    );
}

#[test]
fn inspect_cancel_close_and_attach_metadata_map_exact_dtos() {
    let sandbox = Sandbox::new("operations");
    let cancel = json!({
        "session_id": SESSION_ID,
        "generation_id": GENERATION_ID,
        "turn_id": TURN_ID,
        "outcome": "cancelled",
        "session_state": "ready",
    });
    let attach = json!({
        "session_id": SESSION_ID,
        "generation_id": GENERATION_ID,
        "token": "sensitive-attach-token",
        "endpoint": "/private/attach.sock",
        "expires_at_ms": 1000,
        "read_only": false,
    });
    let server = spawn_native_server(
        sandbox.bind(),
        vec![
            success("session_snapshot", snapshot(4)),
            success("turn_cancelled", cancel),
            success("session_closed", close_result(true)),
            success("attach_capability", attach),
        ],
    );

    let mut inspect = command(&sandbox.socket, &sandbox.root);
    inspect.args([
        "--output",
        "text",
        "inspect",
        SESSION_ID,
        "--generation",
        GENERATION_ID,
    ]);
    let inspect = run(inspect, None);
    assert!(inspect.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&inspect.stdout).unwrap()["last_sequence"],
        4
    );
    assert!(inspect.stderr.is_empty());

    let mut cancel = command(&sandbox.socket, &sandbox.root);
    cancel.args([
        "--output",
        "ndjson",
        "cancel",
        SESSION_ID,
        "--generation",
        GENERATION_ID,
        TURN_ID,
    ]);
    let cancel = run(cancel, None);
    assert!(cancel.status.success());
    assert_eq!(json_lines(&cancel.stdout)[0]["type"], "turn_cancelled");
    assert!(cancel.stderr.is_empty());

    let mut close = command(&sandbox.socket, &sandbox.root);
    close.args([
        "--output",
        "json",
        "close",
        SESSION_ID,
        "--generation",
        GENERATION_ID,
        "--policy",
        "force",
    ]);
    let close = run(close, None);
    assert!(close.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&close.stdout).unwrap()["process_reaped"],
        true
    );
    assert!(close.stderr.is_empty());

    let mut attach = command(&sandbox.socket, &sandbox.root);
    attach.args([
        "--output",
        "json",
        "attach",
        SESSION_ID,
        "--generation",
        GENERATION_ID,
        "--rows",
        "40",
        "--cols",
        "120",
    ]);
    let attach = run(attach, None);
    assert!(attach.status.success());
    let attach_json: Value = serde_json::from_slice(&attach.stdout).unwrap();
    assert_eq!(attach_json["token"], "sensitive-attach-token");
    assert_eq!(
        attach
            .stdout_text()
            .matches("sensitive-attach-token")
            .count(),
        1
    );
    assert!(attach.stderr.is_empty());

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        serde_json::to_value(&requests[0].request).unwrap(),
        json!({
            "method": "inspect_session",
            "params": {"session_id": SESSION_ID, "generation_id": GENERATION_ID},
        })
    );
    assert_eq!(
        serde_json::to_value(&requests[1].request).unwrap(),
        json!({
            "method": "cancel_turn",
            "params": {
                "session_id": SESSION_ID,
                "generation_id": GENERATION_ID,
                "turn_id": TURN_ID,
            },
        })
    );
    assert_eq!(
        serde_json::to_value(&requests[2].request).unwrap(),
        json!({
            "method": "close_session",
            "params": {
                "session_id": SESSION_ID,
                "generation_id": GENERATION_ID,
                "policy": "force",
            },
        })
    );
    assert_eq!(
        serde_json::to_value(&requests[3].request).unwrap(),
        json!({
            "method": "attach_session",
            "params": {
                "session_id": SESSION_ID,
                "generation_id": GENERATION_ID,
                "read_only": false,
                "size": {"rows": 40, "cols": 120},
            },
        })
    );
}

#[test]
fn operation_specific_text_json_and_ndjson_shapes_are_stable() {
    let sandbox = Sandbox::new("operation-modes");
    let cancel = || {
        json!({
            "session_id": SESSION_ID,
            "generation_id": GENERATION_ID,
            "turn_id": TURN_ID,
            "outcome": "already_terminal",
            "session_state": "ready",
        })
    };
    let attach = || {
        json!({
            "session_id": SESSION_ID,
            "generation_id": GENERATION_ID,
            "token": "one-use-token",
            "endpoint": "/private/attach.sock",
            "expires_at_ms": 1000,
            "read_only": false,
        })
    };
    let server = spawn_native_server(
        sandbox.bind(),
        vec![
            success("session_snapshot", snapshot(4)),
            success("session_snapshot", snapshot(4)),
            success("turn_cancelled", cancel()),
            success("turn_cancelled", cancel()),
            success("session_closed", close_result(true)),
            success(
                "session_closed",
                json!({
                    "session_id": SESSION_ID,
                    "generation_id": GENERATION_ID,
                    "already_closed": true,
                    "process_reaped": true,
                }),
            ),
            success("attach_capability", attach()),
        ],
    );

    for mode in ["json", "ndjson"] {
        let mut inspect = command(&sandbox.socket, &sandbox.root);
        inspect.args([
            "--output",
            mode,
            "inspect",
            SESSION_ID,
            "--generation",
            GENERATION_ID,
        ]);
        let output = run(inspect, None);
        assert!(output.status.success());
        if mode == "ndjson" {
            assert_eq!(json_lines(&output.stdout)[0]["type"], "session_snapshot");
        } else {
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout).unwrap()["last_sequence"],
                4
            );
        }
        assert!(output.stderr.is_empty());
    }

    for mode in ["text", "json"] {
        let mut cancel_command = command(&sandbox.socket, &sandbox.root);
        cancel_command.args([
            "--output",
            mode,
            "cancel",
            SESSION_ID,
            "--generation",
            GENERATION_ID,
            TURN_ID,
        ]);
        let output = run(cancel_command, None);
        assert!(output.status.success());
        if mode == "text" {
            assert!(output.stdout_text().contains("outcome=AlreadyTerminal"));
        } else {
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout).unwrap()["outcome"],
                "already_terminal"
            );
        }
        assert!(output.stderr.is_empty());
    }

    for mode in ["text", "ndjson"] {
        let mut close = command(&sandbox.socket, &sandbox.root);
        close.args([
            "--output",
            mode,
            "close",
            SESSION_ID,
            "--generation",
            GENERATION_ID,
        ]);
        let output = run(close, None);
        assert!(output.status.success());
        if mode == "text" {
            assert!(output.stdout_text().contains("process_reaped=true"));
        } else {
            let records = json_lines(&output.stdout);
            assert_eq!(records[0]["type"], "session_closed");
            assert_eq!(records[0]["data"]["already_closed"], true);
        }
        assert!(output.stderr.is_empty());
    }

    let mut attach_command = command(&sandbox.socket, &sandbox.root);
    attach_command.args([
        "--output",
        "ndjson",
        "attach",
        SESSION_ID,
        "--generation",
        GENERATION_ID,
    ]);
    let output = run(attach_command, None);
    assert!(output.status.success());
    let records = json_lines(&output.stdout);
    assert_eq!(records[0]["type"], "attach_capability");
    assert_eq!(records[0]["data"]["token"], "one-use-token");
    assert!(output.stderr.is_empty());

    assert_eq!(server.join().unwrap().len(), 7);
}

/// A complete health tree for the sessions the same reply carries.
///
/// Written as "every layer" rather than as a literal list so a layer added to
/// `HealthLayerName` joins it automatically. A fixture that named a fixed
/// subset would go stale into `not_established` -- which the daemon would then
/// report as `unproven`, correctly, and this test would fail for a reason that
/// has nothing to do with what it checks.
///
/// The `sessions` layer is DERIVED, by calling the producer the daemon itself
/// calls. Every other layer is stamped `exercised`, which is a shape those
/// layers really do take.
///
/// This is not tidiness. The previous version stamped `exercised` on ALL EIGHT
/// layers and was handed `sessions: []`, then asserted `status == "healthy"` on
/// the result. No daemon can emit that pair: `sessions_layer` is a total
/// function of the session list, and for an empty list it returns
/// `not_established` (before the encoding was split) or `nothing_to_exercise`
/// (after) -- never `exercised`. So the assertion held over a report that does
/// not exist, and the encoding defect it was positioned to catch shipped
/// underneath it. Hardening the fixture to `HealthLayerName::ALL` at HEAD made
/// it more thorough about the wrong thing.
fn every_layer_healthy_for(sessions: &Value) -> Value {
    let probes: Vec<SessionProbe> =
        serde_json::from_value(sessions.clone()).expect("the session list is a real probe list");
    Value::Array(
        HealthLayerName::ALL
            .iter()
            .map(|layer| {
                let entry = if *layer == HealthLayerName::Sessions {
                    HealthLayer::for_sessions(&probes)
                } else {
                    HealthLayer::new(*layer, LayerFinding::Exercised, "exercised", Value::Null)
                };
                serde_json::to_value(entry).expect("a layer serializes")
            })
            .collect(),
    )
}

fn diagnosis_reply(sessions: Value) -> NativeReply {
    diagnosis_reply_with_layers(every_layer_healthy_for(&sessions), sessions)
}

fn diagnosis_reply_with_layers(layers: Value, sessions: Value) -> NativeReply {
    success(
        "diagnosis",
        json!({
            "layers": layers,
            "runtime": {
                "outcome": "pass",
                "finding": "private_runtime_responsive",
                "elapsed_ms": 1,
                "live_private_terminals": 1,
            },
            "sessions": sessions,
        }),
    )
}

fn doctor_report(stdout: &[u8], mode: &str) -> Value {
    if mode == "ndjson" {
        let records = json_lines(stdout);
        assert_eq!(records[0]["type"], "doctor");
        records[0]["data"].clone()
    } else {
        serde_json::from_slice::<Value>(stdout).unwrap()
    }
}

fn run_doctor(sandbox: &Sandbox, mode: &str) -> (ProcessOutput, Value) {
    let mut doctor = command(&sandbox.socket, &sandbox.root);
    doctor.args(["--output", mode, "doctor", "--claude", "/bin/sh", "--cwd"]);
    doctor.arg(&sandbox.root);
    let output = run(doctor, None);
    let report = doctor_report(&output.stdout, mode);
    (output, report)
}

#[test]
fn doctor_is_turn_free_and_reports_healthy_and_unhealthy_boundaries() {
    let sandbox = Sandbox::new("doctor");
    // Two requests per invocation now, not one. `ping` still carries the only
    // version evidence; `diagnose` is the only request that reaches anything
    // behind the accept loop.
    let server = spawn_native_server(
        sandbox.bind(),
        vec![
            ping_reply(),
            diagnosis_reply(json!([])),
            ping_reply(),
            diagnosis_reply(json!([])),
            ping_reply(),
            diagnosis_reply(json!([])),
        ],
    );

    for mode in ["text", "json", "ndjson"] {
        let (output, report) = run_doctor(&sandbox, mode);
        assert!(output.status.success(), "{}", output.stderr_text());
        assert_eq!(report["status"], "healthy");
        // The daemon this fixture models: it holds no caller sessions, which is
        // the PERMANENT shape of one serving only stateless turns. The sessions
        // layer therefore reads `nothing_to_exercise`, which is `pass`, and the
        // whole report reads healthy with exit 0.
        //
        // Before the encoding was split this exact daemon reported `unproven`
        // and exited 1, on every invocation, forever -- and this assertion
        // passed anyway, because the fixture handed `doctor` a layer entry no
        // daemon can produce for an empty session list.
        assert_eq!(
            report["diagnosis"]["layers"]
                .as_array()
                .and_then(|layers| layers
                    .iter()
                    .find(|layer| layer["layer"] == "sessions")
                    .cloned()),
            Some(serde_json::to_value(HealthLayer::for_sessions(&[])).unwrap()),
            "the fixture's sessions layer must be the one the producer emits: {report}"
        );
        assert_eq!(
            report["diagnosis"]["layers"]
                .as_array()
                .unwrap()
                .iter()
                .find(|layer| layer["layer"] == "sessions")
                .unwrap()["finding"],
            "nothing_to_exercise"
        );
        assert_eq!(report["socket_exists"], true);
        assert_eq!(report["socket_is_unix_socket"], true);
        assert_eq!(report["socket_owner_only"], true);
        assert_eq!(report["server_version"], "test-server");
        assert_eq!(report["errors"], json!([]));
        assert_eq!(report["unproven"], json!([]));
        // The daemon's own answer travels verbatim. A pool supervisor reads
        // per-session findings out of here; a report that only published the
        // fold would make that impossible.
        assert_eq!(
            report["diagnosis"]["runtime"]["finding"],
            "private_runtime_responsive"
        );
        assert_eq!(report["diagnosis"]["sessions"], json!([]));
        assert!(output.stderr.is_empty());
    }
    let requests = server.join().unwrap();
    // Still turn-free, and now also proven to ASK. A `doctor` that only pinged
    // passed the old version of this assertion while testing nothing.
    assert!(
        requests
            .iter()
            .all(|request| matches!(request.request, Request::Ping | Request::Diagnose))
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| matches!(request.request, Request::Diagnose))
            .count(),
        3
    );

    // THE REGRESSION. Both daemons answer, the accept loop is perfect, and one
    // session's private terminal is gone. This is what used to print
    // `"healthy": true`.
    let missing = Sandbox::new("doctor-terminal-missing");
    let server = spawn_native_server(
        missing.bind(),
        vec![
            ping_reply(),
            diagnosis_reply(json!([{
                "session_id": SESSION_ID,
                "generation_id": GENERATION_ID,
                "outcome": "fail",
                "finding": "terminal_missing",
                "state": "ready",
                "private_terminal_present": false,
            }])),
        ],
    );
    let (output, report) = run_doctor(&missing, "json");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(report["status"], "unhealthy");
    // The daemon-level checks all passed, which is exactly why a report folded
    // from them alone was wrong.
    assert_eq!(report["socket_exists"], true);
    assert_eq!(report["server_version"], "test-server");
    // TWO entries, and that is the fixture becoming honest rather than a
    // weakened assertion. A daemon whose only session has lost its terminal
    // reports the fault twice by construction: once in the `sessions` LAYER,
    // which is a fold of the probes, and once in the per-session list, which is
    // the report. The old fixture asserted `errors.len() == 1` only because it
    // stamped `exercised` on a sessions layer built from a failing probe -- a
    // combination `sessions_layer` cannot produce.
    let errors = report["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(
        errors[0]
            .as_str()
            .is_some_and(|text| text.starts_with("sessions: 1 of 1 session(s)")),
        "the layer fold must name the fault too: {errors:?}"
    );
    let rendered = errors[1].as_str().unwrap();
    assert!(rendered.contains(SESSION_ID), "{rendered}");
    assert!(
        rendered.contains("does not report this session's terminal"),
        "{rendered}"
    );
    assert!(output.stderr_text().contains("doctor checks failed"));
    server.join().unwrap();

    // A LAYER NOBODY REPORTED IS NOT A HEALTHY LAYER. The daemon answers
    // `diagnose`, its runtime probe passes, it holds no sessions, and it
    // reports NO health layers -- which is exactly what a pmuxd that knows
    // `diagnose` but predates the tree sends. Every field this report carries
    // says health, and the answer must still not be `healthy`: nothing about
    // configuration, the control plane, the sidecar, the broker, the
    // compatibility profile, the pool or performance was established.
    //
    // `doctor` NAMES each one rather than folding silently. An operator told
    // `unproven` with no reason cannot act; an operator told which eight layers
    // went unreported can.
    let layerless = Sandbox::new("doctor-no-layers");
    let server = spawn_native_server(
        layerless.bind(),
        vec![
            ping_reply(),
            diagnosis_reply_with_layers(json!([]), json!([])),
        ],
    );
    let (output, report) = run_doctor(&layerless, "json");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        report["status"], "unproven",
        "a daemon that established no layer must not read as healthy: {report}"
    );
    assert_eq!(report["errors"], json!([]));
    let unreported = report["unproven"].as_array().unwrap();
    assert_eq!(
        unreported.len(),
        pseudomux_protocol::v1::HealthLayerName::ALL.len(),
        "one entry per unreported layer: {unreported:?}"
    );
    for layer in [
        "configuration",
        "control plane",
        "private runtime (rmux sidecar)",
        "launch broker",
        "compatibility profile",
        "stateless pool",
        "sessions",
        "performance",
    ] {
        assert!(
            unreported
                .iter()
                .any(|entry| entry.as_str().is_some_and(|text| text.starts_with(layer))),
            "no entry names the {layer} layer: {unreported:?}"
        );
    }
    server.join().unwrap();

    // THE THIRD STATE. The daemon is reachable and refuses the probe -- an
    // older pmuxd that does not know the method is the concrete case. Nothing
    // failed, and nothing behind the accept loop was proven either, so this
    // must be neither `healthy` nor `unhealthy`.
    let unproven = Sandbox::new("doctor-unproven");
    let server = spawn_native_server(
        unproven.bind(),
        vec![
            ping_reply(),
            NativeReply::Error {
                code: "unsupported_feature",
                message: "unknown method",
                retryable: false,
                details: json!({}),
            },
        ],
    );
    let (output, report) = run_doctor(&unproven, "json");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(report["status"], "unproven");
    assert_eq!(report["errors"], json!([]));
    assert_eq!(report["server_version"], "test-server");
    assert!(report.get("diagnosis").is_none(), "{report}");
    let unproven_reasons = report["unproven"].as_array().unwrap();
    assert_eq!(unproven_reasons.len(), 1, "{unproven_reasons:?}");
    assert!(
        unproven_reasons[0]
            .as_str()
            .unwrap()
            .contains("nothing behind its accept loop was tested"),
        "{unproven_reasons:?}"
    );
    assert!(
        output
            .stderr_text()
            .contains("doctor could not prove every check it ran")
    );
    server.join().unwrap();

    let unhealthy_sandbox = Sandbox::new("doctor-unhealthy");
    let mut doctor = command(&unhealthy_sandbox.socket, &unhealthy_sandbox.root);
    doctor.args(["--output", "json", "doctor", "--claude", "/bin/sh"]);
    let output = run(doctor, None);
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "unhealthy");
    assert_eq!(report["socket_exists"], false);
    assert!(output.stderr_text().contains("doctor checks failed"));
}

#[test]
fn probe_dry_run_is_redacted_and_launch_keep_are_bounded() {
    let dry = Sandbox::new("probe-dry");
    for mode in ["text", "json", "ndjson"] {
        let mut probe = command(&dry.socket, &dry.root);
        probe.args(["--output", mode, "probe"]);
        add_launch_args(&mut probe, &dry.root);
        probe.args([
            "--settings-json",
            r#"{"token":"settings-secret"}"#,
            "--mcp-json",
            r#"{"token":"mcp-secret"}"#,
            "--system-prompt",
            "system-prompt-secret",
        ]);
        let output = run(probe, None);
        assert!(output.status.success(), "{}", output.stderr_text());
        let rendered = output.stdout_text();
        for secret in [
            "environment-secret",
            "settings-secret",
            "mcp-secret",
            "system-prompt-secret",
        ] {
            assert!(!rendered.contains(secret));
        }
        let report = match mode {
            "json" => serde_json::from_slice::<Value>(&output.stdout).unwrap(),
            "ndjson" => json_lines(&output.stdout)[0]["data"].clone(),
            _ => serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        };
        assert_eq!(report["launched"], false);
        assert_eq!(
            report["request"]["environment"]["snapshot"]["redacted"],
            true
        );
        assert_eq!(
            report["request"]["claude"]["system_prompt"]["prompt_redacted"],
            true
        );
        assert!(output.stderr.is_empty());
    }

    let launch = Sandbox::new("probe-launch");
    let server = spawn_native_server(
        launch.bind(),
        vec![
            success("session_started", session_handle()),
            success("session_snapshot", snapshot(0)),
            success("session_closed", close_result(true)),
            success("session_started", session_handle()),
            success("session_snapshot", snapshot(0)),
        ],
    );
    let mut closing = command(&launch.socket, &launch.root);
    closing.args([
        "--output",
        "json",
        "probe",
        "--launch",
        "--session-id",
        SESSION_ID,
    ]);
    add_launch_args(&mut closing, &launch.root);
    let closing = run(closing, None);
    assert!(closing.status.success(), "{}", closing.stderr_text());
    let closing_report: Value = serde_json::from_slice(&closing.stdout).unwrap();
    assert_eq!(closing_report["launched"], true);
    assert_eq!(closing_report["close"]["process_reaped"], true);

    let mut keeping = command(&launch.socket, &launch.root);
    keeping.args([
        "--output",
        "json",
        "probe",
        "--launch",
        "--keep",
        "--session-id",
        SESSION_ID,
    ]);
    add_launch_args(&mut keeping, &launch.root);
    let keeping = run(keeping, None);
    assert!(keeping.status.success(), "{}", keeping.stderr_text());
    let keeping_report: Value = serde_json::from_slice(&keeping.stdout).unwrap();
    assert_eq!(keeping_report["launched"], true);
    assert!(keeping_report.get("close").is_none());

    let requests = server.join().unwrap();
    assert!(matches!(requests[0].request, Request::StartSession(_)));
    assert!(matches!(requests[1].request, Request::InspectSession(_)));
    assert!(matches!(requests[2].request, Request::CloseSession(_)));
    assert!(matches!(requests[3].request, Request::StartSession(_)));
    assert!(matches!(requests[4].request, Request::InspectSession(_)));
}

#[test]
fn probe_inspection_failure_force_closes_even_with_keep_and_combines_cleanup_failure() {
    let sandbox = Sandbox::new("probe-inspect-failure");
    let server = spawn_native_server(
        sandbox.bind(),
        vec![
            success("session_started", session_handle()),
            NativeReply::Error {
                code: "session_not_found",
                message: "probe inspection failed",
                retryable: false,
                details: json!({"phase": "inspect"}),
            },
            success("session_closed", close_result(true)),
            success("session_started", session_handle()),
            NativeReply::Error {
                code: "internal",
                message: "second probe inspection failed",
                retryable: false,
                details: json!({"phase": "inspect"}),
            },
            NativeReply::Error {
                code: "recovery_failed",
                message: "probe cleanup boundary remained occupied",
                retryable: true,
                details: json!({"process_boundary": "not_empty"}),
            },
        ],
    );

    for (keep, combined) in [(false, false), (true, true)] {
        let mut probe = command(&sandbox.socket, &sandbox.root);
        probe.args([
            "--output",
            "json",
            "probe",
            "--launch",
            "--session-id",
            SESSION_ID,
        ]);
        if keep {
            probe.arg("--keep");
        }
        add_launch_args(&mut probe, &sandbox.root);
        probe.args(["--system-prompt", "probe-system-prompt-secret"]);
        let output = run(probe, None);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let stderr = output.stderr_text();
        assert!(stderr.contains("probe inspection failed"));
        assert!(!stderr.contains("probe-system-prompt-secret"));
        assert!(!stderr.contains("environment-secret"));
        if combined {
            assert!(stderr.contains("recovery_failed"));
            assert!(stderr.contains("probe cleanup boundary remained occupied"));
            assert!(stderr.contains(SESSION_ID));
            assert!(stderr.contains(GENERATION_ID));
        }
    }

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 6);
    for base in [0, 3] {
        assert!(matches!(requests[base].request, Request::StartSession(_)));
        assert!(matches!(
            requests[base + 1].request,
            Request::InspectSession(_)
        ));
        let Request::CloseSession(close) = &requests[base + 2].request else {
            panic!("probe inspection failure did not force-close the exact generation");
        };
        assert_eq!(close.policy, ClosePolicy::Force);
        assert_eq!(close.session_id.to_string(), SESSION_ID);
        assert_eq!(close.generation_id.to_string(), GENERATION_ID);
    }
}

#[test]
fn close_and_probe_reject_success_responses_without_process_reap_proof() {
    let sandbox = Sandbox::new("close-proof");
    let server = spawn_native_server(
        sandbox.bind(),
        vec![
            success("session_closed", close_result(false)),
            success("session_started", session_handle()),
            success("session_snapshot", snapshot(0)),
            success("session_closed", close_result(false)),
        ],
    );

    let mut close = command(&sandbox.socket, &sandbox.root);
    close.args([
        "--output",
        "ndjson",
        "close",
        SESSION_ID,
        "--generation",
        GENERATION_ID,
    ]);
    let close = run(close, None);
    assert_eq!(close.status.code(), Some(1));
    assert!(close.stdout.is_empty());
    assert!(
        close
            .stderr_text()
            .contains("without confirming that its process was reaped")
    );

    let mut probe = command(&sandbox.socket, &sandbox.root);
    probe.args([
        "--output",
        "json",
        "probe",
        "--launch",
        "--session-id",
        SESSION_ID,
    ]);
    add_launch_args(&mut probe, &sandbox.root);
    let probe = run(probe, None);
    assert_eq!(probe.status.code(), Some(1));
    assert!(probe.stdout.is_empty());
    assert!(
        probe
            .stderr_text()
            .contains("without confirming that its process was reaped")
    );

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(matches!(requests[0].request, Request::CloseSession(_)));
    assert!(matches!(requests[1].request, Request::StartSession(_)));
    assert!(matches!(requests[2].request, Request::InspectSession(_)));
    assert!(matches!(requests[3].request, Request::CloseSession(_)));
}

#[test]
fn turn_replay_gap_reuses_the_immutable_turn_id_and_commits_once() {
    let sandbox = Sandbox::new("turn-replay");
    let server = spawn_native_server(
        sandbox.bind(),
        vec![
            success("turn_accepted", turn_accepted(false, 1)),
            success("events", replay_gap_batch(0, 5)),
            success("turn_accepted", turn_accepted(true, 6)),
            success(
                "events",
                event_batch(vec![completed_event(6, "completed", "replayed")], 7),
            ),
        ],
    );

    let mut turn = command(&sandbox.socket, &sandbox.root);
    turn.args(["--output", "ndjson"]);
    add_turn_target(&mut turn);
    turn.arg("replay prompt");
    let output = run(turn, None);
    assert!(output.status.success(), "{}", output.stderr_text());
    let records = json_lines(&output.stdout);
    assert_eq!(
        records
            .iter()
            .map(|record| record["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["replay_gap", "event", "result"]
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| record["type"] == "result")
            .count(),
        1
    );
    assert_eq!(records.last().unwrap()["data"]["text"], "replayed");
    assert!(output.stderr_text().contains("replay gap encountered"));

    let requests = server.join().unwrap();
    let run_turns = requests
        .iter()
        .filter_map(|request| match &request.request {
            Request::RunTurn(request) => Some(request),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(run_turns.len(), 2);
    assert_eq!(
        run_turns[0].turn.turn_id,
        TURN_ID.parse::<uuid::Uuid>().unwrap()
    );
    assert_eq!(run_turns[1].turn.turn_id, run_turns[0].turn.turn_id);
    assert_eq!(run_turns[0].turn.prompt, "replay prompt");
    assert_eq!(run_turns[1].turn.prompt, "replay prompt");
    assert_eq!(
        serde_json::to_value(&requests[1].request).unwrap(),
        json!({
            "method": "subscribe_events",
            "params": {
                "session_id": SESSION_ID,
                "generation_id": GENERATION_ID,
                "after_sequence": 0,
                "wait_ms": 30000,
                "max_events": 128,
            },
        })
    );
    assert_eq!(
        serde_json::to_value(&requests[3].request).unwrap()["params"]["after_sequence"],
        5
    );
}

#[test]
fn completed_turn_covers_text_and_json_result_boundaries() {
    let sandbox = Sandbox::new("turn-formats");
    let mut replies = completion_replies("completed", "text result");
    replies.extend(completion_replies("completed", "json result"));
    let server = spawn_native_server(sandbox.bind(), replies);

    let mut text = command(&sandbox.socket, &sandbox.root);
    text.args(["--output", "text"]);
    add_turn_target(&mut text);
    text.arg("text prompt");
    let text = run(text, None);
    assert!(text.status.success(), "{}", text.stderr_text());
    assert_eq!(text.stdout_text(), "text result\n");

    let mut json_command = command(&sandbox.socket, &sandbox.root);
    json_command.args(["--output", "json"]);
    add_turn_target(&mut json_command);
    json_command.arg("json prompt");
    let json_output = run(json_command, None);
    assert!(
        json_output.status.success(),
        "{}",
        json_output.stderr_text()
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&json_output.stdout).unwrap()["text"],
        "json result"
    );

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(matches!(requests[0].request, Request::RunTurn(_)));
    assert!(matches!(requests[1].request, Request::SubscribeEvents(_)));
    assert!(matches!(requests[2].request, Request::RunTurn(_)));
    assert!(matches!(requests[3].request, Request::SubscribeEvents(_)));
}

#[test]
fn noncompleted_turn_results_never_emit_a_terminal_result_record() {
    let sandbox = Sandbox::new("turn-noncompleted");
    let mut replies = Vec::new();
    for outcome in ["failed", "cancelled"] {
        for _mode in ["text", "json", "ndjson"] {
            replies.extend(completion_replies(outcome, "must-not-commit"));
        }
    }
    let server = spawn_native_server(sandbox.bind(), replies);

    for outcome in ["failed", "cancelled"] {
        for mode in ["text", "json", "ndjson"] {
            let mut turn = command(&sandbox.socket, &sandbox.root);
            turn.args(["--output", mode]);
            add_turn_target(&mut turn);
            turn.arg(format!("{outcome}-turn-prompt-secret"));
            let output = run(turn, None);

            assert_eq!(output.status.code(), Some(1));
            if mode == "ndjson" {
                let records = json_lines(&output.stdout);
                assert_eq!(records.len(), 1, "false terminal output: {records:?}");
                assert_eq!(records[0]["type"], "event");
                assert_eq!(records[0]["data"]["event"]["data"]["outcome"], outcome);
            } else {
                assert!(
                    output.stdout.is_empty(),
                    "{mode} emitted a false terminal result: {}",
                    output.stdout_text()
                );
            }
            let stderr = output.stderr_text();
            assert!(stderr.contains("ended with outcome"));
            assert!(!stderr.contains("turn-prompt-secret"));
            assert!(!stderr.contains("environment-secret"));
        }
    }

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 12);
    assert!(
        requests
            .chunks_exact(2)
            .all(|pair| matches!(pair[0].request, Request::RunTurn(_))
                && matches!(pair[1].request, Request::SubscribeEvents(_)))
    );
}

#[test]
fn cancel_recovery_failure_is_nonzero_and_withholds_success_in_every_mode() {
    let sandbox = Sandbox::new("cancel-recovery-failed");
    let recovery_failed = || {
        json!({
            "session_id": SESSION_ID,
            "generation_id": GENERATION_ID,
            "turn_id": TURN_ID,
            "outcome": "recovery_failed",
            "session_state": "tainted",
        })
    };
    let server = spawn_native_server(
        sandbox.bind(),
        ["text", "json", "ndjson"]
            .into_iter()
            .map(|_| success("turn_cancelled", recovery_failed()))
            .collect(),
    );

    for mode in ["text", "json", "ndjson"] {
        let mut cancel = command(&sandbox.socket, &sandbox.root);
        cancel.args([
            "--output",
            mode,
            "cancel",
            SESSION_ID,
            "--generation",
            GENERATION_ID,
            TURN_ID,
        ]);
        let output = run(cancel, None);

        assert_eq!(output.status.code(), Some(1));
        assert!(
            output.stdout.is_empty(),
            "{mode} emitted a false cancellation success: {}",
            output.stdout_text()
        );
        let stderr = output.stderr_text();
        assert!(stderr.contains("cancellation did not recover the session"));
        assert!(stderr.contains(SESSION_ID));
        assert!(stderr.contains(TURN_ID));
        assert!(!stderr.contains("environment-secret"));
    }

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(
        requests
            .iter()
            .all(|request| matches!(request.request, Request::CancelTurn(_)))
    );
}

#[test]
fn run_ndjson_commits_exactly_once_only_after_cleanup_proof() {
    let sandbox = Sandbox::new("run-success");
    let server = spawn_native_server(
        sandbox.bind(),
        vec![
            success("session_started", session_handle()),
            success("turn_accepted", turn_accepted(false, 1)),
            success(
                "events",
                event_batch(
                    vec![
                        warning_event(1),
                        completed_event(2, "completed", "run complete"),
                    ],
                    3,
                ),
            ),
            success("session_closed", close_result(true)),
        ],
    );

    let mut command_line = command(&sandbox.socket, &sandbox.root);
    command_line.args([
        "--output",
        "ndjson",
        "oneshot",
        "--session-id",
        SESSION_ID,
        "--turn-id",
        TURN_ID,
    ]);
    add_launch_args(&mut command_line, &sandbox.root);
    command_line.arg("run prompt");
    let output = run(command_line, None);
    assert!(output.status.success(), "{}", output.stderr_text());
    let records = json_lines(&output.stdout);
    assert_eq!(
        records
            .iter()
            .map(|record| record["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["event", "event", "result"]
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| record["type"] == "result")
            .count(),
        1
    );
    assert_eq!(records.last().unwrap()["data"]["text"], "run complete");
    assert!(output.stderr_text().contains("session"));
    assert!(output.stderr_text().contains("accepted"));

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 4);
    let Request::StartSession(start) = &requests[0].request else {
        panic!("expected start");
    };
    assert_eq!(
        start.identity,
        SessionIdentity::New {
            session_id: Some(SESSION_ID.parse().unwrap())
        }
    );
    let Request::RunTurn(turn) = &requests[1].request else {
        panic!("expected turn");
    };
    assert_eq!(turn.turn.turn_id, TURN_ID.parse::<uuid::Uuid>().unwrap());
    assert_eq!(turn.turn.prompt, "run prompt");
    let Request::CloseSession(close) = &requests[3].request else {
        panic!("expected close");
    };
    assert_eq!(close.policy, ClosePolicy::Graceful);
}

#[test]
fn completed_run_covers_text_and_json_only_after_each_close() {
    let sandbox = Sandbox::new("run-formats");
    let mut replies = Vec::new();
    for (text, sequence) in [("text run", 1), ("json run", 1)] {
        replies.extend([
            success("session_started", session_handle()),
            success("turn_accepted", turn_accepted(false, sequence)),
            success(
                "events",
                event_batch(
                    vec![completed_event(sequence, "completed", text)],
                    sequence + 1,
                ),
            ),
            success("session_closed", close_result(true)),
        ]);
    }
    let server = spawn_native_server(sandbox.bind(), replies);

    let mut text = command(&sandbox.socket, &sandbox.root);
    text.args([
        "--output",
        "text",
        "oneshot",
        "--session-id",
        SESSION_ID,
        "--turn-id",
        TURN_ID,
    ]);
    add_launch_args(&mut text, &sandbox.root);
    text.arg("text run prompt");
    let text = run(text, None);
    assert!(text.status.success(), "{}", text.stderr_text());
    assert_eq!(text.stdout_text(), "text run\n");

    let mut json_command = command(&sandbox.socket, &sandbox.root);
    json_command.args([
        "--output",
        "json",
        "oneshot",
        "--session-id",
        SESSION_ID,
        "--turn-id",
        TURN_ID,
    ]);
    add_launch_args(&mut json_command, &sandbox.root);
    json_command.arg("json run prompt");
    let json_output = run(json_command, None);
    assert!(
        json_output.status.success(),
        "{}",
        json_output.stderr_text()
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&json_output.stdout).unwrap()["text"],
        "json run"
    );

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 8);
    assert!(matches!(requests[3].request, Request::CloseSession(_)));
    assert!(matches!(requests[7].request, Request::CloseSession(_)));
}

#[test]
fn run_cleanup_failure_keeps_observations_but_withholds_result_marker() {
    let sandbox = Sandbox::new("run-cleanup");
    let server = spawn_native_server(
        sandbox.bind(),
        vec![
            success("session_started", session_handle()),
            success("turn_accepted", turn_accepted(false, 1)),
            success(
                "events",
                event_batch(
                    vec![
                        warning_event(1),
                        completed_event(2, "completed", "must not commit"),
                    ],
                    3,
                ),
            ),
            success("session_closed", close_result(false)),
        ],
    );

    let mut command_line = command(&sandbox.socket, &sandbox.root);
    command_line.args([
        "--output",
        "ndjson",
        "oneshot",
        "--session-id",
        SESSION_ID,
        "--turn-id",
        TURN_ID,
    ]);
    add_launch_args(&mut command_line, &sandbox.root);
    command_line.arg("cleanup prompt");
    let output = run(command_line, None);
    assert_eq!(output.status.code(), Some(1));
    let records = json_lines(&output.stdout);
    assert_eq!(records.len(), 2);
    assert!(records.iter().all(|record| record["type"] == "event"));
    assert!(
        output
            .stderr_text()
            .contains("without confirming that its process was reaped")
    );

    let requests = server.join().unwrap();
    let Request::CloseSession(close) = &requests[3].request else {
        panic!("expected close");
    };
    assert_eq!(close.policy, ClosePolicy::Graceful);
}

#[test]
fn run_failed_and_cancelled_terminal_results_never_emit_success_commit_markers() {
    let sandbox = Sandbox::new("run-noncompleted");
    let mut replies = Vec::new();
    for (outcome, text) in [
        ("failed", "failed terminal result"),
        ("cancelled", "cancelled terminal result"),
    ] {
        replies.extend([
            success("session_started", session_handle()),
            success("turn_accepted", turn_accepted(false, 1)),
            success(
                "events",
                event_batch(vec![completed_event(1, outcome, text)], 2),
            ),
            success("session_closed", close_result(true)),
        ]);
    }
    let server = spawn_native_server(sandbox.bind(), replies);

    for outcome in ["failed", "cancelled"] {
        let mut command_line = command(&sandbox.socket, &sandbox.root);
        command_line.args([
            "--output",
            "ndjson",
            "oneshot",
            "--session-id",
            SESSION_ID,
            "--turn-id",
            TURN_ID,
        ]);
        add_launch_args(&mut command_line, &sandbox.root);
        command_line.arg(format!("{outcome} prompt"));
        let output = run(command_line, None);
        assert_eq!(output.status.code(), Some(1));
        let records = json_lines(&output.stdout);
        assert_eq!(records.len(), 1, "false commit output: {records:?}");
        assert_eq!(records[0]["type"], "event");
        assert_eq!(records[0]["data"]["event"]["data"]["outcome"], outcome);
        assert!(output.stderr_text().contains("ended with outcome"));
    }

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 8);
    for close_index in [3, 7] {
        let Request::CloseSession(close) = &requests[close_index].request else {
            panic!("expected close");
        };
        assert_eq!(close.policy, ClosePolicy::Graceful);
    }
}

#[test]
fn run_combines_turn_and_cleanup_failures_without_false_commit() {
    let sandbox = Sandbox::new("run-combined");
    let server = spawn_native_server(
        sandbox.bind(),
        vec![
            success("session_started", session_handle()),
            success("turn_accepted", turn_accepted(false, 1)),
            success(
                "events",
                event_batch(
                    vec![failed_event(
                        1,
                        "turn transcript schema rejected",
                        json!({
                            "field": "message.content",
                            "backend_matcher": "turn-failure-backend-secret",
                        }),
                    )],
                    2,
                ),
            ),
            NativeReply::Error {
                code: "recovery_failed",
                message: "cleanup boundary remained occupied",
                retryable: true,
                details: json!({"process_boundary": "not_empty"}),
            },
        ],
    );

    let mut command_line = command(&sandbox.socket, &sandbox.root);
    command_line.args([
        "--output",
        "ndjson",
        "oneshot",
        "--session-id",
        SESSION_ID,
        "--turn-id",
        TURN_ID,
    ]);
    add_launch_args(&mut command_line, &sandbox.root);
    command_line.arg("combined prompt");
    let output = run(command_line, None);
    assert_eq!(output.status.code(), Some(1));
    let records = json_lines(&output.stdout);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["type"], "event");
    let stderr = output.stderr_text();
    assert!(stderr.contains("recovery_failed"));
    assert!(stderr.contains("turn transcript schema rejected"));
    assert!(stderr.contains("cleanup boundary remained occupied"));
    assert!(!stderr.contains("turn-failure-backend-secret"));
    assert!(!stderr.contains("message.content"));
    assert!(!stderr.contains("combined prompt"));
    assert!(!stderr.contains("environment-secret"));

    let requests = server.join().unwrap();
    let Request::CloseSession(close) = &requests[3].request else {
        panic!("expected force close after turn failure");
    };
    assert_eq!(close.policy, ClosePolicy::Force);
}

#[test]
fn prompt_sources_normalize_and_accept_the_exact_byte_limit() {
    let sandbox = Sandbox::new("prompts");
    let prompt_file = sandbox.root.join("prompt.txt");
    fs::write(&prompt_file, b"file\r\nprompt\rtext").unwrap();
    let mut replies = Vec::new();
    for text in [
        "positional",
        "stdin",
        "file",
        "dash",
        "exact-limit",
        "normalized-limit",
    ] {
        replies.extend(completion_replies("completed", text));
    }
    let server = spawn_native_server(sandbox.bind(), replies);

    let mut positional = command(&sandbox.socket, &sandbox.root);
    positional.args(["--output", "json"]);
    add_turn_target(&mut positional);
    positional.arg("positional\r\nprompt\rtext");
    assert!(run(positional, None).status.success());

    let mut stdin_command = command(&sandbox.socket, &sandbox.root);
    stdin_command.args(["--output", "json"]);
    add_turn_target(&mut stdin_command);
    assert!(
        run(stdin_command, Some(b"stdin\r\nprompt\rtext"))
            .status
            .success()
    );

    let mut file_command = command(&sandbox.socket, &sandbox.root);
    file_command.args(["--output", "json"]);
    add_turn_target(&mut file_command);
    file_command.arg("--prompt-file").arg(&prompt_file);
    assert!(run(file_command, None).status.success());

    let mut dash_command = command(&sandbox.socket, &sandbox.root);
    dash_command.args(["--output", "json"]);
    add_turn_target(&mut dash_command);
    dash_command.args(["--prompt-file", "-"]);
    assert!(
        run(dash_command, Some(b"dash\r\nprompt\rtext"))
            .status
            .success()
    );

    let exact_limit = vec![b'x'; MAX_PROMPT_BYTES];
    let mut exact_command = command(&sandbox.socket, &sandbox.root);
    exact_command.args(["--output", "json"]);
    add_turn_target(&mut exact_command);
    assert!(run(exact_command, Some(&exact_limit)).status.success());

    // Content, not just terminators: `"\r\n" * MAX` stood here until 2026-08-09
    // and normalizes to nothing at all now that the trailing trim is the
    // composer's own, so it tested the emptiness refusal rather than the length
    // check it was written for. `"x\r\n" * (MAX/2)` still has a RAW length above
    // the limit and a NORMALIZED length below it, which is the property.
    let normalized_limit = b"x\r\n".repeat(MAX_PROMPT_BYTES / 2);
    let mut normalized_command = command(&sandbox.socket, &sandbox.root);
    normalized_command.args(["--output", "json"]);
    add_turn_target(&mut normalized_command);
    assert!(
        run(normalized_command, Some(&normalized_limit))
            .status
            .success()
    );

    let requests = server.join().unwrap();
    let prompts = requests
        .iter()
        .filter_map(|request| match &request.request {
            Request::RunTurn(request) => Some(request.turn.prompt.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        &prompts[..4],
        [
            "positional\nprompt\ntext",
            "stdin\nprompt\ntext",
            "file\nprompt\ntext",
            "dash\nprompt\ntext",
        ]
    );
    assert_eq!(prompts[4].len(), MAX_PROMPT_BYTES);
    assert!(prompts[4].bytes().all(|byte| byte == b'x'));
    // `"x\r\n" * (MAX/2)` is 1.5 * MAX raw bytes and normalizes to `"x\n"` *
    // (MAX/2) with the last newline trimmed -- MAX - 1 bytes, accepted. The
    // normalization runs before the length check, which is deliberate: line
    // endings and the trailing run are artifacts of how the text was produced
    // rather than prompt content, so neither is charged against the caller's
    // byte budget.
    assert_eq!(prompts[5].len(), MAX_PROMPT_BYTES - 1);
    assert_eq!(prompts[5].matches('\r').count(), 0);
    assert!(prompts[5].starts_with("x\nx\n"));
    assert!(prompts[5].ends_with("\nx"));
}

#[test]
fn invalid_prompts_and_source_conflicts_fail_before_daemon_contact() {
    let sandbox = Sandbox::new("prompt-reject");
    let listener = sandbox.bind();
    let cases = [
        (Vec::new(), "prompt must not be empty"),
        (vec![0xff, 0xfe], "prompt must be valid UTF-8"),
        (
            b"unsafe\x1bprompt".to_vec(),
            "prompt contains an unsafe control character",
        ),
        (
            b"unsafe\0prompt".to_vec(),
            "prompt contains an unsafe control character",
        ),
        (
            b"unsafe\x07prompt".to_vec(),
            "prompt contains an unsafe control character",
        ),
        (
            b" \t\n/compact".to_vec(),
            "slash commands require a future typed control API",
        ),
        // The second composer mode, at the process boundary. A prompt beginning
        // `!` was MEASURED at Claude Code 2.1.226 switching the composer into
        // bash mode and running the rest as a shell command on the host; this
        // case is here because the `/compact` one above passed throughout, so
        // the suite's own name was the only thing claiming coverage of it.
        (
            b"!echo PMUX_BASH_MODE_ESCAPE > /tmp/pmux-escape".to_vec(),
            "switches the composer into bash mode",
        ),
        // And the character the composer accepts and rewrites. A tab used to be
        // admitted here and refused four steps later by an acknowledgement it
        // could not satisfy, destroying a pooled instance to say so.
        (
            b"nonce\tprompt".to_vec(),
            "recorded by the composer as four spaces",
        ),
        (
            vec![b'x'; MAX_PROMPT_BYTES + 1],
            "prompt exceeds the 1048576-byte CLI limit",
        ),
    ];
    for (input, expected) in cases {
        let mut command_line = command(&sandbox.socket, &sandbox.root);
        add_turn_target(&mut command_line);
        let output = run(command_line, Some(&input));
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let stderr = output.stderr_text();
        assert!(
            stderr.contains(expected),
            "stderr did not contain {expected:?}: {stderr}"
        );
        assert!(!stderr.contains("I/O error"));
    }

    let prompt_file = sandbox.root.join("prompt.txt");
    fs::write(&prompt_file, b"file prompt").unwrap();
    let mut conflict = command(&sandbox.socket, &sandbox.root);
    add_turn_target(&mut conflict);
    conflict
        .arg("positional")
        .arg("--prompt-file")
        .arg(prompt_file);
    let conflict = run(conflict, None);
    assert_eq!(conflict.status.code(), Some(2));
    assert!(conflict.stdout.is_empty());

    listener.set_nonblocking(true).unwrap();
    let error = listener.accept().unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
}

#[test]
fn daemon_error_malformed_identity_result_and_unavailability_are_runtime_failures() {
    let sandbox = Sandbox::new("daemon-errors");
    let server = spawn_native_server(
        sandbox.bind(),
        vec![
            NativeReply::Error {
                code: "rate_limited",
                message: "bounded public rejection",
                retryable: true,
                details: json!({"retry_after_ms": 1000}),
            },
            NativeReply::Malformed(b"{not-json".to_vec()),
            NativeReply::MismatchedSuccess {
                kind: "pong",
                data: json!({"server_version": "wrong-id", "protocol_version": 1}),
            },
            success("session_started", session_handle()),
        ],
    );
    for expected in [
        "RateLimited",
        "invalid JSON frame",
        "response request id",
        "pmuxd returned session_started, expected pong",
    ] {
        let mut ping = command(&sandbox.socket, &sandbox.root);
        ping.args(["--output", "json", "ping"]);
        let output = run(ping, None);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(
            output.stderr_text().contains(expected),
            "stderr did not contain {expected:?}: {}",
            output.stderr_text()
        );
    }
    assert_eq!(server.join().unwrap().len(), 4);

    let unavailable = Sandbox::new("daemon-unavailable");
    let mut ping = command(&unavailable.socket, &unavailable.root);
    ping.args(["--output", "json", "ping"]);
    let output = run(ping, None);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(output.stderr_text().contains("I/O error"));
}

#[test]
fn clap_and_config_rejections_have_stable_exit_boundaries() {
    let sandbox = Sandbox::new("clap");
    let mut missing_socket = pmux_process();
    missing_socket.env_clear().arg("ping");
    let missing_socket = run(missing_socket, None);
    assert_eq!(missing_socket.status.code(), Some(2));
    assert!(missing_socket.stdout.is_empty());

    let mut invalid_uuid = command(&sandbox.socket, &sandbox.root);
    invalid_uuid.args(["inspect", "not-a-uuid", "--generation", GENERATION_ID]);
    let invalid_uuid = run(invalid_uuid, None);
    assert_eq!(invalid_uuid.status.code(), Some(2));
    assert!(invalid_uuid.stdout.is_empty());

    let mut unknown = command(&sandbox.socket, &sandbox.root);
    unknown.args(["--unknown-flag", "ping"]);
    let unknown = run(unknown, None);
    assert_eq!(unknown.status.code(), Some(2));
    assert!(unknown.stdout.is_empty());

    let mut relative = command(Path::new("relative.sock"), &sandbox.root);
    relative.arg("ping");
    let relative = run(relative, None);
    assert_eq!(relative.status.code(), Some(1));
    assert!(relative.stdout.is_empty());
    assert!(relative.stderr_text().contains("absolute Unix socket"));

    let mut keep_without_launch = command(&sandbox.socket, &sandbox.root);
    keep_without_launch.args(["probe", "--keep", "--claude", "/bin/sh"]);
    let keep_without_launch = run(keep_without_launch, None);
    assert_eq!(keep_without_launch.status.code(), Some(2));
    assert!(keep_without_launch.stdout.is_empty());
}

#[test]
fn turn_disconnect_policy_and_deadline_are_preserved_in_the_dto() {
    let sandbox = Sandbox::new("turn-policy");
    let server = spawn_native_server(
        sandbox.bind(),
        completion_replies("completed", "policy result"),
    );
    let mut turn = command(&sandbox.socket, &sandbox.root);
    turn.args(["--output", "json"]);
    add_turn_target(&mut turn);
    turn.args([
        "--timeout-secs",
        "3",
        "--on-disconnect",
        "cancel-turn",
        "--heartbeat-timeout-ms",
        "2500",
        "policy prompt",
    ]);
    let before_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let output = run(turn, None);
    let after_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    assert!(output.status.success(), "{}", output.stderr_text());

    let requests = server.join().unwrap();
    let Request::RunTurn(request) = &requests[0].request else {
        panic!("expected run_turn");
    };
    assert_eq!(request.turn.prompt, "policy prompt");
    assert_eq!(
        request.turn.lease.on_disconnect,
        DisconnectAction::CancelTurn
    );
    assert_eq!(request.turn.lease.heartbeat_timeout_ms, Some(2500));
    let deadline = request.turn.deadline_unix_ms.unwrap();
    assert!(deadline >= before_ms + 3000);
    assert!(deadline <= after_ms + 3000);
}
