use std::collections::BTreeSet;
use std::path::PathBuf;

use pseudomux_protocol::v1::{
    EventEnvelope, MAX_SAFE_JSON_INTEGER, RequestEnvelope, ResponseEnvelope,
};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, Deserialize)]
struct Golden {
    schema_version: u16,
    ids: GoldenIds,
    requests_and_results: Vec<GoldenExchange>,
    events: Vec<GoldenEvent>,
    error: Value,
    durable_ids: DurableIds,
}

#[derive(Clone, Deserialize)]
struct GoldenIds {
    request_id: String,
    session_id: String,
    generation_id: String,
    turn_id: String,
    other_id: String,
    rotated_transcript_session_id: String,
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
struct DurableIds {
    namespace: String,
    cases: Vec<DurableIdCase>,
}

#[derive(Clone, Deserialize)]
struct DurableIdCase {
    attempt: String,
    turn_id: String,
}

#[derive(Deserialize)]
struct SharedCases {
    strict_request_object_pointers: Vec<StrictRequestObjects>,
    strict_request_unit_variant_mutations: Vec<StrictRequestVariant>,
    client_required_field_deletions: RequiredFieldDeletions,
    reserved_turn_lease_cases: ReservedTurnLeaseCases,
}

#[derive(Deserialize)]
struct StrictRequestObjects {
    method: String,
    pointers: Vec<String>,
}

#[derive(Deserialize)]
struct StrictRequestVariant {
    id: String,
    method: String,
    pointer: String,
    replacement: Value,
}

#[derive(Deserialize)]
struct RequiredFieldDeletions {
    result_envelope: Vec<String>,
    results: Vec<ResultRequiredFields>,
    event_envelope: Vec<String>,
    events: Vec<EventRequiredFields>,
    error: Vec<String>,
}

#[derive(Deserialize)]
struct ResultRequiredFields {
    method: String,
    pointers: Vec<String>,
}

#[derive(Deserialize)]
struct EventRequiredFields {
    event_type: String,
    pointers: Vec<String>,
}

#[derive(Deserialize)]
struct ReservedTurnLeaseCases {
    expected_error: ExpectedError,
    cases: Vec<ReservedTurnLeaseCase>,
}

#[derive(Deserialize)]
struct ExpectedError {
    code: String,
    retryable: bool,
}

#[derive(Deserialize)]
struct ReservedTurnLeaseCase {
    id: String,
    operation: String,
    lease: Value,
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance/v1/golden.json")
}

fn golden() -> Golden {
    serde_json::from_slice(&std::fs::read(golden_path()).unwrap()).unwrap()
}

fn cases_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance/v1/cases.json")
}

fn cases() -> SharedCases {
    serde_json::from_slice(&std::fs::read(cases_path()).unwrap()).unwrap()
}

/// The v1 method surface, read from the file the whole corpus is pinned to.
///
/// THE COUNT IS DERIVED FROM THIS AND NEVER WRITTEN OUT. Until this function
/// existed, three languages each carried a hand-written `11` -- here, in
/// `clients/typescript/tests/golden-conformance.test.mjs`, and in
/// `clients/python/tests/test_golden_conformance.py` -- and
/// `tests/conformance/v1/README.md` claimed `golden.json` held "one complete
/// request/result pair for every method". MEASURED against
/// `manifest.json`, it held eleven of twelve: `run_stateless`, the whole of
/// Path B and the only producer of `stateless_result`, had no golden pair in
/// any language while both shipped clients implemented and validated it.
///
/// A literal freezes the corpus at the size it had the day it was written.
/// Deleting an entry reddens it; failing to ADD one does not -- which is
/// exactly how an appended method slips through, and `run_stateless` did.
/// `shared_manifest_matches_the_closed_v1_surface` already fixed this defect
/// for `manifest.json` with an exhaustive `match`
/// (`v1_conformance_vectors.rs:126-135` records the history); the fix was
/// applied to that checker and not to this one.
fn manifest_methods() -> Vec<String> {
    manifest_surface().methods
}

/// The v1 event surface, read from the same file for the same reason.
///
/// **THE FIX WAS APPLIED TO ONE HALF OF ONE FILE.** `manifest_methods` above
/// was written to replace three hand-written copies of `11` -- and the two
/// EVENT assertions in this same file, in that same commit, stayed the literal
/// `14`. `manifest.events` is pinned to the Rust enum by an exhaustive match in
/// `v1_conformance_vectors.rs`, and nothing read it here at all; neither client
/// asserts event coverage against the manifest either.
///
/// MEASURED: appending `"future_event"` to `manifest.events` left all eight
/// Rust golden tests green. That is exactly the shape a literal cannot catch --
/// deleting an event reddens it, failing to ADD one does not -- and it is the
/// shape `run_stateless` slipped through on the method side.
fn manifest_events() -> Vec<String> {
    manifest_surface().events
}

fn manifest_surface() -> ManifestSurface {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance/v1/manifest.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[derive(Deserialize)]
struct ManifestSurface {
    methods: Vec<String>,
    events: Vec<String>,
}

fn remove_pointer(value: &mut Value, pointer: &str) -> Option<Value> {
    let components = pointer
        .strip_prefix('/')?
        .split('/')
        .map(|component| component.replace("~1", "/").replace("~0", "~"))
        .collect::<Vec<_>>();
    remove_components(value, &components)
}

fn remove_components(value: &mut Value, components: &[String]) -> Option<Value> {
    let (head, tail) = components.split_first()?;
    if tail.is_empty() {
        return match value {
            Value::Object(object) => object.remove(head),
            Value::Array(array) => array.get_mut(head.parse::<usize>().ok()?).map(Value::take),
            _ => None,
        };
    }
    match value {
        Value::Object(object) => remove_components(object.get_mut(head)?, tail),
        Value::Array(array) => remove_components(array.get_mut(head.parse::<usize>().ok()?)?, tail),
        _ => None,
    }
}

fn set_pointer(value: &mut Value, pointer: &str, replacement: Value) {
    let target = value
        .pointer_mut(pointer)
        .unwrap_or_else(|| panic!("golden pointer {pointer} must exist"));
    *target = replacement;
}

fn insert_object_field(value: &mut Value, pointer: &str, field: &str, replacement: Value) {
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
        object.insert(field.to_owned(), replacement).is_none(),
        "mutation field {field} already existed at {pointer:?}"
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
                    let next = format!("{pointer}/{}", escape(key));
                    visit(child, &next, pointers);
                }
            }
            Value::Array(array) => {
                for (index, child) in array.iter().enumerate() {
                    let next = format!("{pointer}/{index}");
                    visit(child, &next, pointers);
                }
            }
            _ => {}
        }
    }

    let mut pointers = Vec::new();
    visit(value, "", &mut pointers);
    pointers
}

fn assert_exact_pointer_inventory(label: &str, actual: &[String], expected: Vec<String>) {
    let actual_set = actual.iter().cloned().collect::<BTreeSet<_>>();
    let expected_set = expected.into_iter().collect::<BTreeSet<_>>();
    assert_eq!(
        actual.len(),
        actual_set.len(),
        "{label} contains duplicates"
    );
    assert_eq!(
        actual_set, expected_set,
        "{label} drifted from Rust authority"
    );
}

fn compatibility_paths(prefix: &str) -> Vec<String> {
    [
        "claude_version",
        "os",
        "arch",
        "terminal_profile",
        "input_transport",
        "tested",
        "transcript_drain_ms",
    ]
    .into_iter()
    .map(|field| format!("{prefix}/{field}"))
    .collect()
}

fn token_usage_paths(prefix: &str) -> Vec<String> {
    [
        "input_tokens",
        "output_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ]
    .into_iter()
    .map(|field| format!("{prefix}/{field}"))
    .collect()
}

fn snapshot_required_paths(prefix: &str, populated_optionals: bool) -> Vec<String> {
    let mut paths = [
        "session_id",
        "generation_id",
        "transcript_session_id",
        "cell",
        "state",
        "cwd",
        "compatibility",
        "created_at_ms",
        "updated_at_ms",
        "resumable",
        "last_sequence",
    ]
    .into_iter()
    .map(|field| format!("{prefix}/{field}"))
    .collect::<Vec<_>>();
    paths.extend(compatibility_paths(&format!("{prefix}/compatibility")));
    if populated_optionals {
        paths.extend(
            ["turn_id", "outcome", "completed_at_ms", "final_sequence"]
                .into_iter()
                .map(|field| format!("{prefix}/last_turn/{field}")),
        );
        paths.extend(
            ["kind", "message"]
                .into_iter()
                .map(|field| format!("{prefix}/needs_input/{field}")),
        );
    }
    paths
}

fn turn_result_required_paths(prefix: &str, full_optionals: bool) -> Vec<String> {
    let mut paths = [
        "session_id",
        "generation_id",
        "turn_id",
        "outcome",
        "text",
        "usage",
        "timings",
        "claude_version",
        "compatibility",
        "completion",
        "final_sequence",
    ]
    .into_iter()
    .map(|field| format!("{prefix}/{field}"))
    .collect::<Vec<_>>();
    for category in ["main", "sidechain", "combined"] {
        paths.push(format!("{prefix}/usage/{category}"));
        paths.extend(token_usage_paths(&format!("{prefix}/usage/{category}")));
    }
    paths.extend(
        ["submitted_at_ms", "completed_at_ms"]
            .into_iter()
            .map(|field| format!("{prefix}/timings/{field}")),
    );
    paths.extend(compatibility_paths(&format!("{prefix}/compatibility")));
    paths.extend(
        [
            "authority",
            "prompt_acknowledged",
            "terminal_message_observed",
            "terminal_prompt_observed",
            "terminal_quiet_observed",
            "transcript_drained",
            "lifecycle_hook_observed",
        ]
        .into_iter()
        .map(|field| format!("{prefix}/completion/{field}")),
    );
    paths.extend(
        ["kind", "text"]
            .into_iter()
            .map(|field| format!("{prefix}/final_blocks/0/{field}")),
    );
    if full_optionals {
        paths.extend(
            ["kind", "id", "name", "input"]
                .into_iter()
                .map(|field| format!("{prefix}/final_blocks/1/{field}")),
        );
        paths.extend(
            ["kind", "tool_use_id", "content", "is_error"]
                .into_iter()
                .map(|field| format!("{prefix}/final_blocks/2/{field}")),
        );
        paths.extend(
            ["kind", "block_type", "data"]
                .into_iter()
                .map(|field| format!("{prefix}/final_blocks/3/{field}")),
        );
        paths.extend(
            ["tool_use_id", "name", "input", "status"]
                .into_iter()
                .map(|field| format!("{prefix}/tools/0/{field}")),
        );
        paths.extend(
            ["code", "message"]
                .into_iter()
                .map(|field| format!("{prefix}/warnings/0/{field}")),
        );
        paths.push(format!("{prefix}/stop_reason/kind"));
    }
    paths
}

fn result_required_paths(result_type: &str) -> Vec<String> {
    let prefix = "/result/data";
    let direct = |fields: &[&str]| {
        fields
            .iter()
            .map(|field| format!("{prefix}/{field}"))
            .collect::<Vec<_>>()
    };
    match result_type {
        "pong" => direct(&["server_version", "protocol_version"]),
        "session_started" => {
            let mut paths = direct(&[
                "session_id",
                "generation_id",
                "state",
                "compatibility",
                "created_at_ms",
                "last_sequence",
            ]);
            paths.extend(compatibility_paths(&format!("{prefix}/compatibility")));
            paths
        }
        "turn_accepted" => direct(&[
            "session_id",
            "generation_id",
            "turn_id",
            "replayed",
            "state",
            "next_sequence",
        ]),
        "turn_cancelled" => direct(&[
            "session_id",
            "generation_id",
            "turn_id",
            "outcome",
            "session_state",
        ]),
        "session_snapshot" => snapshot_required_paths(prefix, true),
        "attach_capability" => direct(&[
            "session_id",
            "generation_id",
            "token",
            "endpoint",
            "expires_at_ms",
            "read_only",
        ]),
        "session_closed" => direct(&[
            "session_id",
            "generation_id",
            "already_closed",
            "process_reaped",
        ]),
        "events" => direct(&[
            "next_sequence",
            "events/0/schema_version",
            "events/0/session_id",
            "events/0/generation_id",
            "events/0/sequence",
            "events/0/timestamp_ms",
            "events/0/event",
            "events/0/event/type",
            "events/0/event/data",
            "events/0/event/data/session_state",
        ]),
        "turn_result" => turn_result_required_paths(prefix, true),
        "session_cleared" => direct(&[
            "session_id",
            "generation_id",
            "transcript_session_id",
            "rotated",
            "state",
        ]),
        // `state` and `private_terminal_present` are deliberately absent: both
        // are omitted whenever the observation behind them was not made, and a
        // reader must never be able to mistake an omitted observation for a
        // negative one.
        "diagnosis" => direct(&[
            "runtime",
            "runtime/outcome",
            "runtime/finding",
            "runtime/elapsed_ms",
            "sessions",
            "sessions/0/session_id",
            "sessions/0/generation_id",
            "sessions/0/outcome",
            "sessions/0/finding",
        ]),
        // `reported_model` and `effort` are deliberately absent: the first is
        // what the transcript said and is omitted when it said nothing, and the
        // second is omitted when the resolved model takes no depth setting.
        // `stop_reason` is optional as a whole; its `kind` is required whenever
        // the object is present, which is the same rule `turn_result` follows.
        "stateless_result" => {
            let mut paths = direct(&["model", "text", "usage", "claude_version"]);
            for scope in ["main", "sidechain", "combined"] {
                paths.push(format!("{prefix}/usage/{scope}"));
                paths.extend(token_usage_paths(&format!("{prefix}/usage/{scope}")));
            }
            paths.push(format!("{prefix}/stop_reason/kind"));
            paths
        }
        // Nothing INSIDE `spec` is required, and that is not an omission:
        // `spec` is opaque on a response, echoed for the caller to decode with
        // the strict `AgentSpec` type, so requiring a key here would state a
        // rule this decoder does not enforce. `config_digest` IS required: it
        // is the identity of the configuration and a descriptor without one
        // names nothing.
        "agent_created" | "agent" | "agent_updated" => direct(&[
            "agent_id",
            "version",
            "config_digest",
            "spec",
            "created_at_ms",
            "updated_at_ms",
        ]),
        // `agents` itself is omitted when empty, so it cannot be required; each
        // summary that IS present must be complete.
        "agent_list" => direct(&[
            "agents/0/agent_id",
            "agents/0/version",
            "agents/0/config_digest",
            "agents/0/name",
            "agents/0/cell",
            "agents/0/updated_at_ms",
        ]),
        other => panic!("unexpected golden result {other}"),
    }
}

fn event_required_paths(event_type: &str) -> Vec<String> {
    let prefix = "/event/data";
    let direct = |fields: &[&str]| {
        fields
            .iter()
            .map(|field| format!("{prefix}/{field}"))
            .collect::<Vec<_>>()
    };
    match event_type {
        "session_state_changed" => direct(&["previous", "current"]),
        "prompt_acknowledged" => direct(&["prompt_uuid", "transcript_offset"]),
        "logical_message" => {
            let mut paths = direct(&[
                "message_id",
                "scope",
                "blocks",
                "terminal",
                "blocks/0/kind",
                "blocks/0/text",
                "blocks/1/kind",
                "blocks/1/id",
                "blocks/1/name",
                "blocks/1/input",
                "blocks/2/kind",
                "blocks/2/tool_use_id",
                "blocks/2/content",
                "blocks/2/is_error",
                "blocks/3/kind",
                "blocks/3/block_type",
                "blocks/3/data",
                "stop_reason/kind",
            ]);
            paths.extend(token_usage_paths(&format!("{prefix}/usage")));
            paths
        }
        "tool_started" => direct(&["tool_use_id", "name", "input"]),
        "tool_completed" => direct(&["tool_use_id", "output", "is_error"]),
        "rate_limit" => direct(&["status"]),
        "needs_input" => direct(&["kind", "message"]),
        "terminal_candidate" => direct(&["message_id", "stop_reason/kind"]),
        "turn_completed" => turn_result_required_paths(prefix, false),
        "turn_cancelled" => direct(&["outcome", "recovered_to_ready"]),
        "turn_failed" => direct(&["code", "message", "retryable"]),
        "warning" => direct(&["code", "message"]),
        "replay_gap" => {
            let mut paths = direct(&[
                "requested_after",
                "oldest_available",
                "next_sequence",
                "snapshot",
            ]);
            paths.extend(snapshot_required_paths(
                &format!("{prefix}/snapshot"),
                false,
            ));
            paths
        }
        "heartbeat" => direct(&["session_state"]),
        other => panic!("unexpected golden event {other}"),
    }
}

#[test]
fn shared_golden_frames_are_exact_rust_v1_values() {
    let golden = golden();
    let manifest_methods = manifest_methods();
    let manifest_events = manifest_events();
    assert_eq!(golden.schema_version, 1);
    for id in [
        golden.ids.request_id,
        golden.ids.session_id,
        golden.ids.generation_id,
        golden.ids.turn_id,
        golden.ids.other_id,
        golden.ids.rotated_transcript_session_id,
    ] {
        Uuid::parse_str(&id).unwrap();
    }

    let mut methods = BTreeSet::new();
    let mut results = BTreeSet::new();
    for exchange in golden.requests_and_results {
        assert!(methods.insert(exchange.method.clone()));
        let request: RequestEnvelope = serde_json::from_value(exchange.request.clone())
            .unwrap_or_else(|error| panic!("golden request {}: {error}", exchange.method));
        assert_eq!(serde_json::to_value(request).unwrap(), exchange.request);

        let response: ResponseEnvelope = serde_json::from_value(exchange.response.clone())
            .unwrap_or_else(|error| panic!("golden response {}: {error}", exchange.method));
        assert_eq!(serde_json::to_value(response).unwrap(), exchange.response);
        assert!(
            results.insert(
                exchange.response["result"]["type"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            )
        );
    }
    // COMPARED TO THE SURFACE, NOT TO A NUMBER, and by NAME rather than by
    // count so the failure says which method is uncovered. See
    // [`manifest_methods`] for what a literal here cost.
    assert_eq!(
        methods,
        manifest_methods.iter().cloned().collect::<BTreeSet<_>>(),
        "golden.json must carry one complete request/result pair for every method in \
         manifest.json, which is exactly what tests/conformance/v1/README.md promises"
    );
    // Each method's result type is distinct, so covering every method covers
    // every result.
    assert_eq!(results.len(), methods.len());

    let mut event_types = BTreeSet::new();
    for event in golden.events {
        assert!(event_types.insert(event.event_type.clone()));
        let frame: EventEnvelope = serde_json::from_value(event.frame.clone())
            .unwrap_or_else(|error| panic!("golden event {}: {error}", event.event_type));
        assert_eq!(serde_json::to_value(frame).unwrap(), event.frame);
    }
    // COMPARED TO THE SURFACE, NOT TO A NUMBER, exactly as the methods above
    // are, and by NAME so the failure says which event is uncovered. See
    // [`manifest_events`] for what the literal `14` here cost.
    assert_eq!(
        event_types,
        manifest_events.iter().cloned().collect::<BTreeSet<_>>(),
        "golden.json must carry one complete frame for every event in manifest.json, which is \
         exactly what tests/conformance/v1/README.md promises"
    );

    let error: ResponseEnvelope = serde_json::from_value(golden.error.clone()).unwrap();
    assert_eq!(serde_json::to_value(error).unwrap(), golden.error);
}

#[test]
fn every_strict_request_object_pointer_rejects_an_additive_field() {
    let golden = golden();
    let shared = cases();
    assert_eq!(
        shared.strict_request_object_pointers.len(),
        manifest_methods().len()
    );
    assert_eq!(
        shared
            .strict_request_object_pointers
            .iter()
            .map(|entry| entry.pointers.len())
            .sum::<usize>(),
        // 44 before `diagnose`. Like `ping` it is a bare method, so its only
        // strict object is the envelope itself. 45 before `run_stateless`,
        // whose two are the envelope and its own `params`. 47 before the four
        // agent methods, which add 28: twelve each for `create_agent` and
        // `update_agent` (envelope, `params`, and the ten strict objects of an
        // `AgentSpec`) and two each for `get_agent` and `list_agents`.
        77,
        "the reviewed strict request-object inventory changed"
    );

    let mut methods = BTreeSet::new();
    for entry in shared.strict_request_object_pointers {
        assert!(methods.insert(entry.method.clone()), "duplicate method");
        let exchange = golden
            .requests_and_results
            .iter()
            .find(|exchange| exchange.method == entry.method)
            .unwrap_or_else(|| panic!("strict request method {} has no golden", entry.method));
        let mut pointers = BTreeSet::new();
        for pointer in entry.pointers {
            assert!(pointers.insert(pointer.clone()), "duplicate {pointer:?}");
            let mut mutated = exchange.request.clone();
            insert_object_field(
                &mut mutated,
                &pointer,
                "future_request_field",
                Value::Bool(true),
            );
            assert!(
                serde_json::from_value::<RequestEnvelope>(mutated).is_err(),
                "{} accepted an unknown field at strict object {pointer:?}",
                entry.method
            );
        }
    }
    assert_eq!(methods.len(), golden.requests_and_results.len());

    assert_eq!(
        shared.strict_request_unit_variant_mutations.len(),
        3,
        "all internally tagged request unit variants require explicit mutations"
    );
    let mut variant_ids = BTreeSet::new();
    for variant in shared.strict_request_unit_variant_mutations {
        assert!(variant_ids.insert(variant.id.clone()), "duplicate variant");
        let exchange = golden
            .requests_and_results
            .iter()
            .find(|exchange| exchange.method == variant.method)
            .unwrap_or_else(|| panic!("strict request variant {} has no golden", variant.id));
        let mut mutated = exchange.request.clone();
        set_pointer(&mut mutated, &variant.pointer, variant.replacement);
        assert!(
            serde_json::from_value::<RequestEnvelope>(mutated.clone()).is_ok(),
            "{} baseline unit variant must remain a valid request",
            variant.id
        );
        insert_object_field(
            &mut mutated,
            &variant.pointer,
            "future_request_field",
            Value::Bool(true),
        );
        assert!(
            serde_json::from_value::<RequestEnvelope>(mutated).is_err(),
            "{} accepted an unknown field on its unit variant",
            variant.id
        );
    }
}

#[test]
fn shared_required_field_inventory_exactly_matches_rust_authority() {
    let golden = golden();
    let shared = cases().client_required_field_deletions;
    assert_exact_pointer_inventory(
        "result envelope",
        &shared.result_envelope,
        [
            "/version",
            "/request_id",
            "/result",
            "/result/type",
            "/result/data",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
    );
    assert_eq!(shared.results.len(), golden.requests_and_results.len());
    let mut methods = BTreeSet::new();
    for fields in &shared.results {
        assert!(methods.insert(fields.method.clone()), "duplicate method");
        let exchange = golden
            .requests_and_results
            .iter()
            .find(|exchange| exchange.method == fields.method)
            .unwrap_or_else(|| panic!("required-field method {} has no golden", fields.method));
        let result_type = exchange.response["result"]["type"].as_str().unwrap();
        assert_exact_pointer_inventory(
            &format!("result {result_type}"),
            &fields.pointers,
            result_required_paths(result_type),
        );
    }
    assert_eq!(methods.len(), golden.requests_and_results.len());

    assert_exact_pointer_inventory(
        "event envelope",
        &shared.event_envelope,
        [
            "/schema_version",
            "/session_id",
            "/generation_id",
            "/sequence",
            "/timestamp_ms",
            "/event",
            "/event/type",
            "/event/data",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
    );
    assert_eq!(shared.events.len(), golden.events.len());
    let mut event_types = BTreeSet::new();
    for fields in &shared.events {
        assert!(
            event_types.insert(fields.event_type.clone()),
            "duplicate event type"
        );
        assert!(
            golden
                .events
                .iter()
                .any(|event| event.event_type == fields.event_type),
            "required-field event {} has no golden",
            fields.event_type
        );
        assert_exact_pointer_inventory(
            &format!("event {}", fields.event_type),
            &fields.pointers,
            event_required_paths(&fields.event_type),
        );
    }
    assert_eq!(event_types.len(), golden.events.len());

    assert_exact_pointer_inventory(
        "error response",
        &shared.error,
        [
            "/version",
            "/request_id",
            "/error",
            "/error/code",
            "/error/message",
            "/error/retryable",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
    );
}

#[test]
fn every_golden_result_event_and_error_rejects_the_shared_required_field_inventory() {
    let golden = golden();
    let shared = cases().client_required_field_deletions;
    for fields in &shared.results {
        let exchange = golden
            .requests_and_results
            .iter()
            .find(|exchange| exchange.method == fields.method)
            .unwrap();
        for pointer in shared.result_envelope.iter().chain(&fields.pointers) {
            let mut missing = exchange.response.clone();
            assert!(
                remove_pointer(&mut missing, pointer).is_some(),
                "{} pointer {pointer} is not represented by its full golden",
                fields.method
            );
            assert!(
                serde_json::from_value::<ResponseEnvelope>(missing).is_err(),
                "{} accepted deletion of required field {pointer}",
                fields.method
            );
        }
    }

    for fields in &shared.events {
        let event = golden
            .events
            .iter()
            .find(|event| event.event_type == fields.event_type)
            .unwrap();
        for pointer in shared.event_envelope.iter().chain(&fields.pointers) {
            let mut missing = event.frame.clone();
            assert!(
                remove_pointer(&mut missing, pointer).is_some(),
                "{} pointer {pointer} is not represented by its full golden",
                event.event_type
            );
            assert!(
                serde_json::from_value::<EventEnvelope>(missing).is_err(),
                "{} accepted deletion of required field {pointer}",
                event.event_type
            );
        }
    }

    for pointer in &shared.error {
        let mut missing = golden.error.clone();
        assert!(remove_pointer(&mut missing, pointer).is_some());
        assert!(serde_json::from_value::<ResponseEnvelope>(missing).is_err());
    }
}

#[test]
fn every_golden_result_event_and_error_accepts_additions_at_every_object_boundary() {
    let golden = golden();
    let mut result_boundaries = 0;
    for exchange in &golden.requests_and_results {
        for pointer in object_pointers(&exchange.response) {
            result_boundaries += 1;
            let mut additive = exchange.response.clone();
            insert_object_field(
                &mut additive,
                &pointer,
                "future_minor_field",
                serde_json::json!({"opaque": true}),
            );
            assert!(
                serde_json::from_value::<ResponseEnvelope>(additive).is_ok(),
                "{} rejected an additive field at {pointer:?}",
                exchange.method
            );
        }
    }
    // 58 before `diagnose`; its exchange adds six object boundaries -- the
    // envelope, `result`, `result/data`, `result/data/runtime`, and one per
    // entry of `result/data/sessions`. 64 before `run_stateless`, whose
    // exchange adds eight -- the envelope, `result`, `result/data`,
    // `result/data/stop_reason`, `result/data/usage`, and one per usage scope.
    // 72 before the agent methods, whose four exchanges add 46: fourteen for
    // each of the three descriptors -- the envelope, `result`, `result/data`,
    // and the eleven objects of the echoed `spec`, which is OPAQUE on a
    // response and therefore additive like every other result boundary -- plus
    // `agent_list`'s envelope, `result`, `result/data` and its one summary.
    assert_eq!(
        result_boundaries, 118,
        "review new result object boundaries"
    );

    let mut event_boundaries = 0;
    for event in &golden.events {
        for pointer in object_pointers(&event.frame) {
            event_boundaries += 1;
            let mut additive = event.frame.clone();
            insert_object_field(
                &mut additive,
                &pointer,
                "future_minor_field",
                serde_json::json!({"opaque": true}),
            );
            assert!(
                serde_json::from_value::<EventEnvelope>(additive).is_ok(),
                "{} rejected an additive field at {pointer:?}",
                event.event_type
            );
        }
    }
    assert_eq!(event_boundaries, 67, "review new event object boundaries");

    let error_pointers = object_pointers(&golden.error);
    assert_eq!(
        error_pointers.len(),
        3,
        "review new error object boundaries"
    );
    for pointer in error_pointers {
        let mut additive = golden.error.clone();
        insert_object_field(
            &mut additive,
            &pointer,
            "future_minor_field",
            serde_json::json!({"opaque": true}),
        );
        assert!(
            serde_json::from_value::<ResponseEnvelope>(additive).is_ok(),
            "error response rejected an additive field at {pointer:?}"
        );
    }
}

#[test]
fn reserved_turn_lease_vectors_decode_as_valid_requests() {
    let golden = golden();
    let reserved = cases().reserved_turn_lease_cases;
    assert_eq!(reserved.expected_error.code, "unsupported_feature");
    assert!(!reserved.expected_error.retryable);
    assert_eq!(reserved.cases.len(), 6);

    for case in reserved.cases {
        let mut request = golden
            .requests_and_results
            .iter()
            .find(|exchange| exchange.method == case.operation)
            .unwrap_or_else(|| panic!("{} has no request golden", case.id))
            .request
            .clone();
        set_pointer(&mut request, "/params/turn/lease", case.lease.clone());
        let decoded = serde_json::from_value::<RequestEnvelope>(request).unwrap_or_else(|error| {
            panic!("{} must decode before service rejection: {error}", case.id)
        });
        let encoded = serde_json::to_value(decoded).unwrap();
        assert_eq!(
            encoded["params"]["turn"]["lease"], case.lease,
            "{} lease changed while decoding",
            case.id
        );
    }
}

#[derive(Clone, Copy)]
enum NumericDocument {
    Request(&'static str),
    Response(&'static str),
    Event(&'static str),
}

#[derive(Clone, Copy)]
struct NumericField {
    document: NumericDocument,
    pointer: &'static str,
    maximum: u64,
}

#[test]
fn safe_integer_wire_field_inventory_enforces_inclusive_bounds() {
    let inventory = [
        NumericField {
            document: NumericDocument::Request("start_session"),
            pointer: "/params/lifecycle/hook_timeout_ms",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Request("start_session"),
            pointer: "/params/retention/idle_ttl_ms",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Request("run_turn"),
            pointer: "/params/turn/deadline_unix_ms",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Request("run_turn"),
            pointer: "/params/turn/lease/heartbeat_timeout_ms",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Request("subscribe_events"),
            pointer: "/params/after_sequence",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Request("subscribe_events"),
            pointer: "/params/wait_ms",
            maximum: 30_000,
        },
        NumericField {
            document: NumericDocument::Request("subscribe_events"),
            pointer: "/params/max_events",
            maximum: 512,
        },
        NumericField {
            document: NumericDocument::Request("run_once"),
            pointer: "/params/session/lifecycle/hook_timeout_ms",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Request("run_once"),
            pointer: "/params/turn/deadline_unix_ms",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("start_session"),
            pointer: "/result/data/created_at_ms",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("start_session"),
            pointer: "/result/data/last_sequence",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("start_session"),
            pointer: "/result/data/compatibility/transcript_drain_ms",
            maximum: 60_000,
        },
        NumericField {
            document: NumericDocument::Response("run_turn"),
            pointer: "/result/data/next_sequence",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("inspect_session"),
            pointer: "/result/data/created_at_ms",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("inspect_session"),
            pointer: "/result/data/updated_at_ms",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("inspect_session"),
            pointer: "/result/data/idle_deadline_ms",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("inspect_session"),
            pointer: "/result/data/last_sequence",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("inspect_session"),
            pointer: "/result/data/last_turn/completed_at_ms",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("inspect_session"),
            pointer: "/result/data/last_turn/final_sequence",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("attach_session"),
            pointer: "/result/data/expires_at_ms",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("run_once"),
            pointer: "/result/data/usage/main/input_tokens",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("run_once"),
            pointer: "/result/data/usage/main/output_tokens",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("run_once"),
            pointer: "/result/data/usage/main/cache_creation_input_tokens",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("run_once"),
            pointer: "/result/data/usage/main/cache_read_input_tokens",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("run_once"),
            pointer: "/result/data/usage/sidechain/input_tokens",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("run_once"),
            pointer: "/result/data/usage/sidechain/output_tokens",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("run_once"),
            pointer: "/result/data/usage/sidechain/cache_creation_input_tokens",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("run_once"),
            pointer: "/result/data/usage/sidechain/cache_read_input_tokens",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("run_once"),
            pointer: "/result/data/usage/combined/input_tokens",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("run_once"),
            pointer: "/result/data/usage/combined/output_tokens",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("run_once"),
            pointer: "/result/data/usage/combined/cache_creation_input_tokens",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("run_once"),
            pointer: "/result/data/usage/combined/cache_read_input_tokens",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("run_once"),
            pointer: "/result/data/timings/submitted_at_ms",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("run_once"),
            pointer: "/result/data/timings/prompt_acknowledged_at_ms",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("run_once"),
            pointer: "/result/data/timings/terminal_candidate_at_ms",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("run_once"),
            pointer: "/result/data/timings/completed_at_ms",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("run_once"),
            pointer: "/result/data/timings/drain_ms",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("run_once"),
            pointer: "/result/data/timings/last_transcript_activity_at_ms",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("run_once"),
            pointer: "/result/data/timings/stop_hook_at_ms",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("run_once"),
            pointer: "/result/data/tools/0/started_at_ms",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("run_once"),
            pointer: "/result/data/tools/0/completed_at_ms",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("run_once"),
            pointer: "/result/data/final_sequence",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Response("subscribe_events"),
            pointer: "/result/data/next_sequence",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Event("prompt_acknowledged"),
            pointer: "/event/data/transcript_offset",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Event("rate_limit"),
            pointer: "/event/data/resets_at_ms",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Event("heartbeat"),
            pointer: "/sequence",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Event("heartbeat"),
            pointer: "/timestamp_ms",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Event("replay_gap"),
            pointer: "/event/data/requested_after",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Event("replay_gap"),
            pointer: "/event/data/oldest_available",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Event("replay_gap"),
            pointer: "/event/data/next_sequence",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
        NumericField {
            document: NumericDocument::Event("replay_gap"),
            pointer: "/event/data/snapshot/last_sequence",
            maximum: MAX_SAFE_JSON_INTEGER,
        },
    ];
    assert_eq!(
        inventory.len(),
        51,
        "the reviewed numeric inventory changed"
    );

    let golden = golden();
    for field in inventory {
        let (mut value, label): (Value, String) = match field.document {
            NumericDocument::Request(method) => (
                golden
                    .requests_and_results
                    .iter()
                    .find(|exchange| exchange.method == method)
                    .unwrap()
                    .request
                    .clone(),
                format!("request {method}"),
            ),
            NumericDocument::Response(method) => (
                golden
                    .requests_and_results
                    .iter()
                    .find(|exchange| exchange.method == method)
                    .unwrap()
                    .response
                    .clone(),
                format!("response {method}"),
            ),
            NumericDocument::Event(event_type) => (
                golden
                    .events
                    .iter()
                    .find(|event| event.event_type == event_type)
                    .unwrap()
                    .frame
                    .clone(),
                format!("event {event_type}"),
            ),
        };
        set_pointer(&mut value, field.pointer, Value::from(field.maximum));
        let accepted = match field.document {
            NumericDocument::Request(_) => serde_json::from_value::<RequestEnvelope>(value.clone())
                .and_then(|decoded| serde_json::to_value(decoded).map(|_| ())),
            NumericDocument::Response(_) => {
                serde_json::from_value::<ResponseEnvelope>(value.clone())
                    .and_then(|decoded| serde_json::to_value(decoded).map(|_| ()))
            }
            NumericDocument::Event(_) => serde_json::from_value::<EventEnvelope>(value.clone())
                .and_then(|decoded| serde_json::to_value(decoded).map(|_| ())),
        };
        assert!(
            accepted.is_ok(),
            "{label} {} rejected its inclusive maximum {}: {accepted:?}",
            field.pointer,
            field.maximum
        );

        set_pointer(&mut value, field.pointer, Value::from(field.maximum + 1));
        let rejected = match field.document {
            NumericDocument::Request(_) => {
                serde_json::from_value::<RequestEnvelope>(value).is_err()
            }
            NumericDocument::Response(_) => {
                serde_json::from_value::<ResponseEnvelope>(value).is_err()
            }
            NumericDocument::Event(_) => serde_json::from_value::<EventEnvelope>(value).is_err(),
        };
        assert!(
            rejected,
            "{label} {} accepted one past maximum {}",
            field.pointer, field.maximum
        );
    }
}

#[test]
fn rust_recomputes_every_shared_durable_uuid_v5() {
    let durable = golden().durable_ids;
    let namespace = Uuid::parse_str(&durable.namespace).unwrap();
    let mut attempts = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for case in durable.cases {
        assert!(attempts.insert(case.attempt.clone()));
        let recomputed = uuid_v5(namespace, case.attempt.as_bytes());
        assert_eq!(recomputed.to_string(), case.turn_id, "{}", case.attempt);
        assert!(ids.insert(recomputed));
    }
}

fn uuid_v5(namespace: Uuid, name: &[u8]) -> Uuid {
    let mut material = Vec::with_capacity(16 + name.len());
    material.extend_from_slice(namespace.as_bytes());
    material.extend_from_slice(name);
    let digest = sha1(&material);
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

fn sha1(input: &[u8]) -> [u8; 20] {
    let bit_len = (input.len() as u64) * 8;
    let mut padded = input.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    let mut state = [
        0x6745_2301_u32,
        0xefcd_ab89,
        0x98ba_dcfe,
        0x1032_5476,
        0xc3d2_e1f0,
    ];
    for chunk in padded.chunks_exact(64) {
        let mut words = [0_u32; 80];
        for (index, word) in words[..16].iter_mut().enumerate() {
            let offset = index * 4;
            *word = u32::from_be_bytes(chunk[offset..offset + 4].try_into().unwrap());
        }
        for index in 16..80 {
            words[index] =
                (words[index - 3] ^ words[index - 8] ^ words[index - 14] ^ words[index - 16])
                    .rotate_left(1);
        }

        let [mut a, mut b, mut c, mut d, mut e] = state;
        for (index, word) in words.into_iter().enumerate() {
            let (function, constant) = match index {
                0..=19 => ((b & c) | ((!b) & d), 0x5a82_7999),
                20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
                _ => (b ^ c ^ d, 0xca62_c1d6),
            };
            let next = a
                .rotate_left(5)
                .wrapping_add(function)
                .wrapping_add(e)
                .wrapping_add(constant)
                .wrapping_add(word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = next;
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
    }

    let mut digest = [0_u8; 20];
    for (index, word) in state.into_iter().enumerate() {
        digest[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    digest
}
