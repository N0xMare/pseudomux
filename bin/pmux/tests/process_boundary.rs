#![cfg(unix)]

#[allow(dead_code)]
mod support;

use std::fs;
use std::path::Path;
use std::process::Command;

use pseudomux_protocol::v1::{HealthLayer, HealthLayerName, LayerFinding, Request, SessionProbe};
use serde_json::{Value, json};

use support::{
    GENERATION_ID, MAX_PROMPT_BYTES, NativeReply, ProcessOutput, SESSION_ID, Sandbox, command,
    json_lines, pmux_process, run, session_handle, spawn_native_server, success,
};

fn ping_reply() -> NativeReply {
    success(
        "pong",
        json!({"server_version": "test-server", "protocol_version": 1}),
    )
}

fn stateless_reply(text: &str) -> NativeReply {
    success(
        "stateless_result",
        json!({
            "model": "sonnet",
            "text": text,
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
            "claude_version": "test",
        }),
    )
}

fn add_run_args(command: &mut Command) {
    command.args(["--output", "json", "run", "--model", "sonnet"]);
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
    doctor.args(["--output", mode, "doctor", "--claude", "/bin/sh"]);
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
fn prompt_sources_normalize_and_accept_the_exact_byte_limit() {
    let sandbox = Sandbox::new("prompts");
    let prompt_file = sandbox.root.join("prompt.txt");
    fs::write(&prompt_file, b"file\r\nprompt\rtext").unwrap();
    let replies = [
        "positional",
        "stdin",
        "file",
        "dash",
        "exact-limit",
        "normalized-limit",
    ]
    .into_iter()
    .map(stateless_reply)
    .collect();
    let server = spawn_native_server(sandbox.bind(), replies);

    let mut positional = command(&sandbox.socket, &sandbox.root);
    add_run_args(&mut positional);
    positional.arg("positional\r\nprompt\rtext");
    assert!(run(positional, None).status.success());

    let mut stdin_command = command(&sandbox.socket, &sandbox.root);
    add_run_args(&mut stdin_command);
    assert!(
        run(stdin_command, Some(b"stdin\r\nprompt\rtext"))
            .status
            .success()
    );

    let mut file_command = command(&sandbox.socket, &sandbox.root);
    add_run_args(&mut file_command);
    file_command.arg("--prompt-file").arg(&prompt_file);
    assert!(run(file_command, None).status.success());

    let mut dash_command = command(&sandbox.socket, &sandbox.root);
    add_run_args(&mut dash_command);
    dash_command.args(["--prompt-file", "-"]);
    assert!(
        run(dash_command, Some(b"dash\r\nprompt\rtext"))
            .status
            .success()
    );

    let exact_limit = vec![b'x'; MAX_PROMPT_BYTES];
    let mut exact_command = command(&sandbox.socket, &sandbox.root);
    add_run_args(&mut exact_command);
    assert!(run(exact_command, Some(&exact_limit)).status.success());

    // Content, not just terminators: `"\r\n" * MAX` stood here until 2026-08-09
    // and normalizes to nothing at all now that the trailing trim is the
    // composer's own, so it tested the emptiness refusal rather than the length
    // check it was written for. `"x\r\n" * (MAX/2)` still has a RAW length above
    // the limit and a NORMALIZED length below it, which is the property.
    let normalized_limit = b"x\r\n".repeat(MAX_PROMPT_BYTES / 2);
    let mut normalized_command = command(&sandbox.socket, &sandbox.root);
    add_run_args(&mut normalized_command);
    assert!(
        run(normalized_command, Some(&normalized_limit))
            .status
            .success()
    );

    let requests = server.join().unwrap();
    let prompts = requests
        .iter()
        .filter_map(|request| match &request.request {
            Request::RunStateless(request) => Some(request.prompt.as_str()),
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
        add_run_args(&mut command_line);
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
    add_run_args(&mut conflict);
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

    let mut unknown_inspect = command(&sandbox.socket, &sandbox.root);
    unknown_inspect.args(["inspect", "not-a-uuid", "--generation", GENERATION_ID]);
    let unknown_inspect = run(unknown_inspect, None);
    assert_eq!(unknown_inspect.status.code(), Some(2));
    assert!(unknown_inspect.stdout.is_empty());
    assert!(
        unknown_inspect
            .stderr_text()
            .contains("unrecognized subcommand")
    );

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

    let mut unknown_probe = command(&sandbox.socket, &sandbox.root);
    unknown_probe.args(["probe", "--keep", "--claude", "/bin/sh"]);
    let unknown_probe = run(unknown_probe, None);
    assert_eq!(unknown_probe.status.code(), Some(2));
    assert!(unknown_probe.stdout.is_empty());
    assert!(
        unknown_probe
            .stderr_text()
            .contains("unrecognized subcommand")
    );
}
