#![cfg(unix)]

mod cli;
mod output;

use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::Path;

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use pseudomux_client::{
    ClientError, DEFAULT_RUN_ONCE_TIMEOUT, EventStreamItem, EventStreamOptions, PmuxClient,
    RUN_ONCE_RESPONSE_MARGIN, SequencedEventStream,
};
use pseudomux_protocol::v1::{
    AgentDescriptor, AgentVersion, AttachSessionRequest, CancelOutcome, CancelTurnResult,
    ClosePolicy, CloseSessionResult, DaemonDiagnosis, EffortLevel, EventPayload, HealthLayerName,
    ProbeOutcome, RunStatelessRequest, RuntimeFinding, SessionFinding, SessionGenerationId,
    SessionId, StartSessionRequest, TerminalSize, TurnId, TurnOutcome, TurnRequest, TurnResult,
};
use serde::Serialize;
use serde_json::Value;

use crate::cli::{
    AgentCommand, Cli, Command, OutputMode, build_agent_create_spec, build_start_request,
    build_turn_request, dropped_environment_names, read_agent_spec, read_prompt, resolve_cwd,
    resolve_executable,
};

#[tokio::main]
async fn main() {
    // NOT `Cli::parse()`. The launch flags carry an `env =` binding, and once
    // the derived struct exists a value clap read from the environment and one
    // the caller typed are the same `Some`. `--agent` refuses the second and
    // must not refuse the first: see `Cli::parse_recording_argument_sources`.
    if let Err(error) = execute(Cli::parse_recording_argument_sources()).await {
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

/// A stored agent version number, refused at the client where the caller can
/// still fix it.
fn agent_version(value: u64) -> Result<AgentVersion> {
    AgentVersion::new(value)
        .map_err(|_| anyhow::anyhow!("an agent version starts at 1; there is no version 0"))
}

/// The text rendering of one stored agent version.
///
/// It prints the digest, which is IDENTITY, beside the version, which is only
/// ORDER -- because the question a caller actually has after an update is "is
/// this the configuration I wrote", and two versions with equal digests are the
/// same configuration.
///
/// Environment values and inline document bodies arrive already redacted by the
/// daemon; nothing is redacted here, so this rendering cannot disagree with the
/// `--output json` frame beside it.
fn render_agent_descriptor(descriptor: &AgentDescriptor) -> Result<String> {
    // Decoded with the STRICT type, which is where `deny_unknown_fields` is
    // wanted: a daemon that answered with a field this build does not
    // understand is named here rather than rendered as if it were understood.
    // The `--output json` frame beside this one still carried the whole
    // document, so nothing is lost by refusing to summarize it.
    let spec = descriptor.typed_spec().with_context(|| {
        format!(
            "agent {} version {} carries a configuration this pmux does not understand; read it \
             with --output json",
            descriptor.agent_id, descriptor.version
        )
    })?;
    let spec = &spec;
    let mut lines = vec![
        format!("agent_id={}", descriptor.agent_id),
        format!("version={}", descriptor.version),
        format!("config_digest={}", descriptor.config_digest),
        format!("name={}", spec.name),
    ];
    if let Some(description) = &spec.description {
        lines.push(format!("description={description}"));
    }
    lines.push(format!("claude={}", spec.claude.executable));
    if let Some(model) = &spec.claude.model {
        lines.push(format!("model={model}"));
    }
    if let Some(effort) = spec.claude.effort {
        lines.push(format!("effort={effort}"));
    }
    lines.push(format!("cell={}", spec.cell));
    if let Some(root) = &spec.containment.workspace_root {
        lines.push(format!("containment.workspace_root={root}"));
    }
    lines.push(format!(
        "containment.require_config_isolation={}",
        spec.containment.require_config_isolation
    ));
    if !spec.environment.set.is_empty() {
        lines.push(format!(
            "environment.set={} (values are sha256 digests; the daemon never returns them in the clear)",
            spec.environment.set.keys().cloned().collect::<Vec<_>>().join(",")
        ));
    }
    if !spec.environment.unset.is_empty() {
        lines.push(format!(
            "environment.unset={}",
            spec.environment
                .unset
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    Ok(lines.join("\n"))
}

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
        Command::Start { launch } => {
            let request = build_start_request(&launch)?;
            let handle = client.start_session(request).await?;
            emit(
                mode,
                "session_started",
                &handle,
                &format!(
                    "session_id={}\ngeneration_id={}",
                    handle.session_id, handle.generation_id
                ),
            )
        }
        Command::Oneshot {
            launch,
            prompt,
            turn,
        } => {
            let start = build_start_request(&launch)?;
            let prompt = read_prompt(&prompt)?;
            let turn = build_turn_request(&turn, prompt)?;
            let handle = client.start_session(start).await?;
            eprintln!("pmux: session {} started", handle.session_id);
            let result =
                execute_turn(&client, handle.session_id, handle.generation_id, turn, mode).await;
            let close_policy = if result.is_ok() {
                ClosePolicy::Graceful
            } else {
                ClosePolicy::Force
            };
            let cleanup = close_session_with_proof(
                &client,
                handle.session_id,
                handle.generation_id,
                close_policy,
            )
            .await
            .map(|_| ());
            finish_run(mode, result, cleanup)
        }
        Command::Turn {
            session,
            generation,
            prompt,
            turn,
        } => {
            let prompt = read_prompt(&prompt)?;
            let turn = build_turn_request(&turn, prompt)?;
            let result = execute_turn(
                &client,
                session,
                SessionGenerationId::from_uuid(generation),
                turn,
                mode,
            )
            .await?;
            require_completed_turn(&result)?;
            emit_turn_result(mode, &result)
        }
        Command::Inspect {
            session,
            generation,
        } => {
            let snapshot = client
                .inspect_session(session, SessionGenerationId::from_uuid(generation))
                .await?;
            let text = serde_json::to_string_pretty(&snapshot)?;
            emit(mode, "session_snapshot", &snapshot, &text)
        }
        Command::Cancel {
            session,
            generation,
            turn,
        } => {
            let result = client
                .cancel_turn(session, SessionGenerationId::from_uuid(generation), turn)
                .await?;
            require_successful_cancel(&result)?;
            emit(
                mode,
                "turn_cancelled",
                &result,
                &format!(
                    "turn={} outcome={:?} session_state={:?}",
                    result.turn_id, result.outcome, result.session_state
                ),
            )
        }
        Command::Close {
            session,
            generation,
            policy,
        } => {
            let result = close_session_with_proof(
                &client,
                session,
                SessionGenerationId::from_uuid(generation),
                policy.into(),
            )
            .await?;
            emit(
                mode,
                "session_closed",
                &result,
                &format!(
                    "session={} already_closed={} process_reaped={}",
                    result.session_id, result.already_closed, result.process_reaped
                ),
            )
        }
        Command::Clear {
            session,
            generation,
            expect_transcript,
            deadline_unix_ms,
        } => {
            let result = client
                .clear_session(
                    session,
                    SessionGenerationId::from_uuid(generation),
                    expect_transcript,
                    deadline_unix_ms,
                )
                .await?;
            emit(
                mode,
                "session_cleared",
                &result,
                &format!(
                    "session={} transcript={} rotated={} state={:?}",
                    result.session_id, result.transcript_session_id, result.rotated, result.state
                ),
            )
        }
        Command::Attach {
            session,
            generation,
            read_only,
            rows,
            cols,
        } => {
            let capability = client
                .attach_session(AttachSessionRequest {
                    session_id: session,
                    generation_id: SessionGenerationId::from_uuid(generation),
                    read_only,
                    size: rows
                        .zip(cols)
                        .map(|(rows, cols)| TerminalSize { rows, cols }),
                })
                .await?;
            if mode == OutputMode::Text {
                let endpoint = std::path::PathBuf::from(capability.endpoint);
                let token = capability.token;
                tokio::task::spawn_blocking(move || {
                    pseudomux_rmux::attach_capability_terminal(&endpoint, &token)
                })
                .await
                .context("attach terminal worker failed")??;
                return Ok(());
            }
            let text = format!(
                "endpoint={}\ntoken={}\nexpires_at_ms={}\nread_only={}",
                capability.endpoint,
                capability.token,
                capability.expires_at_ms,
                capability.read_only
            );
            emit(mode, "attach_capability", &capability, &text)
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
        Command::Agent { command } => match command {
            AgentCommand::Create {
                spec_file,
                from_profile,
                profile_file,
                name,
                claude,
            } => {
                let spec = build_agent_create_spec(
                    spec_file.as_deref(),
                    from_profile.as_deref(),
                    profile_file.as_deref(),
                    name.as_deref(),
                    claude.as_deref(),
                )?;
                let descriptor = client.create_agent(spec).await?;
                emit(
                    mode,
                    "agent_created",
                    &descriptor,
                    &render_agent_descriptor(&descriptor)?,
                )
            }
            AgentCommand::List => {
                let list = client.list_agents().await?;
                let mut lines: Vec<String> = list
                    .agents
                    .iter()
                    .map(|summary| {
                        format!(
                            "{} v{} {} cell={} {}",
                            summary.agent_id,
                            summary.version,
                            &summary.config_digest[..12.min(summary.config_digest.len())],
                            summary.cell,
                            summary.name
                        )
                    })
                    .collect();
                if lines.is_empty() && list.unreadable.is_empty() {
                    lines.push("no stored agents".to_owned());
                }
                // PRINTED, NEVER DROPPED. The daemon reports the records it
                // could not read instead of answering the whole listing with
                // the first one's refusal; a client that then omitted them
                // would show a stored agent simply ceasing to exist.
                for failure in &list.unreadable {
                    lines.push(format!(
                        "{} UNREADABLE {}",
                        failure.agent_id, failure.reason
                    ));
                }
                emit(mode, "agent_list", &list, &lines.join("\n"))
            }
            AgentCommand::Get { agent_id, version } => {
                let version = version.map(agent_version).transpose()?;
                let descriptor = client.get_agent(agent_id, version).await?;
                emit(
                    mode,
                    "agent",
                    &descriptor,
                    &render_agent_descriptor(&descriptor)?,
                )
            }
            AgentCommand::Update {
                agent_id,
                expected_version,
                spec_file,
            } => {
                let spec = read_agent_spec(&spec_file)?;
                let descriptor = client
                    .update_agent(agent_id, agent_version(expected_version)?, spec)
                    .await?;
                emit(
                    mode,
                    "agent_updated",
                    &descriptor,
                    &render_agent_descriptor(&descriptor)?,
                )
            }
        },
        Command::Doctor { claude, cwd } => {
            let report = doctor(&client, &cli.socket, &claude, cwd.as_deref()).await;
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
        Command::Probe {
            launch_args,
            launch,
            keep,
        } => {
            let request = build_start_request(&launch_args)?;
            let sanitized = sanitized_start_request(&request)?;
            // Computed before the launch so the dry run and `--launch` report
            // the same audit surface; see `EnvironmentRemovals`.
            let environment_removed = EnvironmentRemovals::new(dropped_environment_names(&request));
            if !launch {
                let report = ProbeReport {
                    request: sanitized,
                    environment_removed,
                    launched: false,
                    session: None,
                    snapshot: None,
                    close: None,
                };
                let text = serde_json::to_string_pretty(&report)?;
                return emit(mode, "probe", &report, &text);
            }

            let handle = client.start_session(request).await?;
            let snapshot = match client
                .inspect_session(handle.session_id, handle.generation_id)
                .await
            {
                Ok(snapshot) => snapshot,
                Err(inspect_error) => {
                    // A failed probe has not handed a usable session handle to
                    // the caller. Even `--keep` therefore cannot transfer
                    // ownership: force-close the exact generation and preserve
                    // both failures without including the launch request.
                    let cleanup = close_session_with_proof(
                        &client,
                        handle.session_id,
                        handle.generation_id,
                        ClosePolicy::Force,
                    )
                    .await
                    .map(|_| ());
                    return Err(probe_inspection_failure(
                        inspect_error.into(),
                        cleanup,
                        handle.session_id,
                        handle.generation_id,
                    ));
                }
            };
            let close = if keep {
                None
            } else {
                Some(serde_json::to_value(
                    close_session_with_proof(
                        &client,
                        handle.session_id,
                        handle.generation_id,
                        ClosePolicy::Graceful,
                    )
                    .await?,
                )?)
            };
            let report = ProbeReport {
                request: sanitized,
                environment_removed,
                launched: true,
                session: Some(serde_json::to_value(handle)?),
                snapshot: Some(serde_json::to_value(snapshot)?),
                close,
            };
            let text = serde_json::to_string_pretty(&report)?;
            emit(mode, "probe", &report, &text)
        }
    }
}

async fn execute_turn(
    client: &PmuxClient,
    session_id: SessionId,
    generation_id: SessionGenerationId,
    turn: TurnRequest,
    mode: OutputMode,
) -> Result<TurnResult> {
    for replay_attempt in 0..=1 {
        let accepted = client
            .run_turn(session_id, generation_id, turn.clone())
            .await?;
        eprintln!(
            "pmux: turn {} accepted (replayed={})",
            accepted.turn_id, accepted.replayed
        );
        let stream = client.event_stream(
            session_id,
            generation_id,
            accepted.next_sequence.saturating_sub(1),
            EventStreamOptions::default(),
        );
        let wait = wait_for_turn(stream, turn.turn_id, mode);
        tokio::pin!(wait);
        let guard = tokio::time::sleep(turn_infrastructure_guard(turn.deadline_unix_ms));
        tokio::pin!(guard);
        let outcome = tokio::select! {
            biased;
            result = &mut wait => result?,
            signal = tokio::signal::ctrl_c() => {
                signal.context("failed to install Ctrl-C handler")?;
                eprintln!("pmux: interrupt received; cancelling turn {}", turn.turn_id);
                best_effort_cancel(client, session_id, generation_id, turn.turn_id).await;
                bail!("turn interrupted by user");
            }
            () = &mut guard => {
                // The actor owns the immutable turn deadline. This later guard
                // bounds a daemon or event transport that remained unavailable
                // after the full recovery and transcript-drain response margin.
                // Exact cancellation is idempotent and is the only safe final
                // reconciliation attempt; it never resubmits prompt input.
                eprintln!(
                    "pmux: infrastructure guard expired; cancelling turn {}",
                    turn.turn_id
                );
                best_effort_cancel(client, session_id, generation_id, turn.turn_id).await;
                bail!(
                    "turn outcome was not published before the infrastructure guard; inspect session {} turn {}",
                    session_id,
                    turn.turn_id
                );
            }
        };
        match outcome {
            WaitOutcome::Complete(result) => return Ok(*result),
            WaitOutcome::ReplayGap => {
                if replay_attempt == 1 {
                    bail!(
                        "turn result replay was unavailable after retry; inspect session {session_id}"
                    );
                }
                eprintln!(
                    "pmux: replay gap encountered; resubmitting idempotent turn {}",
                    turn.turn_id
                );
            }
        }
    }
    unreachable!("bounded replay loop always returns")
}

fn turn_infrastructure_guard(deadline_unix_ms: Option<u64>) -> std::time::Duration {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        });
    turn_infrastructure_guard_at(deadline_unix_ms, now_ms)
}

fn turn_infrastructure_guard_at(deadline_unix_ms: Option<u64>, now_ms: u64) -> std::time::Duration {
    deadline_unix_ms.map_or(DEFAULT_RUN_ONCE_TIMEOUT, |deadline| {
        std::time::Duration::from_millis(deadline.saturating_sub(now_ms))
            .saturating_add(RUN_ONCE_RESPONSE_MARGIN)
    })
}

enum WaitOutcome {
    Complete(Box<TurnResult>),
    ReplayGap,
}

async fn wait_for_turn(
    mut events: SequencedEventStream,
    turn_id: TurnId,
    mode: OutputMode,
) -> Result<WaitOutcome> {
    while let Some(item) = events.next().await {
        let item = match item {
            Ok(item) => item,
            Err(error @ (ClientError::Io(_) | ClientError::Timeout { .. })) => {
                eprintln!(
                    "pmux: event connection interrupted after sequence {}; retrying: {error}",
                    events.after_sequence()
                );
                continue;
            }
            Err(error) => return Err(error.into()),
        };

        match item {
            EventStreamItem::ReplayGap(gap) => {
                if mode == OutputMode::Ndjson {
                    output::ndjson("replay_gap", &gap)?;
                }
                eprintln!(
                    "pmux: event replay gap after {}; recovery snapshot sequence {}",
                    gap.requested_after, gap.snapshot.last_sequence
                );
                return Ok(WaitOutcome::ReplayGap);
            }
            EventStreamItem::Event(event) => {
                let event = *event;
                if mode == OutputMode::Ndjson {
                    output::ndjson("event", &event)?;
                }
                let event_turn_id = event.turn_id;
                if event_turn_id.is_some_and(|id| id != turn_id) {
                    continue;
                }
                match event.event {
                    EventPayload::TurnCompleted(result) if result.turn_id == turn_id => {
                        return Ok(WaitOutcome::Complete(result));
                    }
                    EventPayload::TurnFailed(error) if event_turn_id == Some(turn_id) => {
                        bail!(
                            "turn failed code={:?} message={:?} retryable={}",
                            error.code,
                            error.message,
                            error.retryable
                        );
                    }
                    EventPayload::TurnCancelled(cancelled) if event_turn_id == Some(turn_id) => {
                        bail!("turn cancelled: {:?}", cancelled.outcome);
                    }
                    EventPayload::NeedsInput(input) => {
                        eprintln!(
                            "pmux: Claude needs input ({:?}): {}",
                            input.kind, input.message
                        );
                    }
                    _ => {}
                }
            }
        }
    }
    bail!("event stream ended before turn {turn_id} completed")
}

fn emit_turn_result(mode: OutputMode, result: &TurnResult) -> Result<()> {
    match mode {
        OutputMode::Text => output::text(&result.text),
        OutputMode::Json => output::json(result),
        OutputMode::Ndjson => output::ndjson("result", result),
    }
}

fn finish_run(mode: OutputMode, turn: Result<TurnResult>, cleanup: Result<()>) -> Result<()> {
    match (turn, cleanup) {
        (Ok(result), Ok(())) => {
            // The final record is the one-shot commit marker. Validate the
            // semantic outcome before writing any text/JSON/NDJSON success so
            // a failed or cancelled TurnResult cannot be mistaken for a
            // committed run by a downstream parser.
            require_completed_turn(&result)?;
            emit_turn_result(mode, &result)
        }
        (Ok(result), Err(cleanup_error)) => match require_completed_turn(&result) {
            Ok(()) => Err(cleanup_error),
            Err(turn_error) => combine_turn_and_cleanup(Err(turn_error), Err(cleanup_error)),
        },
        (Err(turn_error), Ok(())) => Err(turn_error),
        (Err(turn_error), Err(cleanup_error)) => {
            combine_turn_and_cleanup(Err(turn_error), Err(cleanup_error))
        }
    }
}

fn require_completed_turn(result: &TurnResult) -> Result<()> {
    if result.outcome == TurnOutcome::Completed {
        Ok(())
    } else {
        bail!(
            "turn {} ended with outcome {:?}",
            result.turn_id,
            result.outcome
        )
    }
}

fn require_successful_cancel(result: &CancelTurnResult) -> Result<()> {
    if result.outcome != CancelOutcome::RecoveryFailed {
        Ok(())
    } else {
        bail!(
            "turn {} cancellation did not recover the session; inspect session {} and close it if tainted",
            result.turn_id,
            result.session_id
        )
    }
}

async fn best_effort_cancel(
    client: &PmuxClient,
    session_id: SessionId,
    generation_id: SessionGenerationId,
    turn_id: TurnId,
) {
    if let Err(error) = client.cancel_turn(session_id, generation_id, turn_id).await {
        eprintln!("pmux: cancellation request failed: {error}");
    }
}

async fn close_session_with_proof(
    client: &PmuxClient,
    session_id: SessionId,
    generation_id: SessionGenerationId,
    policy: ClosePolicy,
) -> Result<CloseSessionResult> {
    let result = client
        .close_session(session_id, generation_id, policy)
        .await?;
    require_process_reaped(result)
}

fn require_process_reaped(result: CloseSessionResult) -> Result<CloseSessionResult> {
    if result.process_reaped {
        Ok(result)
    } else {
        bail!(
            "session {} closed without confirming that its process was reaped",
            result.session_id
        )
    }
}

fn probe_inspection_failure(
    inspect_error: anyhow::Error,
    cleanup: Result<()>,
    session_id: SessionId,
    generation_id: SessionGenerationId,
) -> anyhow::Error {
    match cleanup {
        Ok(()) => inspect_error,
        Err(cleanup_error) => anyhow::anyhow!(
            "{}",
            serde_json::json!({
                "code": "recovery_failed",
                "message": "probe inspection failed and session cleanup could not be confirmed",
                "session_id": session_id,
                "generation_id": generation_id,
                "inspection_error": format!("{inspect_error:#}"),
                "cleanup_error": format!("{cleanup_error:#}"),
            })
        ),
    }
}

fn combine_turn_and_cleanup(turn: Result<()>, cleanup: Result<()>) -> Result<()> {
    match (turn, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(turn_error), Err(cleanup_error)) => Err(anyhow::anyhow!(
            "{}",
            serde_json::json!({
                "code": "recovery_failed",
                "message": "turn processing failed and session cleanup could not be confirmed",
                "turn_error": format!("{turn_error:#}"),
                "cleanup_error": format!("{cleanup_error:#}"),
            })
        )),
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
/// `doctor` is a VIEW of the daemon's health tree plus the four local checks
/// only a client can make -- socket mode, socket type, Claude executable,
/// working directory. It is deliberately not a second health story: every claim
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
    cwd: Option<String>,
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

async fn doctor(
    client: &PmuxClient,
    socket: &Path,
    claude: &Path,
    cwd: Option<&Path>,
) -> DoctorReport {
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
    let cwd = match resolve_cwd(cwd) {
        Ok(path) => Some(path.display().to_string()),
        Err(error) => {
            errors.push(format!("working directory: {error:#}"));
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
        cwd,
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

#[derive(Serialize)]
struct ProbeReport {
    request: Value,
    environment_removed: EnvironmentRemovals,
    launched: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snapshot: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    close: Option<Value>,
}

/// The probe's audit surface for the launch environment.
///
/// `spec.md:459-463` makes the removal set the thing `pmux probe` renders, and
/// makes it render **names only** — the sanitized request already reduces both
/// `snapshot` and `set` to counts, and this must not reintroduce a value.
///
/// Two honest limitations are recorded in the payload rather than in a comment
/// no caller reads. First, a dry-run probe reaches no daemon (`spec.md:1214`),
/// so the set is evaluated locally. Second, protocol v1 has no field carrying
/// `ResolvedClaudeLaunch::removed_environment_keys`, so `--launch` reports the
/// same locally evaluated set rather than the daemon's own.
#[derive(Serialize)]
struct EnvironmentRemovals {
    count: usize,
    names: Vec<String>,
    source: &'static str,
    note: &'static str,
}

impl EnvironmentRemovals {
    fn new(names: Vec<String>) -> Self {
        Self {
            count: names.len(),
            names,
            source: "client_policy_mirror",
            note: "Names the launch allowlist, the auth policy, or the terminal profile keeps \
                   from the child. Values are never reported. Protocol v1 publishes no removal \
                   set, so --launch shows this same locally computed list. Restore a name with \
                   --env KEY=VALUE, or --env-passthrough KEY to keep its value off the command \
                   line.",
        }
    }
}

fn sanitized_start_request(request: &StartSessionRequest) -> Result<Value> {
    let mut value = serde_json::to_value(request)?;
    if let Some(environment) = value
        .as_object_mut()
        .and_then(|object| object.get_mut("environment"))
        .and_then(Value::as_object_mut)
    {
        let snapshot_count = environment
            .get("snapshot")
            .and_then(Value::as_object)
            .map_or(0, serde_json::Map::len);
        let set_count = environment
            .get("set")
            .and_then(Value::as_object)
            .map_or(0, serde_json::Map::len);
        environment.insert(
            "snapshot".into(),
            serde_json::json!({"redacted": true, "variable_count": snapshot_count}),
        );
        environment.insert(
            "set".into(),
            serde_json::json!({"redacted": true, "variable_count": set_count}),
        );
    }
    if let Some(claude) = value
        .as_object_mut()
        .and_then(|object| object.get_mut("claude"))
        .and_then(Value::as_object_mut)
    {
        for key in ["settings", "mcp_configs"] {
            if let Some(sources) = claude.get_mut(key).and_then(Value::as_array_mut) {
                for source in sources {
                    if source.get("source").and_then(Value::as_str) == Some("inline") {
                        source["document"] = serde_json::json!({"redacted": true});
                    }
                }
            }
        }
        if let Some(system_prompt) = claude
            .get_mut("system_prompt")
            .and_then(Value::as_object_mut)
            && let Some(prompt) = system_prompt.remove("prompt")
        {
            let character_count = prompt.as_str().map_or(0, |value| value.chars().count());
            system_prompt.insert("prompt_redacted".into(), Value::Bool(true));
            system_prompt.insert("character_count".into(), character_count.into());
        }
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn_result(outcome: TurnOutcome) -> TurnResult {
        serde_json::from_value(serde_json::json!({
            "session_id": "00000000-0000-4000-8000-000000000001",
            "generation_id": "00000000-0000-4000-8000-000000000002",
            "turn_id": "00000000-0000-4000-8000-000000000003",
            "outcome": match outcome {
                TurnOutcome::Completed => "completed",
                TurnOutcome::Cancelled => "cancelled",
                TurnOutcome::Failed => "failed",
            },
            "text": "done",
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
            "timings": {"submitted_at_ms": 1, "completed_at_ms": 2},
            "claude_version": "test",
            "compatibility": {
                "claude_version": "test",
                "os": "test",
                "arch": "test",
                "terminal_profile": "transparent",
                "input_transport": "sdk",
                "tested": true,
                "transcript_drain_ms": 1
            },
            "completion": {
                "authority": "transcript",
                "prompt_acknowledged": true,
                "terminal_message_observed": true,
                "terminal_prompt_observed": true,
                "terminal_quiet_observed": true,
                "transcript_drained": true,
                "lifecycle_hook_observed": false
            },
            "final_sequence": 1
        }))
        .unwrap()
    }

    #[test]
    fn probe_never_serializes_environment_values() {
        let mut request = serde_json::from_value::<StartSessionRequest>(serde_json::json!({
            "identity": {"mode": "new"},
            "cwd": "/tmp",
            "claude": {
                "executable": "/bin/sh",
                "settings": [{"source": "inline", "document": {"token": "settings-secret"}}],
                "mcp_configs": [{"source": "inline", "document": {"token": "mcp-secret"}}],
                "system_prompt": {"mode": "replace", "prompt": "prompt-secret"}
            },
            "environment": {"snapshot": {"SECRET": "do-not-print"}}
        }))
        .unwrap();
        request
            .environment
            .snapshot
            .insert("TOKEN".into(), "also-secret".into());
        let sanitized = sanitized_start_request(&request).unwrap();
        let encoded = serde_json::to_string(&sanitized).unwrap();
        assert!(!encoded.contains("do-not-print"));
        assert!(!encoded.contains("also-secret"));
        assert!(!encoded.contains("settings-secret"));
        assert!(!encoded.contains("mcp-secret"));
        assert!(!encoded.contains("prompt-secret"));
        assert_eq!(sanitized["environment"]["snapshot"]["variable_count"], 2);
    }

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
    fn failed_and_cancelled_turn_results_are_cli_failures() {
        assert!(require_completed_turn(&turn_result(TurnOutcome::Completed)).is_ok());
        assert!(require_completed_turn(&turn_result(TurnOutcome::Failed)).is_err());
        assert!(require_completed_turn(&turn_result(TurnOutcome::Cancelled)).is_err());
    }

    #[test]
    fn turn_and_cleanup_failures_are_both_preserved() {
        let error = combine_turn_and_cleanup(
            Err(anyhow::anyhow!("turn failed")),
            Err(anyhow::anyhow!("cleanup failed")),
        )
        .unwrap_err();
        let details: serde_json::Value = serde_json::from_str(&error.to_string()).unwrap();
        assert_eq!(details["code"], "recovery_failed");
        assert_eq!(details["turn_error"], "turn failed");
        assert_eq!(details["cleanup_error"], "cleanup failed");
    }

    #[test]
    fn infrastructure_guard_is_after_the_actor_response_margin() {
        assert_eq!(
            turn_infrastructure_guard_at(Some(11_000), 10_000),
            std::time::Duration::from_secs(121)
        );
        assert_eq!(
            turn_infrastructure_guard_at(Some(9_000), 10_000),
            RUN_ONCE_RESPONSE_MARGIN
        );
        assert_eq!(
            turn_infrastructure_guard_at(None, 10_000),
            DEFAULT_RUN_ONCE_TIMEOUT
        );
    }

    #[test]
    fn run_withholds_success_when_cleanup_is_unconfirmed() {
        let error = finish_run(
            OutputMode::Json,
            Ok(turn_result(TurnOutcome::Completed)),
            Err(anyhow::anyhow!("cleanup failed")),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "cleanup failed");

        let combined = finish_run(
            OutputMode::Json,
            Ok(turn_result(TurnOutcome::Failed)),
            Err(anyhow::anyhow!("cleanup failed")),
        )
        .unwrap_err();
        let details: serde_json::Value = serde_json::from_str(&combined.to_string()).unwrap();
        assert_eq!(details["code"], "recovery_failed");
        assert!(
            details["turn_error"]
                .as_str()
                .unwrap()
                .contains("outcome Failed")
        );
    }
}
