#![cfg(unix)]

mod cli;
mod output;

use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::Path;

use anyhow::{Result, bail};
use clap::Parser;
use pseudomux_client::{ClientError, PmuxClient};
use pseudomux_protocol::v1::{
    DaemonDiagnosis, EffortLevel, HealthLayerName, ProbeOutcome, RunStatelessRequest,
    RuntimeFinding, SessionFinding,
};
use serde::Serialize;
use serde_json::Value;

use crate::cli::{Cli, Command, OutputMode, read_prompt, resolve_executable};

#[tokio::main]
async fn main() {
    if let Err(error) = execute(Cli::parse()).await {
        eprintln!("pmux: {error:#}");
        if let Some(recommendation) = server_error_details(&error) {
            eprintln!("pmux: {recommendation}");
        }
        std::process::exit(1);
    }
}

/// The ONE key inside a daemon refusal's `details` that is written to be read by
/// a person, and the only one this CLI prints.
///
/// THE DAEMON ALREADY NAMES THE ALTERNATIVE and the client was throwing it
/// away. `ClientError::Server`'s `Display` renders `code`, `message` and
/// `retryable` and drops `details`. MEASURED: `pmux ask --model no-such-model`
/// printed only
///
/// ```text
/// pmux: pmuxd error code=InvalidConfig message="model no-such-model is not
/// admitted to the stateless pool: a model with no table entry has no instance
/// class" retryable=false
/// ```
///
/// while the very same `ErrorBody` carried the answer.
///
/// ONE NAMED FIELD, NEVER THE WHOLE OBJECT. The first version of this printed
/// `details` verbatim, and `bin/pmux/tests/cli_contract_matrix.rs`'s
/// `every_command_and_output_mode_has_a_framed_runtime_failure_boundary` caught
/// it immediately: `details` is a general-purpose diagnostic channel that also
/// carries attach capability tokens and backend matcher text, and a rendering
/// that prints all of it prints those too. A key allowlist would be the wrong
/// repair for the same reason it is wrong everywhere else in this tree -- a
/// hand-written set beside a growing one. So the contract runs the other way:
/// `recommendation` is the advice channel, a refusal that has advice puts it
/// there, and nothing else in `details` is ever rendered.
///
/// Re-exported rather than respelled: the daemon writes this key through
/// [`pseudomux_protocol::v1::ErrorBody::advising`] and `bin/pmux-mcp` reads it
/// through the same constant, so a rename is a compile error in three places
/// instead of a channel that goes quiet in two of them.
use pseudomux_protocol::v1::RECOMMENDATION_KEY;

fn server_error_details(error: &anyhow::Error) -> Option<String> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<ClientError>())
        .and_then(|client_error| match client_error {
            ClientError::Server(body) => body
                .details
                .get(RECOMMENDATION_KEY)
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            _ => None,
        })
}

async fn execute(cli: Cli) -> Result<()> {
    if !cli.socket.is_absolute() {
        bail!("--socket/PMUX_SOCKET must be an absolute Unix socket path");
    }
    let client = PmuxClient::new(cli.socket.clone())?;
    let mode = cli.output;

    match cli.command {
        Command::Ping => {
            let pong = client.ping().await?;
            emit(
                mode,
                "pong",
                &pong,
                &format!(
                    "pong server={} protocol={}",
                    pong.server_version, pong.protocol_version
                ),
            )
        }
        Command::Run {
            model,
            effort,
            prompt,
            deadline_unix_ms,
        } => {
            let prompt = read_prompt(&prompt)?;
            let result = client
                .run_stateless(RunStatelessRequest {
                    model,
                    effort: effort.map(EffortLevel::from),
                    prompt,
                    deadline_unix_ms,
                })
                .await?;
            // The answer first, on its own, so `pmux run ... | head -1` is the
            // text and nothing else. The accounting follows it, and
            // `cache_read_input_tokens` is on the same line as `input_tokens`
            // deliberately: a cached prompt reports almost all of its context
            // there and almost none in `input_tokens`, MEASURED at
            // `input_tokens=2 cache_read_input_tokens=1130` for a 450-token
            // prompt. A reader shown only the first number would conclude the
            // turn carried no context at all.
            let text = format!(
                "{}\n\nmodel={}{}\neffort={}\nclaude={}\ninput_tokens={} output_tokens={} \
                 cache_creation_input_tokens={} cache_read_input_tokens={}",
                result.text,
                result.model,
                result
                    .reported_model
                    .as_deref()
                    .map(|reported| format!(" reported_model={reported}"))
                    .unwrap_or_default(),
                // `EffortLevel::as_str`, not `{effort:?}` lowercased. The
                // lowercase trick is correct only for a variant set in which no
                // spelling differs from its identifier by anything but case,
                // which is a property of today's five and not a rule -- the same
                // shape that put `--effort XHigh` in a refusal message.
                result
                    .effort
                    .map_or("-", pseudomux_protocol::v1::EffortLevel::as_str),
                result.claude_version,
                result.usage.main.input_tokens,
                result.usage.main.output_tokens,
                result.usage.main.cache_creation_input_tokens,
                result.usage.main.cache_read_input_tokens,
            );
            emit(mode, "stateless_result", &result, &text)
        }
        Command::Doctor { claude } => {
            let report = doctor(&client, &cli.socket, &claude).await;
            let status = report.status;
            let text = serde_json::to_string_pretty(&report)?;
            emit(mode, "doctor", &report, &text)?;
            // Both non-healthy states exit 1. Exit status 2 is reserved for the
            // parser and there is no third code to spend here: the distinction
            // the operator needs is `status`, which is emitted in every output
            // mode before this returns. What is NOT allowed is what used to
            // happen -- an unprovable answer exiting 0.
            match status {
                DoctorStatus::Healthy => Ok(()),
                DoctorStatus::Unhealthy => bail!("doctor checks failed"),
                DoctorStatus::Unproven => bail!("doctor could not prove every check it ran"),
            }
        }
    }
}

fn emit<T: Serialize>(mode: OutputMode, kind: &str, value: &T, text: &str) -> Result<()> {
    match mode {
        OutputMode::Text => output::text(text),
        OutputMode::Json => output::json(value),
        OutputMode::Ndjson => output::ndjson(kind, value),
    }
}

/// The one thing `doctor` is allowed to say, and the three answers it has.
///
/// `doctor` is a VIEW of the daemon's health tree plus the local checks only a
/// client can make -- socket mode, socket type, and Claude executable. It is
/// deliberately not a second health story: every claim
/// it makes about the daemon comes from `diagnose`'s layers, rendered through
/// their own `detail` strings, and the fold below is `ProbeOutcome`'s severity
/// order with the same three values under different names.
///
/// This replaced a `healthy: bool`. The boolean was not merely imprecise, it
/// was structurally incapable of being right: it was computed as
/// `errors.is_empty()` over four checks, three of which never left this
/// process and one of which reached only the daemon's accept loop. Every check
/// it did not run, and every check it could not run, arrived at the operator
/// as `"healthy": true`.
///
/// A third state is what makes "I could not prove it" expressible at all. The
/// fold errs toward refusing: an unprovable answer is a bad report, and a
/// wrong one is worse.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DoctorStatus {
    /// Every check this report lists ran to completion and passed.
    Healthy,
    /// At least one check ran to completion and could not be proven; none
    /// failed outright.
    Unproven,
    /// At least one check ran to completion and found a fault.
    Unhealthy,
}

#[derive(Serialize)]
struct DoctorReport {
    status: DoctorStatus,
    socket: String,
    socket_exists: bool,
    socket_is_unix_socket: bool,
    socket_owner_only: bool,
    server_version: Option<String>,
    protocol_version: Option<u16>,
    claude_executable: Option<String>,
    /// The daemon's own answer, verbatim.
    ///
    /// `None` means the daemon did not produce one, which is the case
    /// `unproven` exists for: a daemon too old to know `diagnose`, or one that
    /// could not be reached, leaves every claim about the private runtime and
    /// every session unmade. It used to leave them *unasked*, and that read as
    /// health.
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnosis: Option<DaemonDiagnosis>,
    /// Checks that ran and found a fault.
    errors: Vec<String>,
    /// Checks that could not be completed. Deliberately a separate list rather
    /// than a severity field inside `errors`: the two demand different
    /// operator responses, and a single list would have to be read to tell
    /// them apart.
    unproven: Vec<String>,
}

impl DoctorReport {
    /// `unhealthy` outranks `unproven`, which outranks `healthy`.
    const fn fold(errors: &[String], unproven: &[String]) -> DoctorStatus {
        if !errors.is_empty() {
            DoctorStatus::Unhealthy
        } else if !unproven.is_empty() {
            DoctorStatus::Unproven
        } else {
            DoctorStatus::Healthy
        }
    }
}

async fn doctor(client: &PmuxClient, socket: &Path, claude: &Path) -> DoctorReport {
    let mut errors = Vec::new();
    let mut unproven = Vec::new();
    let metadata = std::fs::metadata(socket);
    let (socket_exists, socket_is_unix_socket, socket_owner_only) = match metadata {
        Ok(metadata) => (
            true,
            metadata.file_type().is_socket(),
            metadata.permissions().mode() & 0o077 == 0,
        ),
        Err(error) => {
            errors.push(format!("socket metadata: {error}"));
            (false, false, false)
        }
    };
    if socket_exists && !socket_is_unix_socket {
        errors.push("socket path is not a Unix socket".into());
    }
    if socket_exists && !socket_owner_only {
        errors.push("socket has group/other permission bits; expected owner-only access".into());
    }

    let (server_version, protocol_version) = match client.ping().await {
        Ok(pong) if pong.protocol_version == pseudomux_protocol::v1::PROTOCOL_VERSION => {
            (Some(pong.server_version), Some(pong.protocol_version))
        }
        Ok(pong) => {
            errors.push(format!(
                "ping payload protocol {} does not match {}",
                pong.protocol_version,
                pseudomux_protocol::v1::PROTOCOL_VERSION
            ));
            (Some(pong.server_version), Some(pong.protocol_version))
        }
        Err(error) => {
            errors.push(format!("ping: {error}"));
            (None, None)
        }
    };
    // Attempted unconditionally, including after a failed ping. A skipped
    // probe is the failure mode this whole command is being repaired for; a
    // second symptom of one fault is only noise.
    let diagnosis = match client.diagnose().await {
        Ok(diagnosis) => {
            collect_diagnosis_findings(&diagnosis, &mut errors, &mut unproven);
            Some(diagnosis)
        }
        Err(error) => {
            unproven.push(format!(
                "private runtime: the daemon did not complete a diagnosis, \
                 so nothing behind its accept loop was tested: {error}"
            ));
            None
        }
    };
    let claude_executable = match resolve_executable(Some(claude)) {
        Ok(path) => Some(path.display().to_string()),
        Err(error) => {
            errors.push(format!("Claude executable: {error:#}"));
            None
        }
    };
    DoctorReport {
        status: DoctorReport::fold(&errors, &unproven),
        socket: socket.display().to_string(),
        socket_exists,
        socket_is_unix_socket,
        socket_owner_only,
        server_version,
        protocol_version,
        claude_executable,
        diagnosis,
        errors,
        unproven,
    }
}

/// Renders the daemon's typed findings into the two operator-facing lists.
///
/// The routing is `ProbeOutcome`'s and not this function's: a `fail` becomes an
/// error and an `unproven` becomes an unproven entry, whatever the finding is.
/// A local `match` on findings here would be a second copy of the severity
/// table, free to drift from the one the daemon derived its outcomes from.
fn collect_diagnosis_findings(
    diagnosis: &DaemonDiagnosis,
    errors: &mut Vec<String>,
    unproven: &mut Vec<String>,
) {
    // The layers first, because they are the report and the two lists below are
    // the narrower views of it that predate them. A layer's own `detail` is
    // rendered verbatim: it is the daemon's statement about what it exercised,
    // and a local re-wording here would be a second copy of the finding table.
    for layer in &diagnosis.layers {
        let text = format!("{}: {}", layer_name_text(layer.layer), layer.detail);
        match layer.outcome {
            ProbeOutcome::Pass => {}
            ProbeOutcome::Unproven => unproven.push(text),
            ProbeOutcome::Fail => errors.push(text),
        }
    }
    // A layer the daemon did not report at all. This is `unproven` and never
    // silence: an older daemon that knows `diagnose` but not the tree reaches
    // here, and reporting nothing for its layers is exactly the failure this
    // command was repaired for.
    for missing in diagnosis.missing_layers() {
        unproven.push(format!(
            "{}: the daemon reported no entry for this layer, so nothing about it was \
             established",
            layer_name_text(missing)
        ));
    }
    let runtime = format!(
        "private runtime: {} after {} ms",
        runtime_finding_text(diagnosis.runtime.finding),
        diagnosis.runtime.elapsed_ms
    );
    match diagnosis.runtime.outcome {
        ProbeOutcome::Pass => {}
        ProbeOutcome::Unproven => unproven.push(runtime),
        ProbeOutcome::Fail => errors.push(runtime),
    }
    for session in &diagnosis.sessions {
        let text = format!(
            "session {} generation {}: {}",
            session.session_id,
            session.generation_id,
            session_finding_text(session.finding)
        );
        match session.outcome {
            ProbeOutcome::Pass => {}
            ProbeOutcome::Unproven => unproven.push(text),
            ProbeOutcome::Fail => errors.push(text),
        }
    }
}

const fn layer_name_text(layer: HealthLayerName) -> &'static str {
    match layer {
        HealthLayerName::Configuration => "configuration",
        HealthLayerName::ControlPlane => "control plane",
        HealthLayerName::PrivateRuntime => "private runtime (rmux sidecar)",
        HealthLayerName::LaunchBroker => "launch broker",
        HealthLayerName::CompatibilityProfile => "compatibility profile",
        HealthLayerName::Pool => "stateless pool",
        HealthLayerName::Sessions => "sessions",
        HealthLayerName::Performance => "performance",
    }
}

const fn runtime_finding_text(finding: RuntimeFinding) -> &'static str {
    match finding {
        RuntimeFinding::PrivateRuntimeResponsive => {
            "the private rmux sidecar completed a dispatch-path request and the launch broker is accepting"
        }
        RuntimeFinding::ControlPlaneUnreachable => {
            "the private rmux socket could not be connected to"
        }
        RuntimeFinding::ControlPlaneUnresponsive => {
            "the private rmux sidecar accepted a connection and did not answer within its operation deadline"
        }
        RuntimeFinding::ControlPlaneRefused => "the private rmux sidecar answered with an error",
        RuntimeFinding::LaunchBrokerStopped => {
            "the launch broker's accept loop has ended; its socket still exists but no longer listens, so every later session start is refused at the launcher's connect"
        }
    }
}

const fn session_finding_text(finding: SessionFinding) -> &'static str {
    match finding {
        SessionFinding::TerminalPresent => {
            "the private rmux sidecar reports this session's terminal"
        }
        SessionFinding::TerminalMissing => {
            "pmux would still accept work here, and the private rmux sidecar does not report this session's terminal"
        }
        SessionFinding::SessionDeclaredUnusable => {
            "pmux has already declared this session unusable; close it to release its registry slot and its Claude process"
        }
        SessionFinding::SessionActorUnresponsive => {
            "this session's actor did not report its state within the probe's bound"
        }
        SessionFinding::SessionClosedDuringProbe => {
            "this session left the registry while the probe was running"
        }
        SessionFinding::NotProbed => {
            "the private control-plane probe did not complete, so this session's terminal was not looked for"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A daemon refusal's advice reaches the operator, and NOTHING else in
    /// `details` does.
    ///
    /// Both halves matter and the second one is why this test exists. The first
    /// version of `server_error_details` printed `details` verbatim, which put
    /// an attach capability token on stderr; `cli_contract_matrix.rs`'s
    /// `every_command_and_output_mode_has_a_framed_runtime_failure_boundary`
    /// caught it. The repair is a CONTRACT rather than a key allowlist:
    /// `recommendation` is the channel a refusal writes advice to, and it is the
    /// only key rendered. The sensitive keys below are the exact ones that
    /// matrix plants.
    #[test]
    fn a_daemon_refusal_renders_its_recommendation_and_no_other_detail() {
        let advised = anyhow::Error::from(ClientError::Server(
            pseudomux_protocol::v1::ErrorBody::new(
                pseudomux_protocol::v1::ErrorCode::InvalidConfig,
                "model no-such-model is not admitted to the stateless pool",
            )
            .with_details(serde_json::json!({
                "violation": "model_not_admitted_to_pool",
                "recommendation": "name one of claude-opus-5, claude-sonnet-5",
                "attach_token": "attach-capability-token-secret",
                "backend_matcher": "backend-matcher-secret",
            })),
        ));
        let rendered = server_error_details(&advised).expect("advice must reach the operator");
        assert_eq!(rendered, "name one of claude-opus-5, claude-sonnet-5");
        for secret in ["attach-capability-token-secret", "backend-matcher-secret"] {
            assert!(
                !rendered.contains(secret),
                "the CLI rendered {secret:?} out of a refusal's details: {rendered}"
            );
        }

        // A refusal with details but no advice renders none of them.
        let unadvised = anyhow::Error::from(ClientError::Server(
            pseudomux_protocol::v1::ErrorBody::new(
                pseudomux_protocol::v1::ErrorCode::Internal,
                "bounded public rejection",
            )
            .with_details(serde_json::json!({
                "attach_token": "attach-capability-token-secret",
            })),
        ));
        assert_eq!(server_error_details(&unadvised), None);

        // And a refusal with no details at all is not a panic.
        let bare =
            anyhow::Error::from(ClientError::Server(pseudomux_protocol::v1::ErrorBody::new(
                pseudomux_protocol::v1::ErrorCode::Internal,
                "bare",
            )));
        assert_eq!(server_error_details(&bare), None);
        assert_eq!(
            server_error_details(&anyhow::anyhow!("not a server error")),
            None
        );
    }

    #[test]
    fn doctor_report_does_not_name_process_cwd() {
        let report = DoctorReport {
            status: DoctorStatus::Healthy,
            socket: "/tmp/pmux.sock".into(),
            socket_exists: true,
            socket_is_unix_socket: true,
            socket_owner_only: true,
            server_version: None,
            protocol_version: None,
            claude_executable: None,
            diagnosis: None,
            errors: Vec::new(),
            unproven: Vec::new(),
        };
        let value = serde_json::to_value(&report).unwrap();
        assert!(value.get("cwd").is_none(), "{value}");
        assert_eq!(DoctorReport::fold(&[], &[]), DoctorStatus::Healthy);

        const SOURCE: &str = include_str!("main.rs");
        let start = SOURCE
            .find("async fn doctor(")
            .expect("doctor() is no longer in this module");
        let tail = &SOURCE[start..];
        let end = tail
            .find("\nfn collect_diagnosis_findings(")
            .expect("doctor() is no longer followed by collect_diagnosis_findings");
        let doctor = &tail[..end];
        assert!(
            !doctor.contains("resolve_cwd"),
            "doctor() must not resolve a process cwd"
        );
        assert!(
            !doctor.contains("current directory"),
            "doctor() must not name the process current directory"
        );
        assert!(
            !doctor.contains("current_dir"),
            "doctor() must not call std::env::current_dir for health"
        );
        assert!(
            !doctor.contains("cwd:") && !doctor.contains("cwd ="),
            "doctor() must not assign a cwd field"
        );
    }
}
