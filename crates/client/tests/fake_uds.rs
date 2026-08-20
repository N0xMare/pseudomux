#![cfg(unix)]

use std::path::Path;
use std::time::Duration;

use futures_util::StreamExt;
use pseudomux_client::{
    ClientError, ClientOptions, EventStreamItem, EventStreamOptions, PmuxClient,
};
use pseudomux_protocol::v1::{
    AttachSessionRequest, AuthPolicy, ClaudeLaunchConfig, ClosePolicy, CompatibilityPolicy,
    CompatibilityReport, EffortLevel, EnvironmentSpec, ErrorBody, ErrorCode, EventBatch,
    EventEnvelope, EventPayload, Heartbeat, InputTransport, LifecycleMode, PROTOCOL_VERSION,
    Request, RequestEnvelope, ResponseEnvelope, ResponseResult, RetentionPolicy, RunOnceRequest,
    RunStatelessRequest, SessionCell, SessionGenerationId, SessionHandle, SessionIdentity,
    SessionSnapshot, SessionState, StartSessionRequest, StatelessResult, SystemPromptPolicy,
    TerminalProfile, TerminalSpec, TurnLeasePolicy, TurnOutcome, TurnRequest, TurnSummary,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use uuid::Uuid;

const SESSION_ID: Uuid = Uuid::from_u128(22);
const TURN_ID: Uuid = Uuid::from_u128(33);
const GENERATION_ID: SessionGenerationId = SessionGenerationId::from_u128(44);
const OTHER_ID: Uuid = Uuid::from_u128(9_999);

#[derive(Clone, Deserialize)]
struct SharedErrorCase {
    id: String,
    valid: bool,
    body: Value,
}

#[derive(Clone, Deserialize)]
struct SharedReplayCase {
    id: String,
    valid: bool,
    requested_after: u64,
    oldest_available: u64,
    snapshot_last: u64,
    gap_next: u64,
    batch_next: u64,
    event_sequences: Vec<u64>,
}

#[derive(Clone, Deserialize)]
struct SharedNumericCase {
    id: String,
    literal: String,
    protocol_owned_valid: bool,
    opaque_json_valid: bool,
}

#[derive(Deserialize)]
struct SharedCases {
    error_bodies: Vec<SharedErrorCase>,
    replay_batches: Vec<SharedReplayCase>,
    numeric_boundaries: Vec<SharedNumericCase>,
}

fn shared_cases() -> SharedCases {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/conformance/v1/cases.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn socket_listener() -> (TempDir, std::path::PathBuf, UnixListener) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pmuxd.sock");
    let listener = UnixListener::bind(&path).unwrap();
    (directory, path, listener)
}

async fn read_request(stream: &mut UnixStream) -> RequestEnvelope {
    let mut header = [0_u8; 4];
    stream.read_exact(&mut header).await.unwrap();
    let length = u32::from_be_bytes(header) as usize;
    let mut payload = vec![0; length];
    stream.read_exact(&mut payload).await.unwrap();
    serde_json::from_slice(&payload).unwrap()
}

async fn write_json_frame(stream: &mut UnixStream, value: &impl Serialize) {
    let payload = serde_json::to_vec(value).unwrap();
    write_raw_frame(stream, &payload).await;
}

async fn write_raw_frame(stream: &mut UnixStream, payload: &[u8]) {
    stream
        .write_all(&(payload.len() as u32).to_be_bytes())
        .await
        .unwrap();
    stream.write_all(payload).await.unwrap();
    stream.flush().await.unwrap();
}

fn start_request() -> StartSessionRequest {
    StartSessionRequest {
        identity: SessionIdentity::New {
            session_id: Some(SESSION_ID),
        },
        cwd: "/work/project".into(),
        agent: None,
        claude: Some(ClaudeLaunchConfig {
            executable: "/usr/local/bin/claude".into(),
            model: Some("sonnet".into()),
            effort: Some(EffortLevel::High),
            permission_mode: None,
            allowed_tools: vec![],
            denied_tools: vec![],
            settings: vec![],
            mcp_configs: vec![],
            plugin_dirs: vec![],
            system_prompt: SystemPromptPolicy::Default,
            extra_args: vec![],
        }),
        environment: EnvironmentSpec::default(),
        auth_policy: AuthPolicy::Subscription,
        config_isolation: None,
        terminal: TerminalSpec::default(),
        lifecycle: LifecycleMode::Transcript,
        retention: RetentionPolicy::OneShot,
        compatibility: CompatibilityPolicy::RequireTested,
        cell: SessionCell::Full,
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

fn snapshot(last_sequence: u64) -> SessionSnapshot {
    SessionSnapshot {
        session_id: SESSION_ID,
        generation_id: GENERATION_ID,
        transcript_session_id: SESSION_ID,
        cell: pseudomux_protocol::v1::SessionCell::Full,
        state: SessionState::Ready,
        cwd: "/work/project".into(),
        active_turn_id: None,
        claude_version: Some("2.1.207".into()),
        compatibility: compatibility_report(),
        created_at_ms: 1,
        updated_at_ms: 2,
        idle_deadline_ms: None,
        resumable: true,
        last_sequence,
        last_turn: Some(TurnSummary {
            turn_id: TURN_ID,
            outcome: TurnOutcome::Completed,
            completed_at_ms: 2,
            final_sequence: last_sequence,
        }),
        needs_input: None,
        agent: None,
    }
}

fn heartbeat(sequence: u64) -> EventEnvelope {
    EventEnvelope::new(
        SESSION_ID,
        GENERATION_ID,
        None,
        sequence,
        1_000 + sequence,
        EventPayload::Heartbeat(Heartbeat {
            session_state: SessionState::Ready,
        }),
    )
}

fn stateless_result() -> Value {
    let tokens = json!({
        "input_tokens": 10,
        "output_tokens": 2,
        "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": 0
    });
    json!({
        "model": "claude-sonnet-5",
        "text": "done",
        "usage": {
            "main": tokens,
            "sidechain": {
                "input_tokens": 0,
                "output_tokens": 0,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0
            },
            "combined": tokens
        },
        "claude_version": "2.1.207"
    })
}

fn turn_result_wire(turn_id: Uuid) -> Value {
    let tokens = json!({
        "input_tokens": 1,
        "output_tokens": 1,
        "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": 0
    });
    json!({
        "session_id": SESSION_ID,
        "generation_id": GENERATION_ID,
        "turn_id": turn_id,
        "outcome": "completed",
        "text": "done",
        "usage": {
            "main": tokens,
            "sidechain": tokens,
            "combined": tokens
        },
        "timings": {"submitted_at_ms": 1, "completed_at_ms": 2},
        "claude_version": "2.1.207",
        "compatibility": compatibility_report(),
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
    })
}

async fn accept_one(listener: &UnixListener) -> UnixStream {
    listener.accept().await.unwrap().0
}

fn assert_socket_path(client: &PmuxClient, expected: &Path) {
    assert_eq!(client.socket_path(), expected);
}

#[test]
fn client_rejects_relative_socket_paths_before_connecting() {
    let error = PmuxClient::new("relative/pmux.sock").unwrap_err();
    assert!(matches!(error, ClientError::InvalidOptions(_)));
    assert!(error.to_string().contains("absolute"));
}

#[test]
fn client_cannot_raise_the_protocol_frame_ceiling() {
    let error = PmuxClient::with_options(
        "/tmp/pmux-test.sock",
        ClientOptions {
            max_frame_bytes: pseudomux_protocol::v1::MAX_NATIVE_FRAME_BYTES + 1,
            ..ClientOptions::default()
        },
    )
    .unwrap_err();
    assert!(matches!(error, ClientError::InvalidOptions(_)));
}

#[tokio::test]
async fn typed_start_session_succeeds_over_explicit_socket() {
    let (_directory, path, listener) = socket_listener();
    let server = tokio::spawn(async move {
        let mut stream = accept_one(&listener).await;
        let request = read_request(&mut stream).await;
        assert_eq!(request.version, PROTOCOL_VERSION);
        assert!(matches!(request.request, Request::StartSession(_)));
        let response = ResponseEnvelope::success(
            request.request_id,
            ResponseResult::SessionStarted(SessionHandle {
                session_id: SESSION_ID,
                generation_id: GENERATION_ID,
                state: SessionState::Booting,
                compatibility: compatibility_report(),
                created_at_ms: 100,
                last_sequence: 0,
                agent: None,
            }),
        );
        write_json_frame(&mut stream, &response).await;
    });

    let client = PmuxClient::new(&path).unwrap();
    assert_socket_path(&client, &path);
    let handle = client.start_session(start_request()).await.unwrap();

    assert_eq!(handle.session_id, SESSION_ID);
    assert_eq!(handle.state, SessionState::Booting);
    server.await.unwrap();
}

#[tokio::test]
async fn client_accepts_additive_response_and_nested_result_fields() {
    let (_directory, path, listener) = socket_listener();
    let server = tokio::spawn(async move {
        let mut stream = accept_one(&listener).await;
        let request = read_request(&mut stream).await;
        write_json_frame(
            &mut stream,
            &serde_json::json!({
                "version": PROTOCOL_VERSION,
                "request_id": request.request_id,
                "future_envelope_field": {"opaque": true},
                "result": {
                    "type": "pong",
                    "future_result_field": 17,
                    "data": {
                        "server_version": "future-minor",
                        "protocol_version": PROTOCOL_VERSION,
                        "future_pong_field": [1, 2, 3]
                    }
                }
            }),
        )
        .await;
    });

    let pong = PmuxClient::new(path).unwrap().ping().await.unwrap();
    assert_eq!(pong.server_version, "future-minor");
    assert_eq!(pong.protocol_version, PROTOCOL_VERSION);
    server.await.unwrap();
}

#[tokio::test]
async fn client_accepts_additive_event_batch_and_payload_fields() {
    let (_directory, path, listener) = socket_listener();
    let server = tokio::spawn(async move {
        let mut stream = accept_one(&listener).await;
        let request = read_request(&mut stream).await;
        write_json_frame(
            &mut stream,
            &serde_json::json!({
                "version": PROTOCOL_VERSION,
                "request_id": request.request_id,
                "result": {
                    "type": "events",
                    "data": {
                        "events": [{
                            "schema_version": PROTOCOL_VERSION,
                            "session_id": SESSION_ID,
                            "generation_id": GENERATION_ID,
                            "sequence": 1,
                            "timestamp_ms": 1_001,
                            "future_event_envelope_field": true,
                            "event": {
                                "type": "heartbeat",
                                "future_event_wrapper_field": "opaque",
                                "data": {
                                    "session_state": "ready",
                                    "future_heartbeat_field": 1
                                }
                            }
                        }],
                        "next_sequence": 2,
                        "future_batch_field": {"opaque": true}
                    }
                }
            }),
        )
        .await;
    });

    let batch = PmuxClient::new(path)
        .unwrap()
        .subscribe_events(pseudomux_protocol::v1::SubscribeEventsRequest {
            session_id: SESSION_ID,
            generation_id: GENERATION_ID,
            after_sequence: 0,
            wait_ms: 0,
            max_events: 8,
        })
        .await
        .unwrap();
    assert_eq!(batch.events.len(), 1);
    assert_eq!(batch.events[0].sequence, 1);
    assert!(matches!(
        batch.events[0].event,
        EventPayload::Heartbeat(Heartbeat {
            session_state: SessionState::Ready
        })
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn structured_server_error_is_preserved() {
    let (_directory, path, listener) = socket_listener();
    let server = tokio::spawn(async move {
        let mut stream = accept_one(&listener).await;
        let request = read_request(&mut stream).await;
        let error = ErrorBody::new(ErrorCode::RateLimited, "quota exhausted")
            .retryable(true)
            .with_details(serde_json::json!({"resets_at_ms": 42}));
        write_json_frame(
            &mut stream,
            &ResponseEnvelope::failure(request.request_id, error),
        )
        .await;
    });

    let error = PmuxClient::new(path)
        .unwrap()
        .inspect_session(SESSION_ID, GENERATION_ID)
        .await
        .unwrap_err();
    match error {
        ClientError::Server(body) => {
            assert_eq!(body.code, ErrorCode::RateLimited);
            assert!(body.retryable);
            assert_eq!(body.details["resets_at_ms"], 42);
        }
        other => panic!("unexpected error: {other:?}"),
    }
    server.await.unwrap();
}

#[tokio::test]
async fn shared_strict_error_body_vectors_reach_the_rust_client() {
    let cases = shared_cases().error_bodies;
    let server_cases = cases.clone();
    let (_directory, path, listener) = socket_listener();
    let server = tokio::spawn(async move {
        for case in server_cases {
            let mut stream = accept_one(&listener).await;
            let request = read_request(&mut stream).await;
            write_json_frame(
                &mut stream,
                &json!({
                    "version": PROTOCOL_VERSION,
                    "request_id": request.request_id,
                    "error": case.body,
                }),
            )
            .await;
        }
    });

    let client = PmuxClient::new(path).unwrap();
    for case in cases {
        let result = client.request(Request::Ping).await;
        if case.valid {
            assert!(
                matches!(result, Err(ClientError::Server(_))),
                "shared error vector {} should be a typed server error",
                case.id
            );
        } else {
            assert!(
                matches!(result, Err(ClientError::Json(_))),
                "shared error vector {} should fail protocol decoding",
                case.id
            );
        }
    }
    server.await.unwrap();
}

#[tokio::test]
async fn shared_replay_gap_vectors_reach_the_rust_client() {
    let cases = shared_cases().replay_batches;
    let server_cases = cases.clone();
    let (_directory, path, listener) = socket_listener();
    let server = tokio::spawn(async move {
        for case in server_cases {
            let mut stream = accept_one(&listener).await;
            let request = read_request(&mut stream).await;
            let events = case
                .event_sequences
                .into_iter()
                .map(|sequence| serde_json::to_value(heartbeat(sequence)).unwrap())
                .collect::<Vec<_>>();
            let mut gap_snapshot = serde_json::to_value(snapshot(case.snapshot_last)).unwrap();
            gap_snapshot["last_sequence"] = json!(case.snapshot_last);
            write_json_frame(
                &mut stream,
                &json!({
                    "version": PROTOCOL_VERSION,
                    "request_id": request.request_id,
                    "result": {
                        "type": "events",
                        "data": {
                            "events": events,
                            "next_sequence": case.batch_next,
                            "replay_gap": {
                                "requested_after": case.requested_after,
                                "oldest_available": case.oldest_available,
                                "next_sequence": case.gap_next,
                                "snapshot": gap_snapshot,
                            }
                        }
                    }
                }),
            )
            .await;
        }
    });

    let client = PmuxClient::new(path).unwrap();
    for case in cases {
        let result = client
            .subscribe_events(pseudomux_protocol::v1::SubscribeEventsRequest {
                session_id: SESSION_ID,
                generation_id: GENERATION_ID,
                after_sequence: case.requested_after,
                wait_ms: 0,
                max_events: 8,
            })
            .await;
        if case.valid {
            let batch = result.unwrap_or_else(|error| {
                panic!("shared replay vector {} should be valid: {error}", case.id)
            });
            assert_eq!(
                batch.replay_gap.unwrap().snapshot.last_sequence,
                case.snapshot_last
            );
        } else {
            assert!(
                matches!(result, Err(ClientError::Json(_))),
                "shared replay vector {} should fail protocol decoding",
                case.id
            );
        }
    }
    server.await.unwrap();
}

#[tokio::test]
async fn shared_safe_integer_boundaries_reach_rust_client_input_validation() {
    let cases = shared_cases().numeric_boundaries;
    let server_cases = cases.clone();
    let (_directory, path, listener) = socket_listener();
    let server = tokio::spawn(async move {
        for case in server_cases {
            let mut stream = accept_one(&listener).await;
            let request = read_request(&mut stream).await;
            let response = json!({
                "version": 1,
                "request_id": request.request_id,
                "result": {
                    "type": "session_started",
                    "data": {
                        "session_id": SESSION_ID,
                        "generation_id": GENERATION_ID,
                        "state": "booting",
                        "compatibility": compatibility_report(),
                        "created_at_ms": "__PMUX_NUMBER__",
                        "last_sequence": 0
                    }
                }
            });
            let payload = serde_json::to_string(&response)
                .unwrap()
                .replace("\"__PMUX_NUMBER__\"", &case.literal);
            write_raw_frame(&mut stream, payload.as_bytes()).await;
        }
    });

    let client = PmuxClient::new(path).unwrap();
    for case in &cases {
        let result = client.start_session(start_request()).await;
        if case.protocol_owned_valid {
            assert!(result.is_ok(), "protocol numeric vector {}", case.id);
        } else {
            assert!(
                matches!(result, Err(ClientError::Json(_))),
                "protocol numeric vector {}",
                case.id
            );
        }
    }
    server.await.unwrap();

    let server_cases = cases.clone();
    let (_directory, path, listener) = socket_listener();
    let server = tokio::spawn(async move {
        for case in server_cases {
            let mut stream = accept_one(&listener).await;
            let request = read_request(&mut stream).await;
            let response = json!({
                "version": 1,
                "request_id": request.request_id,
                "error": {
                    "code": "internal",
                    "message": "synthetic",
                    "retryable": false,
                    "details": {"nested": ["__PMUX_NUMBER__"]}
                }
            });
            let payload = serde_json::to_string(&response)
                .unwrap()
                .replace("\"__PMUX_NUMBER__\"", &case.literal);
            write_raw_frame(&mut stream, payload.as_bytes()).await;
        }
    });

    let client = PmuxClient::new(path).unwrap();
    for case in cases {
        let result = client.ping().await;
        if case.opaque_json_valid {
            assert!(
                matches!(result, Err(ClientError::Server(_))),
                "opaque numeric vector {}",
                case.id
            );
        } else {
            assert!(
                matches!(result, Err(ClientError::Json(_))),
                "opaque numeric vector {}",
                case.id
            );
        }
    }
    server.await.unwrap();
}

#[tokio::test]
async fn rust_client_refuses_out_of_domain_integers_before_connecting() {
    let cases = shared_cases().numeric_boundaries;
    let protocol_cases = cases
        .iter()
        .filter_map(|case| {
            case.literal
                .parse::<u64>()
                .ok()
                .map(|value| (case.clone(), value))
        })
        .collect::<Vec<_>>();
    let valid_count = protocol_cases
        .iter()
        .filter(|(case, _)| case.protocol_owned_valid)
        .count();
    let (_directory, path, listener) = socket_listener();
    let server = tokio::spawn(async move {
        for _ in 0..valid_count {
            let mut stream = accept_one(&listener).await;
            let request = read_request(&mut stream).await;
            write_json_frame(
                &mut stream,
                &json!({
                    "version": 1,
                    "request_id": request.request_id,
                    "result": {
                        "type": "turn_accepted",
                        "data": {
                            "session_id": SESSION_ID,
                            "generation_id": GENERATION_ID,
                            "turn_id": TURN_ID,
                            "replayed": false,
                            "state": "running",
                            "next_sequence": 1
                        }
                    }
                }),
            )
            .await;
        }
    });
    let client = PmuxClient::new(path).unwrap();
    for (case, value) in protocol_cases {
        let result = client
            .run_turn(
                SESSION_ID,
                GENERATION_ID,
                TurnRequest {
                    turn_id: TURN_ID,
                    prompt: "numeric boundary".into(),
                    deadline_unix_ms: Some(value),
                    lease: TurnLeasePolicy::default(),
                },
            )
            .await;
        if case.protocol_owned_valid {
            assert!(
                result.is_ok(),
                "outbound protocol vector {}: {result:?}",
                case.id
            );
        } else {
            assert!(
                matches!(result, Err(ClientError::Json(_))),
                "outbound protocol vector {}: {result:?}",
                case.id
            );
        }
    }
    server.await.unwrap();

    let opaque_cases = cases
        .into_iter()
        .map(|case| {
            let value = serde_json::from_str::<Value>(&case.literal).unwrap();
            (case, value)
        })
        .collect::<Vec<_>>();
    let valid_count = opaque_cases
        .iter()
        .filter(|(case, _)| case.opaque_json_valid)
        .count();
    let (_directory, path, listener) = socket_listener();
    let server = tokio::spawn(async move {
        for _ in 0..valid_count {
            let mut stream = accept_one(&listener).await;
            let request = read_request(&mut stream).await;
            write_json_frame(
                &mut stream,
                &json!({
                    "version": 1,
                    "request_id": request.request_id,
                    "result": {
                        "type": "session_started",
                        "data": {
                            "session_id": SESSION_ID,
                            "generation_id": GENERATION_ID,
                            "state": "booting",
                            "compatibility": compatibility_report(),
                            "created_at_ms": 1,
                            "last_sequence": 0
                        }
                    }
                }),
            )
            .await;
        }
    });
    let client = PmuxClient::new(path).unwrap();
    for (case, value) in opaque_cases {
        let mut request = start_request();
        request.claude.as_mut().expect("inline launch").settings =
            vec![pseudomux_protocol::v1::ConfigSource::Inline {
                document: json!({"nested": [value]}),
            }];
        let result = client.start_session(request).await;
        if case.opaque_json_valid {
            assert!(
                result.is_ok(),
                "outbound opaque vector {}: {result:?}",
                case.id
            );
        } else {
            assert!(
                matches!(result, Err(ClientError::Json(_))),
                "outbound opaque vector {}: {result:?}",
                case.id
            );
        }
    }
    server.await.unwrap();
}

#[tokio::test]
async fn unsupported_response_version_is_rejected() {
    let (_directory, path, listener) = socket_listener();
    let server = tokio::spawn(async move {
        let mut stream = accept_one(&listener).await;
        let request = read_request(&mut stream).await;
        let mut response = ResponseEnvelope::success(
            request.request_id,
            ResponseResult::Pong(pseudomux_protocol::v1::Pong {
                server_version: "future".into(),
                protocol_version: 2,
            }),
        );
        response.version = 2;
        write_json_frame(&mut stream, &response).await;
    });

    let error = PmuxClient::new(path).unwrap().ping().await.unwrap_err();
    assert!(matches!(
        error,
        ClientError::UnsupportedProtocolVersion {
            expected: 1,
            actual: 2
        }
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn advertised_oversized_frame_is_rejected_before_allocation() {
    let (_directory, path, listener) = socket_listener();
    let server = tokio::spawn(async move {
        let mut stream = accept_one(&listener).await;
        let _request = read_request(&mut stream).await;
        stream.write_all(&1_025_u32.to_be_bytes()).await.unwrap();
        stream.flush().await.unwrap();
    });
    let options = ClientOptions {
        max_frame_bytes: 1_024,
        connect_timeout: Duration::from_secs(1),
        request_timeout: Duration::from_secs(1),
    };

    let error = PmuxClient::with_options(path, options)
        .unwrap()
        .ping()
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ClientError::FrameTooLarge {
            advertised: 1_025,
            maximum: 1_024
        }
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn mismatched_request_id_is_rejected() {
    let (_directory, path, listener) = socket_listener();
    let server = tokio::spawn(async move {
        let mut stream = accept_one(&listener).await;
        let _request = read_request(&mut stream).await;
        let response = ResponseEnvelope::success(
            Uuid::from_u128(9_999),
            ResponseResult::Pong(pseudomux_protocol::v1::Pong {
                server_version: "test".into(),
                protocol_version: 1,
            }),
        );
        write_json_frame(&mut stream, &response).await;
    });

    let error = PmuxClient::new(path).unwrap().ping().await.unwrap_err();
    assert!(matches!(error, ClientError::MismatchedRequestId { .. }));
    server.await.unwrap();
}

#[tokio::test]
async fn every_typed_method_rejects_contextually_mismatched_results() {
    let (_directory, path, listener) = socket_listener();
    let server = tokio::spawn(async move {
        for index in 0..9 {
            let mut stream = accept_one(&listener).await;
            let request = read_request(&mut stream).await;
            let (result_type, data) = match index {
                0 => (
                    "pong",
                    json!({"server_version": "test", "protocol_version": 2}),
                ),
                1 => (
                    "session_started",
                    json!({
                        "session_id": OTHER_ID,
                        "generation_id": GENERATION_ID,
                        "state": "booting",
                        "compatibility": compatibility_report(),
                        "created_at_ms": 1,
                        "last_sequence": 0
                    }),
                ),
                2 => {
                    let mut value = serde_json::to_value(snapshot(0)).unwrap();
                    value["session_id"] = json!(OTHER_ID);
                    ("session_snapshot", value)
                }
                3 => (
                    "turn_accepted",
                    json!({
                        "session_id": SESSION_ID,
                        "generation_id": OTHER_ID,
                        "turn_id": TURN_ID,
                        "replayed": false,
                        "state": "running",
                        "next_sequence": 1
                    }),
                ),
                4 => (
                    "turn_cancelled",
                    json!({
                        "session_id": SESSION_ID,
                        "generation_id": GENERATION_ID,
                        "turn_id": OTHER_ID,
                        "outcome": "cancelled",
                        "session_state": "ready"
                    }),
                ),
                5 => (
                    "attach_capability",
                    json!({
                        "session_id": OTHER_ID,
                        "generation_id": GENERATION_ID,
                        "token": "opaque",
                        "endpoint": "/tmp/attach.sock",
                        "expires_at_ms": 10,
                        "read_only": true
                    }),
                ),
                6 => (
                    "session_closed",
                    json!({
                        "session_id": SESSION_ID,
                        "generation_id": OTHER_ID,
                        "already_closed": false,
                        "process_reaped": true
                    }),
                ),
                7 => ("turn_result", turn_result_wire(OTHER_ID)),
                8 => (
                    "events",
                    json!({
                        "events": [{
                            "schema_version": 1,
                            "session_id": OTHER_ID,
                            "generation_id": GENERATION_ID,
                            "sequence": 1,
                            "timestamp_ms": 1,
                            "event": {
                                "type": "heartbeat",
                                "data": {"session_state": "ready"}
                            }
                        }],
                        "next_sequence": 2
                    }),
                ),
                _ => unreachable!(),
            };
            write_json_frame(
                &mut stream,
                &json!({
                    "version": 1,
                    "request_id": request.request_id,
                    "result": {"type": result_type, "data": data}
                }),
            )
            .await;
        }
    });

    let client = PmuxClient::new(path).unwrap();
    assert!(matches!(
        client.ping().await.unwrap_err(),
        ClientError::UnsupportedProtocolVersion {
            expected: 1,
            actual: 2
        }
    ));
    assert!(matches!(
        client.start_session(start_request()).await.unwrap_err(),
        ClientError::ResultSessionMismatch { .. }
    ));
    assert!(matches!(
        client
            .inspect_session(SESSION_ID, GENERATION_ID)
            .await
            .unwrap_err(),
        ClientError::ResultSessionMismatch { .. }
    ));
    assert!(matches!(
        client
            .run_turn(
                SESSION_ID,
                GENERATION_ID,
                TurnRequest {
                    turn_id: TURN_ID,
                    prompt: "test".into(),
                    deadline_unix_ms: None,
                    lease: TurnLeasePolicy::default(),
                },
            )
            .await
            .unwrap_err(),
        ClientError::ResultGenerationMismatch { .. }
    ));
    assert!(matches!(
        client
            .cancel_turn(SESSION_ID, GENERATION_ID, TURN_ID)
            .await
            .unwrap_err(),
        ClientError::ResultTurnMismatch { .. }
    ));
    assert!(matches!(
        client
            .attach_session(AttachSessionRequest {
                session_id: SESSION_ID,
                generation_id: GENERATION_ID,
                read_only: true,
                size: None,
            })
            .await
            .unwrap_err(),
        ClientError::ResultSessionMismatch { .. }
    ));
    assert!(matches!(
        client
            .close_session(SESSION_ID, GENERATION_ID, ClosePolicy::Graceful)
            .await
            .unwrap_err(),
        ClientError::ResultGenerationMismatch { .. }
    ));
    assert!(matches!(
        client
            .run_once(RunOnceRequest {
                session: start_request(),
                turn: TurnRequest {
                    turn_id: TURN_ID,
                    prompt: "test".into(),
                    deadline_unix_ms: None,
                    lease: TurnLeasePolicy::default(),
                },
            })
            .await
            .unwrap_err(),
        ClientError::ResultTurnMismatch { .. }
    ));
    assert!(matches!(
        client
            .subscribe_events(pseudomux_protocol::v1::SubscribeEventsRequest {
                session_id: SESSION_ID,
                generation_id: GENERATION_ID,
                after_sequence: 0,
                wait_ms: 0,
                max_events: 8,
            })
            .await
            .unwrap_err(),
        ClientError::EventSessionMismatch { .. }
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn run_once_uses_the_turn_window_instead_of_the_short_rpc_timeout() {
    let (_directory, path, listener) = socket_listener();
    let server = tokio::spawn(async move {
        let mut stream = accept_one(&listener).await;
        let request = read_request(&mut stream).await;
        assert!(matches!(request.request, Request::RunOnce(_)));
        tokio::time::sleep(Duration::from_millis(75)).await;
        write_json_frame(
            &mut stream,
            &ResponseEnvelope::failure(
                request.request_id,
                ErrorBody::new(ErrorCode::InvalidConfig, "synthetic response"),
            ),
        )
        .await;
    });
    let client = PmuxClient::with_options(
        path,
        ClientOptions {
            max_frame_bytes: 1024 * 1024,
            connect_timeout: Duration::from_secs(1),
            request_timeout: Duration::from_millis(20),
        },
    )
    .unwrap();
    let error = client
        .run_once(RunOnceRequest {
            session: start_request(),
            turn: TurnRequest {
                turn_id: TURN_ID,
                prompt: "wait past the RPC timeout".into(),
                deadline_unix_ms: None,
                lease: TurnLeasePolicy::default(),
            },
        })
        .await
        .unwrap_err();
    assert!(matches!(error, ClientError::Server(_)));
    server.await.unwrap();
}

#[tokio::test]
async fn run_stateless_resource_fields_refuse_as_protocol_error() {
    const NAMES: [&str; 5] = [
        "session_id",
        "generation_id",
        "cwd",
        "config_root",
        "system_prompt",
    ];
    let (_directory, path, listener) = socket_listener();
    let server = tokio::spawn(async move {
        for named in NAMES {
            let mut stream = accept_one(&listener).await;
            let request = read_request(&mut stream).await;
            assert!(matches!(request.request, Request::RunStateless(_)));
            let mut response = serde_json::to_value(ResponseEnvelope::success(
                request.request_id,
                ResponseResult::StatelessResult(Box::new(
                    serde_json::from_value::<StatelessResult>(stateless_result()).unwrap(),
                )),
            ))
            .unwrap();
            response["result"]["data"][named] = json!("named-pool-resource");
            write_json_frame(&mut stream, &response).await;
        }
    });

    let client = PmuxClient::new(&path).unwrap();
    for named in NAMES {
        let error = client
            .run_stateless(RunStatelessRequest {
                model: "claude-sonnet-5".into(),
                effort: None,
                prompt: "hello".into(),
                deadline_unix_ms: None,
            })
            .await
            .unwrap_err();
        assert!(
            !matches!(error, ClientError::InvalidOptions(_)),
            "{named} must not be InvalidOptions: {error:?}"
        );
        assert!(
            matches!(
                error,
                ClientError::UnexpectedResult { actual, .. } if actual == named
            ),
            "{named} must be UnexpectedResult: {error:?}"
        );
        assert!(error.to_string().contains(named), "{error}");
    }
    server.await.unwrap();
}

#[tokio::test]
async fn event_stream_reconnects_advances_cursor_and_surfaces_replay_gap() {
    let (_directory, path, listener) = socket_listener();
    let server = tokio::spawn(async move {
        let mut first = accept_one(&listener).await;
        let first_request = read_request(&mut first).await;
        let Request::SubscribeEvents(params) = first_request.request else {
            panic!("expected event subscription")
        };
        assert_eq!(params.after_sequence, 0);
        let first_batch = EventBatch {
            events: vec![heartbeat(1), heartbeat(2)],
            next_sequence: 3,
            replay_gap: None,
        };
        write_json_frame(
            &mut first,
            &ResponseEnvelope::success(
                first_request.request_id,
                ResponseResult::Events(first_batch),
            ),
        )
        .await;

        let mut second = accept_one(&listener).await;
        let second_request = read_request(&mut second).await;
        let Request::SubscribeEvents(params) = second_request.request else {
            panic!("expected event subscription")
        };
        assert_eq!(params.after_sequence, 2);
        let gap = pseudomux_protocol::v1::ReplayGap {
            requested_after: 2,
            oldest_available: 8,
            next_sequence: 10,
            snapshot: Box::new(snapshot(9)),
        };
        let second_batch = EventBatch {
            events: vec![],
            next_sequence: 10,
            replay_gap: Some(gap),
        };
        write_json_frame(
            &mut second,
            &ResponseEnvelope::success(
                second_request.request_id,
                ResponseResult::Events(second_batch),
            ),
        )
        .await;
    });

    let client = PmuxClient::new(path).unwrap();
    let mut events = client.event_stream(
        SESSION_ID,
        GENERATION_ID,
        0,
        EventStreamOptions {
            wait_ms: 100,
            max_events: 8,
        },
    );

    let EventStreamItem::Event(first) = events.next().await.unwrap().unwrap() else {
        panic!("expected first event")
    };
    assert_eq!(first.sequence, 1);
    let EventStreamItem::Event(second) = events.next().await.unwrap().unwrap() else {
        panic!("expected second event")
    };
    assert_eq!(second.sequence, 2);
    let EventStreamItem::ReplayGap(gap) = events.next().await.unwrap().unwrap() else {
        panic!("expected replay gap")
    };
    assert_eq!(gap.snapshot.last_sequence, 9);
    assert_eq!(events.after_sequence(), 9);
    server.await.unwrap();
}

#[tokio::test]
async fn event_stream_publishes_only_yielded_cursor_and_resumes_without_loss() {
    let (_directory, path, listener) = socket_listener();
    let server = tokio::spawn(async move {
        let mut first = accept_one(&listener).await;
        let first_request = read_request(&mut first).await;
        let Request::SubscribeEvents(params) = first_request.request else {
            panic!("expected event subscription")
        };
        assert_eq!(params.after_sequence, 0);
        write_json_frame(
            &mut first,
            &ResponseEnvelope::success(
                first_request.request_id,
                ResponseResult::Events(EventBatch {
                    events: vec![heartbeat(1), heartbeat(2), heartbeat(3)],
                    next_sequence: 4,
                    replay_gap: None,
                }),
            ),
        )
        .await;

        let mut resumed = accept_one(&listener).await;
        let resumed_request = read_request(&mut resumed).await;
        let Request::SubscribeEvents(params) = resumed_request.request else {
            panic!("expected resumed event subscription")
        };
        assert_eq!(params.after_sequence, 1);
        write_json_frame(
            &mut resumed,
            &ResponseEnvelope::success(
                resumed_request.request_id,
                ResponseResult::Events(EventBatch {
                    events: vec![heartbeat(2), heartbeat(3)],
                    next_sequence: 4,
                    replay_gap: None,
                }),
            ),
        )
        .await;
    });

    let client = PmuxClient::new(&path).unwrap();
    let mut first =
        client.event_stream(SESSION_ID, GENERATION_ID, 0, EventStreamOptions::default());
    let EventStreamItem::Event(event) = first.next().await.unwrap().unwrap() else {
        panic!("expected first event")
    };
    assert_eq!(event.sequence, 1);
    assert_eq!(
        first.after_sequence(),
        1,
        "unconsumed events must not be published as durable cursor progress"
    );
    drop(first);

    let mut resumed =
        client.event_stream(SESSION_ID, GENERATION_ID, 1, EventStreamOptions::default());
    for expected in [2, 3] {
        let EventStreamItem::Event(event) = resumed.next().await.unwrap().unwrap() else {
            panic!("expected resumed event")
        };
        assert_eq!(event.sequence, expected);
        assert_eq!(resumed.after_sequence(), expected);
    }
    server.await.unwrap();
}

#[tokio::test]
async fn event_stream_retries_same_cursor_after_transport_failure() {
    let (_directory, path, listener) = socket_listener();
    let server = tokio::spawn(async move {
        let mut first = accept_one(&listener).await;
        let first_request = read_request(&mut first).await;
        let Request::SubscribeEvents(params) = first_request.request else {
            panic!("expected event subscription")
        };
        assert_eq!(params.after_sequence, 4);
        drop(first);

        let mut second = accept_one(&listener).await;
        let second_request = read_request(&mut second).await;
        let Request::SubscribeEvents(params) = second_request.request else {
            panic!("expected event subscription")
        };
        assert_eq!(params.after_sequence, 4);
        let batch = EventBatch {
            events: vec![heartbeat(5)],
            next_sequence: 6,
            replay_gap: None,
        };
        write_json_frame(
            &mut second,
            &ResponseEnvelope::success(second_request.request_id, ResponseResult::Events(batch)),
        )
        .await;
    });

    let client = PmuxClient::new(path).unwrap();
    let mut events =
        client.event_stream(SESSION_ID, GENERATION_ID, 4, EventStreamOptions::default());
    assert!(matches!(
        events.next().await.unwrap().unwrap_err(),
        ClientError::Io(_)
    ));
    let EventStreamItem::Event(event) = events.next().await.unwrap().unwrap() else {
        panic!("expected event after reconnect")
    };
    assert_eq!(event.sequence, 5);
    assert_eq!(events.after_sequence(), 5);
    server.await.unwrap();
}

#[tokio::test]
async fn out_of_order_event_is_a_typed_sequence_error() {
    let (_directory, path, listener) = socket_listener();
    let server = tokio::spawn(async move {
        let mut stream = accept_one(&listener).await;
        let request = read_request(&mut stream).await;
        let batch = EventBatch {
            events: vec![heartbeat(2)],
            next_sequence: 3,
            replay_gap: None,
        };
        write_json_frame(
            &mut stream,
            &ResponseEnvelope::success(request.request_id, ResponseResult::Events(batch)),
        )
        .await;
    });

    let client = PmuxClient::new(path).unwrap();
    let mut events =
        client.event_stream(SESSION_ID, GENERATION_ID, 0, EventStreamOptions::default());
    let error = events.next().await.unwrap().unwrap_err();
    assert!(matches!(
        error,
        ClientError::InvalidEventSequence {
            expected: 1,
            actual: 2
        }
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn safe_max_event_cursor_fails_closed_instead_of_saturating() {
    let (_directory, path, listener) = socket_listener();
    let server = tokio::spawn(async move {
        let mut stream = accept_one(&listener).await;
        let request = read_request(&mut stream).await;
        write_json_frame(
            &mut stream,
            &ResponseEnvelope::success(
                request.request_id,
                ResponseResult::Events(EventBatch {
                    events: vec![],
                    next_sequence: pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER,
                    replay_gap: None,
                }),
            ),
        )
        .await;
    });

    let error = PmuxClient::new(path)
        .unwrap()
        .subscribe_events(pseudomux_protocol::v1::SubscribeEventsRequest {
            session_id: SESSION_ID,
            generation_id: GENERATION_ID,
            after_sequence: pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER,
            wait_ms: 0,
            max_events: 8,
        })
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ClientError::EventCursorOverflow {
            cursor: pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER
        }
    ));
    server.await.unwrap();
}
