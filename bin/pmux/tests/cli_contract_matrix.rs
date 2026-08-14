#![cfg(unix)]

#[allow(dead_code)]
mod support;

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};

use support::{
    GENERATION_ID, NativeReply, SESSION_ID, Sandbox, TURN_ID, command, json_lines, run,
    session_handle, spawn_native_server, success,
};

const OUTPUT_MODES: [&str; 3] = ["text", "json", "ndjson"];
const SETTINGS_SECRET: &str = "settings-config-secret";
const SYSTEM_PROMPT_SECRET: &str = "system-prompt-secret";
const TURN_PROMPT_SECRET: &str = "turn-prompt-secret";
const PEER_DETAIL_SECRET: &str = "backend-matcher-secret";
const CAPABILITY_SECRET: &str = "attach-capability-token-secret";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Surface {
    Ping,
    Start,
    Turn,
    Run,
    Inspect,
    Cancel,
    Close,
    Clear,
    Attach,
    Doctor,
    Probe,
    Ask,
    Agent,
}

impl Surface {
    const ALL: [Self; 13] = [
        Self::Ping,
        Self::Start,
        Self::Turn,
        Self::Run,
        Self::Inspect,
        Self::Cancel,
        Self::Close,
        Self::Clear,
        Self::Attach,
        Self::Doctor,
        Self::Probe,
        Self::Ask,
        Self::Agent,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::Ping => "ping",
            Self::Start => "start",
            Self::Turn => "turn",
            Self::Run => "run",
            Self::Inspect => "inspect",
            Self::Cancel => "cancel",
            Self::Close => "close",
            Self::Clear => "clear",
            Self::Attach => "attach",
            Self::Doctor => "doctor",
            Self::Probe => "probe",
            Self::Ask => "ask",
            Self::Agent => "agent",
        }
    }

    fn valid_command(self, socket: &Path, root: &Path, mode: &str) -> Command {
        let mut process = command(socket, root);
        process.args(["--output", mode]);
        self.append_valid_args(&mut process, root);
        process
    }

    fn append_valid_args(self, process: &mut Command, root: &Path) {
        match self {
            Self::Ping => {
                process.arg("ping");
            }
            Self::Start => {
                process.args([
                    "start",
                    "--session-id",
                    SESSION_ID,
                    "--claude",
                    "/bin/sh",
                    "--cwd",
                ]);
                process.arg(root).args([
                    "--settings-json",
                    r#"{"token":"settings-config-secret"}"#,
                    "--system-prompt",
                    SYSTEM_PROMPT_SECRET,
                ]);
            }
            Self::Turn => {
                process.args([
                    "turn",
                    SESSION_ID,
                    "--generation",
                    GENERATION_ID,
                    "--turn-id",
                    TURN_ID,
                    TURN_PROMPT_SECRET,
                ]);
            }
            Self::Run => {
                process.args([
                    "run",
                    "--session-id",
                    SESSION_ID,
                    "--turn-id",
                    TURN_ID,
                    "--claude",
                    "/bin/sh",
                    "--cwd",
                ]);
                process.arg(root).args([
                    "--settings-json",
                    r#"{"token":"settings-config-secret"}"#,
                    "--system-prompt",
                    SYSTEM_PROMPT_SECRET,
                    TURN_PROMPT_SECRET,
                ]);
            }
            Self::Inspect => {
                process.args(["inspect", SESSION_ID, "--generation", GENERATION_ID]);
            }
            Self::Cancel => {
                process.args(["cancel", SESSION_ID, "--generation", GENERATION_ID, TURN_ID]);
            }
            Self::Close => {
                process.args(["close", SESSION_ID, "--generation", GENERATION_ID]);
            }
            Self::Clear => {
                process.args([
                    "clear",
                    SESSION_ID,
                    "--generation",
                    GENERATION_ID,
                    "--expect-transcript",
                    SESSION_ID,
                ]);
            }
            Self::Attach => {
                process.args(["attach", SESSION_ID, "--generation", GENERATION_ID]);
            }
            Self::Doctor => {
                process.args(["doctor", "--claude", "/bin/sh", "--cwd"]);
                process.arg(root);
            }
            Self::Probe => {
                process.args([
                    "probe",
                    "--launch",
                    "--session-id",
                    SESSION_ID,
                    "--claude",
                    "/bin/sh",
                    "--cwd",
                ]);
                process.arg(root).args([
                    "--settings-json",
                    r#"{"token":"settings-config-secret"}"#,
                    "--system-prompt",
                    SYSTEM_PROMPT_SECRET,
                ]);
            }
            Self::Ask => {
                process.args(["ask", "--model", "sonnet", TURN_PROMPT_SECRET]);
            }
            Self::Agent => {
                process.args(["agent", "list"]);
            }
        }
    }
}

fn public_runtime_error() -> NativeReply {
    NativeReply::Error {
        code: "internal",
        message: "bounded public rejection",
        retryable: false,
        details: json!({
            "backend_matcher": PEER_DETAIL_SECRET,
            "attach_token": CAPABILITY_SECRET,
        }),
    }
}

/// The subcommand set is DERIVED from the binary, not restated in [`Surface`].
///
/// It was restated: `ALL` was eleven of `Command`'s thirteen variants, and the
/// two it omitted were `ask` -- which `pmux --help` calls the entire surface of
/// Path B -- and `agent`. Neither had a framed runtime failure, a malformed
/// frame, an unavailable daemon, a parser boundary or a local-validation
/// boundary asserted in any output mode, under a file whose every test is named
/// "for every command".
#[test]
fn the_matrix_covers_every_subcommand_pmux_publishes() {
    let sandbox = Sandbox::new("subcommand-census");
    let output = run(
        {
            let mut process = command(&sandbox.socket, &sandbox.root);
            process.arg("--help");
            process
        },
        None,
    );
    let help = String::from_utf8(output.stdout).expect("--help is utf-8");
    let (_, commands) = help
        .split_once("Commands:")
        .expect("`pmux --help` no longer publishes a command list");
    let published = commands
        .lines()
        .take_while(|line| !line.starts_with("Options:"))
        .filter_map(|line| {
            let name = line.strip_prefix("  ")?.split_whitespace().next()?;
            (name != "help" && name.starts_with(char::is_lowercase)).then(|| name.to_owned())
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        published.len() > 8,
        "parsed only {published:?} out of `pmux --help`"
    );
    let covered = Surface::ALL
        .iter()
        .map(|surface| surface.name().to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(published, covered);
}

fn wrong_result(surface: Surface) -> NativeReply {
    if matches!(surface, Surface::Ping | Surface::Doctor) {
        success("session_started", session_handle())
    } else {
        success(
            "pong",
            json!({"server_version": "wrong-result", "protocol_version": 1}),
        )
    }
}

fn failure_rendering(surface: Surface, mode: &str, stdout: &[u8]) -> String {
    if surface != Surface::Doctor {
        assert!(
            stdout.is_empty(),
            "{} {mode} polluted stdout on failure: {}",
            surface.name(),
            String::from_utf8_lossy(stdout)
        );
        return String::new();
    }

    let report = match mode {
        "ndjson" => {
            let records = json_lines(stdout);
            assert_eq!(records.len(), 1);
            assert_eq!(records[0]["type"], "doctor");
            records[0]["data"].clone()
        }
        "text" | "json" => serde_json::from_slice::<Value>(stdout).unwrap(),
        other => panic!("unexpected output mode {other}"),
    };
    // Stronger than the `healthy == false` this replaced. Every boundary in
    // this matrix breaks the daemon in a way `doctor` can positively observe --
    // an absent socket, a malformed frame, a wrong result type -- so the report
    // must land on `unhealthy` and not on `unproven`. A report that could only
    // say "not healthy" would pass this assertion while having proven nothing.
    assert_eq!(report["status"], "unhealthy");
    serde_json::to_string(&report).unwrap()
}

fn assert_no_sensitive_values(rendered: &str) {
    for secret in [
        SETTINGS_SECRET,
        SYSTEM_PROMPT_SECRET,
        TURN_PROMPT_SECRET,
        PEER_DETAIL_SECRET,
        CAPABILITY_SECRET,
        "environment-secret",
    ] {
        assert!(
            !rendered.contains(secret),
            "failure rendering exposed {secret:?}: {rendered}"
        );
    }
}

#[test]
fn every_command_and_output_mode_has_a_framed_runtime_failure_boundary() {
    for surface in Surface::ALL {
        for mode in OUTPUT_MODES {
            let sandbox = Sandbox::new(&format!("{}-{mode}-runtime", surface.name()));
            let server = spawn_native_server(sandbox.bind(), vec![public_runtime_error()]);
            let output = run(
                surface.valid_command(&sandbox.socket, &sandbox.root, mode),
                None,
            );

            assert_eq!(
                output.status.code(),
                Some(1),
                "{} {mode}: {}",
                surface.name(),
                output.stderr_text()
            );
            let report = failure_rendering(surface, mode, &output.stdout);
            let rendered = format!("{report}\n{}", output.stderr_text());
            assert!(rendered.contains("bounded public rejection"));
            assert_no_sensitive_values(&rendered);
            assert_eq!(server.join().unwrap().len(), 1);
        }
    }
}

#[test]
fn every_daemon_using_command_rejects_malformed_and_wrong_result_frames() {
    for surface in Surface::ALL {
        for (fault, reply, expected) in [
            (
                "malformed",
                NativeReply::Malformed(b"{\"partial\":\"peer-payload-secret\"".to_vec()),
                "invalid JSON frame",
            ),
            ("wrong-result", wrong_result(surface), "expected"),
        ] {
            let sandbox = Sandbox::new(&format!("{}-{fault}", surface.name()));
            let server = spawn_native_server(sandbox.bind(), vec![reply]);
            let output = run(
                surface.valid_command(&sandbox.socket, &sandbox.root, "json"),
                None,
            );

            assert_eq!(output.status.code(), Some(1), "{} {fault}", surface.name());
            let report = failure_rendering(surface, "json", &output.stdout);
            let rendered = format!("{report}\n{}", output.stderr_text());
            assert!(
                rendered.contains(expected),
                "{} {fault} omitted {expected:?}: {rendered}",
                surface.name()
            );
            assert!(!rendered.contains("peer-payload-secret"));
            assert_no_sensitive_values(&rendered);
            assert_eq!(server.join().unwrap().len(), 1);
        }
    }
}

#[test]
fn every_daemon_using_command_has_a_bounded_unavailable_boundary() {
    for surface in Surface::ALL {
        let sandbox = Sandbox::new(&format!("{}-unavailable", surface.name()));
        let output = run(
            surface.valid_command(&sandbox.socket, &sandbox.root, "json"),
            None,
        );

        assert_eq!(
            output.status.code(),
            Some(1),
            "{} unexpectedly succeeded",
            surface.name()
        );
        let report = failure_rendering(surface, "json", &output.stdout);
        let rendered = format!("{report}\n{}", output.stderr_text());
        assert!(
            rendered.contains("I/O error"),
            "{} unavailable diagnostic: {rendered}",
            surface.name()
        );
        assert_no_sensitive_values(&rendered);
    }
}

#[test]
fn parser_misuse_is_exit_two_for_every_command() {
    for surface in Surface::ALL {
        let sandbox = Sandbox::new(&format!("{}-parser", surface.name()));
        let prompt_file = sandbox.root.join("prompt.txt");
        fs::write(&prompt_file, "file prompt").unwrap();
        let mut process = command(&sandbox.socket, &sandbox.root);

        match surface {
            Surface::Ping => {
                process.args(["ping", "--definitely-invalid"]);
            }
            Surface::Start => {
                process.args(["start", "--session-id", SESSION_ID, "--resume", SESSION_ID]);
            }
            Surface::Turn => {
                process.args([
                    "turn",
                    "not-a-uuid",
                    "--generation",
                    GENERATION_ID,
                    "prompt",
                ]);
            }
            Surface::Run => {
                process.args(["run", "--claude", "/bin/sh", "positional"]);
                process.arg("--prompt-file").arg(&prompt_file);
            }
            Surface::Inspect => {
                process.args(["inspect", SESSION_ID]);
            }
            Surface::Cancel => {
                process.args([
                    "cancel",
                    SESSION_ID,
                    "--generation",
                    GENERATION_ID,
                    "not-a-uuid",
                ]);
            }
            Surface::Close => {
                process.args([
                    "close",
                    SESSION_ID,
                    "--generation",
                    GENERATION_ID,
                    "--policy",
                    "not-a-policy",
                ]);
            }
            Surface::Clear => {
                process.args([
                    "clear",
                    SESSION_ID,
                    "--generation",
                    GENERATION_ID,
                    "--expect-transcript",
                    "not-a-uuid",
                ]);
            }
            Surface::Attach => {
                process.args([
                    "attach",
                    SESSION_ID,
                    "--generation",
                    GENERATION_ID,
                    "--rows",
                    "24",
                ]);
            }
            Surface::Doctor => {
                process.args(["doctor", "--definitely-invalid"]);
            }
            Surface::Probe => {
                process.args(["probe", "--keep", "--claude", "/bin/sh"]);
            }
            Surface::Ask => {
                process.args(["ask", "--effort", "definitely-not-a-tier", "prompt"]);
            }
            Surface::Agent => {
                process.args(["agent", "get", "not-a-uuid"]);
            }
        }

        let output = run(process, None);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{} parser boundary: {}",
            surface.name(),
            output.stderr_text()
        );
        assert!(output.stdout.is_empty());
        assert!(output.stderr_text().starts_with("error:"));
        assert_no_sensitive_values(&output.stderr_text());
    }
}

#[test]
fn parsed_local_validation_is_exit_one_for_every_command() {
    for surface in Surface::ALL {
        let sandbox = Sandbox::new(&format!("{}-local-validation", surface.name()));
        let output = run(
            surface.valid_command(Path::new("relative.sock"), &sandbox.root, "json"),
            None,
        );

        assert_eq!(output.status.code(), Some(1), "{}", surface.name());
        assert!(output.stdout.is_empty());
        assert!(
            output
                .stderr_text()
                .contains("must be an absolute Unix socket path")
        );
        assert_no_sensitive_values(&output.stderr_text());
    }
}
