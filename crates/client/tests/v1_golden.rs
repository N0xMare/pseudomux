#![cfg(unix)]

use std::path::PathBuf;

use pseudomux_client::{ClientError, PmuxClient};
use pseudomux_protocol::v1::{
    ErrorCode, Request, RequestEnvelope, ResponsePayload, ResponseResult, SubscribeEventsRequest,
};
use serde::Deserialize;
use serde_json::Value;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use uuid::Uuid;

#[derive(Clone, Deserialize)]
struct Golden {
    ids: GoldenIds,
    requests_and_results: Vec<GoldenExchange>,
    events: Vec<GoldenEvent>,
    error: Value,
}

#[derive(Clone, Deserialize)]
struct GoldenIds {
    request_id: String,
    session_id: String,
    generation_id: String,
    turn_id: String,
}

#[derive(Clone, Deserialize)]
struct GoldenExchange {
    method: String,
    request: Value,
    response: Value,
}

#[derive(Clone, Deserialize)]
struct GoldenEvent {
    #[serde(rename = "type")]
    event_type: String,
    frame: Value,
}

#[derive(Clone, Deserialize)]
struct SharedCases {
    client_required_field_deletions: RequiredFieldDeletions,
    client_negative_matrix: Vec<NegativeCase>,
    reserved_turn_lease_cases: ReservedTurnLeaseCases,
}

#[derive(Clone, Deserialize)]
struct RequiredFieldDeletions {
    result_envelope: Vec<String>,
    results: Vec<ResultRequiredFields>,
    event_envelope: Vec<String>,
    events: Vec<EventRequiredFields>,
    error: Vec<String>,
}

#[derive(Clone, Deserialize)]
struct ResultRequiredFields {
    method: String,
    pointers: Vec<String>,
}

#[derive(Clone, Deserialize)]
struct EventRequiredFields {
    event_type: String,
    pointers: Vec<String>,
}

#[derive(Clone, Deserialize)]
struct ReservedTurnLeaseCases {
    expected_error: ExpectedError,
    cases: Vec<ReservedTurnLeaseCase>,
}

#[derive(Clone, Deserialize)]
struct ExpectedError {
    code: String,
    retryable: bool,
}

#[derive(Clone, Deserialize)]
struct ReservedTurnLeaseCase {
    id: String,
    operation: String,
    lease: Value,
}

#[derive(Clone, Deserialize)]
struct NegativeCase {
    id: String,
    operation: String,
    #[serde(default)]
    after_sequence: u64,
    error_category: String,
    response: Value,
}

fn vector_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/conformance/v1")
        .join(name)
}

fn read_vector<T: for<'de> Deserialize<'de>>(name: &str) -> T {
    serde_json::from_slice(&std::fs::read(vector_path(name)).unwrap()).unwrap()
}

fn socket_listener() -> (TempDir, PathBuf, UnixListener) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pmuxd.sock");
    let listener = UnixListener::bind(&path).unwrap();
    (directory, path, listener)
}

async fn read_value(stream: &mut UnixStream) -> Value {
    let mut header = [0_u8; 4];
    stream.read_exact(&mut header).await.unwrap();
    let mut payload = vec![0; u32::from_be_bytes(header) as usize];
    stream.read_exact(&mut payload).await.unwrap();
    serde_json::from_slice(&payload).unwrap()
}

async fn write_value(stream: &mut UnixStream, value: &Value) {
    let payload = serde_json::to_vec(value).unwrap();
    stream
        .write_all(&(payload.len() as u32).to_be_bytes())
        .await
        .unwrap();
    stream.write_all(&payload).await.unwrap();
    stream.flush().await.unwrap();
}

fn replace_request_id(value: &mut Value, request_id: &str) {
    match value {
        Value::String(text) if text == "$REQUEST_ID" => *text = request_id.to_owned(),
        Value::Array(array) => {
            for item in array {
                replace_request_id(item, request_id);
            }
        }
        Value::Object(object) => {
            for item in object.values_mut() {
                replace_request_id(item, request_id);
            }
        }
        _ => {}
    }
}

fn remove_pointer(value: &mut Value, pointer: &str) {
    let (parent, field) = pointer.rsplit_once('/').unwrap();
    value
        .pointer_mut(parent)
        .and_then(Value::as_object_mut)
        .and_then(|object| object.remove(field))
        .unwrap_or_else(|| panic!("shared deletion pointer {pointer} must exist"));
}

fn insert_object_field(value: &mut Value, pointer: &str) {
    let target = if pointer.is_empty() {
        value
    } else {
        value
            .pointer_mut(pointer)
            .unwrap_or_else(|| panic!("golden object pointer {pointer} must exist"))
    };
    let object = target
        .as_object_mut()
        .unwrap_or_else(|| panic!("golden pointer {pointer:?} must identify an object"));
    assert!(
        object
            .insert(
                "future_minor_field".to_owned(),
                serde_json::json!({"opaque": true}),
            )
            .is_none(),
        "mutation field already existed at {pointer:?}"
    );
}

fn object_pointers(value: &Value) -> Vec<String> {
    fn escape(component: &str) -> String {
        component.replace('~', "~0").replace('/', "~1")
    }

    fn visit(value: &Value, pointer: &str, pointers: &mut Vec<String>) {
        match value {
            Value::Object(object) => {
                pointers.push(pointer.to_owned());
                for (key, child) in object {
                    visit(child, &format!("{pointer}/{}", escape(key)), pointers);
                }
            }
            Value::Array(array) => {
                for (index, child) in array.iter().enumerate() {
                    visit(child, &format!("{pointer}/{index}"), pointers);
                }
            }
            _ => {}
        }
    }

    let mut pointers = Vec::new();
    visit(value, "", &mut pointers);
    pointers
}

async fn invoke_typed(
    client: &PmuxClient,
    request: Request,
) -> Result<ResponseResult, ClientError> {
    match request {
        Request::Ping => client.ping().await.map(ResponseResult::Pong),
        Request::StartSession(params) => client
            .start_session(params)
            .await
            .map(ResponseResult::SessionStarted),
        Request::RunTurn(params) => client
            .run_turn(params.session_id, params.generation_id, params.turn)
            .await
            .map(ResponseResult::TurnAccepted),
        Request::CancelTurn(params) => client
            .cancel_turn(params.session_id, params.generation_id, params.turn_id)
            .await
            .map(ResponseResult::TurnCancelled),
        Request::InspectSession(params) => client
            .inspect_session(params.session_id, params.generation_id)
            .await
            .map(|snapshot| ResponseResult::SessionSnapshot(Box::new(snapshot))),
        Request::AttachSession(params) => client
            .attach_session(params)
            .await
            .map(ResponseResult::AttachCapability),
        Request::CloseSession(params) => client
            .close_session(params.session_id, params.generation_id, params.policy)
            .await
            .map(ResponseResult::SessionClosed),
        Request::SubscribeEvents(params) => client
            .subscribe_events(params)
            .await
            .map(ResponseResult::Events),
        Request::RunOnce(params) => client
            .run_once(params)
            .await
            .map(|result| ResponseResult::TurnResult(Box::new(result))),
        Request::ClearSession(params) => client
            .clear_session(
                params.session_id,
                params.generation_id,
                params.expected_transcript_session_id,
                params.deadline_unix_ms,
            )
            .await
            .map(ResponseResult::SessionCleared),
        Request::Diagnose => client
            .diagnose()
            .await
            .map(|diagnosis| ResponseResult::Diagnosis(Box::new(diagnosis))),
        Request::RunStateless(params) => client
            .run_stateless(params)
            .await
            .map(|result| ResponseResult::StatelessResult(Box::new(result))),
        Request::CreateAgent(params) => client
            .create_agent(params.spec)
            .await
            .map(|descriptor| ResponseResult::AgentCreated(Box::new(descriptor))),
        Request::GetAgent(params) => client
            .get_agent(params.agent_id, params.version)
            .await
            .map(|descriptor| ResponseResult::Agent(Box::new(descriptor))),
        Request::ListAgents(_) => client
            .list_agents()
            .await
            .map(|list| ResponseResult::AgentList(Box::new(list))),
        Request::UpdateAgent(params) => client
            .update_agent(params.agent_id, params.expected_version, params.spec)
            .await
            .map(|descriptor| ResponseResult::AgentUpdated(Box::new(descriptor))),
    }
}

fn request_for(golden: &Golden, method: &str) -> Request {
    serde_json::from_value::<RequestEnvelope>(
        golden
            .requests_and_results
            .iter()
            .find(|exchange| exchange.method == method)
            .unwrap()
            .request
            .clone(),
    )
    .unwrap()
    .request
}

#[tokio::test]
async fn every_typed_method_sends_exact_golden_requests_and_accepts_matching_results() {
    let golden: Golden = read_vector("golden.json");
    let server_golden = golden.clone();
    let (_directory, path, listener) = socket_listener();
    let server = tokio::spawn(async move {
        for exchange in server_golden.requests_and_results {
            let (mut stream, _) = listener.accept().await.unwrap();
            let actual = read_value(&mut stream).await;
            let generated_id = actual["request_id"]
                .as_str()
                .unwrap_or_else(|| panic!("{} did not send a request UUID", exchange.method));
            Uuid::parse_str(generated_id)
                .unwrap_or_else(|error| panic!("{} request UUID: {error}", exchange.method));
            let mut normalized = actual.clone();
            normalized["request_id"] = Value::String(server_golden.ids.request_id.clone());
            assert_eq!(normalized, exchange.request, "{} request", exchange.method);

            let mut response = exchange.response;
            response["request_id"] = Value::String(generated_id.to_owned());
            write_value(&mut stream, &response).await;
        }
    });

    let client = PmuxClient::new(path).unwrap();
    for exchange in &golden.requests_and_results {
        let request = serde_json::from_value::<RequestEnvelope>(exchange.request.clone())
            .unwrap()
            .request;
        let actual = invoke_typed(&client, request)
            .await
            .unwrap_or_else(|error| panic!("{} failed: {error}", exchange.method));
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            exchange.response["result"],
            "{} result",
            exchange.method
        );
    }
    server.await.unwrap();
}

#[tokio::test]
async fn shared_negative_identity_schema_sequence_cursor_gap_and_exhaustion_matrix_fails_closed() {
    let golden: Golden = read_vector("golden.json");
    let cases: SharedCases = read_vector("cases.json");
    let server_cases = cases.client_negative_matrix.clone();
    let (_directory, path, listener) = socket_listener();
    let server = tokio::spawn(async move {
        for case in server_cases {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_value(&mut stream).await;
            let request_id = request["request_id"].as_str().unwrap();
            let mut response = case.response;
            replace_request_id(&mut response, request_id);
            write_value(&mut stream, &response).await;
        }
    });

    let client = PmuxClient::new(path).unwrap();
    for case in cases.client_negative_matrix {
        let request = match case.operation.as_str() {
            "subscribe_events" => {
                let Request::SubscribeEvents(mut params) = request_for(&golden, "subscribe_events")
                else {
                    unreachable!()
                };
                params.after_sequence = case.after_sequence;
                Request::SubscribeEvents(SubscribeEventsRequest { ..params })
            }
            operation => request_for(&golden, operation),
        };
        let error = invoke_typed(&client, request).await.expect_err(&case.id);
        let matches_category = match case.error_category.as_str() {
            "response_identity" => matches!(error, ClientError::MismatchedRequestId { .. }),
            "schema_version" => {
                matches!(error, ClientError::UnsupportedProtocolVersion { .. })
            }
            "schema" => matches!(error, ClientError::Json(_)),
            "result_session" => matches!(error, ClientError::ResultSessionMismatch { .. }),
            "result_generation" => {
                matches!(error, ClientError::ResultGenerationMismatch { .. })
            }
            "result_turn" => matches!(error, ClientError::ResultTurnMismatch { .. }),
            "event_session" => matches!(error, ClientError::EventSessionMismatch { .. }),
            "event_generation" => {
                matches!(error, ClientError::EventGenerationMismatch { .. })
            }
            "event_sequence" => matches!(error, ClientError::InvalidEventSequence { .. }),
            "batch_cursor" => matches!(error, ClientError::InvalidBatchCursor { .. }),
            "replay_gap" => {
                matches!(
                    error,
                    ClientError::Json(_) | ClientError::InvalidReplayGap { .. }
                )
            }
            "cursor_exhaustion" => matches!(error, ClientError::EventCursorOverflow { .. }),
            other => panic!("unknown shared error category {other}"),
        };
        assert!(
            matches_category,
            "negative case {} expected {}, got {error:?}",
            case.id, case.error_category
        );
    }
    server.await.unwrap();

    for id in [
        golden.ids.session_id,
        golden.ids.generation_id,
        golden.ids.turn_id,
    ] {
        Uuid::parse_str(&id).unwrap();
    }
}

#[tokio::test]
async fn shared_required_field_inventory_rejects_every_nested_result_event_and_error_deletion() {
    let golden: Golden = read_vector("golden.json");
    let cases: SharedCases = read_vector("cases.json");
    // DERIVED FROM THE CORPUS, never from a literal: a method appended to
    // `golden.json` with no required-field inventory of its own must redden
    // here rather than pass by having no cases. See
    // `crates/protocol/tests/v1_golden.rs`'s `manifest_methods` for the
    // eleven-of-twelve this replaced.
    assert_eq!(
        cases.client_required_field_deletions.results.len(),
        golden.requests_and_results.len()
    );
    assert_eq!(
        cases.client_required_field_deletions.events.len(),
        golden.events.len()
    );
    assert_eq!(
        cases
            .client_required_field_deletions
            .results
            .iter()
            .map(|fields| {
                fields.pointers.len() + cases.client_required_field_deletions.result_envelope.len()
            })
            .sum::<usize>(),
        // 187 before `diagnose`; its nine required result pointers plus the
        // five shared envelope pointers add fourteen. 201 before
        // `run_stateless`, whose twenty required result pointers plus the same
        // five add twenty-five. 226 before the four agent methods: three
        // descriptors of six plus five, and `agent_list`'s six plus five.
        270
    );
    assert_eq!(
        cases
            .client_required_field_deletions
            .events
            .iter()
            .map(|fields| {
                fields.pointers.len() + cases.client_required_field_deletions.event_envelope.len()
            })
            .sum::<usize>(),
        223
    );
    assert_eq!(cases.client_required_field_deletions.error.len(), 6);
    let server_golden = golden.clone();
    let server_deletions = cases.client_required_field_deletions.clone();
    let (_directory, path, listener) = socket_listener();
    let server = tokio::spawn(async move {
        for fields in &server_deletions.results {
            for pointer in server_deletions
                .result_envelope
                .iter()
                .chain(&fields.pointers)
            {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = read_value(&mut stream).await;
                let mut response = server_golden
                    .requests_and_results
                    .iter()
                    .find(|exchange| exchange.method == fields.method)
                    .unwrap()
                    .response
                    .clone();
                response["request_id"] = request["request_id"].clone();
                remove_pointer(&mut response, pointer);
                write_value(&mut stream, &response).await;
            }
        }
        for fields in &server_deletions.events {
            for pointer in server_deletions
                .event_envelope
                .iter()
                .chain(&fields.pointers)
            {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = read_value(&mut stream).await;
                let mut frame = server_golden
                    .events
                    .iter()
                    .find(|event| event.event_type == fields.event_type)
                    .unwrap()
                    .frame
                    .clone();
                frame["sequence"] = Value::from(1);
                remove_pointer(&mut frame, pointer);
                let response = serde_json::json!({
                    "version": 1,
                    "request_id": request["request_id"],
                    "result": {"type": "events", "data": {"events": [frame], "next_sequence": 2}}
                });
                write_value(&mut stream, &response).await;
            }
        }
        for pointer in &server_deletions.error {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_value(&mut stream).await;
            let mut response = server_golden.error.clone();
            response["request_id"] = request["request_id"].clone();
            remove_pointer(&mut response, pointer);
            write_value(&mut stream, &response).await;
        }
    });

    let client = PmuxClient::new(path).unwrap();
    for fields in &cases.client_required_field_deletions.results {
        for pointer in cases
            .client_required_field_deletions
            .result_envelope
            .iter()
            .chain(&fields.pointers)
        {
            let error = invoke_typed(&client, request_for(&golden, &fields.method))
                .await
                .expect_err(&format!("{} {pointer}", fields.method));
            assert!(
                matches!(&error, ClientError::Json(_))
                    || (pointer == "/version"
                        && matches!(&error, ClientError::InvalidProtocolVersion)),
                "{error:?}"
            );
        }
    }
    let Request::SubscribeEvents(mut subscription) = request_for(&golden, "subscribe_events")
    else {
        unreachable!()
    };
    subscription.after_sequence = 0;
    for fields in &cases.client_required_field_deletions.events {
        for pointer in cases
            .client_required_field_deletions
            .event_envelope
            .iter()
            .chain(&fields.pointers)
        {
            let error = client
                .subscribe_events(subscription.clone())
                .await
                .expect_err(&format!("{} {pointer}", fields.event_type));
            assert!(matches!(error, ClientError::Json(_)), "{error:?}");
        }
    }
    for pointer in &cases.client_required_field_deletions.error {
        let error = client
            .ping()
            .await
            .expect_err(&format!("error response {pointer}"));
        assert!(
            matches!(&error, ClientError::Json(_))
                || (pointer == "/version" && matches!(&error, ClientError::InvalidProtocolVersion)),
            "{error:?}"
        );
    }
    server.await.unwrap();
}

#[tokio::test]
async fn shared_goldens_accept_additive_fields_at_every_result_event_and_error_object_boundary() {
    let golden: Golden = read_vector("golden.json");
    let mut successes = Vec::new();
    let mut result_boundaries = 0;
    for exchange in &golden.requests_and_results {
        for pointer in object_pointers(&exchange.response) {
            result_boundaries += 1;
            let mut response = exchange.response.clone();
            insert_object_field(&mut response, &pointer);
            successes.push((
                format!("{} {pointer:?}", exchange.method),
                request_for(&golden, &exchange.method),
                response,
            ));
        }
    }
    // 58 before `diagnose`; its exchange adds six object boundaries -- the
    // envelope, `result`, `result/data`, `result/data/runtime`, and one per
    // entry of `result/data/sessions` -- and every one of them must still
    // tolerate an unknown field, because response DTOs evolve additively. 64
    // before `run_stateless`, whose exchange adds eight: the envelope,
    // `result`, `result/data`, `result/data/stop_reason`, `result/data/usage`,
    // and one per usage scope. 72 before the agent methods, whose four
    // exchanges add 46. The echoed `spec` is OPAQUE on a response, so its
    // boundaries are additive like every other result boundary.
    assert_eq!(
        result_boundaries, 118,
        "review new result object boundaries"
    );

    let Request::SubscribeEvents(mut subscription) = request_for(&golden, "subscribe_events")
    else {
        unreachable!()
    };
    subscription.after_sequence = 0;
    let mut event_boundaries = 0;
    for event in &golden.events {
        let mut base_frame = event.frame.clone();
        base_frame["sequence"] = Value::from(1);
        for pointer in object_pointers(&base_frame) {
            event_boundaries += 1;
            let mut frame = base_frame.clone();
            insert_object_field(&mut frame, &pointer);
            successes.push((
                format!("{} {pointer:?}", event.event_type),
                Request::SubscribeEvents(subscription.clone()),
                serde_json::json!({
                    "version": 1,
                    "request_id": golden.ids.request_id,
                    "result": {"type": "events", "data": {"events": [frame], "next_sequence": 2}}
                }),
            ));
        }
    }
    assert_eq!(event_boundaries, 67, "review new event object boundaries");

    let error_pointers = object_pointers(&golden.error);
    assert_eq!(
        error_pointers.len(),
        3,
        "review new error object boundaries"
    );
    let mut additive_errors = Vec::new();
    for pointer in error_pointers {
        let mut response = golden.error.clone();
        insert_object_field(&mut response, &pointer);
        additive_errors.push((format!("error {pointer:?}"), response));
    }

    let server_successes = successes
        .iter()
        .map(|(_, _, response)| response.clone())
        .collect::<Vec<_>>();
    let server_errors = additive_errors
        .iter()
        .map(|(_, response)| response.clone())
        .collect::<Vec<_>>();
    let (_directory, path, listener) = socket_listener();
    let server = tokio::spawn(async move {
        for mut response in server_successes.into_iter().chain(server_errors) {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_value(&mut stream).await;
            response["request_id"] = request["request_id"].clone();
            write_value(&mut stream, &response).await;
        }
    });

    let client = PmuxClient::new(path).unwrap();
    for (label, request, _) in successes {
        invoke_typed(&client, request)
            .await
            .unwrap_or_else(|error| panic!("{label} rejected additive field: {error}"));
    }
    for (label, _) in additive_errors {
        let error = client.ping().await.expect_err(&label);
        assert!(
            matches!(error, ClientError::Server(_)),
            "{label} was not accepted as a server error: {error:?}"
        );
    }
    server.await.unwrap();
}

#[tokio::test]
async fn reserved_turn_leases_are_sent_then_surface_stable_unsupported_feature_errors() {
    let golden: Golden = read_vector("golden.json");
    let cases: SharedCases = read_vector("cases.json");
    let reserved = cases.reserved_turn_lease_cases;
    assert_eq!(reserved.expected_error.code, "unsupported_feature");
    assert!(!reserved.expected_error.retryable);
    assert_eq!(reserved.cases.len(), 6);

    let mut requests = Vec::new();
    for case in &reserved.cases {
        let mut wire = golden
            .requests_and_results
            .iter()
            .find(|exchange| exchange.method == case.operation)
            .unwrap_or_else(|| panic!("{} has no request golden", case.id))
            .request
            .clone();
        wire["params"]["turn"]["lease"] = case.lease.clone();
        let request = serde_json::from_value::<RequestEnvelope>(wire.clone())
            .unwrap_or_else(|error| panic!("{} must be a valid request DTO: {error}", case.id))
            .request;
        requests.push((case.id.clone(), request, wire));
    }

    let server_requests = requests
        .iter()
        .map(|(id, _, wire)| (id.clone(), wire.clone()))
        .collect::<Vec<_>>();
    let expected_error = reserved.expected_error.clone();
    let fixed_request_id = golden.ids.request_id.clone();
    let (_directory, path, listener) = socket_listener();
    let server = tokio::spawn(async move {
        for (id, expected) in server_requests {
            let (mut stream, _) = listener.accept().await.unwrap();
            let actual = read_value(&mut stream).await;
            let request_id = actual["request_id"].as_str().unwrap().to_owned();
            let mut normalized = actual;
            normalized["request_id"] = Value::String(fixed_request_id.clone());
            assert_eq!(normalized, expected, "{id} request changed");
            write_value(
                &mut stream,
                &serde_json::json!({
                    "version": 1,
                    "request_id": request_id,
                    "error": {
                        "code": expected_error.code,
                        "message": "reserved turn lease values require a future leased connection API",
                        "retryable": expected_error.retryable
                    }
                }),
            )
            .await;
        }
    });

    let client = PmuxClient::new(path).unwrap();
    for (id, request, _) in requests {
        let error = invoke_typed(&client, request).await.expect_err(&id);
        let ClientError::Server(body) = error else {
            panic!("{id} must surface a typed server error, got {error:?}")
        };
        assert_eq!(body.code, ErrorCode::UnsupportedFeature, "{id}");
        assert!(!body.retryable, "{id}");
    }
    server.await.unwrap();
}

#[test]
fn golden_success_responses_contain_exactly_one_success_payload() {
    let golden: Golden = read_vector("golden.json");
    for exchange in golden.requests_and_results {
        let response: pseudomux_protocol::v1::ResponseEnvelope =
            serde_json::from_value(exchange.response).unwrap();
        assert!(matches!(response.payload, ResponsePayload::Success(_)));
    }
}
