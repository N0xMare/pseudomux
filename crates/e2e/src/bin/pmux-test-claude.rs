//! Deterministic external-process double for credential-free pmux E2E tests.
//!
//! It is intentionally a real foreground PTY program. It verifies interactive
//! argv, consumes exactly one bracketed paste and Enter per turn, renders a
//! cursor-correlated editor, and appends Claude-shaped JSONL. It is not a pmux
//! semantic oracle; production pmux decides whether those external artifacts
//! form a valid turn.

#![allow(
    unsafe_code,
    reason = "the test-only PTY process must install signals and configure termios through libc"
)]

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use pseudomux_e2e::{
    TEST_ANTHROPIC_SECRET, TEST_ATTESTATION_VERSION, TEST_ENV_ATTESTATION_MARKER,
    TEST_ENV_PATCHED_VALUE, TEST_ENV_SAFE_CONFIG_VALUE, TEST_ENV_SET_ONLY_VALUE,
    TEST_PROVIDER_SECRET, TEST_SUBSCRIPTION_KEYS, TEST_TRANSPARENT_EXACT_KEYS,
    TEST_TRANSPARENT_PREFIXES,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";
const MAX_PROMPT_BYTES: usize = 1024 * 1024;
/// The one slash command pmux's privileged control channel ever types.
const CLEAR_COMMAND: &str = "/clear";
const MAX_SYNTHETIC_RESULT_BYTES: usize = 4 * 1024 * 1024;
const FORBIDDEN_FLAGS: &[&str] = &[
    "-p",
    "--print",
    "--bg",
    "--background",
    "--continue",
    "--input-format",
    "--output-format",
    "--teammate-mode",
];

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
static DESCENDANT_ESCAPE_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sigint(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

extern "C" fn request_descendant_escape(_: libc::c_int) {
    DESCENDANT_ESCAPE_REQUESTED.store(true, Ordering::SeqCst);
}

fn main() {
    if let Err(error) = run() {
        eprintln!("pmux-test-claude: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("PMUX_TEST_HELPER_ROLE").as_deref() == Ok("escaping-descendant") {
        return run_escaping_descendant();
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|argument| argument == "--version") {
        println!("9.9.9 (pmux deterministic interactive test double)");
        return Ok(());
    }
    for forbidden in FORBIDDEN_FLAGS {
        if args.iter().any(|argument| argument == forbidden) {
            return Err(
                format!("forbidden non-interactive flag reached child: {forbidden}").into(),
            );
        }
    }

    let (session_id, launch_mode) = parse_identity(&args)?;
    let cwd = std::env::current_dir()?.canonicalize()?;
    let config_root = PathBuf::from(
        std::env::var("CLAUDE_CONFIG_DIR")
            .map_err(|_| "CLAUDE_CONFIG_DIR is required by the test double")?,
    );
    let state_root = PathBuf::from(
        std::env::var("PMUX_TEST_STATE_DIR")
            .map_err(|_| "PMUX_TEST_STATE_DIR is required by the test double")?,
    );
    std::fs::create_dir_all(&state_root)?;
    let project_root = config_root.join("projects/pmux-e2e");
    std::fs::create_dir_all(&project_root)?;
    let mut session_id = session_id;
    let mut transcript = project_root.join(format!("{session_id}.jsonl"));
    record_launch(&state_root, &session_id, launch_mode, &args, &cwd)?;
    run_lifecycle_hooks(&args, "SessionStart", &session_id, &transcript, &state_root)?;

    let _escaping_descendant =
        if std::env::var("PMUX_TEST_SPAWN_ESCAPING_DESCENDANT").as_deref() == Ok("1") {
            Some(spawn_escaping_descendant(&state_root, &session_id)?)
        } else {
            None
        };

    install_sigint_handler()?;
    let _terminal_guard = RawTerminalGuard::install(io::stdin().as_raw_fd())?;
    let mut parent_uuid = last_uuid(&transcript)?;
    if std::env::var("PMUX_TEST_STARTUP_MODAL").as_deref() == Ok("permission") {
        render_permission_modal()?;
        loop {
            std::thread::sleep(Duration::from_secs(30));
        }
    }
    render_ready()?;

    loop {
        let prompt_bytes = match read_bracketed_paste() {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                INTERRUPTED.store(false, Ordering::SeqCst);
                render_ready()?;
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if prompt_bytes.len() > MAX_PROMPT_BYTES {
            return Err("test prompt exceeded the production service limit".into());
        }
        let prompt = String::from_utf8(prompt_bytes)?;
        if prompt.contains(['\r', '\n', '\x1b']) {
            return Err("the deterministic E2E cell requires a safe single-line prompt".into());
        }

        if prompt.starts_with("PMUX_TEST_AMBIGUOUS_PASTE") {
            write_terminal(b"\x1b[2J\x1b[1;1Hunrelated terminal revision")?;
            let mut unexpected = [0_u8; 1];
            read_exact_interruptible(&mut unexpected)?;
            let evidence = state_root.join(format!("unexpected-input-{session_id}.bin"));
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&evidence)?;
            file.write_all(&unexpected)?;
            file.sync_all()?;
            return Err(format!(
                "pmux sent input {:02x} after an unproven pasted editor render",
                unexpected[0]
            )
            .into());
        }

        if prompt.starts_with("PMUX_TEST_ADMISSION_MODAL") {
            render_permission_modal()?;
            let mut unexpected = [0_u8; 1];
            read_exact_interruptible(&mut unexpected)?;
            return Err(format!(
                "pmux sent input {:02x} after the post-paste admission modal",
                unexpected[0]
            )
            .into());
        }

        render_editor(&prompt)?;
        let mut enter = [0_u8; 1];
        read_exact_interruptible(&mut enter)?;
        if enter[0] != b'\r' {
            return Err(format!("expected one carriage-return Enter, got {:02x}", enter[0]).into());
        }
        ensure_no_queued_input()?;

        // The prompt is attested HERE, at the single point where a submission
        // has been accepted and before any branch below can consume it. Placed
        // after the last `continue` instead, it would have to be repeated in
        // each arm, and the arm somebody forgets is the one that makes the
        // ledger silently narrower than the sentence "every prompt this process
        // received". `launches.jsonl` already carries the full argv keyed by
        // this same `cwd`, so nothing about the class key is restated here:
        // a reader joins the two files and reads argv, and a class key that
        // grows a third component cannot leave this row behind.
        record_prompt(&state_root, &cwd, &config_root, &session_id, &prompt)?;

        // `/clear` is the one privileged control command pmux types, and it is
        // not a turn. MEASURED on Claude Code 2.1.220: the command abandons the
        // bound transcript untouched and opens a new `<new-uuid>.jsonl` beside
        // it whose first five rows are a fixed preamble -- including the
        // command echo that names which slash command actually ran. The double
        // reproduces that shape because it is the only artifact pmux's rebind
        // and assert-empty are decided from.
        if prompt == CLEAR_COMMAND {
            let rotated = Uuid::new_v4().to_string();
            let rotated_transcript = project_root.join(format!("{rotated}.jsonl"));
            write_clear_preamble(&rotated_transcript, &rotated, &cwd, CLEAR_COMMAND)?;
            session_id = rotated;
            transcript = rotated_transcript;
            parent_uuid = None;
            render_ready()?;
            continue;
        }

        let user_uuid = Uuid::new_v4().to_string();
        append_json(
            &transcript,
            &json!({
                "type": "user",
                "uuid": user_uuid,
                "parentUuid": parent_uuid,
                "sessionId": session_id,
                "cwd": cwd,
                "promptSource": "typed",
                "promptId": Uuid::new_v4(),
                "message": {"content": prompt},
            }),
        )?;

        if prompt.starts_with("PMUX_TEST_POST_ENTER_MODAL_PERMISSION") {
            render_permission_modal()?;
            let mut answer = [0_u8; 2];
            read_exact_interruptible(&mut answer)?;
            if answer != *b"y\r" {
                return Err(format!(
                    "expected the explicit attach answer 79 0d, got {:02x} {:02x}",
                    answer[0], answer[1]
                )
                .into());
            }
            ensure_no_queued_input()?;
        }

        if prompt.starts_with("PMUX_TEST_CANCEL") {
            write_terminal(b"\x1b[2J\x1b[1;1Hworking")?;
            wait_for_interrupt();
            INTERRUPTED.store(false, Ordering::SeqCst);
            parent_uuid = Some(user_uuid);
            render_ready()?;
            continue;
        }

        if prompt.starts_with("PMUX_TEST_SCHEMA_DRIFT") {
            let drift_uuid = Uuid::new_v4().to_string();
            append_json(
                &transcript,
                &json!({
                    "type": "future_semantic_row",
                    "uuid": drift_uuid,
                    "parentUuid": user_uuid,
                    "sessionId": session_id,
                    "cwd": cwd,
                    "payload": {"must_not_be_accepted": true},
                }),
            )?;
            parent_uuid = Some(drift_uuid);
            render_ready()?;
            continue;
        }

        let assistant_uuid = if prompt.starts_with("PMUX_TEST_RICH_RESULT") {
            append_rich_turn(&transcript, &session_id, &cwd, &user_uuid)?
        } else {
            let response_text = response_text(&prompt)?;
            let assistant_uuid = Uuid::new_v4().to_string();
            append_json(
                &transcript,
                &json!({
                    "type": "assistant",
                    "uuid": assistant_uuid,
                    "parentUuid": user_uuid,
                    "sessionId": session_id,
                    "cwd": cwd,
                    "requestId": Uuid::new_v4(),
                    "message": {
                        "id": Uuid::new_v4(),
                        "model": "pmux-test-model",
                        "content": [{"type": "text", "text": response_text}],
                        "stop_reason": "end_turn",
                        "usage": {
                            "input_tokens": 3,
                            "output_tokens": 1,
                            "cache_creation_input_tokens": 0,
                            "cache_read_input_tokens": 0
                        }
                    }
                }),
            )?;
            assistant_uuid
        };
        parent_uuid = Some(assistant_uuid);
        run_lifecycle_hooks(&args, "Stop", &session_id, &transcript, &state_root)?;
        render_ready()?;
    }
}

fn append_rich_turn(
    transcript: &Path,
    session_id: &str,
    cwd: &Path,
    user_uuid: &str,
) -> io::Result<String> {
    let sidechain_uuid = Uuid::new_v4().to_string();
    append_json(
        transcript,
        &json!({
            "type": "assistant",
            "uuid": sidechain_uuid,
            "parentUuid": user_uuid,
            "isSidechain": true,
            "sessionId": session_id,
            "cwd": cwd,
            "requestId": Uuid::new_v4(),
            "message": {
                "id": Uuid::new_v4(),
                "model": "pmux-sidechain-model",
                "content": [{"type": "text", "text": "rich sidechain must not leak"}],
                "stop_reason": "end_turn",
                "usage": {
                    "input_tokens": 101,
                    "output_tokens": 102,
                    "cache_creation_input_tokens": 103,
                    "cache_read_input_tokens": 104
                }
            }
        }),
    )?;

    let tool_message_id = Uuid::new_v4().to_string();
    let tool_request_id = Uuid::new_v4().to_string();
    let tool_thinking_uuid = Uuid::new_v4().to_string();
    append_json(
        transcript,
        &json!({
            "type": "assistant",
            "uuid": tool_thinking_uuid,
            "parentUuid": user_uuid,
            "isSidechain": false,
            "sessionId": session_id,
            "cwd": cwd,
            "requestId": tool_request_id,
            "message": {
                "id": tool_message_id,
                "model": "pmux-rich-tool-model",
                "content": [{"type": "thinking", "thinking": "rich hidden tool thinking"}],
                "stop_reason": null,
                "usage": {
                    "input_tokens": 11,
                    "output_tokens": 12,
                    "cache_creation_input_tokens": 13,
                    "cache_read_input_tokens": 14
                }
            }
        }),
    )?;
    let tool_call_uuid = Uuid::new_v4().to_string();
    append_json(
        transcript,
        &json!({
            "type": "assistant",
            "uuid": tool_call_uuid,
            "parentUuid": tool_thinking_uuid,
            "isSidechain": false,
            "sessionId": session_id,
            "cwd": cwd,
            "requestId": tool_request_id,
            "message": {
                "id": tool_message_id,
                "model": "pmux-rich-tool-model",
                "content": [{
                    "type": "tool_use",
                    "id": "pmux-rich-tool-1",
                    "name": "Read",
                    "input": {"file_path": "RICH.md", "line": 7}
                }],
                "stop_reason": "tool_use",
                "usage": {
                    "input_tokens": 11,
                    "output_tokens": 12,
                    "cache_creation_input_tokens": 13,
                    "cache_read_input_tokens": 14
                }
            }
        }),
    )?;

    let tool_result_uuid = Uuid::new_v4().to_string();
    append_json(
        transcript,
        &json!({
            "type": "user",
            "uuid": tool_result_uuid,
            "parentUuid": tool_call_uuid,
            "isSidechain": false,
            "sessionId": session_id,
            "cwd": cwd,
            "promptId": Uuid::new_v4(),
            "message": {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "pmux-rich-tool-1",
                    "content": {"content": "rich tool output", "line_count": 1},
                    "is_error": false
                }]
            }
        }),
    )?;

    let final_message_id = Uuid::new_v4().to_string();
    let final_request_id = Uuid::new_v4().to_string();
    let final_thinking_uuid = Uuid::new_v4().to_string();
    append_json(
        transcript,
        &json!({
            "type": "assistant",
            "uuid": final_thinking_uuid,
            "parentUuid": tool_result_uuid,
            "isSidechain": false,
            "sessionId": session_id,
            "cwd": cwd,
            "requestId": final_request_id,
            "message": {
                "id": final_message_id,
                "model": "pmux-rich-final-model",
                "content": [{"type": "thinking", "thinking": "rich hidden final thinking"}],
                "stop_reason": null,
                "usage": {
                    "input_tokens": 21,
                    "output_tokens": 22,
                    "cache_creation_input_tokens": 23,
                    "cache_read_input_tokens": 24
                }
            }
        }),
    )?;
    let final_uuid = Uuid::new_v4().to_string();
    append_json(
        transcript,
        &json!({
            "type": "assistant",
            "uuid": final_uuid,
            "parentUuid": final_thinking_uuid,
            "isSidechain": false,
            "sessionId": session_id,
            "cwd": cwd,
            "requestId": final_request_id,
            "message": {
                "id": final_message_id,
                "model": "pmux-rich-final-model",
                "content": [
                    {"type": "text", "text": "rich final "},
                    {"type": "text", "text": "answer"}
                ],
                "stop_reason": "end_turn",
                "usage": {
                    "input_tokens": 21,
                    "output_tokens": 22,
                    "cache_creation_input_tokens": 23,
                    "cache_read_input_tokens": 24
                }
            }
        }),
    )?;
    Ok(final_uuid)
}

fn spawn_escaping_descendant(
    state_root: &Path,
    session_id: &str,
) -> io::Result<std::process::Child> {
    let pid_file = state_root.join(format!("escape-descendant-{session_id}.pid"));
    let ready_file = state_root.join(format!("escape-descendant-{session_id}.ready"));
    let escaped_file = state_root.join(format!("escape-descendant-{session_id}.escaped"));
    let mut child = Command::new(std::env::current_exe()?)
        .env("PMUX_TEST_HELPER_ROLE", "escaping-descendant")
        .env("PMUX_TEST_ESCAPE_PID_FILE", &pid_file)
        .env("PMUX_TEST_ESCAPE_READY_FILE", &ready_file)
        .env("PMUX_TEST_ESCAPED_FILE", &escaped_file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !ready_file.is_file() {
        if let Some(status) = child.try_wait()? {
            return Err(io::Error::other(format!(
                "escaping descendant exited before readiness with {status}"
            )));
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "escaping descendant did not become ready",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(child)
}

/// Controlled fixture used only by the real-process close-retry E2E. It starts
/// as a direct member of the fake Claude process session. Closing the PTY sends
/// SIGHUP to that foreground group; the helper then leaves the owned session,
/// records the transition, restores default signal behavior, and waits for the
/// test to clean only its retained exact identity.
fn run_escaping_descendant() -> Result<(), Box<dyn std::error::Error>> {
    let pid_file = PathBuf::from(
        std::env::var_os("PMUX_TEST_ESCAPE_PID_FILE")
            .ok_or("escaping descendant requires PMUX_TEST_ESCAPE_PID_FILE")?,
    );
    let ready_file = PathBuf::from(
        std::env::var_os("PMUX_TEST_ESCAPE_READY_FILE")
            .ok_or("escaping descendant requires PMUX_TEST_ESCAPE_READY_FILE")?,
    );
    let escaped_file = PathBuf::from(
        std::env::var_os("PMUX_TEST_ESCAPED_FILE")
            .ok_or("escaping descendant requires PMUX_TEST_ESCAPED_FILE")?,
    );

    install_escape_signal_handlers()?;
    std::fs::write(&pid_file, format!("{}\n", std::process::id()))?;
    std::fs::write(&ready_file, b"ready\n")?;

    while !DESCENDANT_ESCAPE_REQUESTED.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(10));
    }
    let pid = i32::try_from(std::process::id())?;
    // SAFETY: this dedicated fixture changes only its own POSIX session.
    let session_id = unsafe { libc::setsid() };
    if session_id != pid {
        return Err(io::Error::last_os_error().into());
    }
    restore_escape_signal_defaults()?;
    std::fs::write(&escaped_file, b"escaped\n")?;
    loop {
        std::thread::sleep(Duration::from_secs(30));
    }
}

fn install_escape_signal_handlers() -> io::Result<()> {
    for signal in [libc::SIGHUP, libc::SIGTERM] {
        // SAFETY: the handler performs only a lock-free atomic store and is
        // installed in the dedicated descendant fixture before it publishes
        // readiness.
        let previous = unsafe {
            libc::signal(
                signal,
                request_descendant_escape as *const () as libc::sighandler_t,
            )
        };
        if previous == libc::SIG_ERR {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn restore_escape_signal_defaults() -> io::Result<()> {
    for signal in [libc::SIGHUP, libc::SIGTERM] {
        // SAFETY: this dedicated fixture restores only its own signal
        // dispositions after it has left the product-owned session.
        let previous = unsafe { libc::signal(signal, libc::SIG_DFL) };
        if previous == libc::SIG_ERR {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn response_text(prompt: &str) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(value) = prompt.strip_prefix("PMUX_TEST_ECHO:") {
        return Ok(format!("pmux-test-echo:{value}"));
    }
    if let Some(raw_bytes) = prompt.strip_prefix("PMUX_TEST_LARGE_RESULT:") {
        let bytes = raw_bytes.parse::<usize>()?;
        if !(1..=MAX_SYNTHETIC_RESULT_BYTES).contains(&bytes) {
            return Err(format!(
                "synthetic result size must be between 1 and {MAX_SYNTHETIC_RESULT_BYTES} bytes"
            )
            .into());
        }
        return Ok("r".repeat(bytes));
    }
    Ok("pmux-test-ok".to_owned())
}

fn run_lifecycle_hooks(
    args: &[String],
    event: &str,
    session_id: &str,
    transcript: &Path,
    state_root: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let payload = serde_json::to_vec(&json!({
        "session_id": session_id,
        "hook_event_name": event,
        "transcript_path": transcript,
    }))?;
    let mut index = 0;
    while index < args.len() {
        if args[index] != "--settings" {
            index += 1;
            continue;
        }
        let path = args.get(index + 1).ok_or("--settings has no value")?;
        let document: Value = serde_json::from_slice(&std::fs::read(path)?)?;
        let Some(entries) = document
            .get("hooks")
            .and_then(|hooks| hooks.get(event))
            .and_then(Value::as_array)
        else {
            index += 2;
            continue;
        };
        for entry in entries {
            let hooks = entry
                .get("hooks")
                .and_then(Value::as_array)
                .ok_or("hook entry has no hooks array")?;
            for hook in hooks {
                if hook.get("type").and_then(Value::as_str) != Some("command") {
                    continue;
                }
                let command = hook
                    .get("command")
                    .and_then(Value::as_str)
                    .ok_or("command hook has no command")?;
                let mut child = Command::new("/bin/sh")
                    .arg("-c")
                    .arg(command)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()?;
                child
                    .stdin
                    .take()
                    .ok_or("hook child stdin was unavailable")?
                    .write_all(&payload)?;
                let output = child.wait_with_output()?;
                if !output.status.success()
                    || !output.stdout.is_empty()
                    || !output.stderr.is_empty()
                {
                    return Err(format!(
                        "lifecycle hook {event} failed without a clean stdio boundary: {}",
                        output.status
                    )
                    .into());
                }
                append_json(
                    &state_root.join("hook-invocations.jsonl"),
                    &json!({
                        "event": event,
                        "session_id": session_id,
                        "status": "success",
                    }),
                )?;
            }
        }
        index += 2;
    }
    Ok(())
}

fn parse_identity(args: &[String]) -> Result<(String, &'static str), Box<dyn std::error::Error>> {
    let mut found = None;
    let mut index = 0;
    while index < args.len() {
        let mode = match args[index].as_str() {
            "--session-id" => Some("new"),
            "--resume" => Some("resume"),
            _ => None,
        };
        if let Some(mode) = mode {
            let raw = args.get(index + 1).ok_or("session flag has no value")?;
            let value = Uuid::parse_str(raw)?.to_string();
            if found.replace((value, mode)).is_some() {
                return Err("multiple session identity flags reached the child".into());
            }
            index += 2;
        } else {
            index += 1;
        }
    }
    found.ok_or_else(|| "no forced session identity reached the child".into())
}

fn record_launch(
    state_root: &Path,
    session_id: &str,
    mode: &str,
    args: &[String],
    cwd: &Path,
) -> io::Result<()> {
    let forbidden = args
        .iter()
        .filter(|argument| FORBIDDEN_FLAGS.contains(&argument.as_str()))
        .count();
    let executable = std::env::current_exe()?.canonicalize()?;
    let executable_digest = digest_file(&executable)?;
    let environment = attest_environment()?;
    append_json(
        &state_root.join("launches.jsonl"),
        &json!({
            "attestation_version": TEST_ATTESTATION_VERSION,
            "session_id": session_id,
            "mode": mode,
            "pid": std::process::id(),
            "process_start_identity": current_process_start_identity()?,
            "process_group_id": current_process_group_id(),
            "process_session_id": current_process_session_id()?,
            "executable_path": executable,
            "executable_sha256": executable_digest,
            "cwd": cwd,
            "forbidden_flag_count": forbidden,
            "argv_count": args.len(),
            "argv": args,
            "environment": environment,
        }),
    )
}

/// One accepted submission, attested by the process that received it.
///
/// The pool erases an instance's whole tree when it recycles it, so the
/// transcript is not a durable record of which process served which caller.
/// This file lives under `PMUX_TEST_STATE_DIR`, which is outside every pool
/// root, so a wave that recycles instances mid-flight still has a complete
/// record afterwards.
///
/// It deliberately carries NO model, NO effort and NO argv. `launches.jsonl`
/// already carries the whole argv keyed by the same `cwd`, and `cwd` is
/// `<parent>/<slot>/<epoch>/cwd`, which one process owns for its whole life.
/// A reader joins on it. Restating a two-field summary of the class key here
/// would be a second copy that a third component could silently leave behind.
fn record_prompt(
    state_root: &Path,
    cwd: &Path,
    config_root: &Path,
    session_id: &str,
    prompt: &str,
) -> io::Result<()> {
    append_json(
        &state_root.join("prompts.jsonl"),
        &json!({
            "attestation_version": TEST_ATTESTATION_VERSION,
            "pid": std::process::id(),
            "cwd": cwd,
            "config_root": config_root,
            "bound_session_id": session_id,
            "prompt": prompt,
        }),
    )
}

fn attest_environment() -> io::Result<Value> {
    let variables = std::env::vars().collect::<BTreeMap<_, _>>();
    require_environment_value(
        &variables,
        "PMUX_TEST_ENV_ATTESTATION",
        TEST_ENV_ATTESTATION_MARKER,
    )?;
    require_environment_value(&variables, "PMUX_TEST_PATCH_ORDER", TEST_ENV_PATCHED_VALUE)?;
    require_environment_value(&variables, "PMUX_TEST_SET_ONLY", TEST_ENV_SET_ONLY_VALUE)?;
    require_environment_value(
        &variables,
        "PMUX_TEST_CALLER_SAFE_CONFIG",
        TEST_ENV_SAFE_CONFIG_VALUE,
    )?;
    require_environment_value(&variables, "TERM", "xterm-256color")?;

    if variables.contains_key("PMUX_TEST_UNSET_ME") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "snapshot-unset environment value reached the child",
        ));
    }
    let unexpected_keys = variables
        .keys()
        .filter(|name| {
            TEST_SUBSCRIPTION_KEYS.contains(&name.as_str())
                || TEST_TRANSPARENT_EXACT_KEYS.contains(&name.as_str())
                || TEST_TRANSPARENT_PREFIXES
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
        })
        .cloned()
        .collect::<Vec<_>>();
    if !unexpected_keys.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "credential/provider/parent identity keys reached child: {}",
                unexpected_keys.join(",")
            ),
        ));
    }
    if variables
        .values()
        .any(|value| value.contains(TEST_ANTHROPIC_SECRET) || value.contains(TEST_PROVIDER_SECRET))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "synthetic stripped secret value reached child",
        ));
    }

    let expected_path = variables
        .get("PMUX_TEST_EXPECTED_PATH")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "expected PATH is missing"))?;
    require_environment_value(&variables, "PATH", expected_path)?;
    let home = variables
        .get("HOME")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "HOME is missing"))?;
    let config_root = variables.get("CLAUDE_CONFIG_DIR").ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "CLAUDE_CONFIG_DIR is missing")
    })?;
    let state_root = variables.get("PMUX_TEST_STATE_DIR").ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "PMUX_TEST_STATE_DIR is missing")
    })?;

    Ok(json!({
        "attestation_marker": TEST_ENV_ATTESTATION_MARKER,
        "patch_order": TEST_ENV_PATCHED_VALUE,
        "set_only": TEST_ENV_SET_ONLY_VALUE,
        "caller_safe_config": TEST_ENV_SAFE_CONFIG_VALUE,
        "unset_present": false,
        "forbidden_keys_present": unexpected_keys,
        "stripped_secret_values_present": false,
        "term": "xterm-256color",
        "path": expected_path,
        "home": home,
        "claude_config_dir": config_root,
        "state_dir": state_root,
    }))
}

fn require_environment_value(
    variables: &BTreeMap<String, String>,
    name: &str,
    expected: &str,
) -> io::Result<()> {
    match variables.get(name).map(String::as_str) {
        Some(actual) if actual == expected => Ok(()),
        Some(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("environment value {name} did not match its attested value"),
        )),
        None => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("environment value {name} is missing"),
        )),
    }
}

fn digest_file(path: &Path) -> io::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

#[cfg(target_os = "linux")]
fn current_process_start_identity() -> io::Result<String> {
    let stat = std::fs::read_to_string("/proc/self/stat")?;
    let (_, fields) = stat
        .rsplit_once(") ")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed /proc/self/stat"))?;
    let start_ticks = fields.split_whitespace().nth(19).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "missing process start time in /proc/self/stat",
        )
    })?;
    Ok(format!("linux_boot_ticks:{start_ticks}"))
}

#[cfg(target_os = "macos")]
fn current_process_start_identity() -> io::Result<String> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>())
        .map_err(|_| io::Error::other("proc_bsdinfo size does not fit c_int"))?;
    let pid = libc::c_int::try_from(std::process::id())
        .map_err(|_| io::Error::other("pid does not fit c_int"))?;
    // SAFETY: `info` points to writable storage exactly matching the requested
    // PROC_PIDTBSDINFO flavor and is initialized only after a full-size read.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read < size {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: proc_pidinfo reported a complete proc_bsdinfo structure.
    let info = unsafe { info.assume_init() };
    Ok(format!(
        "macos_timeval:{}:{}",
        info.pbi_start_tvsec, info.pbi_start_tvusec
    ))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn current_process_start_identity() -> io::Result<String> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "exact process start identity is unsupported on this platform",
    ))
}

fn current_process_group_id() -> i32 {
    // SAFETY: getpgrp has no pointer arguments and cannot fail.
    unsafe { libc::getpgrp() }
}

fn current_process_session_id() -> io::Result<i32> {
    // SAFETY: getsid(0) queries the calling process and has no pointer arguments.
    let session_id = unsafe { libc::getsid(0) };
    if session_id < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(session_id)
    }
}

/// The transcript preamble a slash command opens, MEASURED verbatim on Claude
/// Code 2.1.220. Row 0 is the rotation anchor pmux matches on; rows 2 and 3 are
/// `user` rows whose string content is the caveat and the command echo; row 4 is
/// the command's empty stdout; row 5 is the `last-prompt` marker.
fn write_clear_preamble(
    path: &Path,
    session_id: &str,
    cwd: &Path,
    command_name: &str,
) -> io::Result<()> {
    let caveat_uuid = Uuid::new_v4().to_string();
    let echo_uuid = Uuid::new_v4().to_string();
    let stdout_uuid = Uuid::new_v4().to_string();
    for row in [
        json!({"type": "mode", "mode": "normal", "sessionId": session_id}),
        json!({
            "type": "file-history-snapshot",
            "messageId": echo_uuid,
            "snapshot": {"messageId": echo_uuid, "trackedFileBackups": {}},
            "isSnapshotUpdate": false,
        }),
        json!({
            "parentUuid": null,
            "isSidechain": false,
            "type": "user",
            "message": {
                "role": "user",
                "content": "<local-command-caveat>Caveat: The messages below were generated by the user while running local commands. DO NOT respond to these messages or otherwise consider them in your response unless the user explicitly asks you to.</local-command-caveat>",
            },
            "isMeta": true,
            "uuid": caveat_uuid,
            "cwd": cwd,
            "sessionId": session_id,
        }),
        json!({
            "parentUuid": caveat_uuid,
            "isSidechain": false,
            "type": "user",
            "message": {
                "role": "user",
                "content": format!(
                    "<command-name>{command_name}</command-name>\n            <command-message>{}</command-message>\n            <command-args></command-args>",
                    command_name.trim_start_matches('/'),
                ),
            },
            "uuid": echo_uuid,
            "cwd": cwd,
            "sessionId": session_id,
        }),
        json!({
            "parentUuid": echo_uuid,
            "isSidechain": false,
            "type": "system",
            "subtype": "local_command",
            "content": "<local-command-stdout></local-command-stdout>",
            "level": "info",
            "uuid": stdout_uuid,
            "isMeta": false,
            "cwd": cwd,
            "sessionId": session_id,
        }),
        json!({"type": "last-prompt", "leafUuid": stdout_uuid, "sessionId": session_id}),
    ] {
        append_json(path, &row)?;
    }
    Ok(())
}

/// Append one whole JSON line, in ONE write.
///
/// The row is serialized into memory first and the newline is part of that one
/// buffer. `serde_json::to_writer` on an unbuffered `File` issues a separate
/// `write` per token -- MEASURED: five concurrent pool instances appending to
/// one `prompts.jsonl` produced `Error("key must be a string", line: 1, column:
/// 25)`, one process's tokens spliced into another's row. Every file this
/// function writes is either shared by every instance in a pool
/// (`launches.jsonl`, `prompts.jsonl`) or is a transcript that pmux's own
/// parser refuses on a malformed line, so a partially written row is not a
/// cosmetic problem.
///
/// A single `write` to an `O_APPEND` descriptor advances the offset and copies
/// the buffer as one operation, so two processes cannot interleave. The short
/// write is REFUSED rather than looped: `write_all` would retry from the middle
/// of the row, which is the interleaving this exists to prevent, and a caller
/// that gets an error still has a whole file.
fn append_json(path: &Path, value: &Value) -> io::Result<()> {
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let written = file.write(&line)?;
    if written != line.len() {
        return Err(io::Error::other(format!(
            "appending to {} wrote {written} of {} bytes; a partial row cannot be completed \
             without interleaving with another instance's row",
            path.display(),
            line.len()
        )));
    }
    file.sync_data()
}

fn last_uuid(path: &Path) -> io::Result<Option<String>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut latest = None;
    for line in BufReader::new(file).lines() {
        let value: Value = serde_json::from_str(&line?)?;
        if let Some(uuid) = value.get("uuid").and_then(Value::as_str) {
            latest = Some(uuid.to_owned());
        }
    }
    Ok(latest)
}

fn install_sigint_handler() -> io::Result<()> {
    // SAFETY: the handler only performs a lock-free atomic store, and `signal`
    // installs it for this single-purpose test process before worker activity.
    let previous = unsafe {
        libc::signal(
            libc::SIGINT,
            handle_sigint as *const () as libc::sighandler_t,
        )
    };
    if previous == libc::SIG_ERR {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

struct RawTerminalGuard {
    fd: libc::c_int,
    original: libc::termios,
}

impl RawTerminalGuard {
    fn install(fd: libc::c_int) -> io::Result<Self> {
        // SAFETY: `fd` is stdin in the PTY child and both termios pointers are
        // valid for the duration of each libc call.
        unsafe {
            let mut original = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut original) != 0 {
                return Err(io::Error::last_os_error());
            }
            let mut raw = original;
            libc::cfmakeraw(&mut raw);
            raw.c_lflag |= libc::ISIG;
            raw.c_cc[libc::VMIN] = 1;
            raw.c_cc[libc::VTIME] = 0;
            if libc::tcsetattr(fd, libc::TCSANOW, &raw) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { fd, original })
        }
    }
}

impl Drop for RawTerminalGuard {
    fn drop(&mut self) {
        // SAFETY: this restores the termios snapshot captured from the same fd.
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.original);
        }
    }
}

fn read_bracketed_paste() -> io::Result<Vec<u8>> {
    let mut prefix = [0_u8; PASTE_START.len()];
    read_exact_interruptible(&mut prefix)?;
    if prefix != PASTE_START {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "input did not begin with bracketed-paste start",
        ));
    }

    let mut payload = Vec::new();
    let mut candidate = Vec::new();
    loop {
        let mut byte = [0_u8; 1];
        read_exact_interruptible(&mut byte)?;
        candidate.push(byte[0]);
        while !PASTE_END.starts_with(&candidate) {
            payload.push(candidate.remove(0));
            if payload.len() > MAX_PROMPT_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "bracketed paste exceeded test bound",
                ));
            }
        }
        if candidate == PASTE_END {
            return Ok(payload);
        }
    }
}

fn read_exact_interruptible(mut buffer: &mut [u8]) -> io::Result<()> {
    while !buffer.is_empty() {
        match io::stdin().read(buffer) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(read) => buffer = &mut buffer[read..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                if INTERRUPTED.load(Ordering::SeqCst) {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn ensure_no_queued_input() -> io::Result<()> {
    let mut descriptor = libc::pollfd {
        fd: io::stdin().as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `descriptor` is a valid one-element pollfd array.
    let ready = unsafe { libc::poll(&mut descriptor, 1, 100) };
    if ready < 0 {
        return Err(io::Error::last_os_error());
    }
    if ready > 0 && descriptor.revents & libc::POLLIN != 0 {
        let mut extra = [0_u8; 1];
        io::stdin().read_exact(&mut extra)?;
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected byte after sole Enter: {:02x}", extra[0]),
        ));
    }
    Ok(())
}

fn wait_for_interrupt() {
    while !INTERRUPTED.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn render_ready() -> io::Result<()> {
    write_terminal(
        b"\x1b[?2004h\x1b[2J\x1b[1;1Hpmux deterministic Claude test double\x1b[22;1H\xe2\x9d\xaf \x1b[22;3H",
    )
}

fn render_permission_modal() -> io::Result<()> {
    write_terminal(
        b"\x1b[2J\x1b[18;1HPermission required\x1b[19;1HAllow this command or deny it?\x1b[22;1Hchoose",
    )
}

fn render_editor(prompt: &str) -> io::Result<()> {
    if prompt.starts_with('/') {
        return render_command_menu(prompt);
    }
    let mut rendered =
        b"\x1b[?2004h\x1b[2J\x1b[1;1Hpmux deterministic Claude test double\x1b[22;1H\xe2\x9d\xaf "
            .to_vec();
    rendered.extend_from_slice(prompt.as_bytes());
    write_terminal(&rendered)
}

/// The slash-command menu, reproduced from a capture of Claude Code 2.1.220.
///
/// A slash command in the composer opens a ruled menu of candidates and moves
/// the composer UP the screen to make room. The selected entry is marked by a
/// foreground colour and nothing else — no reverse video, no background, no
/// attribute bit, no marker glyph, no change of indentation — and that colour
/// covers the whole row INCLUDING the blanks between the command token and its
/// description, while an unselected row colours only the characters it renders
/// and leaves those blanks at the terminal default.
///
/// The double reproduces this because it is what pmux's pre-Enter selection
/// proof reads, and because without it the double would be the one screen in the
/// system on which that proof cannot be exercised at all.
///
/// `PMUX_TEST_CLEAR_MENU` drives the menu into the shapes the capture also
/// recorded, so an end-to-end run can show the refusal as well as the pass:
///
/// * unset or `selects-typed` — the typed command is the selected entry.
/// * `selects-other` — a NEIGHBOUR is selected while the composer still reads
///   `/clear`. MEASURED: Enter here runs the neighbour.
/// * `no-menu` — the composer is settled and no menu has painted, which is the
///   real 14–32 ms window after a paste.
/// * `two-selected` — two entries carry the selected shape.
fn render_command_menu(command: &str) -> io::Result<()> {
    const SELECTED: &str = "\x1b[38;5;153m";
    const UNSELECTED: &str = "\x1b[38;5;246m";
    const RESET: &str = "\x1b[39m";
    let mode = std::env::var("PMUX_TEST_CLEAR_MENU").unwrap_or_default();

    let mut rendered = b"\x1b[?2004h\x1b[2J\x1b[1;1Hpmux deterministic Claude test double".to_vec();
    if mode != "no-menu" {
        // The rule, then the candidates, then the composer last so the cursor is
        // left where a real composer leaves it: two cells past the prompt glyph
        // plus the typed command.
        rendered.extend_from_slice(format!("\x1b[10;1H{}", "─".repeat(40)).as_bytes());
        let neighbours = [
            (command, "Start a new session with empty context;"),
            ("/code-review", "Review the current diff for correctness"),
            ("/simplify", "Review the changed code for reuse,"),
        ];
        for (index, (token, description)) in neighbours.iter().enumerate() {
            let row = 11 + index * 2;
            let selected = match mode.as_str() {
                "selects-other" => index == 1,
                "two-selected" => index <= 1,
                _ => index == 0,
            };
            let line = format!("{token:<30}{description}");
            let painted = if selected {
                // One run over the whole row, blanks included.
                format!("{SELECTED}{line}{RESET}")
            } else {
                // Each rendered word coloured, the blanks between them left at
                // the terminal default.
                line.split(' ')
                    .map(|word| {
                        if word.is_empty() {
                            String::new()
                        } else {
                            format!("{UNSELECTED}{word}{RESET}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            rendered.extend_from_slice(format!("\x1b[{row};1H{painted}").as_bytes());
        }
    }
    // MEASURED 2.1.220/2.1.227/2.1.238: the typed command in the composer is
    // the same colour as the selected menu row. prove_control_command_selection
    // matches those two colours; an unstyled composer makes the proof refuse
    // and every pooled /clear remints.
    rendered.extend_from_slice(format!("\x1b[9;1H\u{276f} {SELECTED}{command}{RESET}").as_bytes());
    write_terminal(&rendered)
}

fn write_terminal(bytes: &[u8]) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(bytes)?;
    stdout.flush()
}
