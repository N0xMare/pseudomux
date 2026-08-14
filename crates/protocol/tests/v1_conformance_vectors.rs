use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use pseudomux_protocol::v1::{
    AuthPolicy, CancelOutcome, ClosePolicy, CompatibilityPolicy, CompletionAuthority, ConfigSource,
    DisconnectAction, EffortLevel, ErrorBody, ErrorCode, EventBatch, EventEnvelope, EventPayload,
    HealthLayerName, InputTransport, LayerFinding, LifecycleMode, MessageBlock, MessageScope,
    NeedsInputKind, PermissionMode, ProbeOutcome, RateLimitStatus, Request, RequestEnvelope,
    ResponseEnvelope, ResponseResult, RetentionPolicy, RuntimeFinding, SessionCell, SessionFinding,
    SessionIdentity, SessionState, StopReasonKind, SystemPromptPolicy, TerminalProfile, ToolStatus,
    TurnOutcome,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Deserialize)]
struct Manifest {
    schema_version: u16,
    protocol_version: u16,
    methods: Vec<String>,
    results: Vec<String>,
    events: Vec<String>,
    error_codes: Vec<String>,
}

#[derive(Deserialize)]
struct Cases {
    schema_version: u16,
    error_bodies: Vec<ErrorBodyCase>,
    replay_batches: Vec<ReplayBatchCase>,
    identities: Vec<IdentityCase>,
    nonstandard_json_constants: Vec<String>,
    numeric_boundaries: Vec<NumericBoundaryCase>,
}

#[derive(Deserialize)]
struct ErrorBodyCase {
    id: String,
    valid: bool,
    body: Value,
}

#[derive(Deserialize)]
struct ReplayBatchCase {
    id: String,
    valid: bool,
    requested_after: u64,
    oldest_available: u64,
    snapshot_last: u64,
    gap_next: u64,
    batch_next: u64,
    event_sequences: Vec<u64>,
}

#[derive(Deserialize)]
struct IdentityCase {
    id: String,
    valid: bool,
    value: String,
}

#[derive(Deserialize)]
struct NumericBoundaryCase {
    id: String,
    literal: String,
    protocol_owned_valid: bool,
    opaque_json_valid: bool,
}

fn vector_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/conformance/v1")
        .join(name)
}

fn read_json<T: for<'de> Deserialize<'de>>(name: &str) -> T {
    serde_json::from_slice(&std::fs::read(vector_path(name)).unwrap()).unwrap()
}

/// One tagged v1 enum, as the manifest sees it: the wire tag of every variant,
/// in declaration order, with the compiler enforcing that no variant is missing.
///
/// The `match` inside `exhaustive` is what makes this a proof. It has no
/// wildcard, so a variant added to the Rust enum and not listed here stops
/// compiling; and the tag beside each pattern is compared against the manifest
/// below, so a variant listed here and not in the manifest fails the assertion.
///
/// It exists because the lists it replaces were plain string literals with
/// nothing tying them to any type. `Request::Diagnose` and
/// `Request::RunStateless` were both appended on branches, and this test --
/// named `shared_manifest_matches_the_closed_v1_surface` -- passed with the
/// manifest three methods short of the surface, because the "closed v1 surface"
/// it compared against was a copy of the manifest with a different syntax. The
/// Python and TypeScript clients both pin against that manifest, so what the
/// name promised and what the predicate tested differed by exactly the methods
/// those clients could not see.
///
/// It carries a hand-written tag per variant, which is the one thing it does
/// NOT derive: renaming `Request::Ping`'s wire spelling while this file and the
/// manifest both keep saying `ping` is a change no assertion here would see.
/// It is confined to the three adjacently-tagged envelope enums, whose payloads
/// are entire request/result/event DTOs, so a sample per variant would be the
/// golden corpus written a second time. Every other v1 enum asks serde instead:
/// plain-string enums through [`wire_values!`], and the internally-tagged
/// unions -- whose payloads are one or two fields -- through [`wire_tagged!`].
macro_rules! wire_tags {
    ($ty:ty, [$($pattern:pat => $tag:expr),+ $(,)?]) => {{
        #[allow(unused)]
        fn exhaustive(value: &$ty) {
            match value {
                $($pattern => ()),+
            }
        }
        vec![$($tag.to_string()),+]
    }};
}

/// One plain-string v1 enum, as the manifest sees it. The `match` arm is what
/// makes this exhaustive: adding a variant to the Rust enum without adding it
/// here stops compiling, and adding it here without the manifest fails below.
///
/// It names only the variant, never its spelling: the string is whatever
/// `serde_json` actually emits. That is the difference from [`wire_tags!`], and
/// it is why the error-code list is here rather than there. Error codes were
/// the last manifest section pinned by hand -- a 34-element literal array beside
/// an exhaustive `error_code_name` table -- and both halves of that arrangement
/// were measured to fail. Appending a 35th `ErrorCode` forced one arm of the
/// table and nothing else: every protocol test binary stayed green with the
/// manifest still at 34. Renaming a variant forced the same one arm while its
/// string literal stayed, so the manifest kept the old spelling and the test
/// still passed with the wire saying something else. Both shipped clients throw
/// on a code they do not recognize and both pin only against this manifest, so
/// either mistake reached them as an unparseable frame masking the real error.
macro_rules! wire_values {
    ($ty:ty, [$($variant:path),+ $(,)?]) => {{
        fn exhaustive(value: &$ty) {
            match value {
                $($variant => ()),+
            }
        }
        let values: Vec<String> = vec![$({
            let value = $variant;
            exhaustive(&value);
            serde_json::to_value(value)
                .unwrap()
                .as_str()
                .expect("v1 value enums serialize as plain strings")
                .to_string()
        }),+];
        values
    }};
}

/// The wire image of one internally-tagged v1 union: the discriminant key, and
/// the spelling of every variant in declaration order. Both are read back out
/// of what `serde` emitted, never written here.
#[derive(Debug, Deserialize, PartialEq, Eq)]
struct TaggedUnion {
    tag: String,
    variants: Vec<String>,
}

/// Reduce one sample per variant, in declaration order, to that union's
/// [`TaggedUnion`].
///
/// The tag key is derived rather than named. A candidate is a key that every
/// variant carries, with a string value that no other variant repeats. serde's
/// internal tag always qualifies -- it is emitted for every variant of the enum
/// and two variants cannot spell it the same -- so the true tag is always among
/// the candidates, and this either finds it alone or panics on the ambiguity.
/// It never picks silently between two.
///
/// Each sample is also decoded back through the union's own `Deserialize` and
/// compared. That is not round-trip pedantry: `SystemPromptPolicy`,
/// `LifecycleMode` and `RetentionPolicy` hand-write `Deserialize` over a
/// private mirror enum, so a variant appended to the public enum and not to the
/// mirror serializes perfectly and can never be decoded -- a wire spelling the
/// daemon emits and refuses to read.
fn tagged_union<T>(samples: &[T]) -> TaggedUnion
where
    T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
{
    let objects: Vec<serde_json::Map<String, Value>> = samples
        .iter()
        .map(|sample| {
            let encoded = serde_json::to_value(sample).expect("a v1 union serializes");
            let decoded = serde_json::from_value::<T>(encoded.clone())
                .unwrap_or_else(|error| panic!("{encoded} does not decode back: {error}"));
            assert_eq!(&decoded, sample, "{encoded} decodes to a different value");
            match encoded {
                Value::Object(map) => map,
                other => panic!("an internally-tagged union serializes as an object, got {other}"),
            }
        })
        .collect();

    let candidates: Vec<&String> = objects[0]
        .keys()
        .filter(|key| {
            let spellings: BTreeSet<&str> = objects
                .iter()
                .filter_map(|object| object.get(*key).and_then(Value::as_str))
                .collect();
            spellings.len() == objects.len()
        })
        .collect();
    assert_eq!(
        candidates.len(),
        1,
        "no single key spells the discriminant; candidates {candidates:?} over {objects:?}. \
         Give the variants payloads that do not all carry the same distinct string field \
         (a union of one variant cannot be told apart from its payload at all)"
    );
    let tag = candidates[0].clone();

    let variants = objects
        .iter()
        .map(|object| {
            object[&tag]
                .as_str()
                .expect("the tag is a string")
                .to_string()
        })
        .collect();
    TaggedUnion { tag, variants }
}

/// One internally-tagged v1 union, as the manifest sees it, keyed by its own
/// type name.
///
/// This is [`wire_tags!`] with the hand-written tag taken out. Each variant is
/// named twice: once as a wildcard-free `match` pattern, so appending a variant
/// to the Rust enum stops this file compiling, and once as a *constructed
/// sample*, which is handed to `serde_json` and asked what it spells. The
/// `matches!` between them is what stops a sample drifting to a different
/// variant than the pattern it is filed under, so the two namings cannot
/// disagree about which variant they mean.
///
/// It exists because these six unions -- `ConfigSource`, `LifecycleMode`,
/// `MessageBlock`, `RetentionPolicy`, `SessionIdentity`, `SystemPromptPolicy`
/// -- were pinned by nothing at all. MEASURED, appending one payload-bearing
/// variant to each of the six left `cargo test -p pseudomux-protocol` green six
/// times out of six. The manifest is the only thing the TypeScript and Python
/// clients pin against, so a variant that never reaches it never reaches them
/// either: five of the six are request-side and would have been a shape neither
/// client can spell, and `MessageBlock` is response-side, where both clients'
/// validators throw on a `kind` they do not know.
///
/// [`wire_values!`] structurally could not do this job: it reads the whole
/// serialized value with `as_str`, which is `None` the moment a variant has a
/// payload. This reads one key out of the object instead, and derives which key
/// that is.
macro_rules! wire_tagged {
    ($ty:ty, [$($pattern:pat => $sample:expr),+ $(,)?]) => {{
        #[allow(unused)]
        fn exhaustive(value: &$ty) {
            match value {
                $($pattern => ()),+
            }
        }
        let union = tagged_union(&[$({
            let sample: $ty = $sample;
            assert!(
                matches!(&sample, $pattern),
                // Not a format string: `stringify!` of a struct-literal sample
                // is full of braces.
                "{}",
                concat!(
                    "wire_tagged!(", stringify!($ty), ") files ", stringify!($sample),
                    " under `", stringify!($pattern),
                    "`, which is a different variant, so the tag it contributes is not that \
                     pattern's"
                )
            );
            sample
        }),+]);
        (stringify!($ty).to_string(), union)
    }};
}

#[test]
fn shared_manifest_matches_the_closed_v1_surface() {
    let manifest: Manifest = read_json("manifest.json");
    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.protocol_version, 1);
    assert_eq!(
        manifest.methods,
        wire_tags!(
            Request,
            [
                Request::Ping => "ping",
                Request::StartSession(_) => "start_session",
                Request::RunTurn(_) => "run_turn",
                Request::CancelTurn(_) => "cancel_turn",
                Request::InspectSession(_) => "inspect_session",
                Request::AttachSession(_) => "attach_session",
                Request::CloseSession(_) => "close_session",
                Request::SubscribeEvents(_) => "subscribe_events",
                Request::RunOnce(_) => "run_once",
                Request::ClearSession(_) => "clear_session",
                Request::Diagnose => "diagnose",
                Request::RunStateless(_) => "run_stateless",
                Request::CreateAgent(_) => "create_agent",
                Request::GetAgent(_) => "get_agent",
                Request::ListAgents(_) => "list_agents",
                Request::UpdateAgent(_) => "update_agent",
            ]
        )
    );
    assert_eq!(
        manifest.results,
        wire_tags!(
            ResponseResult,
            [
                ResponseResult::Pong(_) => "pong",
                ResponseResult::SessionStarted(_) => "session_started",
                ResponseResult::TurnAccepted(_) => "turn_accepted",
                ResponseResult::TurnCancelled(_) => "turn_cancelled",
                ResponseResult::SessionSnapshot(_) => "session_snapshot",
                ResponseResult::AttachCapability(_) => "attach_capability",
                ResponseResult::SessionClosed(_) => "session_closed",
                ResponseResult::Events(_) => "events",
                ResponseResult::TurnResult(_) => "turn_result",
                ResponseResult::SessionCleared(_) => "session_cleared",
                ResponseResult::Diagnosis(_) => "diagnosis",
                ResponseResult::StatelessResult(_) => "stateless_result",
                ResponseResult::AgentCreated(_) => "agent_created",
                ResponseResult::Agent(_) => "agent",
                ResponseResult::AgentList(_) => "agent_list",
                ResponseResult::AgentUpdated(_) => "agent_updated",
            ]
        )
    );
    assert_eq!(
        manifest.events,
        wire_tags!(
            EventPayload,
            [
                EventPayload::SessionStateChanged { .. } => "session_state_changed",
                EventPayload::PromptAcknowledged { .. } => "prompt_acknowledged",
                EventPayload::LogicalMessage { .. } => "logical_message",
                EventPayload::ToolStarted { .. } => "tool_started",
                EventPayload::ToolCompleted { .. } => "tool_completed",
                EventPayload::RateLimit { .. } => "rate_limit",
                EventPayload::NeedsInput { .. } => "needs_input",
                EventPayload::TerminalCandidate { .. } => "terminal_candidate",
                EventPayload::TurnCompleted { .. } => "turn_completed",
                EventPayload::TurnCancelled { .. } => "turn_cancelled",
                EventPayload::TurnFailed { .. } => "turn_failed",
                EventPayload::Warning { .. } => "warning",
                EventPayload::ReplayGap { .. } => "replay_gap",
                EventPayload::Heartbeat { .. } => "heartbeat",
            ]
        )
    );

    assert_eq!(
        manifest.error_codes,
        wire_values!(
            ErrorCode,
            [
                ErrorCode::InvalidConfig,
                ErrorCode::UnsupportedFeature,
                ErrorCode::UnsupportedClaudeVersion,
                ErrorCode::ClaudeNotFound,
                ErrorCode::RmuxUnavailable,
                ErrorCode::RmuxIncompatible,
                ErrorCode::PersistenceDisabled,
                ErrorCode::TranscriptUnavailable,
                ErrorCode::SchemaDrift,
                ErrorCode::PromptNotAcknowledged,
                ErrorCode::ResultTooLarge,
                ErrorCode::TurnHistoryCapacityExceeded,
                ErrorCode::SessionBusy,
                ErrorCode::IdConflict,
                ErrorCode::IdCollision,
                ErrorCode::SessionNotFound,
                ErrorCode::StaleSessionGeneration,
                ErrorCode::NeedsTrust,
                ErrorCode::NeedsLogin,
                ErrorCode::NeedsPermission,
                ErrorCode::NeedsUpdate,
                ErrorCode::NeedsInput,
                ErrorCode::RateLimited,
                ErrorCode::AuthenticationFailed,
                ErrorCode::BillingFailed,
                ErrorCode::PermissionDenied,
                ErrorCode::TurnTimeout,
                ErrorCode::Cancelled,
                ErrorCode::RecoveryFailed,
                ErrorCode::ClaudeExited,
                ErrorCode::DaemonLost,
                ErrorCode::ReplayGap,
                ErrorCode::ProtocolVersionMismatch,
                ErrorCode::Internal,
            ]
        )
    );
}

#[test]
fn shared_error_replay_and_identity_cases_match_rust_v1_validation() {
    let cases: Cases = read_json("cases.json");
    assert_eq!(cases.schema_version, 1);

    for case in cases.error_bodies {
        assert_eq!(
            serde_json::from_value::<ErrorBody>(case.body).is_ok(),
            case.valid,
            "error-body vector {}",
            case.id
        );
    }

    for case in cases.replay_batches {
        let events = case
            .event_sequences
            .into_iter()
            .map(|sequence| {
                json!({
                    "schema_version": 1,
                    "session_id": "00000000-0000-4000-8000-000000000022",
                    "generation_id": "00000000-0000-4000-8000-000000000044",
                    "sequence": sequence,
                    "timestamp_ms": sequence,
                    "event": {"type": "heartbeat", "data": {"session_state": "ready"}}
                })
            })
            .collect::<Vec<_>>();
        let batch = json!({
            "events": events,
            "next_sequence": case.batch_next,
            "replay_gap": {
                "requested_after": case.requested_after,
                "oldest_available": case.oldest_available,
                "next_sequence": case.gap_next,
                "snapshot": {
                    "session_id": "00000000-0000-4000-8000-000000000022",
                    "generation_id": "00000000-0000-4000-8000-000000000044",
                    "transcript_session_id": "00000000-0000-4000-8000-000000000022",
                    "cell": "full",
                    "state": "ready",
                    "cwd": "/work/project",
                    "compatibility": {
                        "claude_version": "2.1.207",
                        "os": "macos",
                        "arch": "aarch64",
                        "terminal_profile": "transparent",
                        "input_transport": "sdk",
                        "tested": true,
                        "transcript_drain_ms": 750
                    },
                    "created_at_ms": 1,
                    "updated_at_ms": 2,
                    "resumable": true,
                    "last_sequence": case.snapshot_last
                }
            }
        });
        assert_eq!(
            serde_json::from_value::<EventBatch>(batch).is_ok(),
            case.valid,
            "replay vector {}",
            case.id
        );
    }

    for case in cases.identities {
        let request = json!({
            "version": 1,
            "request_id": case.value,
            "method": "run_turn",
            "params": {
                "session_id": case.value,
                "generation_id": case.value,
                "turn": {"turn_id": case.value, "prompt": "test"}
            }
        });
        let response = json!({
            "version": 1,
            "request_id": case.value,
            "result": {
                "type": "pong",
                "data": {"server_version": "test", "protocol_version": 1}
            }
        });
        let event = json!({
            "schema_version": 1,
            "session_id": case.value,
            "generation_id": case.value,
            "turn_id": case.value,
            "sequence": 1,
            "timestamp_ms": 1,
            "event": {"type": "heartbeat", "data": {"session_state": "ready"}}
        });
        assert_eq!(
            serde_json::from_value::<RequestEnvelope>(request).is_ok(),
            case.valid,
            "request identity vector {}",
            case.id
        );
        assert_eq!(
            serde_json::from_value::<ResponseEnvelope>(response).is_ok(),
            case.valid,
            "response identity vector {}",
            case.id
        );
        assert_eq!(
            serde_json::from_value::<EventEnvelope>(event).is_ok(),
            case.valid,
            "event identity vector {}",
            case.id
        );
    }

    assert_eq!(
        cases.nonstandard_json_constants,
        ["NaN", "Infinity", "-Infinity"]
    );
    for case in cases.numeric_boundaries {
        let protocol_owned = format!(
            r#"{{
                "schema_version": 1,
                "session_id": "00000000-0000-4000-8000-000000000022",
                "generation_id": "00000000-0000-4000-8000-000000000044",
                "sequence": {},
                "timestamp_ms": 1,
                "event": {{"type": "heartbeat", "data": {{"session_state": "ready"}}}}
            }}"#,
            case.literal
        );
        assert_eq!(
            serde_json::from_str::<EventEnvelope>(&protocol_owned).is_ok(),
            case.protocol_owned_valid,
            "protocol-owned numeric vector {}",
            case.id
        );

        let opaque = format!(
            r#"{{
                "version": 1,
                "request_id": "00000000-0000-4000-8000-000000000001",
                "method": "start_session",
                "params": {{
                    "identity": {{"mode": "new"}},
                    "cwd": "/work",
                    "claude": {{
                        "executable": "/claude",
                        "settings": [{{
                            "source": "inline",
                            "document": {{"nested": [{}]}}
                        }}]
                    }}
                }}
            }}"#,
            case.literal
        );
        assert_eq!(
            serde_json::from_str::<RequestEnvelope>(&opaque).is_ok(),
            case.opaque_json_valid,
            "opaque numeric vector {}",
            case.id
        );
    }
}

#[derive(Deserialize)]
struct ValueEnumManifest {
    value_enums: BTreeMap<String, Vec<String>>,
}

#[test]
fn shared_manifest_value_enums_match_the_rust_string_enums() {
    let manifest: ValueEnumManifest = read_json("manifest.json");

    let expected: BTreeMap<String, Vec<String>> = BTreeMap::from([
        (
            "AuthPolicy".to_string(),
            wire_values!(AuthPolicy, [AuthPolicy::Subscription, AuthPolicy::Inherit]),
        ),
        (
            "CancelOutcome".to_string(),
            wire_values!(
                CancelOutcome,
                [
                    CancelOutcome::Cancelled,
                    CancelOutcome::AlreadyTerminal,
                    CancelOutcome::RecoveryFailed,
                ]
            ),
        ),
        (
            "ClosePolicy".to_string(),
            wire_values!(ClosePolicy, [ClosePolicy::Graceful, ClosePolicy::Force]),
        ),
        (
            "CompatibilityPolicy".to_string(),
            wire_values!(
                CompatibilityPolicy,
                [
                    CompatibilityPolicy::RequireTested,
                    CompatibilityPolicy::AllowUntested,
                ]
            ),
        ),
        (
            "CompletionAuthority".to_string(),
            wire_values!(CompletionAuthority, [CompletionAuthority::Transcript]),
        ),
        (
            "DisconnectAction".to_string(),
            wire_values!(
                DisconnectAction,
                [
                    DisconnectAction::Continue,
                    DisconnectAction::CancelTurn,
                    DisconnectAction::CloseSession,
                ]
            ),
        ),
        (
            "EffortLevel".to_string(),
            wire_values!(
                EffortLevel,
                [
                    EffortLevel::Low,
                    EffortLevel::Medium,
                    EffortLevel::High,
                    EffortLevel::XHigh,
                    EffortLevel::Max,
                ]
            ),
        ),
        (
            "HealthLayerName".to_string(),
            wire_values!(
                HealthLayerName,
                [
                    HealthLayerName::Configuration,
                    HealthLayerName::ControlPlane,
                    HealthLayerName::PrivateRuntime,
                    HealthLayerName::LaunchBroker,
                    HealthLayerName::CompatibilityProfile,
                    HealthLayerName::Pool,
                    HealthLayerName::Sessions,
                    HealthLayerName::Performance,
                ]
            ),
        ),
        (
            "InputTransport".to_string(),
            wire_values!(
                InputTransport,
                [
                    InputTransport::Auto,
                    InputTransport::Sdk,
                    InputTransport::AttachedStream,
                ]
            ),
        ),
        (
            "LayerFinding".to_string(),
            wire_values!(
                LayerFinding,
                [
                    LayerFinding::Exercised,
                    LayerFinding::Faulted,
                    LayerFinding::NothingToExercise,
                    LayerFinding::NotEstablished,
                ]
            ),
        ),
        (
            "MessageScope".to_string(),
            wire_values!(
                MessageScope,
                [
                    MessageScope::Main,
                    MessageScope::Sidechain,
                    MessageScope::Team,
                    MessageScope::Metadata,
                ]
            ),
        ),
        (
            "NeedsInputKind".to_string(),
            wire_values!(
                NeedsInputKind,
                [
                    NeedsInputKind::Trust,
                    NeedsInputKind::Login,
                    NeedsInputKind::Permission,
                    NeedsInputKind::Update,
                    NeedsInputKind::Quota,
                    NeedsInputKind::UnknownModal,
                ]
            ),
        ),
        (
            "PermissionMode".to_string(),
            wire_values!(
                PermissionMode,
                [
                    PermissionMode::Default,
                    PermissionMode::AcceptEdits,
                    PermissionMode::Plan,
                    PermissionMode::Auto,
                    PermissionMode::BypassPermissions,
                    PermissionMode::DontAsk,
                    PermissionMode::DangerouslySkipPermissions,
                ]
            ),
        ),
        (
            "ProbeOutcome".to_string(),
            wire_values!(
                ProbeOutcome,
                [
                    ProbeOutcome::Pass,
                    ProbeOutcome::Unproven,
                    ProbeOutcome::Fail,
                ]
            ),
        ),
        (
            "RateLimitStatus".to_string(),
            wire_values!(
                RateLimitStatus,
                [
                    RateLimitStatus::Allowed,
                    RateLimitStatus::Rejected,
                    RateLimitStatus::Unknown,
                ]
            ),
        ),
        (
            "RuntimeFinding".to_string(),
            wire_values!(
                RuntimeFinding,
                [
                    RuntimeFinding::PrivateRuntimeResponsive,
                    RuntimeFinding::ControlPlaneUnreachable,
                    RuntimeFinding::ControlPlaneUnresponsive,
                    RuntimeFinding::ControlPlaneRefused,
                    RuntimeFinding::LaunchBrokerStopped,
                ]
            ),
        ),
        (
            "SessionCell".to_string(),
            wire_values!(SessionCell, [SessionCell::Full, SessionCell::Minified]),
        ),
        (
            "SessionFinding".to_string(),
            wire_values!(
                SessionFinding,
                [
                    SessionFinding::TerminalPresent,
                    SessionFinding::TerminalMissing,
                    SessionFinding::SessionDeclaredUnusable,
                    SessionFinding::SessionActorUnresponsive,
                    SessionFinding::SessionClosedDuringProbe,
                    SessionFinding::NotProbed,
                ]
            ),
        ),
        (
            "SessionState".to_string(),
            wire_values!(
                SessionState,
                [
                    SessionState::Creating,
                    SessionState::Booting,
                    SessionState::Ready,
                    SessionState::Submitting,
                    SessionState::AwaitingPromptAck,
                    SessionState::Running,
                    SessionState::NeedsInput,
                    SessionState::TerminalCandidate,
                    SessionState::Draining,
                    SessionState::Cancelling,
                    SessionState::Tainted,
                    SessionState::Closing,
                    SessionState::Closed,
                    SessionState::Failed,
                ]
            ),
        ),
        (
            "StopReasonKind".to_string(),
            wire_values!(
                StopReasonKind,
                [
                    StopReasonKind::EndTurn,
                    StopReasonKind::StopSequence,
                    StopReasonKind::MaxTokens,
                    StopReasonKind::ToolUse,
                    StopReasonKind::PauseTurn,
                    StopReasonKind::Refusal,
                    StopReasonKind::Error,
                    StopReasonKind::Unknown,
                ]
            ),
        ),
        (
            "TerminalProfile".to_string(),
            wire_values!(
                TerminalProfile,
                [TerminalProfile::Transparent, TerminalProfile::RmuxStandard]
            ),
        ),
        (
            "ToolStatus".to_string(),
            wire_values!(
                ToolStatus,
                [
                    ToolStatus::Requested,
                    ToolStatus::Completed,
                    ToolStatus::Failed,
                    ToolStatus::Cancelled,
                ]
            ),
        ),
        (
            "TurnOutcome".to_string(),
            wire_values!(
                TurnOutcome,
                [
                    TurnOutcome::Completed,
                    TurnOutcome::Cancelled,
                    TurnOutcome::Failed,
                ]
            ),
        ),
    ]);

    assert_eq!(
        manifest.value_enums.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>(),
        "shared manifest value_enums must pin exactly the v1 plain-string enums"
    );
    assert_eq!(manifest.value_enums, expected);
}

#[derive(Deserialize)]
struct TaggedUnionManifest {
    tagged_unions: BTreeMap<String, TaggedUnion>,
}

const SAMPLE_SESSION_ID: Uuid = Uuid::from_u128(0x22);

#[test]
fn shared_manifest_tagged_unions_match_the_rust_tagged_enums() {
    let manifest: TaggedUnionManifest = read_json("manifest.json");

    let expected: BTreeMap<String, TaggedUnion> = BTreeMap::from([
        wire_tagged!(
            ConfigSource,
            [
                ConfigSource::File { .. } => ConfigSource::File { path: "/work/settings.json".into() },
                ConfigSource::Inline { .. } => ConfigSource::Inline { document: json!({}) },
            ]
        ),
        wire_tagged!(
            LifecycleMode,
            [
                LifecycleMode::Transcript => LifecycleMode::Transcript,
                LifecycleMode::Hybrid { .. } => LifecycleMode::Hybrid { hook_timeout_ms: 5_000 },
            ]
        ),
        wire_tagged!(
            MessageBlock,
            [
                MessageBlock::Text { .. } => MessageBlock::Text { text: "ok".into() },
                MessageBlock::ToolUse { .. } => MessageBlock::ToolUse {
                    id: "toolu_1".into(),
                    name: "Read".into(),
                    input: json!({}),
                },
                MessageBlock::ToolResult { .. } => MessageBlock::ToolResult {
                    tool_use_id: "toolu_1".into(),
                    content: json!(""),
                    is_error: false,
                },
                MessageBlock::Unknown { .. } => MessageBlock::Unknown {
                    block_type: "server_tool_use".into(),
                    data: json!({}),
                },
            ]
        ),
        wire_tagged!(
            RetentionPolicy,
            [
                RetentionPolicy::OneShot => RetentionPolicy::OneShot,
                RetentionPolicy::Persistent { .. } => RetentionPolicy::Persistent {
                    idle_ttl_ms: 30 * 60 * 1_000,
                },
            ]
        ),
        wire_tagged!(
            SessionIdentity,
            [
                SessionIdentity::New { .. } => SessionIdentity::New { session_id: None },
                SessionIdentity::Resume { .. } => SessionIdentity::Resume {
                    session_id: SAMPLE_SESSION_ID,
                },
            ]
        ),
        wire_tagged!(
            SystemPromptPolicy,
            [
                SystemPromptPolicy::Default => SystemPromptPolicy::Default,
                SystemPromptPolicy::Append { .. } => SystemPromptPolicy::Append {
                    prompt: "Be precise.".into(),
                },
                SystemPromptPolicy::Replace { .. } => SystemPromptPolicy::Replace {
                    prompt: "Be precise.".into(),
                },
            ]
        ),
    ]);

    assert_eq!(
        manifest.tagged_unions.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>(),
        "shared manifest tagged_unions must pin exactly the v1 internally-tagged unions"
    );
    assert_eq!(manifest.tagged_unions, expected);
}

/// Every `pub enum` in this crate's sources that serde tags *internally*, read
/// out of the sources themselves.
///
/// Internally, so the three envelope enums are excluded by the `content` key
/// that makes them adjacently tagged rather than by their names; `pub`, so the
/// three private mirror enums inside the hand-written `Deserialize` impls are
/// excluded by their own declarations rather than by a `Wire` prefix rule.
///
/// The attribute block is taken as every line back to the blank line or closing
/// brace above the declaration and flattened before it is read, so an attribute
/// `rustfmt` wraps across lines is still seen. A formatting change this misses
/// drops a union out of the set and reddens the test below rather than shrinking
/// it silently.
fn internally_tagged_pub_enums() -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut pending = vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")];
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            for entry in std::fs::read_dir(&path).unwrap() {
                pending.push(entry.unwrap().path());
            }
            continue;
        }
        if path.extension().is_none_or(|extension| extension != "rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = source.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            let Some(rest) = line.trim_start().strip_prefix("pub enum ") else {
                continue;
            };
            let name = rest.trim_end_matches(['{', ' ']);
            let mut attributes = String::new();
            for above in lines[..index].iter().rev() {
                let trimmed = above.trim();
                if trimmed.is_empty() || trimmed == "}" {
                    break;
                }
                attributes.push(' ');
                attributes.push_str(trimmed);
            }
            if attributes.contains("tag = \"") && !attributes.contains("content = ") {
                found.insert(name.to_string());
            }
        }
    }
    found
}

#[test]
fn every_internally_tagged_v1_enum_is_pinned_as_a_tagged_union() {
    let manifest: TaggedUnionManifest = read_json("manifest.json");
    let declared = internally_tagged_pub_enums();

    assert!(
        !declared.is_empty(),
        "the scan of crates/protocol/src found no internally-tagged pub enum at all, \
         so it is no longer deriving anything"
    );
    assert_eq!(
        declared,
        manifest
            .tagged_unions
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        "every internally-tagged v1 enum must appear in manifest.json#tagged_unions; \
         the six pinned above are a hand-written list, and this is what derives it"
    );
}
