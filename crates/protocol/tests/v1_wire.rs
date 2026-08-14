use std::collections::{BTreeMap, BTreeSet};

use pseudomux_protocol::v1::*;
use serde_json::{Value, json};
use uuid::Uuid;

const REQUEST_ID: Uuid = Uuid::from_u128(1);
const SESSION_ID: Uuid = Uuid::from_u128(2);
const TURN_ID: Uuid = Uuid::from_u128(3);
const ROTATED_SESSION_ID: Uuid = Uuid::from_u128(5);
const GENERATION_ID: SessionGenerationId = SessionGenerationId::from_u128(4);

fn launch_config() -> ClaudeLaunchConfig {
    ClaudeLaunchConfig {
        executable: "/opt/claude/bin/claude".into(),
        model: Some("claude-sonnet-4-5".into()),
        effort: Some(EffortLevel::High),
        permission_mode: Some(PermissionMode::Plan),
        allowed_tools: vec!["Read".into()],
        denied_tools: vec!["Bash".into()],
        settings: vec![ConfigSource::Inline {
            document: json!({"hooks": {}}),
        }],
        mcp_configs: vec![ConfigSource::File {
            path: "/work/mcp.json".into(),
        }],
        plugin_dirs: vec!["/work/plugins".into()],
        system_prompt: SystemPromptPolicy::Append {
            prompt: "Be precise.".into(),
        },
        extra_args: vec!["--verbose".into()],
    }
}

fn start_request() -> StartSessionRequest {
    StartSessionRequest {
        identity: SessionIdentity::New {
            session_id: Some(SESSION_ID),
        },
        cwd: "/work/project".into(),
        claude: Some(launch_config()),
        agent: None,
        environment: EnvironmentSpec {
            snapshot: BTreeMap::from([("PATH".into(), "/usr/bin".into())]),
            set: BTreeMap::from([("TERM".into(), "xterm-256color".into())]),
            unset: BTreeSet::from(["ANTHROPIC_API_KEY".into()]),
        },
        auth_policy: AuthPolicy::Subscription,
        config_isolation: None,
        terminal: TerminalSpec {
            rows: 40,
            cols: 132,
            profile: TerminalProfile::Transparent,
            input_transport: InputTransport::Auto,
        },
        lifecycle: LifecycleMode::Hybrid {
            hook_timeout_ms: 5_000,
        },
        retention: RetentionPolicy::Persistent {
            idle_ttl_ms: 1_800_000,
        },
        compatibility: CompatibilityPolicy::RequireTested,
        cell: SessionCell::Minified,
    }
}

fn empty_usage() -> UsageBreakdown {
    UsageBreakdown {
        main: TokenUsage {
            input_tokens: 10,
            output_tokens: 4,
            cache_creation_input_tokens: 2,
            cache_read_input_tokens: 8,
        },
        sidechain: TokenUsage::default(),
        combined: TokenUsage {
            input_tokens: 10,
            output_tokens: 4,
            cache_creation_input_tokens: 2,
            cache_read_input_tokens: 8,
        },
        cost_usd: None,
    }
}

fn compatibility_report() -> CompatibilityReport {
    CompatibilityReport {
        claude_version: "2.1.207".into(),
        os: "macos".into(),
        arch: "aarch64".into(),
        terminal_profile: TerminalProfile::Transparent,
        input_transport: InputTransport::Sdk,
        tested: true,
        transcript_drain_ms: 750,
    }
}

fn turn_result() -> TurnResult {
    TurnResult {
        session_id: SESSION_ID,
        generation_id: GENERATION_ID,
        turn_id: TURN_ID,
        outcome: TurnOutcome::Completed,
        text: "done".into(),
        final_blocks: vec![MessageBlock::Text {
            text: "done".into(),
        }],
        tools: vec![],
        model: Some("claude-sonnet-4-5".into()),
        stop_reason: Some(StopReason {
            kind: StopReasonKind::EndTurn,
            raw: None,
        }),
        usage: empty_usage(),
        // The tool-less shape this fixture encodes: no subagent ran, so no
        // sidechain row exists and the field is absent from the wire bytes.
        sidechain_rows: 0,
        timings: TurnTimings {
            submitted_at_ms: 100,
            prompt_acknowledged_at_ms: Some(110),
            terminal_candidate_at_ms: Some(190),
            completed_at_ms: 200,
            drain_ms: Some(10),
            // Internally coherent zero-gap case: completed_at_ms - drain_ms == 190,
            // which is also terminal_candidate_at_ms -- nothing was appended after
            // the terminal message, so the whole drain window was pure margin.
            last_transcript_activity_at_ms: Some(190),
            // Stop arrived after the last transcript write: the positive
            // difference 195 - 190 is the shape that would make a hook-based
            // fast path sound.
            stop_hook_at_ms: Some(195),
            // The real-world shape this fixture encodes: the read that carried
            // `turn_duration` was the last read to carry anything, so nothing
            // analysis-changing followed it and the second field is absent.
            // Present-with-absent is the observation that would justify a
            // `turn_duration` fast path, so it is the default the fixture holds.
            turn_duration_observed_at_ms: Some(190),
            post_turn_duration_row_observed_at_ms: None,
        },
        warnings: vec![],
        claude_version: "2.1.207".into(),
        compatibility: compatibility_report(),
        completion: CompletionProvenance {
            authority: CompletionAuthority::Transcript,
            prompt_acknowledged: true,
            terminal_message_observed: true,
            terminal_prompt_observed: true,
            terminal_quiet_observed: true,
            transcript_drained: true,
            lifecycle_hook_observed: false,
        },
        final_sequence: 9,
    }
}

fn session_snapshot() -> SessionSnapshot {
    SessionSnapshot {
        session_id: SESSION_ID,
        generation_id: GENERATION_ID,
        transcript_session_id: ROTATED_SESSION_ID,
        cell: SessionCell::Minified,
        state: SessionState::Ready,
        cwd: "/work/project".into(),
        active_turn_id: None,
        claude_version: Some("2.1.207".into()),
        compatibility: compatibility_report(),
        created_at_ms: 10,
        updated_at_ms: 20,
        idle_deadline_ms: Some(30),
        resumable: true,
        last_sequence: 50,
        last_turn: Some(TurnSummary {
            turn_id: TURN_ID,
            outcome: TurnOutcome::Completed,
            completed_at_ms: 19,
            final_sequence: 49,
        }),
        needs_input: None,
        agent: None,
    }
}

fn assert_json_round_trip<T>(value: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let encoded = serde_json::to_vec(value).expect("serialize");
    let decoded = serde_json::from_slice::<T>(&encoded).expect("deserialize");
    assert_eq!(&decoded, value);
}

#[test]
fn start_session_envelope_has_stable_v1_shape() {
    let envelope = RequestEnvelope::new(REQUEST_ID, Request::StartSession(start_request()));
    let actual = serde_json::to_value(&envelope).unwrap();

    assert_eq!(actual["version"], 1);
    assert_eq!(actual["request_id"], REQUEST_ID.to_string());
    assert_eq!(actual["method"], "start_session");
    assert_eq!(actual["params"]["identity"]["mode"], "new");
    assert_eq!(
        actual["params"]["identity"]["session_id"],
        SESSION_ID.to_string()
    );
    assert_eq!(actual["params"]["auth_policy"], "subscription");
    assert_eq!(actual["params"]["terminal"]["profile"], "transparent");
    assert_eq!(actual["params"]["lifecycle"]["mode"], "hybrid");
    assert_eq!(
        actual["params"]["environment"]["unset"],
        json!(["ANTHROPIC_API_KEY"])
    );
    assert_json_round_trip(&envelope);
}

#[test]
fn ping_envelope_is_an_exact_golden_frame() {
    let envelope = RequestEnvelope::new(REQUEST_ID, Request::Ping);
    assert_eq!(
        serde_json::to_string(&envelope).unwrap(),
        r#"{"version":1,"request_id":"00000000-0000-0000-0000-000000000001","method":"ping"}"#
    );
    assert!(is_supported_version(1));
    assert!(!is_supported_version(0));
    assert!(!is_supported_version(2));
}

#[test]
fn native_frame_header_admission_has_an_exact_inclusive_8_mib_boundary() {
    for payload_bytes in [0, 1, MAX_NATIVE_FRAME_BYTES - 1, MAX_NATIVE_FRAME_BYTES] {
        assert_eq!(
            admit_native_frame_header((payload_bytes as u32).to_be_bytes()),
            NativeFrameAdmission::Payload { payload_bytes }
        );
    }

    let advertised_bytes = u32::try_from(MAX_NATIVE_FRAME_BYTES + 1).unwrap();
    assert_eq!(
        admit_native_frame_header(advertised_bytes.to_be_bytes()),
        NativeFrameAdmission::Oversized { advertised_bytes }
    );
    assert_eq!(
        admit_native_frame_header(u32::MAX.to_be_bytes()),
        NativeFrameAdmission::Oversized {
            advertised_bytes: u32::MAX,
        }
    );
}

#[test]
fn native_frame_accumulator_is_fragmentation_invariant_and_preserves_next_frame() {
    let payloads = [b"".as_slice(), b"a", b"fragmented payload"];
    for payload in payloads {
        let mut frame = Vec::with_capacity(payload.len() + 4);
        frame.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());
        frame.extend_from_slice(payload);

        for first_fragment in 0..=frame.len() {
            let mut accumulator = NativeFrameAccumulator::new();
            let (first_consumed, first_progress) = accumulator.push(&frame[..first_fragment]);
            assert_eq!(first_consumed, first_fragment);
            if first_fragment == frame.len() {
                assert_eq!(
                    first_progress,
                    NativeFrameProgress::Payload(payload.to_vec())
                );
                assert!(accumulator.is_empty());
                continue;
            }
            assert_eq!(first_progress, NativeFrameProgress::NeedMore);
            assert_eq!(accumulator.is_empty(), first_fragment == 0);
            let (second_consumed, second_progress) = accumulator.push(&frame[first_fragment..]);
            assert_eq!(second_consumed, frame.len() - first_fragment);
            assert_eq!(
                second_progress,
                NativeFrameProgress::Payload(payload.to_vec())
            );
            assert!(accumulator.is_empty());
        }
    }

    let first_payload = b"first";
    let second_payload = b"second";
    let mut stream = Vec::new();
    stream.extend_from_slice(&(first_payload.len() as u32).to_be_bytes());
    stream.extend_from_slice(first_payload);
    stream.extend_from_slice(&(second_payload.len() as u32).to_be_bytes());
    stream.extend_from_slice(second_payload);
    let first_frame_len = first_payload.len() + 4;
    let mut accumulator = NativeFrameAccumulator::new();
    let (consumed, progress) = accumulator.push(&stream);
    assert_eq!(consumed, first_frame_len);
    assert_eq!(
        progress,
        NativeFrameProgress::Payload(first_payload.to_vec())
    );
    let (consumed, progress) = accumulator.push(&stream[first_frame_len..]);
    assert_eq!(consumed, second_payload.len() + 4);
    assert_eq!(
        progress,
        NativeFrameProgress::Payload(second_payload.to_vec())
    );
}

#[test]
fn native_frame_accumulator_rejects_oversized_header_before_trailing_bytes() {
    let advertised_bytes = u32::try_from(MAX_NATIVE_FRAME_BYTES + 1).unwrap();
    let mut input = advertised_bytes.to_be_bytes().to_vec();
    input.extend_from_slice(b"unread-body-and-next-frame");
    let mut accumulator = NativeFrameAccumulator::new();
    let (consumed, progress) = accumulator.push(&input);
    assert_eq!(consumed, 4);
    assert_eq!(
        progress,
        NativeFrameProgress::Oversized { advertised_bytes }
    );
    assert!(accumulator.is_empty());
    assert_eq!(accumulator.remaining_bytes(), 4);
}

#[test]
fn minimal_start_request_applies_safe_defaults() {
    let raw = json!({
        "version": 1,
        "request_id": REQUEST_ID,
        "method": "start_session",
        "params": {
            "identity": {"mode": "new"},
            "cwd": "/work/project",
            "claude": {"executable": "/usr/local/bin/claude"}
        }
    });

    let decoded: RequestEnvelope = serde_json::from_value(raw).unwrap();
    let Request::StartSession(start) = decoded.request else {
        panic!("wrong method")
    };
    assert_eq!(start.auth_policy, AuthPolicy::Subscription);
    assert_eq!(start.lifecycle, LifecycleMode::Transcript);
    assert_eq!(start.compatibility, CompatibilityPolicy::RequireTested);
    assert_eq!(start.terminal, TerminalSpec::default());
    assert_eq!(start.environment, EnvironmentSpec::default());
    assert_eq!(
        start.retention,
        RetentionPolicy::Persistent {
            idle_ttl_ms: 1_800_000
        }
    );
    assert_eq!(
        start.claude.as_ref().expect("inline launch").system_prompt,
        SystemPromptPolicy::Default
    );
    // Every other omitted field was already asserted here and this one was not,
    // which is how the cell reached the wire as an always-serialized field: the
    // test that pins "an omitted field means exactly this" never mentioned it.
    assert_eq!(start.cell, SessionCell::Full);
}

/// A new client must not brick itself against a daemon that predates the cell.
///
/// `StartSessionRequest` is `deny_unknown_fields`, so a `"cell"` key is a hard
/// rejection on any daemon built before this field existed -- at `version: 1`,
/// with no version bump to warn anyone. The default asks such a daemon for
/// precisely what it already does, so the compatible encoding of "I did not
/// choose a cell" is to say nothing. A non-default cell is a different request
/// and must always be on the wire: refusing it loudly on an old daemon is
/// correct, and silently downgrading it to `full` would run a caller's turns
/// under a proof it did not ask for.
#[test]
fn a_default_cell_is_omitted_from_the_wire_and_a_chosen_one_never_is() {
    let mut start = start_request();
    start.cell = SessionCell::Full;
    let envelope = RequestEnvelope::new(REQUEST_ID, Request::StartSession(start.clone()));
    let encoded = serde_json::to_value(&envelope).unwrap();
    assert_eq!(
        encoded["params"].get("cell"),
        None,
        "a default cell must not appear on the wire: {encoded}"
    );
    assert_json_round_trip(&envelope);

    start.cell = SessionCell::Minified;
    let envelope = RequestEnvelope::new(REQUEST_ID, Request::StartSession(start));
    let encoded = serde_json::to_value(&envelope).unwrap();
    assert_eq!(encoded["params"]["cell"], "minified");
    assert_json_round_trip(&envelope);

    // The same rule inside `run_once`, whose nested start request is the one a
    // caller reaches through a different method and would otherwise miss.
    let mut session = start_request();
    session.cell = SessionCell::Full;
    let once = RunOnceRequest {
        session,
        turn: TurnRequest {
            turn_id: TURN_ID,
            prompt: "Inspect the repository".into(),
            deadline_unix_ms: None,
            lease: TurnLeasePolicy::default(),
        },
    };
    let encoded =
        serde_json::to_value(RequestEnvelope::new(REQUEST_ID, Request::RunOnce(once))).unwrap();
    assert_eq!(
        encoded["params"]["session"].get("cell"),
        None,
        "a default cell must not appear on the wire under run_once: {encoded}"
    );
}

#[test]
fn absent_config_isolation_is_omitted_and_a_named_root_round_trips_strictly() {
    // Absence is the whole encoding of "inherit the caller's root", which is
    // why this is an `Option` rather than a defaulted enum. A daemon built
    // before the field existed rejects unknown fields, so serializing an
    // absent isolation would have broken every un-isolated caller against
    // every pre-existing daemon at `version: 1`.
    let mut start = start_request();
    start.config_isolation = None;
    let envelope = RequestEnvelope::new(REQUEST_ID, Request::StartSession(start.clone()));
    let encoded = serde_json::to_value(&envelope).unwrap();
    assert_eq!(
        encoded["params"].get("config_isolation"),
        None,
        "an absent config isolation must not appear on the wire: {encoded}"
    );
    assert_json_round_trip(&envelope);

    start.config_isolation = Some(ConfigIsolation {
        root: "/var/pmux/config-roots/cell-0".into(),
    });
    let envelope = RequestEnvelope::new(REQUEST_ID, Request::StartSession(start.clone()));
    let encoded = serde_json::to_value(&envelope).unwrap();
    assert_eq!(
        encoded["params"]["config_isolation"],
        serde_json::json!({"root": "/var/pmux/config-roots/cell-0"})
    );
    assert_json_round_trip(&envelope);

    // A typo inside the isolation object is a hard rejection rather than an
    // ignored key, so `--config-isolation-roots` cannot silently launch an
    // un-isolated session.
    let mut typo = encoded.clone();
    typo["params"]["config_isolation"] = serde_json::json!({"roots": "/var/pmux"});
    assert!(serde_json::from_value::<RequestEnvelope>(typo).is_err());
    let mut additive = encoded;
    additive["params"]["config_isolation"]["future_field"] = serde_json::json!(true);
    assert!(serde_json::from_value::<RequestEnvelope>(additive).is_err());

    // The same rule inside `run_once`, whose nested start request is the one a
    // caller reaches through a different method and would otherwise miss.
    let once = RunOnceRequest {
        session: start,
        turn: TurnRequest {
            turn_id: TURN_ID,
            prompt: "Inspect the repository".into(),
            deadline_unix_ms: None,
            lease: TurnLeasePolicy::default(),
        },
    };
    let encoded =
        serde_json::to_value(RequestEnvelope::new(REQUEST_ID, Request::RunOnce(once))).unwrap();
    assert_eq!(
        encoded["params"]["session"]["config_isolation"]["root"],
        "/var/pmux/config-roots/cell-0"
    );
}

#[test]
fn resume_identity_is_explicit_and_round_trips() {
    let mut start = start_request();
    start.identity = SessionIdentity::Resume {
        session_id: SESSION_ID,
    };
    let envelope = RequestEnvelope::new(REQUEST_ID, Request::StartSession(start));

    let actual = serde_json::to_value(&envelope).unwrap();
    assert_eq!(actual["params"]["identity"]["mode"], "resume");
    assert_eq!(
        actual["params"]["identity"]["session_id"],
        SESSION_ID.to_string()
    );
    assert_json_round_trip(&envelope);
}

#[test]
fn run_turn_request_carries_idempotency_deadline_and_lease() {
    let envelope = RequestEnvelope::new(
        REQUEST_ID,
        Request::RunTurn(RunTurnRequest {
            session_id: SESSION_ID,
            generation_id: GENERATION_ID,
            turn: TurnRequest {
                turn_id: TURN_ID,
                prompt: "Inspect the repository".into(),
                deadline_unix_ms: Some(1_800_000_000_000),
                lease: TurnLeasePolicy {
                    on_disconnect: DisconnectAction::CancelTurn,
                    heartbeat_timeout_ms: Some(5_000),
                },
            },
        }),
    );

    let actual = serde_json::to_value(&envelope).unwrap();
    assert_eq!(actual["method"], "run_turn");
    assert_eq!(actual["params"]["turn"]["turn_id"], TURN_ID.to_string());
    assert_eq!(
        actual["params"]["turn"]["lease"]["on_disconnect"],
        "cancel_turn"
    );
    assert_json_round_trip(&envelope);
}

#[test]
fn success_response_is_typed_and_correlated() {
    let response = ResponseEnvelope::success(
        REQUEST_ID,
        ResponseResult::TurnAccepted(TurnAccepted {
            session_id: SESSION_ID,
            generation_id: GENERATION_ID,
            turn_id: TURN_ID,
            replayed: false,
            state: SessionState::AwaitingPromptAck,
            next_sequence: 4,
        }),
    );
    let actual = serde_json::to_value(&response).unwrap();

    assert_eq!(actual["version"], PROTOCOL_VERSION);
    assert_eq!(actual["request_id"], REQUEST_ID.to_string());
    assert_eq!(actual["result"]["type"], "turn_accepted");
    assert_eq!(actual["result"]["data"]["next_sequence"], 4);
    assert!(actual.get("error").is_none());
    assert_json_round_trip(&response);
}

#[test]
fn error_response_preserves_code_retryability_and_details() {
    let error = ErrorBody::new(ErrorCode::SessionBusy, "another turn is active")
        .retryable(true)
        .with_details(json!({"active_turn_id": TURN_ID}));
    let response = ResponseEnvelope::failure(REQUEST_ID, error);
    let actual = serde_json::to_value(&response).unwrap();

    assert_eq!(actual["error"]["code"], "session_busy");
    assert_eq!(actual["error"]["retryable"], true);
    assert_eq!(
        actual["error"]["details"]["active_turn_id"],
        TURN_ID.to_string()
    );
    assert!(actual.get("result").is_none());
    assert_json_round_trip(&response);

    assert_eq!(MAX_NATIVE_FRAME_BYTES, 8 * 1024 * 1024);
    assert_eq!(
        serde_json::to_value(ErrorCode::ResultTooLarge).unwrap(),
        "result_too_large"
    );
    assert_eq!(
        serde_json::to_value(ErrorCode::TurnHistoryCapacityExceeded).unwrap(),
        "turn_history_capacity_exceeded"
    );
}

#[test]
fn event_envelope_is_sequenced_and_turn_correlated() {
    let event = EventEnvelope::new(
        SESSION_ID,
        GENERATION_ID,
        Some(TURN_ID),
        7,
        1_000,
        EventPayload::PromptAcknowledged(PromptAcknowledged {
            prompt_uuid: "transcript-row-uuid".into(),
            prompt_id: Some("prompt-1".into()),
            transcript_offset: 4_096,
        }),
    );
    let actual = serde_json::to_value(&event).unwrap();

    assert_eq!(actual["schema_version"], 1);
    assert_eq!(actual["session_id"], SESSION_ID.to_string());
    assert_eq!(actual["turn_id"], TURN_ID.to_string());
    assert_eq!(actual["sequence"], 7);
    assert_eq!(actual["event"]["type"], "prompt_acknowledged");
    assert_eq!(actual["event"]["data"]["transcript_offset"], 4_096);
    assert_json_round_trip(&event);
}

#[test]
fn turn_result_keeps_subscription_cost_absent_and_usage_separate() {
    let result = turn_result();
    let actual = serde_json::to_value(&result).unwrap();

    assert!(actual["usage"].get("cost_usd").is_none());
    assert_eq!(actual["usage"]["main"]["output_tokens"], 4);
    assert_eq!(actual["usage"]["sidechain"]["output_tokens"], 0);
    assert_eq!(actual["completion"]["authority"], "transcript");
    assert_eq!(actual["final_blocks"][0]["kind"], "text");
    assert_json_round_trip(&result);
}

#[test]
fn turn_timings_publish_the_stop_hook_instant_as_an_omissible_optional() {
    let observed = turn_result();
    let encoded = serde_json::to_value(&observed).unwrap();
    assert_eq!(encoded["timings"]["stop_hook_at_ms"], 195);
    assert_eq!(encoded["timings"]["last_transcript_activity_at_ms"], 190);
    assert_json_round_trip(&observed);

    let mut absent = turn_result();
    absent.timings.stop_hook_at_ms = None;
    let encoded = serde_json::to_value(&absent).unwrap();
    assert!(
        encoded["timings"].get("stop_hook_at_ms").is_none(),
        "a turn with no observed Stop hook must omit the key entirely"
    );
    assert_json_round_trip(&absent);

    // An older producer that never emits the key must decode to the absent
    // instant rather than to a plausible-looking zero.
    let mut legacy = serde_json::to_value(&observed).unwrap();
    assert!(
        legacy["timings"]
            .as_object_mut()
            .unwrap()
            .remove("stop_hook_at_ms")
            .is_some()
    );
    let decoded = serde_json::from_value::<TurnResult>(legacy).unwrap();
    assert_eq!(decoded.timings.stop_hook_at_ms, None);
    assert_eq!(decoded, absent);
}

#[test]
fn turn_timings_publish_the_turn_duration_arrival_order_as_omissible_optionals() {
    let quiet_after_marker = turn_result();
    let encoded = serde_json::to_value(&quiet_after_marker).unwrap();
    assert_eq!(encoded["timings"]["turn_duration_observed_at_ms"], 190);
    assert!(
        encoded["timings"]
            .get("post_turn_duration_row_observed_at_ms")
            .is_none(),
        "nothing analysis-changing followed the marker, so the key must be \
         omitted rather than published as a plausible-looking zero"
    );
    assert_json_round_trip(&quiet_after_marker);

    // A turn on a Claude build that writes no `turn_duration` row publishes
    // neither field, and the pair must still round-trip.
    let mut unmarked = turn_result();
    unmarked.timings.turn_duration_observed_at_ms = None;
    let encoded = serde_json::to_value(&unmarked).unwrap();
    assert!(
        encoded["timings"]
            .get("turn_duration_observed_at_ms")
            .is_none()
    );
    assert!(
        encoded["timings"]
            .get("post_turn_duration_row_observed_at_ms")
            .is_none()
    );
    assert_json_round_trip(&unmarked);

    // A late analysis-changing row: both instants present, and the signed
    // difference stays positive and legible.
    let mut late_row = turn_result();
    late_row.timings.post_turn_duration_row_observed_at_ms = Some(196);
    let encoded = serde_json::to_value(&late_row).unwrap();
    assert_eq!(encoded["timings"]["turn_duration_observed_at_ms"], 190);
    assert_eq!(
        encoded["timings"]["post_turn_duration_row_observed_at_ms"],
        196
    );
    assert_json_round_trip(&late_row);

    // An older producer that emits neither key must decode to absent instants
    // rather than to zeroes that would read as "the marker was observed at the
    // epoch and nothing followed".
    let mut legacy = serde_json::to_value(&late_row).unwrap();
    let timings = legacy["timings"].as_object_mut().unwrap();
    assert!(timings.remove("turn_duration_observed_at_ms").is_some());
    assert!(
        timings
            .remove("post_turn_duration_row_observed_at_ms")
            .is_some()
    );
    let decoded = serde_json::from_value::<TurnResult>(legacy).unwrap();
    assert_eq!(decoded.timings.turn_duration_observed_at_ms, None);
    assert_eq!(decoded.timings.post_turn_duration_row_observed_at_ms, None);
    assert_eq!(decoded, unmarked);
}

#[test]
fn a_stop_hook_that_precedes_the_final_transcript_write_survives_as_a_negative_difference() {
    // The whole point of publishing an instant rather than a duration: a Stop
    // that fires before Claude's last write is the observation that would
    // condemn a hook-based completion fast path, and it must reach a consumer
    // with its sign intact.
    let mut truncating = turn_result();
    truncating.timings.stop_hook_at_ms = Some(185);
    let decoded =
        serde_json::from_value::<TurnResult>(serde_json::to_value(&truncating).unwrap()).unwrap();

    let stop_hook_at_ms = i128::from(decoded.timings.stop_hook_at_ms.unwrap());
    let last_write_at_ms = i128::from(decoded.timings.last_transcript_activity_at_ms.unwrap());
    assert_eq!(
        stop_hook_at_ms - last_write_at_ms,
        -5,
        "the signed difference must stay negative rather than clamp to zero"
    );
    assert_json_round_trip(&truncating);
}

#[test]
fn replay_gap_batch_carries_recovery_snapshot() {
    let batch = EventBatch {
        events: vec![],
        next_sequence: 51,
        replay_gap: Some(ReplayGap {
            requested_after: 3,
            oldest_available: 40,
            next_sequence: 51,
            snapshot: Box::new(session_snapshot()),
        }),
    };
    let response = ResponseEnvelope::success(REQUEST_ID, ResponseResult::Events(batch));
    let actual = serde_json::to_value(&response).unwrap();

    assert_eq!(actual["result"]["type"], "events");
    assert_eq!(
        actual["result"]["data"]["replay_gap"]["oldest_available"],
        40
    );
    assert_eq!(
        actual["result"]["data"]["replay_gap"]["snapshot"]["last_sequence"],
        50
    );
    assert_json_round_trip(&response);
}

#[test]
fn replay_gap_batches_reject_events_and_any_cursor_disagreement() {
    let valid = serde_json::to_value(EventBatch {
        events: vec![],
        next_sequence: 51,
        replay_gap: Some(ReplayGap {
            requested_after: 3,
            oldest_available: 40,
            next_sequence: 51,
            snapshot: Box::new(session_snapshot()),
        }),
    })
    .unwrap();

    let mut with_event = valid.clone();
    with_event["events"] = json!([EventEnvelope::new(
        SESSION_ID,
        GENERATION_ID,
        None,
        51,
        1_000,
        EventPayload::Heartbeat(Heartbeat {
            session_state: SessionState::Ready,
        }),
    )]);
    assert!(serde_json::from_value::<EventBatch>(with_event).is_err());

    let mut gap_disagrees = valid.clone();
    gap_disagrees["replay_gap"]["next_sequence"] = json!(52);
    assert!(serde_json::from_value::<EventBatch>(gap_disagrees).is_err());

    let mut batch_disagrees = valid;
    batch_disagrees["next_sequence"] = json!(52);
    assert!(serde_json::from_value::<EventBatch>(batch_disagrees).is_err());
}

#[test]
fn every_response_result_variant_round_trips() {
    let results = vec![
        ResponseResult::Pong(Pong {
            server_version: "0.2.0".into(),
            protocol_version: PROTOCOL_VERSION,
        }),
        ResponseResult::SessionStarted(SessionHandle {
            session_id: SESSION_ID,
            generation_id: GENERATION_ID,
            state: SessionState::Booting,
            compatibility: compatibility_report(),
            created_at_ms: 1,
            last_sequence: 0,
            agent: None,
        }),
        ResponseResult::TurnAccepted(TurnAccepted {
            session_id: SESSION_ID,
            generation_id: GENERATION_ID,
            turn_id: TURN_ID,
            replayed: true,
            state: SessionState::Running,
            next_sequence: 2,
        }),
        ResponseResult::TurnCancelled(CancelTurnResult {
            session_id: SESSION_ID,
            generation_id: GENERATION_ID,
            turn_id: TURN_ID,
            outcome: CancelOutcome::Cancelled,
            session_state: SessionState::Ready,
        }),
        ResponseResult::SessionSnapshot(Box::new(session_snapshot())),
        ResponseResult::AttachCapability(AttachCapability {
            session_id: SESSION_ID,
            generation_id: GENERATION_ID,
            token: "one-use-token".into(),
            endpoint: "/runtime/pmux/attach.sock".into(),
            expires_at_ms: 500,
            read_only: false,
        }),
        ResponseResult::SessionClosed(CloseSessionResult {
            session_id: SESSION_ID,
            generation_id: GENERATION_ID,
            already_closed: false,
            process_reaped: true,
        }),
        ResponseResult::Events(EventBatch {
            events: vec![],
            next_sequence: 1,
            replay_gap: None,
        }),
        ResponseResult::TurnResult(Box::new(turn_result())),
    ];

    for result in results {
        assert_json_round_trip(&ResponseEnvelope::success(REQUEST_ID, result));
    }
}

#[test]
fn every_event_payload_variant_round_trips() {
    let stop_reason = StopReason {
        kind: StopReasonKind::EndTurn,
        raw: None,
    };
    let payloads = vec![
        EventPayload::SessionStateChanged(SessionStateChanged {
            previous: SessionState::Running,
            current: SessionState::Draining,
            reason: Some("terminal message observed".into()),
        }),
        EventPayload::PromptAcknowledged(PromptAcknowledged {
            prompt_uuid: "row-uuid".into(),
            prompt_id: Some("prompt-id".into()),
            transcript_offset: 12,
        }),
        EventPayload::LogicalMessage(LogicalMessage {
            message_id: "message-id".into(),
            request_id: Some("request-id".into()),
            scope: MessageScope::Main,
            blocks: vec![MessageBlock::Text { text: "ok".into() }],
            model: Some("claude-sonnet-4-5".into()),
            stop_reason: Some(stop_reason.clone()),
            usage: Some(TokenUsage::default()),
            terminal: true,
        }),
        EventPayload::ToolStarted(ToolStarted {
            tool_use_id: "tool-1".into(),
            name: "Read".into(),
            input: json!({"file_path": "/work/README.md"}),
        }),
        EventPayload::ToolCompleted(ToolCompleted {
            tool_use_id: "tool-1".into(),
            output: json!({"content": "hello"}),
            is_error: false,
        }),
        EventPayload::RateLimit(RateLimitEvent {
            status: RateLimitStatus::Rejected,
            resets_at_ms: Some(1_000),
            message: Some("try later".into()),
        }),
        EventPayload::NeedsInput(NeedsInput {
            kind: NeedsInputKind::Trust,
            message: "workspace trust is required".into(),
            details: Value::Null,
        }),
        EventPayload::TerminalCandidate(TerminalCandidate {
            message_id: "message-id".into(),
            stop_reason: Some(stop_reason),
        }),
        EventPayload::TurnCompleted(Box::new(turn_result())),
        EventPayload::TurnCancelled(TurnCancelledEvent {
            outcome: CancelOutcome::Cancelled,
            recovered_to_ready: true,
        }),
        EventPayload::TurnFailed(ErrorBody::new(
            ErrorCode::ClaudeExited,
            "Claude exited unexpectedly",
        )),
        EventPayload::Warning(ProtocolWarning {
            code: "unknown_transcript_row".into(),
            message: "preserved an unrecognized metadata row".into(),
            details: json!({"type": "future_metadata"}),
        }),
        EventPayload::ReplayGap(ReplayGap {
            requested_after: 1,
            oldest_available: 10,
            next_sequence: 20,
            snapshot: Box::new(session_snapshot()),
        }),
        EventPayload::Heartbeat(Heartbeat {
            session_state: SessionState::Ready,
        }),
    ];

    for (sequence, payload) in payloads.into_iter().enumerate() {
        assert_json_round_trip(&EventEnvelope::new(
            SESSION_ID,
            GENERATION_ID,
            Some(TURN_ID),
            sequence as u64,
            1_000 + sequence as u64,
            payload,
        ));
    }
}

#[test]
fn every_method_variant_round_trips() {
    let turn = TurnRequest {
        turn_id: TURN_ID,
        prompt: "hello".into(),
        deadline_unix_ms: None,
        lease: TurnLeasePolicy::default(),
    };
    let requests = vec![
        Request::Ping,
        Request::StartSession(start_request()),
        Request::RunTurn(RunTurnRequest {
            session_id: SESSION_ID,
            generation_id: GENERATION_ID,
            turn: turn.clone(),
        }),
        Request::CancelTurn(CancelTurnRequest {
            session_id: SESSION_ID,
            generation_id: GENERATION_ID,
            turn_id: TURN_ID,
        }),
        Request::InspectSession(InspectSessionRequest {
            session_id: SESSION_ID,
            generation_id: GENERATION_ID,
        }),
        Request::AttachSession(AttachSessionRequest {
            session_id: SESSION_ID,
            generation_id: GENERATION_ID,
            read_only: false,
            size: Some(TerminalSize {
                rows: 30,
                cols: 100,
            }),
        }),
        Request::CloseSession(CloseSessionRequest {
            session_id: SESSION_ID,
            generation_id: GENERATION_ID,
            policy: ClosePolicy::Force,
        }),
        Request::SubscribeEvents(SubscribeEventsRequest {
            session_id: SESSION_ID,
            generation_id: GENERATION_ID,
            after_sequence: 12,
            wait_ms: 1_000,
            max_events: 64,
        }),
        Request::RunOnce(RunOnceRequest {
            session: start_request(),
            turn,
        }),
        Request::ClearSession(ClearSessionRequest {
            session_id: SESSION_ID,
            generation_id: GENERATION_ID,
            expected_transcript_session_id: ROTATED_SESSION_ID,
            deadline_unix_ms: Some(2_000_000_000_000),
        }),
    ];

    for request in requests {
        assert_json_round_trip(&RequestEnvelope::new(REQUEST_ID, request));
    }
}

#[test]
fn request_decoding_requires_version_and_rejects_unknown_fields() {
    let missing_version = json!({
        "request_id": REQUEST_ID,
        "method": "ping"
    });
    assert!(serde_json::from_value::<RequestEnvelope>(missing_version).is_err());

    let extra = json!({
        "version": 1,
        "request_id": REQUEST_ID,
        "method": "ping",
        "surprise": true
    });
    assert!(serde_json::from_value::<RequestEnvelope>(extra).is_err());

    let mut nested_extra = serde_json::to_value(RequestEnvelope::new(
        REQUEST_ID,
        Request::StartSession(start_request()),
    ))
    .unwrap();
    nested_extra["params"]["claude"]
        .as_object_mut()
        .unwrap()
        .insert("future_launch_policy".into(), json!(true));
    assert!(serde_json::from_value::<RequestEnvelope>(nested_extra).is_err());
}

#[test]
fn response_decoding_accepts_additive_fields_at_every_object_boundary() {
    let expected = ResponseEnvelope::success(
        REQUEST_ID,
        ResponseResult::TurnResult(Box::new(turn_result())),
    );
    let mut wire = serde_json::to_value(&expected).unwrap();
    wire.as_object_mut()
        .unwrap()
        .insert("future_envelope_field".into(), json!({"opaque": true}));
    wire["result"]
        .as_object_mut()
        .unwrap()
        .insert("future_result_field".into(), json!(17));
    wire["result"]["data"]
        .as_object_mut()
        .unwrap()
        .insert("future_turn_field".into(), json!("preserved by the sender"));
    wire["result"]["data"]["usage"]
        .as_object_mut()
        .unwrap()
        .insert("future_usage_category".into(), json!({"tokens": 3}));
    wire["result"]["data"]["usage"]["main"]
        .as_object_mut()
        .unwrap()
        .insert("future_token_counter".into(), json!(3));
    wire["result"]["data"]["completion"]
        .as_object_mut()
        .unwrap()
        .insert("future_evidence".into(), json!(true));
    wire["result"]["data"]["final_blocks"][0]
        .as_object_mut()
        .unwrap()
        .insert(
            "future_block_metadata".into(),
            json!({"source": "minor-v1"}),
        );

    let decoded: ResponseEnvelope = serde_json::from_value(wire).unwrap();
    assert_eq!(decoded, expected);
}

#[test]
fn error_and_event_decoding_accept_additive_fields() {
    let expected_error = ResponseEnvelope::failure(
        REQUEST_ID,
        ErrorBody::new(ErrorCode::RateLimited, "wait").retryable(true),
    );
    let mut error_wire = serde_json::to_value(&expected_error).unwrap();
    error_wire
        .as_object_mut()
        .unwrap()
        .insert("future_envelope_field".into(), json!(true));
    error_wire["error"]
        .as_object_mut()
        .unwrap()
        .insert("future_error_field".into(), json!({"retry_after_ms": 50}));
    assert_eq!(
        serde_json::from_value::<ResponseEnvelope>(error_wire).unwrap(),
        expected_error
    );

    let expected_event = EventEnvelope::new(
        SESSION_ID,
        GENERATION_ID,
        Some(TURN_ID),
        7,
        1_000,
        EventPayload::TurnCompleted(Box::new(turn_result())),
    );
    let mut event_wire = serde_json::to_value(&expected_event).unwrap();
    event_wire
        .as_object_mut()
        .unwrap()
        .insert("future_event_envelope_field".into(), json!(true));
    event_wire["event"]
        .as_object_mut()
        .unwrap()
        .insert("future_event_wrapper_field".into(), json!("opaque"));
    event_wire["event"]["data"]
        .as_object_mut()
        .unwrap()
        .insert("future_turn_field".into(), json!(99));
    event_wire["event"]["data"]["usage"]["sidechain"]
        .as_object_mut()
        .unwrap()
        .insert("future_token_counter".into(), json!(1));
    assert_eq!(
        serde_json::from_value::<EventEnvelope>(event_wire).unwrap(),
        expected_event
    );
}

#[test]
fn additive_decoding_keeps_required_fields_and_discriminants_strict() {
    let missing_required = json!({
        "version": 1,
        "request_id": REQUEST_ID,
        "result": {"type": "pong", "data": {"protocol_version": 1}}
    });
    assert!(serde_json::from_value::<ResponseEnvelope>(missing_required).is_err());

    let unknown_result = json!({
        "version": 1,
        "request_id": REQUEST_ID,
        "result": {"type": "future_result", "data": {}}
    });
    assert!(serde_json::from_value::<ResponseEnvelope>(unknown_result).is_err());

    let unknown_event = json!({
        "schema_version": 1,
        "session_id": SESSION_ID,
        "generation_id": GENERATION_ID,
        "sequence": 1,
        "timestamp_ms": 1,
        "event": {"type": "future_event", "data": {}}
    });
    assert!(serde_json::from_value::<EventEnvelope>(unknown_event).is_err());
}

#[test]
fn response_cannot_contain_both_result_and_error() {
    let invalid: Value = json!({
        "version": 1,
        "request_id": REQUEST_ID,
        "result": {"type": "pong", "data": {"server_version": "x", "protocol_version": 1}},
        "error": {"code": "internal", "message": "bad", "retryable": false}
    });
    assert!(serde_json::from_value::<ResponseEnvelope>(invalid).is_err());
}

#[test]
fn error_body_requires_retryable_and_a_known_v1_code() {
    let missing_retryable = json!({"code": "internal", "message": "bad"});
    assert!(serde_json::from_value::<ErrorBody>(missing_retryable).is_err());

    let wrong_retryable = json!({"code": "internal", "message": "bad", "retryable": "false"});
    assert!(serde_json::from_value::<ErrorBody>(wrong_retryable).is_err());

    let unknown_code = json!({"code": "future_error", "message": "bad", "retryable": false});
    assert!(serde_json::from_value::<ErrorBody>(unknown_code).is_err());

    let mut additive = json!({
        "code": "internal",
        "message": "bad",
        "retryable": false,
        "future_field": {"opaque": true}
    });
    assert!(serde_json::from_value::<ErrorBody>(additive.take()).is_ok());
}

#[test]
fn subscribe_event_bounds_are_enforced_during_wire_decode() {
    let request = |wait_ms, max_events| {
        json!({
            "version": 1,
            "request_id": REQUEST_ID,
            "method": "subscribe_events",
            "params": {
                "session_id": SESSION_ID,
                "generation_id": GENERATION_ID,
                "wait_ms": wait_ms,
                "max_events": max_events
            }
        })
    };
    assert!(
        serde_json::from_value::<RequestEnvelope>(request(30_000, 512)).is_ok(),
        "inclusive public bounds must decode"
    );
    assert!(serde_json::from_value::<RequestEnvelope>(request(30_001, 512)).is_err());
    assert!(serde_json::from_value::<RequestEnvelope>(request(30_000, 513)).is_err());
}

#[test]
fn compatibility_report_wire_is_symmetric_for_resolved_sdk_and_bounded_drain() {
    let valid = compatibility_report();
    let mut report = serde_json::to_value(&valid).unwrap();
    assert!(serde_json::from_value::<CompatibilityReport>(report.clone()).is_ok());
    assert!(validate_v1_serializable(&valid).is_ok());

    for transport in ["auto", "attached_stream"] {
        report["input_transport"] = json!(transport);
        assert!(
            serde_json::from_value::<CompatibilityReport>(report.clone()).is_err(),
            "unresolved transport {transport} must be rejected"
        );
    }

    report["input_transport"] = json!("sdk");
    for drain_ms in [0, 60_001] {
        report["transcript_drain_ms"] = json!(drain_ms);
        assert!(
            serde_json::from_value::<CompatibilityReport>(report.clone()).is_err(),
            "out-of-range drain {drain_ms} must be rejected"
        );
    }
    report["transcript_drain_ms"] = json!(60_000);
    assert!(serde_json::from_value::<CompatibilityReport>(report).is_ok());

    for transport in [InputTransport::Auto, InputTransport::AttachedStream] {
        let mut invalid = valid.clone();
        invalid.input_transport = transport;
        assert!(serde_json::to_vec(&invalid).is_err());
        assert!(validate_v1_serializable(&invalid).is_err());
    }
    for drain_ms in [0, 60_001, MAX_SAFE_JSON_INTEGER + 1, u64::MAX] {
        let mut invalid = valid.clone();
        invalid.transcript_drain_ms = drain_ms;
        assert!(serde_json::to_vec(&invalid).is_err());
        assert!(validate_v1_serializable(&invalid).is_err());
    }
    for drain_ms in [1, 60_000] {
        let mut boundary = valid.clone();
        boundary.transcript_drain_ms = drain_ms;
        assert!(validate_v1_serializable(&boundary).is_ok());
    }
}

#[test]
fn usage_decoding_never_fabricates_missing_counters_or_categories() {
    let token_usage = serde_json::to_value(TokenUsage::default()).unwrap();
    for field in [
        "input_tokens",
        "output_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ] {
        let mut missing = token_usage.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<TokenUsage>(missing).is_err(),
            "missing token counter {field} must be rejected"
        );
    }

    let usage = serde_json::to_value(empty_usage()).unwrap();
    for field in ["main", "sidechain", "combined"] {
        let mut missing = usage.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<UsageBreakdown>(missing).is_err(),
            "missing usage category {field} must be rejected"
        );
    }
}

#[test]
fn rust_serialization_rejects_out_of_domain_protocol_and_opaque_integers() {
    let mut event = EventEnvelope::new(
        SESSION_ID,
        GENERATION_ID,
        None,
        MAX_SAFE_JSON_INTEGER,
        MAX_SAFE_JSON_INTEGER,
        EventPayload::Heartbeat(Heartbeat {
            session_state: SessionState::Ready,
        }),
    );
    assert!(serde_json::to_vec(&event).is_ok());
    event.sequence = MAX_SAFE_JSON_INTEGER + 1;
    assert!(serde_json::to_vec(&event).is_err());

    let request_with_document = |number| {
        let mut request = start_request();
        request.claude.as_mut().expect("inline launch").settings = vec![ConfigSource::Inline {
            document: json!({"nested": [number]}),
        }];
        RequestEnvelope::new(REQUEST_ID, Request::StartSession(request))
    };
    assert!(serde_json::to_vec(&request_with_document(MAX_SAFE_JSON_INTEGER as i64)).is_ok());
    assert!(serde_json::to_vec(&request_with_document(-(MAX_SAFE_JSON_INTEGER as i64))).is_ok());
    assert!(
        serde_json::to_vec(&request_with_document((MAX_SAFE_JSON_INTEGER + 1) as i64)).is_err()
    );
    assert!(
        serde_json::to_vec(&request_with_document(
            -((MAX_SAFE_JSON_INTEGER + 1) as i64)
        ))
        .is_err()
    );
    let integral_float = serde_json::Number::from_f64((MAX_SAFE_JSON_INTEGER + 1) as f64).unwrap();
    let mut request = start_request();
    request.claude.as_mut().expect("inline launch").settings = vec![ConfigSource::Inline {
        document: Value::Number(integral_float),
    }];
    assert!(
        serde_json::to_vec(&RequestEnvelope::new(
            REQUEST_ID,
            Request::StartSession(request)
        ))
        .is_err()
    );

    let unsafe_request = request_with_document((MAX_SAFE_JSON_INTEGER + 1) as i64);
    assert!(validate_v1_serializable(&unsafe_request).is_err());
}

#[test]
fn additive_response_and_event_numbers_obey_the_safe_integer_domain() {
    let response = json!({
        "version": 1,
        "request_id": REQUEST_ID,
        "future": {"integer": MAX_SAFE_JSON_INTEGER + 1},
        "result": {"type": "pong", "data": {
            "server_version": "test",
            "protocol_version": 1
        }}
    });
    assert!(serde_json::from_value::<ResponseEnvelope>(response).is_err());

    let event = json!({
        "schema_version": 1,
        "session_id": SESSION_ID,
        "generation_id": GENERATION_ID,
        "sequence": 1,
        "timestamp_ms": 1,
        "future": {"integer": -(MAX_SAFE_JSON_INTEGER as i64) - 1},
        "event": {"type": "heartbeat", "data": {"session_state": "ready"}}
    });
    assert!(serde_json::from_value::<EventEnvelope>(event).is_err());
}

/// The coarse outcome must be derivable from the fine finding, for every
/// finding, on both probe types.
///
/// This is the invariant that lets a reader fold `outcome` without knowing any
/// finding, and it is enforced by construction: `RuntimeProbe::new` and
/// `SessionProbe::new` are the only constructors and they compute `outcome`
/// from `finding`. The test exists so a future variant that is added to the
/// enum but not to the `outcome()` match cannot ship a report whose summary
/// disagrees with its evidence.
#[test]
fn every_probe_finding_derives_its_own_outcome_on_the_wire() {
    let runtime = [
        (RuntimeFinding::PrivateRuntimeResponsive, ProbeOutcome::Pass),
        (RuntimeFinding::ControlPlaneUnreachable, ProbeOutcome::Fail),
        (RuntimeFinding::ControlPlaneUnresponsive, ProbeOutcome::Fail),
        (RuntimeFinding::ControlPlaneRefused, ProbeOutcome::Fail),
        (RuntimeFinding::LaunchBrokerStopped, ProbeOutcome::Fail),
    ];
    fn exhaustive_runtime(finding: RuntimeFinding) {
        match finding {
            RuntimeFinding::PrivateRuntimeResponsive
            | RuntimeFinding::ControlPlaneUnreachable
            | RuntimeFinding::ControlPlaneUnresponsive
            | RuntimeFinding::ControlPlaneRefused
            | RuntimeFinding::LaunchBrokerStopped => (),
        }
    }
    for (finding, outcome) in runtime {
        exhaustive_runtime(finding);
        assert_eq!(finding.outcome(), outcome, "{finding:?}");
        let probe = RuntimeProbe::new(finding, 1, None);
        assert_eq!(probe.outcome, outcome, "{finding:?}");
        let encoded = serde_json::to_value(&probe).unwrap();
        assert_eq!(
            encoded["outcome"],
            serde_json::to_value(outcome).unwrap(),
            "{finding:?}"
        );
    }

    let sessions = [
        (SessionFinding::TerminalPresent, ProbeOutcome::Pass),
        (SessionFinding::TerminalMissing, ProbeOutcome::Fail),
        (
            SessionFinding::SessionDeclaredUnusable,
            ProbeOutcome::Unproven,
        ),
        (
            SessionFinding::SessionActorUnresponsive,
            ProbeOutcome::Unproven,
        ),
        (
            SessionFinding::SessionClosedDuringProbe,
            ProbeOutcome::Unproven,
        ),
        (SessionFinding::NotProbed, ProbeOutcome::Unproven),
    ];
    fn exhaustive_session(finding: SessionFinding) {
        match finding {
            SessionFinding::TerminalPresent
            | SessionFinding::TerminalMissing
            | SessionFinding::SessionDeclaredUnusable
            | SessionFinding::SessionActorUnresponsive
            | SessionFinding::SessionClosedDuringProbe
            | SessionFinding::NotProbed => (),
        }
    }
    for (finding, outcome) in sessions {
        exhaustive_session(finding);
        assert_eq!(finding.outcome(), outcome, "{finding:?}");
        let probe = SessionProbe::new(SESSION_ID, GENERATION_ID, finding, None, None);
        assert_eq!(probe.outcome, outcome, "{finding:?}");
        let encoded = serde_json::to_value(&probe).unwrap();
        assert_eq!(
            encoded["outcome"],
            serde_json::to_value(outcome).unwrap(),
            "{finding:?}"
        );
    }
}

/// A complete tree that a daemon holding `sessions` could actually emit.
///
/// Written as "every layer" rather than as a literal list so that a layer added
/// to [`HealthLayerName`] joins it automatically, and a test that wanted a
/// complete tree keeps getting one.
///
/// The `sessions` layer is built by its PRODUCER, [`HealthLayer::for_sessions`],
/// and not stamped `exercised` like the rest. A tree carrying `sessions: []`
/// beside a `sessions` layer reading `exercised` is a combination no daemon can
/// produce, and asserting `pass` on one proves nothing about the fold a real
/// report goes through.
fn every_layer_healthy_for(sessions: &[SessionProbe]) -> Vec<HealthLayer> {
    HealthLayerName::ALL
        .iter()
        .map(|name| {
            if *name == HealthLayerName::Sessions {
                HealthLayer::for_sessions(sessions)
            } else {
                HealthLayer::new(
                    *name,
                    LayerFinding::Exercised,
                    "exercised",
                    serde_json::Value::Null,
                )
            }
        })
        .collect()
}

/// A layer that is ABSENT from the report is `unproven`, never `pass`.
///
/// The rule this pins is the whole point of a proof tree: not-established must
/// never roll up as healthy, and a layer nobody reported is the purest form of
/// not-established. `ProbeOutcome::fold` over an empty set is `pass` -- correct
/// for sessions, where holding none is a capacity fact -- so a `DaemonDiagnosis`
/// that simply folded `self.layers` would report a daemon that established
/// nothing as healthy.
#[test]
fn a_layer_missing_from_the_tree_is_unproven_and_never_a_pass() {
    let healthy_elsewhere = |layers: Vec<HealthLayer>| DaemonDiagnosis {
        layers,
        runtime: RuntimeProbe::new(RuntimeFinding::PrivateRuntimeResponsive, 1, Some(0)),
        sessions: Vec::new(),
    };

    // The complete tree, every layer healthy for the sessions the report
    // actually carries, nothing else wrong: pass.
    let complete = healthy_elsewhere(every_layer_healthy_for(&[]));
    assert!(complete.missing_layers().is_empty());
    assert_eq!(complete.outcome(), ProbeOutcome::Pass);

    // No layers at all. Every other field says health.
    let none = healthy_elsewhere(Vec::new());
    assert_eq!(none.missing_layers(), HealthLayerName::ALL.to_vec());
    assert_eq!(
        none.outcome(),
        ProbeOutcome::Unproven,
        "a report that established no layer at all must not read as healthy"
    );

    // Each single layer dropped in turn. Removing exactly one must move the
    // fold, or that layer contributes nothing and its presence is decoration.
    for dropped in HealthLayerName::ALL.iter().copied() {
        let layers = every_layer_healthy_for(&[])
            .into_iter()
            .filter(|layer| layer.layer != dropped)
            .collect::<Vec<_>>();
        let short = healthy_elsewhere(layers);
        assert_eq!(short.missing_layers(), vec![dropped], "{dropped:?}");
        assert_eq!(
            short.outcome(),
            ProbeOutcome::Unproven,
            "dropping {dropped:?} left the report reading as healthy"
        );
    }
}

/// Every layer finding derives its own outcome, and `not_established` is
/// `unproven` rather than a shade of pass.
///
/// `nothing_to_exercise` is the fourth, and it is `pass`. It is not a fourth
/// shade of `not_established`: "there was nothing here" and "I could not find
/// out" are different answers, and encoding the first as the second is what
/// made every correct Path B daemon report `unproven` forever.
#[test]
fn every_layer_finding_derives_its_own_outcome_on_the_wire() {
    let cases = [
        (LayerFinding::Exercised, ProbeOutcome::Pass),
        (LayerFinding::Faulted, ProbeOutcome::Fail),
        (LayerFinding::NothingToExercise, ProbeOutcome::Pass),
        (LayerFinding::NotEstablished, ProbeOutcome::Unproven),
    ];
    fn exhaustive(finding: LayerFinding) {
        match finding {
            LayerFinding::Exercised
            | LayerFinding::Faulted
            | LayerFinding::NothingToExercise
            | LayerFinding::NotEstablished => {}
        }
    }
    for (finding, outcome) in cases {
        exhaustive(finding);
        let layer = HealthLayer::new(
            HealthLayerName::Pool,
            finding,
            "detail",
            json!({"observed": true}),
        );
        assert_eq!(layer.outcome, outcome, "{finding:?}");
        let encoded = serde_json::to_value(&layer).unwrap();
        assert_eq!(encoded["outcome"], serde_json::to_value(outcome).unwrap());
        assert_eq!(encoded["finding"], serde_json::to_value(finding).unwrap());
        assert_eq!(
            encoded["detail"], "detail",
            "a layer states what it exercised even when it passed"
        );
        assert_eq!(
            serde_json::from_value::<HealthLayer>(encoded).unwrap(),
            layer
        );
    }
}

/// The sessions layer is built by ONE producer, and an empty registry is
/// `nothing_to_exercise` rather than `not_established`.
///
/// This is the fold `pmux doctor` exits on. MEASURED against a live daemon
/// before the split: a warm pool of two idle instances, every other layer
/// `pass`, `sessions: []`, and the report read
///
/// ```text
/// status: unproven
/// unproven: ['sessions: the registry holds no sessions, so no session was exercised']
/// ```
///
/// with exit status 1. `sessions: []` is the PERMANENT shape of a daemon that
/// serves only stateless turns, because pool instances are deliberately never
/// registered as caller sessions -- so the surface built to stop a boolean
/// `healthy` from lying was crying wolf on every healthy daemon instead.
#[test]
fn the_sessions_layer_has_one_producer_and_an_empty_registry_is_vacuously_fine() {
    let empty = HealthLayer::for_sessions(&[]);
    assert_eq!(empty.layer, HealthLayerName::Sessions);
    assert_eq!(empty.finding, LayerFinding::NothingToExercise);
    assert_eq!(
        empty.outcome,
        ProbeOutcome::Pass,
        "holding no sessions is a capacity fact, which is what `ProbeOutcome::fold` \
         over an empty set already said"
    );
    assert!(
        !empty.detail.is_empty(),
        "a vacuous pass still has to say what was absent"
    );
    assert_eq!(empty.evidence["registered"], 0);

    // Every non-empty shape: the layer's finding is the fold of its probes,
    // derived and not restated.
    for (finding, expected) in [
        (SessionFinding::TerminalPresent, LayerFinding::Exercised),
        (SessionFinding::TerminalMissing, LayerFinding::Faulted),
        (
            SessionFinding::SessionActorUnresponsive,
            LayerFinding::NotEstablished,
        ),
        (
            SessionFinding::SessionDeclaredUnusable,
            LayerFinding::NotEstablished,
        ),
        (
            SessionFinding::SessionClosedDuringProbe,
            LayerFinding::NotEstablished,
        ),
        (SessionFinding::NotProbed, LayerFinding::NotEstablished),
    ] {
        let probes = vec![SessionProbe::new(
            SESSION_ID,
            GENERATION_ID,
            finding,
            None,
            None,
        )];
        let layer = HealthLayer::for_sessions(&probes);
        assert_eq!(layer.finding, expected, "{finding:?}");
        assert_eq!(
            layer.outcome,
            ProbeOutcome::fold(probes.iter().map(|probe| probe.outcome)),
            "the layer outcome must be the fold of its probes, not a second table: {finding:?}"
        );
        assert_eq!(layer.evidence["registered"], 1);
    }

    // The whole point: a layer that HAS a subject and no answer is still
    // `unproven`, so splitting the encoding did not buy health with silence.
    let unanswered = vec![SessionProbe::new(
        SESSION_ID,
        GENERATION_ID,
        SessionFinding::SessionActorUnresponsive,
        None,
        None,
    )];
    assert_eq!(
        HealthLayer::for_sessions(&unanswered).outcome,
        ProbeOutcome::Unproven
    );
}

/// One spelling of an effort tier, and `Debug` is not it.
///
/// `EffortLevel::as_str` is what a message beside a literal `--effort` must
/// use. This pins it against `Serialize`, so the word in a refusal, the word on
/// the wire and the word clap accepts cannot come apart. `{effort:?}` renders
/// `XHigh`, which every one of the three rejects.
#[test]
fn an_effort_tier_has_exactly_one_spelling_and_it_is_not_the_rust_identifier() {
    let every = [
        EffortLevel::Low,
        EffortLevel::Medium,
        EffortLevel::High,
        EffortLevel::XHigh,
        EffortLevel::Max,
    ];
    fn exhaustive(effort: EffortLevel) {
        match effort {
            EffortLevel::Low
            | EffortLevel::Medium
            | EffortLevel::High
            | EffortLevel::XHigh
            | EffortLevel::Max => {}
        }
    }
    for effort in every {
        exhaustive(effort);
        assert_eq!(
            serde_json::to_value(effort).unwrap(),
            Value::String(effort.as_str().to_owned()),
            "{effort:?} spells itself differently on the wire than in a message"
        );
        assert_eq!(
            serde_json::from_value::<EffortLevel>(Value::String(effort.as_str().to_owned()))
                .unwrap(),
            effort,
            "the spelling a message prints must parse back to the tier it names"
        );
        assert_eq!(effort.to_string(), effort.as_str());
    }
    assert_eq!(EffortLevel::XHigh.as_str(), "xhigh");
    assert!(
        serde_json::from_value::<EffortLevel>(Value::String(format!("{:?}", EffortLevel::XHigh)))
            .is_err(),
        "`XHigh` is a Rust identifier and nothing on this surface accepts it"
    );
}

/// `HealthLayerName::ALL` names every variant.
///
/// The array is what `missing_layers` reads to decide what was not established,
/// so a variant absent from it is a layer nothing ever notices is missing. It is
/// built from an exhaustive `match`, which makes a missing variant a compile
/// error; this pins the count too, so a variant added to the match AND the array
/// still has to be admitted deliberately.
#[test]
fn the_layer_inventory_names_every_layer_exactly_once() {
    let named = HealthLayerName::ALL
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        named.len(),
        HealthLayerName::ALL.len(),
        "a layer is listed twice"
    );
    assert_eq!(
        HealthLayerName::ALL,
        &[
            HealthLayerName::Configuration,
            HealthLayerName::ControlPlane,
            HealthLayerName::PrivateRuntime,
            HealthLayerName::LaunchBroker,
            HealthLayerName::CompatibilityProfile,
            HealthLayerName::Pool,
            HealthLayerName::Sessions,
            HealthLayerName::Performance,
        ]
    );
}

/// Evidence of a fault outranks absence of evidence, which outranks evidence of
/// health. The empty fold is `pass` on purpose: a daemon holding no sessions is
/// idle, not broken.
#[test]
fn probe_outcomes_fold_to_the_worst_present_and_an_empty_fold_is_a_pass() {
    use ProbeOutcome::{Fail, Pass, Unproven};

    assert_eq!(ProbeOutcome::fold([]), Pass);
    assert_eq!(ProbeOutcome::fold([Pass, Pass]), Pass);
    assert_eq!(ProbeOutcome::fold([Pass, Unproven]), Unproven);
    assert_eq!(ProbeOutcome::fold([Unproven, Fail]), Fail);
    assert_eq!(ProbeOutcome::fold([Fail, Unproven, Pass]), Fail);
    assert!(Pass < Unproven && Unproven < Fail);

    // One faulty session outranks a responsive runtime and every healthy
    // sibling. A report that folded any other way would let a pool stay green
    // with a dead instance in it.
    let sessions = vec![
        SessionProbe::new(
            SESSION_ID,
            GENERATION_ID,
            SessionFinding::TerminalPresent,
            Some(SessionState::Ready),
            Some(true),
        ),
        SessionProbe::new(
            TURN_ID,
            GENERATION_ID,
            SessionFinding::TerminalMissing,
            Some(SessionState::Ready),
            Some(false),
        ),
    ];
    let diagnosis = DaemonDiagnosis {
        layers: every_layer_healthy_for(&sessions),
        runtime: RuntimeProbe::new(RuntimeFinding::PrivateRuntimeResponsive, 1, Some(1)),
        sessions,
    };
    assert_eq!(diagnosis.outcome(), Fail);
}

/// `diagnose` selects nothing and bounds nothing, so it carries no params, and
/// the report round-trips unchanged.
#[test]
fn diagnose_is_a_bare_method_and_its_report_round_trips() {
    let envelope = RequestEnvelope::new(REQUEST_ID, Request::Diagnose);
    let encoded = serde_json::to_value(&envelope).unwrap();
    assert_eq!(
        encoded,
        json!({"version": 1, "request_id": REQUEST_ID, "method": "diagnose"})
    );
    assert_eq!(
        serde_json::from_value::<RequestEnvelope>(encoded).unwrap(),
        envelope
    );

    let sessions = vec![SessionProbe::new(
        SESSION_ID,
        GENERATION_ID,
        SessionFinding::NotProbed,
        None,
        None,
    )];
    let diagnosis = DaemonDiagnosis {
        layers: every_layer_healthy_for(&sessions),
        runtime: RuntimeProbe::new(RuntimeFinding::ControlPlaneUnresponsive, 10_000, None),
        sessions,
    };
    let response = ResponseEnvelope::success(
        REQUEST_ID,
        ResponseResult::Diagnosis(Box::new(diagnosis.clone())),
    );
    let encoded = serde_json::to_value(&response).unwrap();
    assert_eq!(encoded["result"]["type"], "diagnosis");
    // Absent evidence is omitted rather than encoded as a zero, which is what a
    // reader would otherwise be free to mistake for "no live terminals".
    assert!(
        encoded["result"]["data"]["runtime"]
            .get("live_private_terminals")
            .is_none()
    );
    assert!(
        encoded["result"]["data"]["sessions"][0]
            .get("private_terminal_present")
            .is_none()
    );
    assert!(
        encoded["result"]["data"]["sessions"][0]
            .get("state")
            .is_none()
    );
    assert_eq!(
        serde_json::from_value::<ResponseEnvelope>(encoded).unwrap(),
        response
    );
}

/// The stateless request is the whole Path B surface, and its refusals are what
/// make "the caller names no resource" a property of the wire rather than of a
/// convention. `deny_unknown_fields` refuses each absent field BY NAME rather
/// than ignoring it: a caller that believes it set a system prompt and silently
/// did not is worse than one that gets `InvalidConfig`.
#[test]
fn a_stateless_request_refuses_every_field_it_does_not_declare() {
    let admitted = json!({
        "model": "claude-opus-5",
        "effort": "xhigh",
        "prompt": "what is two plus two",
        "deadline_unix_ms": 1_700_000_000_000_u64
    });
    let request: RunStatelessRequest =
        serde_json::from_value(admitted.clone()).expect("the declared shape round-trips");
    assert_eq!(request.model, "claude-opus-5");
    assert_eq!(request.effort, Some(EffortLevel::XHigh));
    assert_eq!(request.deadline_unix_ms, Some(1_700_000_000_000));
    assert_eq!(
        serde_json::to_value(&request).expect("serializes"),
        admitted,
        "the wire shape is exactly what was accepted"
    );

    // Every resource a caller might reach for, refused by name. Each of these
    // is a name a second caller could also write, which is precisely how the
    // reproduced `CLAUDE_CONFIG_DIR` leak was admitted into a live cell.
    for field in [
        "system_prompt",
        "cwd",
        "config_isolation",
        "session_id",
        "generation_id",
        "turn_id",
        "lease",
        "cell",
        "permission_mode",
        "extra_args",
        "environment",
        "retention",
        "allowed_tools",
        "denied_tools",
        "executable",
        "terminal",
    ] {
        let mut extended = admitted.clone();
        extended
            .as_object_mut()
            .expect("object")
            .insert(field.to_owned(), json!("anything"));
        assert!(
            serde_json::from_value::<RunStatelessRequest>(extended).is_err(),
            "a stateless request must refuse `{field}` by name rather than ignore it"
        );
    }

    // `model` and `prompt` are required; the other two are absent-means-default
    // and must not be serialized when unset.
    for required in ["model", "prompt"] {
        let mut shrunk = admitted.clone();
        shrunk.as_object_mut().expect("object").remove(required);
        assert!(
            serde_json::from_value::<RunStatelessRequest>(shrunk).is_err(),
            "`{required}` is required"
        );
    }
    let minimal = RunStatelessRequest {
        model: "claude-haiku-4-5".into(),
        effort: None,
        prompt: "hello".into(),
        deadline_unix_ms: None,
    };
    assert_eq!(
        serde_json::to_value(&minimal).expect("serializes"),
        json!({"model": "claude-haiku-4-5", "prompt": "hello"}),
        "absent means default, and an absent field is not written"
    );
}

/// The stateless result publishes the product and nothing that names a resource
/// a second call could reach.
#[test]
fn a_stateless_result_publishes_tokens_and_no_reachable_name() {
    let result = StatelessResult {
        model: "claude-opus-5".into(),
        reported_model: Some("claude-opus-5".into()),
        effort: Some(EffortLevel::High),
        text: "four".into(),
        stop_reason: Some(StopReason {
            kind: StopReasonKind::EndTurn,
            raw: None,
        }),
        usage: UsageBreakdown {
            main: TokenUsage {
                input_tokens: 186,
                output_tokens: 3,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
            sidechain: TokenUsage::default(),
            combined: TokenUsage {
                input_tokens: 186,
                output_tokens: 3,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
            cost_usd: None,
        },
        claude_version: "2.1.220".into(),
    };
    let value = serde_json::to_value(&result).expect("serializes");
    let object = value.as_object().expect("object");

    // The published set, pinned. Adding a field later is a compatible minor
    // evolution while removing one is not, which is why every omission below
    // was chosen deliberately and this list is asserted rather than sampled.
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "claude_version",
            "effort",
            "model",
            "reported_model",
            "stop_reason",
            "text",
            "usage",
        ]
    );
    for absent in [
        "session_id",
        "generation_id",
        "turn_id",
        "final_sequence",
        "outcome",
        "final_blocks",
        "tools",
        "timings",
        "compatibility",
        "warnings",
        "completion",
        "replayed",
        "cwd",
        "config_isolation",
    ] {
        assert!(
            !object.contains_key(absent),
            "a stateless result must not publish `{absent}`: a caller that cannot name a resource cannot share one"
        );
    }

    // Results are permissive on unknown fields, so a newer daemon may add one
    // without an older client rejecting the whole frame.
    let mut extended = value.clone();
    extended
        .as_object_mut()
        .expect("object")
        .insert("future_field".into(), json!(1));
    let widened: StatelessResult =
        serde_json::from_value(extended).expect("a result tolerates a field it does not know");
    assert_eq!(widened, result);

    // `reported_model` and `effort` are absent-means-absent: pmux does not
    // fabricate a model the transcript never reported, nor an effort the
    // resolved model does not take.
    let bare = StatelessResult {
        reported_model: None,
        effort: None,
        ..result
    };
    let bare_value = serde_json::to_value(&bare).expect("serializes");
    let bare_object = bare_value.as_object().expect("object");
    assert!(!bare_object.contains_key("reported_model"));
    assert!(!bare_object.contains_key("effort"));
}

/// The two variants are appended last, which is the property the shared
/// conformance manifest's positional comparison depends on.
#[test]
fn the_stateless_variants_are_appended_last_and_carry_their_own_tags() {
    let request = RequestEnvelope {
        version: PROTOCOL_VERSION,
        request_id: REQUEST_ID,
        request: Request::RunStateless(RunStatelessRequest {
            model: "claude-opus-5".into(),
            effort: None,
            prompt: "hello".into(),
            deadline_unix_ms: None,
        }),
    };
    let value = serde_json::to_value(&request).expect("serializes");
    assert_eq!(value["method"], json!("run_stateless"));
    assert_eq!(value["params"]["model"], json!("claude-opus-5"));

    let response = ResponseEnvelope::success(
        REQUEST_ID,
        ResponseResult::StatelessResult(Box::new(StatelessResult {
            model: "claude-opus-5".into(),
            reported_model: None,
            effort: None,
            text: "four".into(),
            stop_reason: None,
            usage: UsageBreakdown::default(),
            claude_version: "2.1.220".into(),
        })),
    );
    let value = serde_json::to_value(&response).expect("serializes");
    assert_eq!(value["result"]["type"], json!("stateless_result"));
    // Boxing is serde-transparent, so the wire shape is unchanged by it.
    assert_eq!(value["result"]["data"]["model"], json!("claude-opus-5"));
    assert!(
        value["result"]["data"].get("session_id").is_none(),
        "not even through the envelope does a pool instance get a name"
    );
}

// ---- The agent resource ------------------------------------------------------

const AGENT_ID: Uuid = Uuid::from_u128(6);

/// One `AgentSpec` with EVERY field written out.
///
/// The struct literal has no `..Default::default()`: a field added to
/// `AgentSpec` is a compile error here, which is what makes the intersection
/// below a derivation rather than a snapshot.
///
/// It shares `launch_config()` with `start_request()`, and its environment map
/// uses the same KEY and its array the same length, deliberately: a JSON leaf
/// path through a map includes the key, so two fixtures that disagreed there
/// would report a collision-free map. That is a failure and not a silent pass --
/// the derived set would then differ from the production list, which is exactly
/// what `the_agent_supplied_start_paths_are_exactly_the_serialized_leaf_collision`
/// asserts -- but it is worth stating so the next reader does not "simplify" it.
fn fully_populated_agent_spec() -> AgentSpec {
    AgentSpec {
        name: "reviewer".into(),
        description: Some("reads and reports".into()),
        claude: launch_config(),
        environment: AgentEnvironmentSpec {
            set: BTreeMap::from([("TERM".into(), "xterm-256color".into())]),
            unset: BTreeSet::from(["ANTHROPIC_API_KEY".into()]),
        },
        auth_policy: AuthPolicy::Inherit,
        terminal: TerminalSpec {
            rows: 40,
            cols: 132,
            profile: TerminalProfile::Transparent,
            input_transport: InputTransport::Sdk,
        },
        lifecycle: LifecycleMode::Hybrid {
            hook_timeout_ms: 5_000,
        },
        retention: RetentionPolicy::Persistent {
            idle_ttl_ms: 1_800_000,
        },
        compatibility: CompatibilityPolicy::AllowUntested,
        cell: SessionCell::Minified,
        containment: AgentContainment {
            workspace_root: Some("/work".into()),
            require_config_isolation: true,
        },
    }
}

/// Every leaf path of a serialized value: a path whose value is not an object.
///
/// Leaves, not interior objects, because that distinction IS the design.
/// `environment` is an object of both `AgentSpec` and `StartSessionRequest`, so
/// an interior-node intersection would say the whole of `environment` collides
/// -- and `environment.snapshot`, the one launch input an agent structurally
/// cannot carry, would be refused beside an agent for no reason.
fn leaf_paths(value: &Value, prefix: &str, into: &mut BTreeSet<String>) {
    match value {
        Value::Object(object) if !object.is_empty() => {
            for (key, child) in object {
                let next = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                leaf_paths(child, &next, into);
            }
        }
        _ => {
            into.insert(prefix.to_owned());
        }
    }
}

/// THE BOTH-MODES CONFLICT SET, COMPUTED FROM THE SERIALIZERS.
///
/// This is the second direction of the derivation `agent_supplied_start_paths`
/// documents. That function is guarded by a `..`-free destructuring, which
/// forces a new `AgentSpec` field to be CLASSIFIED; this test forces it to be
/// classified CORRECTLY, by recomputing the answer from the wire bytes of two
/// fully populated fixtures:
///
/// 1. Serialize a fully populated `AgentSpec` and a fully populated
///    `StartSessionRequest`, and collect the LEAF paths of each.
/// 2. Intersect them.
/// 3. Reduce to the MAXIMAL paths all of whose leaves collide -- so `terminal`,
///    every leaf of which is in both, appears as `terminal`, while
///    `environment`, only two of whose three leaves collide, appears as
///    `environment.set` and `environment.unset`.
///
/// Both fixtures are struct literals with no `..`, so a field added to either
/// type is a compile error until the fixture sets it -- and once it does, the
/// intersection moves, and this assertion is red until the production list
/// moves with it. That is the same serializer-backed technique
/// `validate_v1_serializable` states for itself: "deliberately serializer-backed
/// rather than a second field inventory".
#[test]
fn the_agent_supplied_start_paths_are_exactly_the_serialized_leaf_collision() {
    let spec = serde_json::to_value(fully_populated_agent_spec()).expect("spec serializes");
    let start = serde_json::to_value(start_request()).expect("start serializes");

    let mut spec_leaves = BTreeSet::new();
    leaf_paths(&spec, "", &mut spec_leaves);
    let mut start_leaves = BTreeSet::new();
    leaf_paths(&start, "", &mut start_leaves);

    let collisions: BTreeSet<String> = spec_leaves.intersection(&start_leaves).cloned().collect();
    assert!(
        collisions.contains("claude.executable"),
        "the fixtures no longer share a launch configuration, so this test proves nothing"
    );
    assert!(
        !collisions.contains("environment.snapshot"),
        "environment.snapshot must not collide: an agent has no snapshot field at all, which is \
         what makes a caller's snapshot survive beside an agent with no exception list"
    );

    // Every path either fixture can express, interior nodes included, so the
    // reduction below can ask about a whole sub-object.
    let mut candidates: BTreeSet<String> = BTreeSet::new();
    for leaf in &collisions {
        let mut path = String::new();
        for component in leaf.split('.') {
            if !path.is_empty() {
                path.push('.');
            }
            path.push_str(component);
            candidates.insert(path.clone());
        }
    }

    // A candidate is agent-supplied when EVERY leaf of the start request under
    // it collides. `terminal` qualifies; `environment` does not, because
    // `environment.snapshot` is a leaf of the start request alone.
    let supplied: BTreeSet<String> = candidates
        .iter()
        .filter(|candidate| {
            let prefix = format!("{candidate}.");
            start_leaves
                .iter()
                .filter(|leaf| *leaf == *candidate || leaf.starts_with(&prefix))
                .all(|leaf| collisions.contains(leaf))
        })
        .cloned()
        .collect();
    // ...and only the outermost such path is named in a refusal.
    let maximal: BTreeSet<String> = supplied
        .iter()
        .filter(|path| {
            !supplied
                .iter()
                .any(|other| path.starts_with(&format!("{other}.")))
        })
        .cloned()
        .collect();

    assert_eq!(
        maximal,
        agent_supplied_start_paths()
            .iter()
            .map(|path| (*path).to_owned())
            .collect::<BTreeSet<_>>(),
        "the derived collision between AgentSpec and StartSessionRequest is not the list the \
         both-modes refusal uses"
    );
}

/// A listing carries the records the daemon could not read, and OMITS the
/// field when there are none.
///
/// Both halves matter. The field exists because `list_agents` used to answer
/// the whole listing with one bad record's refusal -- which made `no agent
/// <id>`'s own recommendation, "list the stored agents with `pmux agent
/// list`", unreachable in precisely the state it was offered. The omission
/// matters because a result frame is compared byte-for-byte against the shared
/// corpus in three languages: an ordinary listing must serialize exactly as it
/// did before this field existed.
#[test]
fn a_listing_reports_unreadable_records_and_omits_the_field_when_there_are_none() {
    let empty = serde_json::to_value(AgentList::default()).expect("serializes");
    assert_eq!(
        empty,
        json!({}),
        "an empty listing must be the bytes every release before `unreadable` sent"
    );

    let ordinary = AgentList {
        agents: vec![AgentSummary {
            agent_id: AGENT_ID,
            version: AgentVersion::FIRST,
            config_digest: "0".repeat(64),
            name: "reviewer".into(),
            description: None,
            cell: SessionCell::Full,
            updated_at_ms: 10,
        }],
        unreadable: Vec::new(),
    };
    let value = serde_json::to_value(&ordinary).expect("serializes");
    assert!(
        value
            .as_object()
            .expect("an object")
            .get("unreadable")
            .is_none(),
        "a listing with nothing unreadable must not mention the field: {value}"
    );
    assert_eq!(
        serde_json::from_value::<AgentList>(value).expect("round-trips"),
        ordinary
    );

    let reported = AgentList {
        agents: Vec::new(),
        unreadable: vec![AgentListFailure {
            agent_id: AGENT_ID,
            reason: "agent store /x/1.json is not a readable agent version".into(),
        }],
    };
    let value = serde_json::to_value(&reported).expect("serializes");
    assert_eq!(
        value["unreadable"][0]["agent_id"],
        json!(AGENT_ID.hyphenated().to_string())
    );
    assert_eq!(
        serde_json::from_value::<AgentList>(value).expect("round-trips"),
        reported
    );

    // The id is held to the same canonical-UUID bar every other id on this
    // wire is: a record reported by a spelling a caller cannot re-ask about is
    // not a report.
    serde_json::from_value::<AgentList>(json!({
        "unreadable": [{"agent_id": AGENT_ID.simple().to_string(), "reason": "x"}]
    }))
    .expect_err("a non-canonical id is not an id");
    serde_json::from_value::<AgentList>(json!({
        "unreadable": [{"agent_id": AGENT_ID.hyphenated().to_string()}]
    }))
    .expect_err("a reported failure without a reason says nothing");
}

/// A start may name an agent OR carry launch policy, and never both.
///
/// Checked over the production list rather than over a few interesting fields,
/// so a path added to it is exercised here without anyone remembering this test
/// exists.
#[test]
fn a_start_naming_an_agent_is_refused_by_name_for_every_supplied_path() {
    let base = json!({
        "identity": {"mode": "new"},
        "cwd": "/work/project",
        "agent": {"agent_id": AGENT_ID.hyphenated().to_string(), "version": 3},
    });
    // The baseline is admitted: without it, every assertion below could be
    // passing for a reason that has nothing to do with the conflict.
    let accepted = serde_json::from_value::<StartSessionRequest>(base.clone())
        .expect("an agent reference alone is a complete start");
    assert_eq!(
        accepted.agent,
        Some(AgentRef {
            agent_id: AGENT_ID,
            version: AgentVersion::new(3).expect("3 is a version"),
        })
    );
    assert!(accepted.claude.is_none());

    // ...and so is a caller's own environment snapshot, which no agent can
    // carry and which therefore needs no exception anywhere.
    let mut with_snapshot = base.clone();
    with_snapshot["environment"] = json!({"snapshot": {"PATH": "/usr/bin"}});
    let snapshot_start = serde_json::from_value::<StartSessionRequest>(with_snapshot)
        .expect("a caller's snapshot survives beside an agent");
    assert_eq!(
        snapshot_start.environment.snapshot,
        BTreeMap::from([("PATH".to_owned(), "/usr/bin".to_owned())])
    );

    let offending = |path: &str| -> Value {
        match path {
            "claude" => json!({"executable": "/opt/claude/bin/claude"}),
            "environment.set" => json!({"set": {"TERM": "dumb"}}),
            "environment.unset" => json!({"unset": ["TERM"]}),
            "auth_policy" => json!("inherit"),
            "terminal" => json!({"rows": 40, "cols": 132}),
            "lifecycle" => json!({"mode": "transcript"}),
            "retention" => json!({"mode": "persistent"}),
            "compatibility" => json!("allow_untested"),
            "cell" => json!("minified"),
            other => panic!(
                "agent_supplied_start_paths gained {other:?} and this test has no value for it"
            ),
        }
    };

    for path in agent_supplied_start_paths() {
        let mut request = base.clone();
        let key = path
            .split('.')
            .next()
            .expect("a path has a first component");
        request[key] = offending(path);
        let error = serde_json::from_value::<StartSessionRequest>(request)
            .expect_err("a start naming an agent may not also carry launch policy");
        assert!(
            error.to_string().contains(path),
            "the refusal for {path} does not name it: {error}"
        );

        // ...and the same combination is unrepresentable on the way OUT, so a
        // Rust embedder that built the DTO directly is refused by the same
        // sentence rather than silently having its value dropped.
        //
        // ALL NINE, WITH NO ARM THAT SKIPS. Four of them used to `continue`
        // here, excused by "already unrepresentable-or-omitted on the way out:
        // `claude` is an `Option`, `cell` and the two environment maps carry
        // `skip_serializing_if`, so each is sent when present and refused by
        // name on arrival". "On arrival" is `Deserialize`, and an in-process
        // caller never runs it: `validate_v1_serializable` only serializes.
        // MEASURED against that shape, an embedder sending `cell: minified`
        // beside a `full` agent was accepted and launched `full`.
        let mut typed = serde_json::from_value::<StartSessionRequest>(base.clone()).expect("base");
        match *path {
            "claude" => typed.claude = Some(launch_config()),
            "environment.set" => {
                typed
                    .environment
                    .set
                    .insert("TERM".to_owned(), "dumb".to_owned());
            }
            "environment.unset" => {
                typed.environment.unset.insert("TERM".to_owned());
            }
            "auth_policy" => typed.auth_policy = AuthPolicy::Inherit,
            "terminal" => typed.terminal.rows += 1,
            "lifecycle" => {
                typed.lifecycle = LifecycleMode::Hybrid {
                    hook_timeout_ms: 1_000,
                };
            }
            "retention" => typed.retention = RetentionPolicy::OneShot,
            "compatibility" => typed.compatibility = CompatibilityPolicy::AllowUntested,
            "cell" => typed.cell = SessionCell::Minified,
            other => panic!(
                "agent_supplied_start_paths gained {other:?} and this test has no typed value for \
                 it; a path the serializer does not check is a launch field an embedder can have \
                 silently replaced"
            ),
        }
        let error = validate_v1_serializable(&typed)
            .expect_err("a resolved start cannot carry launch policy beside an agent");
        assert!(
            error.to_string().contains(path),
            "the serializer's refusal for {path} does not name it: {error}"
        );
    }
}

/// The serializer's guard and the decoder's guard walk the SAME list.
///
/// The decoder's has always been derived; the serializer's was five
/// hand-written arms out of nine, and the four it dropped -- `claude`,
/// `environment.set`, `environment.unset` and `cell` -- are exactly the four an
/// embedder could get silently replaced. This asserts the property that made
/// that possible is gone: for every path in the production list, a typed
/// request carrying it beside an agent fails to serialize, and the same request
/// WITHOUT an agent serializes fine.
///
/// It is deliberately not a restatement of the loop above. That one proves each
/// refusal names its path; this one proves the guard is not merely refusing
/// everything, which a `return Err` with no condition would also do.
#[test]
fn a_start_carrying_launch_policy_without_an_agent_still_serializes() {
    let base = json!({
        "identity": {"mode": "new"},
        "cwd": "/work/project",
        "claude": {"executable": "/opt/claude/bin/claude"},
    });
    let mut typed = serde_json::from_value::<StartSessionRequest>(base).expect("an inline start");
    typed.auth_policy = AuthPolicy::Inherit;
    typed.terminal.rows += 1;
    typed.lifecycle = LifecycleMode::Hybrid {
        hook_timeout_ms: 1_000,
    };
    typed.retention = RetentionPolicy::OneShot;
    typed.compatibility = CompatibilityPolicy::AllowUntested;
    typed.cell = SessionCell::Minified;
    typed
        .environment
        .set
        .insert("TERM".to_owned(), "dumb".to_owned());
    typed.environment.unset.insert("NO_COLOR".to_owned());
    validate_v1_serializable(&typed)
        .expect("every agent-supplied path is ordinary launch policy when no agent is named");

    // ...and adding the agent to that exact value is what refuses it.
    typed.agent = Some(AgentRef {
        agent_id: AGENT_ID,
        version: AgentVersion::new(3).expect("3 is a version"),
    });
    let error = validate_v1_serializable(&typed).expect_err("the agent is what makes it a merge");
    assert!(
        error.to_string().contains("claude"),
        "the refusal names the first colliding path in the derived order: {error}"
    );
}

/// A start that names neither is refused, rather than defaulted into one.
#[test]
fn a_start_must_name_either_an_inline_launch_or_an_agent() {
    let error = serde_json::from_value::<StartSessionRequest>(json!({
        "identity": {"mode": "new"},
        "cwd": "/work/project",
    }))
    .expect_err("a start with no launch configuration at all is not a start");
    assert!(error.to_string().contains("claude"), "{error}");
    assert!(error.to_string().contains("agent"), "{error}");
}

/// An inline start emits exactly the bytes it emitted before the agent resource
/// existed.
#[test]
fn an_inline_start_is_byte_identical_to_the_release_before_agents() {
    let value = serde_json::to_value(start_request()).expect("serializes");
    let object = value.as_object().expect("a start is an object");
    assert!(
        !object.contains_key("agent"),
        "an inline start must not mention the field a pre-agent daemon would refuse"
    );
    assert_eq!(
        object.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "identity",
            "cwd",
            "claude",
            "environment",
            "auth_policy",
            "terminal",
            "lifecycle",
            "retention",
            "compatibility",
            // `start_request` selects the minified cell, so this fixture is
            // also the one that proves a non-default `cell` still serializes.
            "cell",
        ]),
        "the inline start's wire field set changed"
    );
}

/// An agent start emits the agent and none of the nine paths the agent supplies.
///
/// The companion of [`an_inline_start_is_byte_identical_to_the_release_before_agents`],
/// and until now the only agent-naming request any test ever put through this
/// serializer was one it REFUSES. So the whole emitting half of the agent
/// branch -- `emit_policy == false` -- was reached by nothing: the field count
/// term that is zero exactly when an agent is named could be made to divide by
/// that zero and the suite stayed green, because no serialization ever
/// evaluated it.
///
/// The expected key set is derived rather than written twice: it is the inline
/// start's own emitted keys, minus every path `agent_supplied_start_paths`
/// names, plus `agent`.
#[test]
fn an_agent_start_emits_the_agent_and_omits_every_path_the_agent_supplies() {
    let mut typed = start_request();
    typed.claude = None;
    typed.environment.set.clear();
    typed.environment.unset.clear();
    typed.auth_policy = AuthPolicy::default();
    typed.terminal = TerminalSpec::default();
    typed.lifecycle = LifecycleMode::default();
    typed.retention = RetentionPolicy::default();
    typed.compatibility = CompatibilityPolicy::default();
    typed.cell = SessionCell::default();
    typed.config_isolation = None;
    typed.agent = Some(AgentRef {
        agent_id: AGENT_ID,
        version: AgentVersion::new(3).expect("3 is a version"),
    });
    validate_v1_serializable(&typed)
        .expect("an agent start carrying none of the agent-supplied paths is serializable");

    let inline_value = serde_json::to_value(start_request()).expect("the inline start serializes");
    let inline: BTreeSet<&str> = inline_value
        .as_object()
        .expect("a start is an object")
        .keys()
        .map(String::as_str)
        .collect();
    let agent_supplied: BTreeSet<&str> = agent_supplied_start_paths()
        .iter()
        .map(|path| path.split_once('.').map_or(*path, |(head, _)| head))
        .collect();
    let mut expected: BTreeSet<&str> = inline.difference(&agent_supplied).copied().collect();
    // `environment` is the one head an agent-supplied path shares with a field
    // the caller keeps: `environment.snapshot` is a fact about the calling
    // process and survives beside an agent.
    expected.insert("environment");
    expected.insert("agent");

    let value = serde_json::to_value(&typed).expect("an agent start serializes");
    assert_eq!(
        value
            .as_object()
            .expect("a start is an object")
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        expected,
        "the agent start's wire field set changed: {value}"
    );
}

/// `AgentVersion` starts at 1, and the newtype owns that domain check so no
/// caller has to remember one.
#[test]
fn an_agent_version_is_never_zero_on_the_wire() {
    let error = serde_json::from_value::<AgentRef>(json!({
        "agent_id": AGENT_ID.hyphenated().to_string(),
        "version": 0,
    }))
    .expect_err("there is no version 0");
    assert!(error.to_string().contains("starts at 1"), "{error}");

    assert_eq!(
        serde_json::to_value(AgentVersion::FIRST).expect("serializes"),
        json!(1)
    );
    assert_eq!(AgentVersion::FIRST.next().get(), 2);
}

/// An agent reference names an exact version and there is no "latest".
#[test]
fn an_agent_reference_has_no_omit_for_latest_shorthand() {
    let error = serde_json::from_value::<AgentRef>(json!({
        "agent_id": AGENT_ID.hyphenated().to_string(),
    }))
    .expect_err("`version` is required: `latest at start time` is the impurity spec 4.4 forbids");
    assert!(error.to_string().contains("version"), "{error}");
}

/// Path B still names no resource, and refuses an agent BY NAME rather than by
/// ignoring it.
#[test]
fn path_b_refuses_an_agent_reference_by_name() {
    let error = serde_json::from_value::<RunStatelessRequest>(json!({
        "model": "sonnet",
        "prompt": "hello",
        "agent_id": AGENT_ID.hyphenated().to_string(),
    }))
    .expect_err("an agent id is a name a caller can write, and two callers can write the same one");
    assert!(error.to_string().contains("agent_id"), "{error}");
}

/// The four agent methods and their four distinct results are appended last.
#[test]
fn the_agent_variants_are_appended_last_and_each_answers_its_own_result() {
    let spec = fully_populated_agent_spec();
    let cases: Vec<(Request, &str)> = vec![
        (
            Request::CreateAgent(CreateAgentRequest { spec: spec.clone() }),
            "create_agent",
        ),
        (
            Request::GetAgent(GetAgentRequest {
                agent_id: AGENT_ID,
                version: None,
            }),
            "get_agent",
        ),
        (Request::ListAgents(ListAgentsRequest {}), "list_agents"),
        (
            Request::UpdateAgent(UpdateAgentRequest {
                agent_id: AGENT_ID,
                expected_version: AgentVersion::FIRST,
                spec: spec.clone(),
            }),
            "update_agent",
        ),
    ];
    for (request, method) in cases {
        let envelope = RequestEnvelope::new(REQUEST_ID, request);
        let value = serde_json::to_value(&envelope).expect("serializes");
        assert_eq!(value["method"], json!(method));
        let decoded: RequestEnvelope = serde_json::from_value(value).expect("round-trips");
        assert_eq!(decoded, envelope);
    }

    let descriptor = AgentDescriptor {
        agent_id: AGENT_ID,
        version: AgentVersion::FIRST,
        config_digest: "0".repeat(64),
        // OPAQUE on the response, and decoded with the strict type by
        // `typed_spec` -- which the assertion below exercises, so the round
        // trip a caller actually makes is what is pinned here.
        spec: serde_json::to_value(&spec).expect("a spec is representable"),
        created_at_ms: 10,
        updated_at_ms: 10,
    };
    assert_eq!(descriptor.typed_spec().expect("decodes strictly"), spec);
    let results = [
        (
            ResponseResult::AgentCreated(Box::new(descriptor.clone())),
            "agent_created",
        ),
        (ResponseResult::Agent(Box::new(descriptor.clone())), "agent"),
        (ResponseResult::AgentList(Box::default()), "agent_list"),
        (
            ResponseResult::AgentUpdated(Box::new(descriptor)),
            "agent_updated",
        ),
    ];
    let mut tags = BTreeSet::new();
    for (result, tag) in results {
        let value = serde_json::to_value(ResponseEnvelope::success(REQUEST_ID, result))
            .expect("serializes");
        assert_eq!(value["result"]["type"], json!(tag));
        assert!(tags.insert(tag), "two methods answer {tag}");
    }
    assert_eq!(tags.len(), 4);
}

/// An `AgentSpec` is strict wherever it appears, including inside a result.
///
/// It is a request body, and `deny_unknown_fields` is what stops
/// `{"auth_polcy": "inherit"}` from being accepted as `subscription`. A type
/// cannot be strict on the way in and lax on the way out without two decoders
/// for one type, and no client in any of the three languages has one.
#[test]
fn an_agent_spec_refuses_an_unknown_field_by_name() {
    let mut value = serde_json::to_value(fully_populated_agent_spec()).expect("serializes");
    value["auth_polcy"] = json!("inherit");
    let error = serde_json::from_value::<AgentSpec>(value)
        .expect_err("a misspelled policy field must never change execution silently");
    assert!(error.to_string().contains("auth_polcy"), "{error}");
}

// ---------------------------------------------------------------------------
// Bounds and predicates found by cargo-mutants
//
// Every test below closes a mutant that survived the first full mutation run of
// `crates/protocol/src/v1.rs` (227 mutants). They share one shape: a limit
// whose message states an exact number that no case ever sat exactly on, or a
// predicate whose two branches were never both taken.
// ---------------------------------------------------------------------------

/// The safe-integer bound is inclusive on both ends of the opaque-JSON gate.
///
/// SURVIVING MUTANT CLOSED: `v1.rs:298 > -> >=`, the float arm. Every existing
/// case sat far outside the range, which cannot tell `>` from `>=` -- so
/// `MAX_SAFE_JSON_INTEGER` itself, the value the message names, had never been
/// shown to survive the gate it is the boundary of. Confirmed by re-running:
/// `>= ` at `:298` is MISSED in the run before this test and CAUGHT in the run
/// after it.
///
/// **AND ONE MUTANT THIS DOES NOT CLOSE, because nothing can.** This comment
/// also claimed `v1.rs:286 > -> >=`, the `as_u64` arm, and the re-run refused
/// that claim: `:287` is MISSED in both runs. It is an EQUIVALENT MUTANT, and
/// the argument is short enough to check. Line 287 is only reached when
/// `as_i64()` returned `None`, which for a JSON integer means the value exceeds
/// `i64::MAX` = 9_223_372_036_854_775_807. `MAX_SAFE_JSON_INTEGER` is
/// 9_007_199_254_740_991, which is smaller. So every value that reaches line
/// 287 is ALREADY strictly greater than the bound, `>` and `>=` agree on all of
/// them, and no input can distinguish the two. `u64::MAX` below exercises the
/// arm; it cannot, and no value can, kill that mutant. It is triaged as
/// unreachable-by-design in `docs/current-state.md` §9.23 rather than counted
/// as a gap.
///
/// Driven through `ErrorBody::details`, which carries `safe_json_value` and is
/// therefore the gate, in BOTH directions: the same validator runs on the way
/// out and on the way in, and a bound that held on one and not the other is a
/// message a daemon can emit and no client can read.
#[test]
fn the_opaque_json_integer_bound_admits_the_exact_edge_and_refuses_one_past() {
    fn round_trip(details: Value) -> Result<Value, String> {
        let body = ErrorBody {
            code: ErrorCode::InvalidConfig,
            message: "m".into(),
            details,
            retryable: false,
        };
        let text = serde_json::to_string(&body).map_err(|error| error.to_string())?;
        let decoded: ErrorBody = serde_json::from_str(&text).map_err(|error| error.to_string())?;
        Ok(decoded.details)
    }

    // The negative edge is DERIVED from the public one rather than restated:
    // `MIN_SAFE_JSON_INTEGER` is private, and a copy of a private constant in a
    // test keeps passing after the constant moves.
    let min_safe = -(MAX_SAFE_JSON_INTEGER as i64);

    // The exact edges, both signs, admitted.
    for edge in [json!(MAX_SAFE_JSON_INTEGER), json!(min_safe), json!(0)] {
        assert_eq!(
            round_trip(json!({ "n": edge })).expect("the exact safe-integer edge is representable"),
            json!({ "n": edge })
        );
    }
    // One past, both signs, refused -- and refused by the message that names
    // the range rather than by serde's own overflow.
    for past in [json!(MAX_SAFE_JSON_INTEGER + 1), json!(min_safe - 1)] {
        let error = round_trip(json!({ "n": past })).expect_err("one past the edge is refused");
        assert!(
            error.contains("outside the signed safe-integer range"),
            "{past}: {error}"
        );
    }
    // The `u64` arm specifically: a value above `i64::MAX` reaches
    // `as_u64` rather than `as_i64`, so the two bounds are two lines.
    let error = round_trip(json!({ "n": u64::MAX })).expect_err("u64::MAX is refused");
    assert!(
        error.contains("outside the signed safe-integer range"),
        "{error}"
    );
}

/// A finite non-integer JSON number survives the opaque gate.
///
/// SURVIVING MUTANT CLOSED: `v1.rs:295 delete !`, which inverts
/// `if !value.is_finite()` into `if value.is_finite()` -- refusing every float
/// pmux carries and admitting the ones it exists to stop. It survived because
/// no case anywhere put a non-integer number in an opaque field, so the whole
/// float arm of `validate_opaque_json` was unexecuted.
#[test]
fn a_finite_fractional_number_survives_the_opaque_json_gate() {
    let body = ErrorBody {
        code: ErrorCode::InvalidConfig,
        message: "m".into(),
        details: json!({"ratio": 0.5, "negative": -12.25, "exponent": 1.0e10}),
        retryable: false,
    };
    let text = serde_json::to_string(&body).expect("a finite float is representable");
    let decoded: ErrorBody = serde_json::from_str(&text).expect("and it decodes again");
    assert_eq!(decoded.details["ratio"], json!(0.5));
    assert_eq!(decoded.details["negative"], json!(-12.25));
    // ...while a float whose integral value is outside the safe range is still
    // refused, so the arm is a filter and not a hole.
    let oversized = ErrorBody {
        code: ErrorCode::InvalidConfig,
        message: "m".into(),
        details: json!({"huge": 9.3e18}),
        retryable: false,
    };
    let error = serde_json::to_string(&oversized)
        .expect_err("an out-of-range integral float is refused")
        .to_string();
    assert!(
        error.contains("outside the signed safe-integer range"),
        "{error}"
    );

    // The float arm's OWN boundary, which is a different line from the integer
    // one and is only reachable through a JSON number carrying a decimal point:
    // `as_i64` answers `None` for it, so both integer arms are skipped. `2^53-1`
    // is exactly representable as `f64`, so this sits ON the bound.
    #[allow(clippy::cast_precision_loss)]
    let edge = MAX_SAFE_JSON_INTEGER as f64;
    assert_eq!(edge.fract(), 0.0, "the fixture must reach the integral arm");
    let body = ErrorBody {
        code: ErrorCode::InvalidConfig,
        message: "m".into(),
        details: json!({ "edge": edge, "negative": -edge }),
        retryable: false,
    };
    serde_json::to_string(&body).expect("the exact float edge is representable, on both signs");
    let past = ErrorBody {
        code: ErrorCode::InvalidConfig,
        message: "m".into(),
        details: json!({ "past": edge + 2.0 }),
        retryable: false,
    };
    let error = serde_json::to_string(&past)
        .expect_err("one representable step past the float edge is refused")
        .to_string();
    assert!(
        error.contains("outside the signed safe-integer range"),
        "{error}"
    );
}

/// `details` is omitted when null and present when it is not.
///
/// SURVIVING MUTANT CLOSED: `v1.rs:2746 value_is_null -> false`, which makes
/// every error body carry `"details": null`. It survived because nothing
/// asserted the ABSENCE of the key -- only that a present one round-trips.
#[test]
fn an_error_body_omits_details_exactly_when_they_are_null() {
    let bare = serde_json::to_value(ErrorBody {
        code: ErrorCode::InvalidConfig,
        message: "m".into(),
        details: Value::Null,
        retryable: false,
    })
    .expect("serializes");
    assert!(
        bare.get("details").is_none(),
        "a null `details` must not reach the wire at all: {bare}"
    );
    let carried = serde_json::to_value(ErrorBody {
        code: ErrorCode::InvalidConfig,
        message: "m".into(),
        details: json!({"recommendation": "fix it"}),
        retryable: false,
    })
    .expect("serializes");
    assert_eq!(carried["details"], json!({"recommendation": "fix it"}));
}

/// The hybrid hook timeout a caller omits is the documented default, and it is
/// not zero.
///
/// SURVIVING MUTANT CLOSED: `v1.rs:2181 default_hook_timeout_ms -> 0`. Zero is
/// a value `agent::validate_agent_spec` refuses by name -- "hybrid hook timeout
/// must be greater than zero" -- so the mutant turns every omitted timeout into
/// a stored agent nobody can create, and every test passed.
#[test]
fn an_omitted_hybrid_hook_timeout_deserializes_to_a_usable_default() {
    let decoded: LifecycleMode =
        serde_json::from_value(json!({"mode": "hybrid"})).expect("the timeout is optional");
    let LifecycleMode::Hybrid { hook_timeout_ms } = decoded else {
        panic!("`hybrid` must decode to the hybrid mode")
    };
    assert!(
        hook_timeout_ms > 0,
        "an omitted hook timeout must be usable, not a value every validator refuses"
    );
    // And it is the value the SHARED conformance corpus carries, read out of
    // that corpus rather than restated here.
    //
    // The default is a cross-language wire contract -- three clients must agree
    // on what an omitted `hook_timeout_ms` means -- and not a private Rust
    // constant, so a copy of the number in this file would be a fourth
    // spelling. Not one golden case omits the field, so the corpus pins the
    // EXPLICIT form and this test is what ties the omitted form to it.
    const GOLDEN: &str = include_str!("../../../tests/conformance/v1/golden.json");
    let carried: BTreeSet<u64> = GOLDEN
        .match_indices("\"hook_timeout_ms\"")
        .map(|(at, _)| {
            GOLDEN[at..]
                .split_once(':')
                .expect("a JSON member has a colon")
                .1
                .trim_start()
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .expect("the corpus carries an integer timeout")
        })
        .collect();
    assert_eq!(
        carried.len(),
        1,
        "the corpus must agree with itself about the hybrid timeout: {carried:?}"
    );
    assert_eq!(
        hook_timeout_ms,
        *carried.iter().next().unwrap(),
        "an omitted hybrid timeout must decode to the value every client's corpus carries"
    );
    // ...and "omitted" and "written out" are then the same session.
    assert_eq!(
        serde_json::from_value::<LifecycleMode>(
            json!({"mode": "hybrid", "hook_timeout_ms": hook_timeout_ms})
        )
        .expect("an explicit timeout decodes"),
        LifecycleMode::Hybrid { hook_timeout_ms }
    );
}

/// The session-layer health counts are keyed on the outcome they name.
///
/// SURVIVING MUTANT CLOSED: `v1.rs:1035 == -> !=` in `HealthLayer::for_sessions`,
/// which inverts every count -- `pass` reports the sessions that did not pass.
/// It survived because no case gave the producer a mixed set: with sessions
/// that all share one outcome, `==` and `!=` differ only in which count is
/// zero, and nothing read the counts.
#[test]
fn the_session_health_counts_name_the_outcome_they_count() {
    fn probe(outcome: ProbeOutcome) -> SessionProbe {
        SessionProbe {
            session_id: SESSION_ID,
            generation_id: GENERATION_ID,
            outcome,
            finding: SessionFinding::TerminalPresent,
            state: None,
            private_terminal_present: Some(true),
        }
    }

    // Three outcomes, three different counts, so no permutation of the three
    // labels can satisfy this by accident.
    let layer = HealthLayer::for_sessions(&[
        probe(ProbeOutcome::Pass),
        probe(ProbeOutcome::Pass),
        probe(ProbeOutcome::Pass),
        probe(ProbeOutcome::Unproven),
        probe(ProbeOutcome::Unproven),
        probe(ProbeOutcome::Fail),
    ]);
    assert_eq!(layer.evidence["registered"], json!(6));
    assert_eq!(layer.evidence["pass"], json!(3));
    assert_eq!(layer.evidence["unproven"], json!(2));
    assert_eq!(layer.evidence["fail"], json!(1));
}

/// A decode refusal pmux wrote is forwarded verbatim, and one it did not is
/// not forwarded at all.
///
/// SURVIVING MUTANTS CLOSED: `v1.rs:1463` replaced with `Some("xyzzy")` and
/// `v1.rs:1464 + -> *` / `-> -`, which are the marker-length arithmetic that
/// decides where the forwarded span starts. Every existing case checked only
/// that SOMETHING came back, which a constant and an off-by-a-multiple both
/// satisfy.
#[test]
fn a_forwarded_decode_refusal_is_the_exact_span_pmux_composed() {
    // The one refusal shape that carries the marker: an agent-named start that
    // also carries a path the agent supplies.
    let mut value = serde_json::to_value(start_request()).expect("serializes");
    value["agent"] = json!({
        "agent_id": Uuid::from_u128(9).hyphenated().to_string(),
        "version": 1,
    });
    let error = serde_json::from_value::<StartSessionRequest>(value)
        .expect_err("a start may not name an agent and its paths at once");
    let forwarded =
        caller_actionable_decode_refusal(&error).expect("pmux composed this refusal itself");
    let whole = error.to_string();
    assert!(
        whole.contains(&forwarded),
        "the forwarded span must be a substring of the refusal: {forwarded:?} vs {whole:?}"
    );
    assert!(
        !forwarded.contains(" at line "),
        "serde's own position suffix must not be forwarded: {forwarded:?}"
    );
    // The span starts AFTER the marker, so the marker itself is never in it.
    // This is the assertion the offset arithmetic answers to: a `find(..) * len`
    // instead of `+ len` lands on zero and forwards the marker with it.
    assert!(
        !forwarded.contains(DECODE_REFUSAL_MARKER),
        "the marker is a frame and not part of the message: {forwarded:?}"
    );
    let after_marker = whole
        .split_once(DECODE_REFUSAL_MARKER)
        .expect("the refusal carries the marker")
        .1;
    assert_eq!(
        forwarded,
        after_marker
            .rsplit_once(" at line ")
            .map_or(after_marker, |(head, _)| head),
        "the forwarded span must be exactly the text between the marker and serde's position"
    );
    assert!(!forwarded.is_empty(), "an empty span forwards nothing");

    // `caller_actionable_decode_refusal` also trims serde's ` at line N column M`
    // suffix. That branch is NOT exercised here and is said so plainly rather
    // than claimed: serde_json attaches no position to a custom error raised
    // from a whole-type `Deserialize` impl, and every refusal carrying
    // `DECODE_REFUSAL_MARKER` is raised from one -- so no input this crate can
    // construct reaches the `rsplit_once` arm.
    // ...and a refusal pmux did NOT write carries no marker and is not
    // forwarded, which is what keeps a caller's own bytes out of the reply.
    let foreign = serde_json::from_str::<StartSessionRequest>("{").expect_err("truncated");
    assert_eq!(caller_actionable_decode_refusal(&foreign), None);
}

/// The frame accumulator reports how many bytes it still needs, exactly.
///
/// SURVIVING MUTANTS CLOSED: `v1.rs:110 - -> +`, `v1.rs:111 - -> +` and
/// `- -> /`, and `v1.rs:108 - -> +`. `remaining_bytes` is a public accessor and
/// no test in any package that `cargo test -p pseudomux-protocol` builds had
/// ever called it; `push`'s own subtraction is exercised only by a fragment
/// that stops part-way through a payload.
///
/// **The payload subtraction needs BOTH conditions at once, and the first draft
/// of this test had only one of them.** `bytes.len() - filled` becomes
/// `bytes.len() + filled` under the mutant, so the two agree whenever
/// `filled == 0` -- and they also agree whenever the input available is at most
/// what the payload still needs, because `min` picks the input either way.
/// Killing it requires a push that is BOTH resuming a part-filled payload
/// (`filled > 0`) AND carrying more bytes than that payload has left. The
/// "two frames in one push" case below starts its payload at `filled == 0`, and
/// the split-payload case above finishes with exactly the bytes it needs, so
/// this mutant SURVIVED both of them and the re-run said so. The third case is
/// the one that closes it.
#[test]
fn the_frame_accumulator_reports_exactly_what_it_still_needs() {
    let payload = b"0123456789";
    let mut framed = (payload.len() as u32).to_be_bytes().to_vec();
    framed.extend_from_slice(payload);

    let mut accumulator = NativeFrameAccumulator::new();
    assert_eq!(
        accumulator.remaining_bytes(),
        4,
        "an empty header needs four"
    );

    // One byte of the header at a time: the count must fall by exactly one.
    for (index, byte) in framed[..4].iter().enumerate() {
        let (consumed, progress) = accumulator.push(&[*byte]);
        assert_eq!(consumed, 1);
        assert!(matches!(progress, NativeFrameProgress::NeedMore));
        if index < 3 {
            assert_eq!(accumulator.remaining_bytes(), 3 - index);
        }
    }
    // The header is complete, so the accumulator now needs the whole payload.
    assert_eq!(accumulator.remaining_bytes(), payload.len());

    // Half the payload, then the rest.
    let (consumed, progress) = accumulator.push(&framed[4..9]);
    assert_eq!(consumed, 5);
    assert!(matches!(progress, NativeFrameProgress::NeedMore));
    assert_eq!(accumulator.remaining_bytes(), payload.len() - 5);

    let (consumed, progress) = accumulator.push(&framed[9..]);
    assert_eq!(consumed, framed.len() - 9);
    match progress {
        NativeFrameProgress::Payload(bytes) => assert_eq!(bytes, payload),
        other => panic!("the frame must complete: {other:?}"),
    }
    assert_eq!(
        accumulator.remaining_bytes(),
        4,
        "and it resets to a header"
    );

    // TWO frames in ONE push. This is what makes the payload subtraction
    // load-bearing: with more input available than the frame needs, `copied`
    // must be exactly what is LEFT of the payload, and a `+` there reads past
    // the frame boundary into the next frame's bytes.
    let mut two = framed.clone();
    two.extend_from_slice(&framed);
    let mut accumulator = NativeFrameAccumulator::new();
    let (consumed, progress) = accumulator.push(&two);
    assert_eq!(
        consumed,
        framed.len(),
        "a push must stop at the first frame boundary and never consume the next frame"
    );
    match progress {
        NativeFrameProgress::Payload(bytes) => assert_eq!(bytes, payload),
        other => panic!("the first frame must complete: {other:?}"),
    }
    let (consumed, progress) = accumulator.push(&two[framed.len()..]);
    assert_eq!(consumed, framed.len());
    assert!(matches!(progress, NativeFrameProgress::Payload(_)));

    // A PART-FILLED payload finished by a push that overshoots the frame
    // boundary. This is the one shape that tells `bytes.len() - filled` from
    // `bytes.len() + filled`: `filled` is non-zero, so the two differ, and the
    // input is longer than the payload has left, so `min` cannot hide the
    // difference. Under the mutant `copied` becomes 11 for a 10-byte buffer
    // holding 4 already, and the copy panics reading past the frame.
    let mut accumulator = NativeFrameAccumulator::new();
    let (consumed, progress) = accumulator.push(&framed[..8]);
    assert_eq!(consumed, 8, "the header and four payload bytes are taken");
    assert!(matches!(progress, NativeFrameProgress::NeedMore));
    assert_eq!(
        accumulator.remaining_bytes(),
        payload.len() - 4,
        "four of ten payload bytes are in"
    );
    // The rest of frame one, followed by the whole of frame two.
    let rest: Vec<u8> = framed[8..].iter().chain(framed.iter()).copied().collect();
    assert!(
        rest.len() > accumulator.remaining_bytes(),
        "the fixture must overshoot the frame boundary or it proves nothing"
    );
    let (consumed, progress) = accumulator.push(&rest);
    assert_eq!(
        consumed,
        payload.len() - 4,
        "a resumed payload must take exactly what it still needs and stop"
    );
    match progress {
        NativeFrameProgress::Payload(bytes) => assert_eq!(bytes, payload),
        other => panic!("the resumed frame must complete: {other:?}"),
    }
}

/// The cell's wire spelling is the one the enum serializes as.
///
/// SURVIVING MUTANTS CLOSED: `v1.rs:1891 SessionCell::as_str -> ""` and
/// `-> "xyzzy"`, and `v1.rs:1900` its `Display`. The accessor exists so a
/// message can name a cell; nothing compared what it names against the value
/// serde puts on the wire, so both could have said anything.
#[test]
fn the_cell_accessor_and_its_display_agree_with_the_wire_spelling() {
    for cell in [SessionCell::Full, SessionCell::Minified] {
        let wire = serde_json::to_value(cell).expect("serializes");
        let wire = wire.as_str().expect("a cell is a wire string");
        assert_eq!(cell.as_str(), wire, "the accessor must name the wire form");
        assert_eq!(cell.to_string(), wire, "and so must Display");
        assert_eq!(
            serde_json::from_value::<SessionCell>(json!(cell.as_str())).expect("decodes"),
            cell,
            "and the name it prints must decode back to it"
        );
    }
    assert_ne!(
        SessionCell::Full.as_str(),
        SessionCell::Minified.as_str(),
        "two cells that printed the same name would make every message ambiguous"
    );
}

/// A generation id prints as the canonical hyphenated UUID it decodes from.
///
/// SURVIVING MUTANT CLOSED: `v1.rs:452` -- the whole `Display` body replaced
/// with `Ok(())`, which makes every diagnostic that names a generation print an
/// empty string.
#[test]
fn a_generation_id_prints_the_uuid_it_carries() {
    let printed = GENERATION_ID.to_string();
    assert_eq!(printed, GENERATION_ID.as_uuid().hyphenated().to_string());
    assert_eq!(
        printed.len(),
        36,
        "a canonical UUID is 36 characters: {printed}"
    );
    assert_ne!(
        printed,
        SessionGenerationId::from_u128(5).to_string(),
        "two generations that printed alike would make a stale-generation report unreadable"
    );
}

/// `sidechain_rows` is omitted when zero and present when it is not.
///
/// SURVIVING MUTANT CLOSED: `v1.rs:204 is_zero_u64 -> true`, which drops the
/// field from every turn result. On a cell launched with its tool surface
/// denied, a counted sidechain row is the ONLY evidence the isolation claim was
/// broken when the subagent's own calls reported no tokens -- and nothing
/// anywhere asserted the field reaches the wire at all.
#[test]
fn a_nonzero_sidechain_row_count_reaches_the_wire_and_a_zero_one_does_not() {
    let mut result = turn_result();
    result.sidechain_rows = 0;
    let bare = serde_json::to_value(&result).expect("serializes");
    assert!(
        bare.get("sidechain_rows").is_none(),
        "the common case must stay byte-identical to every existing golden: {bare}"
    );

    result.sidechain_rows = 3;
    let carried = serde_json::to_value(&result).expect("serializes");
    assert_eq!(carried["sidechain_rows"], json!(3));
    assert_eq!(
        serde_json::from_value::<TurnResult>(carried)
            .expect("decodes")
            .sidechain_rows,
        3
    );
}

/// The retained range's upper edge is inclusive: `oldest_available` may equal
/// the next sequence and may not exceed it.
///
/// SURVIVING MUTANT CLOSED: `v1.rs:3611 > -> >=` in `EventBatch`'s decoder.
/// `oldest_available == expected_next` is the shape a caller sees when EVERY
/// retained event was dropped and the very next one has not arrived — a real
/// gap, and one the mutant refuses as malformed. Every existing case sat well
/// inside the range, which cannot tell `>` from `>=`.
#[test]
fn a_replay_gap_may_retain_from_exactly_the_next_sequence_and_no_further() {
    let snapshot = session_snapshot();
    let expected_next = snapshot.last_sequence + 1;

    let gap = |oldest_available: u64| {
        serde_json::to_value(EventBatch {
            events: vec![],
            next_sequence: expected_next,
            replay_gap: Some(ReplayGap {
                requested_after: 3,
                oldest_available,
                next_sequence: expected_next,
                snapshot: Box::new(session_snapshot()),
            }),
        })
        .expect("serializes")
    };

    // The exact edge: every retained event is gone and the next one has not
    // arrived. Admitted.
    serde_json::from_value::<EventBatch>(gap(expected_next))
        .expect("a gap that retains from exactly the next sequence is a real gap");
    // One past it claims to retain an event that does not exist yet. Refused.
    let error = serde_json::from_value::<EventBatch>(gap(expected_next + 1))
        .expect_err("a retained range past the next sequence proves nothing")
        .to_string();
    assert!(
        error.contains("does not prove that requested events were lost"),
        "{error}"
    );
}
