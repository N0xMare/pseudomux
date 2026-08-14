//! The caller's explicit environment channel, and the audit surface that makes
//! it discoverable.
//!
//! `crates/service/src/claude_launch.rs` filters the inherited snapshot with an
//! allowlist and justifies that with two claims: `EnvironmentSpec::set` is the
//! deliberate bypass, and `pmux probe` stays an honest audit surface. Both are
//! only true if the CLI can reach them, so this file pins:
//!
//! * `--env KEY=VALUE` and `--env-passthrough KEY` reach `set`, `--unset KEY`
//!   reaches `unset`, and every rejection is a parsed-local-validation exit 1;
//! * `--env-passthrough` puts the **name** on the command line and the value
//!   nowhere but the framed request, which is the whole reason it exists;
//! * `pmux probe` lists what the launch policy drops, by name, with no value.
//!
//! There is no longer a drift fence here. The CLI and the daemon evaluate the
//! same `pseudomux_protocol::v1::launch_environment` tables and predicate, so
//! there are no copies left to diverge; the tables' own well-formedness is
//! pinned once, next to them, in `crates/protocol/tests/v1_launch_environment.rs`.

#![cfg(unix)]

#[allow(dead_code)]
mod support;

use std::collections::BTreeSet;
use std::process::Command;

use pseudomux_protocol::v1::{Request, StartSessionRequest};
use serde_json::Value;

use support::{
    SESSION_ID, Sandbox, command, pmux_process, run, session_handle, spawn_native_server, success,
};

/// A value that must never appear in argv, stdout, or stderr.
const PASSTHROUGH_SECRET: &str = "passthrough-value-must-not-reach-argv";

fn launch_command(sandbox: &Sandbox, subcommand: &str) -> Command {
    let mut process = command(&sandbox.socket, &sandbox.root);
    process.args([
        "--output",
        "json",
        subcommand,
        // The fake daemon answers with the shared handle, and the client
        // validates that the identity it gets back is the one it asked for.
        "--session-id",
        SESSION_ID,
        "--claude",
        "/bin/sh",
        "--cwd",
    ]);
    process.arg(&sandbox.root);
    process
}

fn start_command(sandbox: &Sandbox) -> Command {
    launch_command(sandbox, "start")
}

fn probe_command(sandbox: &Sandbox) -> Command {
    launch_command(sandbox, "probe")
}

/// Runs one `pmux start` against a fake daemon and returns the exact DTO the
/// CLI framed, so the assertion is about the wire and not about a local struct.
fn framed_start_request(sandbox: &Sandbox, process: Command) -> StartSessionRequest {
    let server = spawn_native_server(
        sandbox.bind(),
        vec![success("session_started", session_handle())],
    );
    let output = run(process, None);
    assert!(
        output.status.success(),
        "start failed: {}",
        output.stderr_text()
    );
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 1);
    match requests.into_iter().next().unwrap().request {
        Request::StartSession(request) => request,
        other => panic!("expected start_session, got {other:?}"),
    }
}

#[test]
fn env_reaches_the_explicit_set_channel_the_allowlist_never_filters() {
    let sandbox = Sandbox::new("env-set");
    let mut process = start_command(&sandbox);
    // `MCP_GATEWAY_TOKEN` is the review's motivating case: nothing forbids it,
    // the allowlist drops it anyway, and `set` is the only way back.
    process.args([
        "--env",
        "MCP_GATEWAY_TOKEN=explicit-value",
        "--env",
        "EMPTY_IS_LEGAL=",
        "--env",
        "EQUALS_IN_VALUE=a=b=c",
    ]);

    let request = framed_start_request(&sandbox, process);
    assert_eq!(
        request
            .environment
            .set
            .get("MCP_GATEWAY_TOKEN")
            .map(String::as_str),
        Some("explicit-value")
    );
    assert_eq!(
        request
            .environment
            .set
            .get("EMPTY_IS_LEGAL")
            .map(String::as_str),
        Some(""),
        "an empty VALUE is a legal explicit assignment"
    );
    assert_eq!(
        request
            .environment
            .set
            .get("EQUALS_IN_VALUE")
            .map(String::as_str),
        Some("a=b=c"),
        "only the first `=` separates KEY from VALUE"
    );
    assert!(
        !request
            .environment
            .snapshot
            .contains_key("MCP_GATEWAY_TOKEN"),
        "--env must not be smuggled into the inherited snapshot"
    );
    assert!(request.environment.unset.is_empty());
}

#[test]
fn malformed_environment_arguments_are_parsed_local_validation_failures() {
    for (arguments, expected) in [
        (vec!["--env", "NO_SEPARATOR"], "no `=` separator"),
        (vec!["--env", "=orphan-value"], "non-empty environment"),
        (vec!["--env-passthrough", ""], "non-empty environment"),
        (
            vec!["--env-passthrough", "HAS=EQUALS"],
            "may not contain `=`",
        ),
        (vec!["--unset", ""], "non-empty environment"),
        (vec!["--unset", "HAS=EQUALS"], "may not contain `=`"),
        (
            vec!["--env", "DUPLICATE=one", "--env", "DUPLICATE=two"],
            "provided more than once",
        ),
        (
            vec!["--env", "BOTH=one", "--unset", "BOTH"],
            "both unset and set",
        ),
    ] {
        let sandbox = Sandbox::new("env-malformed");
        // No listener is bound: a local validation failure must happen before
        // anything is framed, so the absent daemon must not be what fails.
        let mut process = start_command(&sandbox);
        process.args(&arguments);
        let output = run(process, None);

        assert_eq!(
            output.status.code(),
            Some(1),
            "{arguments:?} is parsed local validation, not parser misuse: {}",
            output.stderr_text()
        );
        assert!(output.stdout.is_empty(), "{arguments:?} polluted stdout");
        let stderr = output.stderr_text();
        assert!(
            stderr.contains(expected),
            "{arguments:?} diagnostic omitted {expected:?}: {stderr}"
        );
        assert!(
            !stderr.contains("I/O error"),
            "{arguments:?} reached the daemon before validating: {stderr}"
        );
    }
}

// NUL rejection cannot be driven through a subprocess -- `std::process::Command`
// refuses to build an argv containing one -- so it is pinned in
// `bin/pmux/src/cli.rs::tests::env_rejects_nul_without_echoing_the_value`.

#[test]
fn env_passthrough_forwards_a_present_variable_and_keeps_its_value_out_of_argv() {
    let sandbox = Sandbox::new("passthrough-present");
    let mut process = start_command(&sandbox);
    process
        .env("FORWARDED_SECRET", PASSTHROUGH_SECRET)
        .args(["--env-passthrough", "FORWARDED_SECRET"]);

    // The point of the flag: the value is in the child's environment, and the
    // command line -- which is what `ps` publishes to every process on the host
    // -- carries only the name.
    assert!(
        process
            .get_args()
            .all(|argument| argument != std::ffi::OsStr::new(PASSTHROUGH_SECRET)),
        "the secret value was placed in argv"
    );
    assert!(
        process
            .get_args()
            .any(|argument| argument == std::ffi::OsStr::new("FORWARDED_SECRET")),
        "the variable name is expected in argv"
    );

    let request = framed_start_request(&sandbox, process);
    assert_eq!(
        request
            .environment
            .set
            .get("FORWARDED_SECRET")
            .map(String::as_str),
        Some(PASSTHROUGH_SECRET),
        "the value must reach the explicit set channel"
    );
}

#[test]
fn env_passthrough_names_the_variable_when_it_is_absent_or_empty() {
    for (value, expected) in [
        (None, "is not set in pmux's own environment"),
        (Some(""), "is set but empty"),
    ] {
        let sandbox = Sandbox::new("passthrough-missing");
        let mut process = start_command(&sandbox);
        if let Some(value) = value {
            process.env("ABSENT_FORWARD", value);
        }
        process.args(["--env-passthrough", "ABSENT_FORWARD"]);
        let output = run(process, None);

        assert_eq!(output.status.code(), Some(1), "{}", output.stderr_text());
        assert!(output.stdout.is_empty());
        let stderr = output.stderr_text();
        assert!(stderr.contains("ABSENT_FORWARD"), "{stderr}");
        assert!(stderr.contains(expected), "{stderr}");
    }
}

#[test]
fn unset_reaches_the_unset_channel_and_removes_an_allowlisted_name() {
    let sandbox = Sandbox::new("unset");
    let mut process = start_command(&sandbox);
    process.args(["--unset", "LANG", "--unset", "LANG", "--unset", "TERMINFO"]);

    let request = framed_start_request(&sandbox, process);
    assert_eq!(
        request.environment.unset,
        BTreeSet::from(["LANG".to_owned(), "TERMINFO".to_owned()]),
        "repeats collapse; both names reach `unset`"
    );
    assert!(
        request.environment.snapshot.contains_key("LANG"),
        "the snapshot stays exact; `unset` is the patch that removes the name"
    );
    assert!(request.environment.set.is_empty());
}

fn probe_report(output: &support::ProcessOutput) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "probe stdout is not JSON ({error}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn removed_names(report: &Value) -> Vec<String> {
    report["environment_removed"]["names"]
        .as_array()
        .expect("probe must publish environment_removed.names")
        .iter()
        .map(|name| name.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn probe_lists_the_names_the_launch_policy_drops_and_no_values() {
    let sandbox = Sandbox::new("probe-drops");
    let mut process = probe_command(&sandbox);
    process
        // Denied by the allowlist: nothing in the product forbids it, and that
        // is exactly the case a user cannot otherwise diagnose.
        .env("EDITOR", "environment-secret")
        // Removed by the subscription auth policy.
        .env("ANTHROPIC_API_KEY", "environment-secret")
        // Removed by the transparent terminal profile.
        .env("TERM_PROGRAM", "environment-secret")
        // Allowlisted: must not be reported as removed.
        .env("SSH_AUTH_SOCK", "/tmp/agent.sock")
        // Dropped by the allowlist, then restored through the explicit channel.
        .env("RESTORED_BY_SET", "environment-secret")
        .args(["--env-passthrough", "RESTORED_BY_SET"]);
    let output = run(process, None);
    assert!(
        output.status.success(),
        "dry-run probe failed: {}",
        output.stderr_text()
    );

    let report = probe_report(&output);
    let names = removed_names(&report);
    for expected in ["EDITOR", "ANTHROPIC_API_KEY", "TERM_PROGRAM"] {
        assert!(
            names.iter().any(|name| name == expected),
            "probe omitted the dropped name {expected}: {names:?}"
        );
    }
    for unexpected in ["SSH_AUTH_SOCK", "PATH", "HOME", "RESTORED_BY_SET"] {
        assert!(
            !names.iter().any(|name| name == unexpected),
            "probe reported a delivered name {unexpected} as removed: {names:?}"
        );
    }
    assert_eq!(
        report["environment_removed"]["count"].as_u64().unwrap() as usize,
        names.len()
    );
    assert!(names.windows(2).all(|pair| pair[0] < pair[1]), "{names:?}");

    // Names only. The redactor already treats environment values as sensitive
    // and this surface must not become the hole in it.
    let rendered = format!("{}\n{}", output.stdout_text(), output.stderr_text());
    assert!(
        !rendered.contains("environment-secret"),
        "probe leaked an environment value: {rendered}"
    );
    assert!(!rendered.contains("/tmp/agent.sock"), "{rendered}");
    // The report has to say how to get a dropped name back, or it diagnoses
    // without resolving.
    let note = report["environment_removed"]["note"].as_str().unwrap();
    assert!(note.contains("--env-passthrough"), "{note}");
    assert!(note.contains("--env KEY=VALUE"), "{note}");
}

#[test]
fn probe_launch_reports_the_same_removal_surface_as_the_dry_run() {
    let sandbox = Sandbox::new("probe-launch-drops");
    let server = spawn_native_server(
        sandbox.bind(),
        vec![
            success("session_started", session_handle()),
            success("session_snapshot", support::snapshot(0)),
            success("session_closed", support::close_result(true)),
        ],
    );
    let mut process = probe_command(&sandbox);
    process.arg("--launch").env("EDITOR", "environment-secret");
    let output = run(process, None);
    assert!(
        output.status.success(),
        "probe --launch failed: {}",
        output.stderr_text()
    );
    assert_eq!(server.join().unwrap().len(), 3);

    let report = probe_report(&output);
    assert_eq!(report["launched"], true);
    let names = removed_names(&report);
    assert!(
        names.iter().any(|name| name == "EDITOR"),
        "probe --launch omitted the dropped name: {names:?}"
    );
    assert!(!output.stdout_text().contains("environment-secret"));
}

#[test]
fn probe_help_documents_the_allowlist_and_both_escape_hatches() {
    let mut process = pmux_process();
    process
        .env_clear()
        .args(["--socket", "/tmp/pmux.sock", "start", "--help"]);
    let output = run(process, None);

    assert!(output.status.success(), "{}", output.stderr_text());
    let help = output.stdout_text();
    assert!(help.contains("allowlisted"), "{help}");
    assert!(help.contains("--env <KEY=VALUE>"), "{help}");
    assert!(help.contains("--env-passthrough <KEY>"), "{help}");
    assert!(help.contains("--unset <KEY>"), "{help}");
}
