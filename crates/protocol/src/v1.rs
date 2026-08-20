//! Versioned domain and wire types for the native pmux API.
//!
//! The protocol deliberately models Claude sessions and turns rather than raw
//! terminal operations. Terminal backend identifiers and transcript internals
//! are never part of this public contract.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use uuid::Uuid;

// The single authoritative launch-environment inheritance policy. It lives
// here rather than only in the daemon that enforces it so every crate that
// already depends on `pseudomux-protocol` shares one table. `pmux probe` and
// `agent_profile` used to be the second readers; both are gone from the
// product. The module's own documentation carries the argument; it is
// deliberately not repeated here as an outer doc comment, because rustdoc
// would then resolve the module's intra-doc links in this scope.
pub mod launch_environment;

/// The only wire version understood by this crate.
pub const PROTOCOL_VERSION: u16 = 1;
/// Maximum JSON payload carried by one native length-delimited frame.
///
/// The four-byte length prefix is not included. Services must size terminal
/// results and event batches against this same public contract before storing
/// or emitting them; transports retain a final defensive check.
pub const MAX_NATIVE_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Admission decision for one native transport's four-byte length prefix.
///
/// Keeping this check in the protocol crate gives every production transport,
/// client, and untrusted-input harness one canonical 8 MiB boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeFrameAdmission {
    Payload { payload_bytes: usize },
    Oversized { advertised_bytes: u32 },
}

/// Decodes and validates a native big-endian frame header without allocating.
#[must_use]
pub const fn admit_native_frame_header(header: [u8; 4]) -> NativeFrameAdmission {
    let advertised_bytes = u32::from_be_bytes(header);
    if advertised_bytes as usize > MAX_NATIVE_FRAME_BYTES {
        NativeFrameAdmission::Oversized { advertised_bytes }
    } else {
        NativeFrameAdmission::Payload {
            payload_bytes: advertised_bytes as usize,
        }
    }
}

/// Result of incrementally feeding one or more byte fragments into a native
/// frame accumulator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeFrameProgress {
    /// The current frame is incomplete.
    NeedMore,
    /// One complete, admitted payload was assembled.
    Payload(Vec<u8>),
    /// The four-byte header advertised a payload above the protocol ceiling.
    /// No payload allocation was performed.
    Oversized { advertised_bytes: u32 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum NativeFrameAccumulatorState {
    Header { bytes: [u8; 4], filled: usize },
    Payload { bytes: Vec<u8>, filled: usize },
}

/// Incrementally assembles exactly one length-delimited native frame at a
/// time, using the same fixed 8 MiB admission boundary as the public daemon.
///
/// `push` consumes at most one complete frame. It reports the number of input
/// bytes consumed so callers can retain a trailing next frame, and resets to
/// an empty header after returning `Payload` or `Oversized`. Allocation occurs
/// only after all four header bytes have been admitted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeFrameAccumulator {
    state: NativeFrameAccumulatorState,
}

impl NativeFrameAccumulator {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: NativeFrameAccumulatorState::Header {
                bytes: [0; 4],
                filled: 0,
            },
        }
    }

    /// True only when no byte of the next frame has been accepted.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        matches!(
            self.state,
            NativeFrameAccumulatorState::Header { filled: 0, .. }
        )
    }

    /// Number of bytes needed to finish the current header or payload.
    #[must_use]
    pub fn remaining_bytes(&self) -> usize {
        match &self.state {
            NativeFrameAccumulatorState::Header { filled, .. } => 4 - filled,
            NativeFrameAccumulatorState::Payload { bytes, filled } => bytes.len() - filled,
        }
    }

    /// Consumes a fragment up to the first complete frame boundary.
    pub fn push(&mut self, input: &[u8]) -> (usize, NativeFrameProgress) {
        let mut consumed = 0;
        loop {
            match &mut self.state {
                NativeFrameAccumulatorState::Header { bytes, filled } => {
                    let copied = (4 - *filled).min(input.len().saturating_sub(consumed));
                    bytes[*filled..*filled + copied]
                        .copy_from_slice(&input[consumed..consumed + copied]);
                    *filled += copied;
                    consumed += copied;
                    if *filled < 4 {
                        return (consumed, NativeFrameProgress::NeedMore);
                    }

                    match admit_native_frame_header(*bytes) {
                        NativeFrameAdmission::Oversized { advertised_bytes } => {
                            *self = Self::new();
                            return (
                                consumed,
                                NativeFrameProgress::Oversized { advertised_bytes },
                            );
                        }
                        NativeFrameAdmission::Payload { payload_bytes: 0 } => {
                            *self = Self::new();
                            return (consumed, NativeFrameProgress::Payload(Vec::new()));
                        }
                        NativeFrameAdmission::Payload { payload_bytes } => {
                            self.state = NativeFrameAccumulatorState::Payload {
                                bytes: vec![0; payload_bytes],
                                filled: 0,
                            };
                        }
                    }
                }
                NativeFrameAccumulatorState::Payload { bytes, filled } => {
                    let copied = (bytes.len() - *filled).min(input.len().saturating_sub(consumed));
                    bytes[*filled..*filled + copied]
                        .copy_from_slice(&input[consumed..consumed + copied]);
                    *filled += copied;
                    consumed += copied;
                    if *filled < bytes.len() {
                        return (consumed, NativeFrameProgress::NeedMore);
                    }
                    let payload = std::mem::take(bytes);
                    *self = Self::new();
                    return (consumed, NativeFrameProgress::Payload(payload));
                }
            }
        }
    }
}

impl Default for NativeFrameAccumulator {
    fn default() -> Self {
        Self::new()
    }
}
/// Maximum duration of one public event-subscription long poll.
pub const MAX_SUBSCRIBE_WAIT_MS: u64 = 30_000;
/// Maximum number of replay events returned by one subscription request.
pub const MAX_SUBSCRIBE_EVENTS: u32 = 512;

/// Returns whether a peer's major wire version is understood by this crate.
#[must_use]
pub const fn is_supported_version(version: u16) -> bool {
    version == PROTOCOL_VERSION
}

pub type RequestId = Uuid;
pub type SessionId = Uuid;
pub type TurnId = Uuid;
pub type TimestampMs = u64;

/// Largest exact nonnegative integer shared by JSON implementations using
/// IEEE-754 binary64 numbers (including JavaScript).
pub const MAX_SAFE_JSON_INTEGER: u64 = 9_007_199_254_740_991;
const MIN_SAFE_JSON_INTEGER: i64 = -9_007_199_254_740_991;

/// `skip_serializing_if` for counters whose zero is the overwhelmingly common
/// case and whose absence therefore keeps a wire message small. Only ever put
/// on a field whose producer computes it on every message; a field that is
/// sometimes not computed must be an `Option`, so that "zero" and "not
/// measured" stay different bytes.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if requires a by-reference predicate"
)]
const fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

mod safe_u64 {
    use super::*;

    pub fn serialize<S>(value: &u64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if *value > MAX_SAFE_JSON_INTEGER {
            return Err(serde::ser::Error::custom(format!(
                "integer must not exceed {MAX_SAFE_JSON_INTEGER}"
            )));
        }
        value.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<u64, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u64::deserialize(deserializer)?;
        if value > MAX_SAFE_JSON_INTEGER {
            return Err(serde::de::Error::custom(format!(
                "integer must not exceed {MAX_SAFE_JSON_INTEGER}"
            )));
        }
        Ok(value)
    }
}

mod optional_safe_u64 {
    use super::*;

    pub fn serialize<S>(value: &Option<u64>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(value) => serializer.serialize_some(&SafeU64(*value)),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Option::<u64>::deserialize(deserializer)?;
        match value {
            Some(value) if value > MAX_SAFE_JSON_INTEGER => Err(serde::de::Error::custom(format!(
                "integer must not exceed {MAX_SAFE_JSON_INTEGER}"
            ))),
            value => Ok(value),
        }
    }

    struct SafeU64(u64);

    impl Serialize for SafeU64 {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            safe_u64::serialize(&self.0, serializer)
        }
    }
}

fn validate_opaque_json(value: &Value) -> Result<(), &'static str> {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
        Value::Array(values) => values.iter().try_for_each(validate_opaque_json),
        Value::Object(values) => values.values().try_for_each(validate_opaque_json),
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                if !(MIN_SAFE_JSON_INTEGER..=MAX_SAFE_JSON_INTEGER as i64).contains(&value) {
                    return Err("opaque JSON integer is outside the signed safe-integer range");
                }
                return Ok(());
            }
            if let Some(value) = number.as_u64() {
                if value > MAX_SAFE_JSON_INTEGER {
                    return Err("opaque JSON integer is outside the signed safe-integer range");
                }
                return Ok(());
            }
            let Some(value) = number.as_f64() else {
                return Err("opaque JSON number is not finite");
            };
            if !value.is_finite() {
                return Err("opaque JSON number is not finite");
            }
            if value.fract() == 0.0 && value.abs() > MAX_SAFE_JSON_INTEGER as f64 {
                return Err("opaque JSON integer is outside the signed safe-integer range");
            }
            Ok(())
        }
    }
}

/// Validates a typed protocol value with the exact serializers used on the
/// protocol-v1 wire without allocating an intermediate JSON document.
///
/// Public Rust callers can construct DTOs directly and therefore bypass the
/// custom deserializers that protect UDS callers. Service entry points use this
/// preflight before side effects so both call paths enforce the same numeric
/// domain. This is deliberately serializer-backed rather than a second field
/// inventory.
pub fn validate_v1_serializable<T>(value: &T) -> Result<(), serde_json::Error>
where
    T: Serialize + ?Sized,
{
    serde_json::to_writer(std::io::sink(), value)
}

mod safe_json_value {
    use super::*;

    pub fn serialize<S>(value: &Value, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        validate_opaque_json(value).map_err(serde::ser::Error::custom)?;
        value.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        validate_opaque_json(&value).map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

mod optional_safe_json_value {
    use super::*;

    pub fn serialize<S>(value: &Option<Value>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(value) => {
                validate_opaque_json(value).map_err(serde::ser::Error::custom)?;
                serializer.serialize_some(value)
            }
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Value>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Option::<Value>::deserialize(deserializer)?;
        if let Some(value) = &value {
            validate_opaque_json(value).map_err(serde::de::Error::custom)?;
        }
        Ok(value)
    }
}

fn deserialize_canonical_uuid<'de, D>(deserializer: D) -> Result<Uuid, D::Error>
where
    D: Deserializer<'de>,
{
    let text = String::deserialize(deserializer)?;
    let uuid = Uuid::parse_str(&text).map_err(serde::de::Error::custom)?;
    if uuid.hyphenated().to_string().eq_ignore_ascii_case(&text) {
        Ok(uuid)
    } else {
        Err(serde::de::Error::custom(
            "UUID must use the canonical 8-4-4-4-12 hyphenated form",
        ))
    }
}

fn deserialize_optional_canonical_uuid<'de, D>(deserializer: D) -> Result<Option<Uuid>, D::Error>
where
    D: Deserializer<'de>,
{
    let text = Option::<String>::deserialize(deserializer)?;
    text.map(|text| {
        let uuid = Uuid::parse_str(&text).map_err(serde::de::Error::custom)?;
        if uuid.hyphenated().to_string().eq_ignore_ascii_case(&text) {
            Ok(uuid)
        } else {
            Err(serde::de::Error::custom(
                "UUID must use the canonical 8-4-4-4-12 hyphenated form",
            ))
        }
    })
    .transpose()
}

/// Opaque identity of one live pmux process incarnation for a Claude session.
///
/// Claude session UUIDs are intentionally resumable and can therefore name
/// multiple sequential processes. Every generation-targeted operation carries
/// this separate fence so a delayed request for process A can never mutate a
/// resumed process B with the same [`SessionId`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct SessionGenerationId(Uuid);

impl<'de> Deserialize<'de> for SessionGenerationId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserialize_canonical_uuid(deserializer).map(Self)
    }
}

impl SessionGenerationId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    #[must_use]
    pub const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }

    #[must_use]
    pub const fn from_u128(value: u128) -> Self {
        Self(Uuid::from_u128(value))
    }
}

impl Default for SessionGenerationId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SessionGenerationId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A client request. Flattening [`Request`] gives the documented wire shape:
/// `{ "version": 1, "request_id": "...", "method": "...", "params": ... }`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestEnvelope {
    pub version: u16,
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub request_id: RequestId,
    #[serde(flatten)]
    pub request: Request,
}

impl RequestEnvelope {
    #[must_use]
    pub fn new(request_id: RequestId, request: Request) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            request_id,
            request,
        }
    }
}

/// Methods in the local native protocol.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    Ping,
    StartSession(StartSessionRequest),
    RunTurn(RunTurnRequest),
    CancelTurn(CancelTurnRequest),
    InspectSession(InspectSessionRequest),
    AttachSession(AttachSessionRequest),
    CloseSession(CloseSessionRequest),
    SubscribeEvents(SubscribeEventsRequest),
    RunOnce(RunOnceRequest),
    // Appended, never inserted: the shared conformance manifest compares this
    // list positionally, and appending never renumbers a position a reader has
    // already memorised.
    ClearSession(ClearSessionRequest),
    /// Completes one real operation against the private runtime and reports
    /// what it found, per session.
    ///
    /// Deliberately a distinct method rather than an extension of [`Self::Ping`].
    /// `Ping` is answered without dereferencing the service at all, so it can
    /// never say anything about the private runtime, the session registry or
    /// the rmux sidecar; widening it would have turned the one request whose
    /// cost is a constant into one whose cost scales with the pool, and every
    /// existing caller pays that without asking. It is also a unit variant for
    /// the same reason `Ping` is: this request selects nothing and bounds
    /// nothing, so there is no parameter a caller could get wrong.
    Diagnose,
    /// One complete `(model, effort, prompt) -> tokens` exchange, served by the
    /// pool, naming no resource.
    ///
    /// Appended after [`Self::Diagnose`] for the reason stated above: the two
    /// arrived on separate branches and the manifest compares positionally, so
    /// the one already carried by the shared corpus keeps its index.
    RunStateless(RunStatelessRequest),
    /// Store one reusable launch configuration and mint version 1.
    ///
    /// The four agent methods are appended in this order, for the reason stated
    /// above, and each answers its own distinct result variant: the shared
    /// golden corpus asserts that no two methods share a result type.
    CreateAgent(CreateAgentRequest),
    GetAgent(GetAgentRequest),
    ListAgents(ListAgentsRequest),
    UpdateAgent(UpdateAgentRequest),
}

/// A response correlated to one request.
///
/// Response and event DTOs intentionally accept unknown object fields. Within
/// protocol v1, adding a field is a backward-compatible minor evolution: older
/// clients retain the fields they understand. Required fields, the exactly-one
/// `result`/`error` invariant, enum discriminants, and protocol versions remain
/// semantic checks. Request DTOs use `deny_unknown_fields` instead so a typo in
/// launch, authentication, or lifecycle policy never changes execution silently.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ResponseEnvelope {
    pub version: u16,
    pub request_id: RequestId,
    #[serde(flatten)]
    pub payload: ResponsePayload,
}

impl<'de> Deserialize<'de> for ResponseEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireResponse {
            version: u16,
            #[serde(deserialize_with = "deserialize_canonical_uuid")]
            request_id: RequestId,
            #[serde(default)]
            result: Option<ResponseResult>,
            #[serde(default)]
            error: Option<ErrorBody>,
        }

        let value = Value::deserialize(deserializer)?;
        validate_opaque_json(&value).map_err(serde::de::Error::custom)?;
        let wire =
            serde_json::from_value::<WireResponse>(value).map_err(serde::de::Error::custom)?;
        let payload = match (wire.result, wire.error) {
            (Some(result), None) => ResponsePayload::Success(Box::new(result)),
            (None, Some(error)) => ResponsePayload::Failure(error),
            (Some(_), Some(_)) => {
                return Err(serde::de::Error::custom(
                    "response must contain exactly one of result or error",
                ));
            }
            (None, None) => {
                return Err(serde::de::Error::custom(
                    "response must contain exactly one of result or error",
                ));
            }
        };

        Ok(Self {
            version: wire.version,
            request_id: wire.request_id,
            payload,
        })
    }
}

impl ResponseEnvelope {
    #[must_use]
    pub fn success(request_id: RequestId, result: ResponseResult) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            request_id,
            payload: ResponsePayload::Success(Box::new(result)),
        }
    }

    #[must_use]
    pub fn failure(request_id: RequestId, error: ErrorBody) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            request_id,
            payload: ResponsePayload::Failure(error),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ResponsePayload {
    #[serde(rename = "result")]
    Success(Box<ResponseResult>),
    #[serde(rename = "error")]
    Failure(ErrorBody),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ResponseResult {
    Pong(Pong),
    SessionStarted(SessionHandle),
    TurnAccepted(TurnAccepted),
    TurnCancelled(CancelTurnResult),
    // Boxed for the same reason as `TurnResult` below: publishing the
    // transcript fence and the cell grew this variant past the point where
    // every other response pays for its size on every move. `Box` is
    // serde-transparent, so the wire shape is unchanged.
    SessionSnapshot(Box<SessionSnapshot>),
    AttachCapability(AttachCapability),
    SessionClosed(CloseSessionResult),
    Events(EventBatch),
    // Boxed for the same reason as `EventPayload::TurnCompleted`: `TurnResult`
    // is by far the largest variant, and the box keeps every other response
    // cheap to move. `Box` is serde-transparent, so the wire shape is
    // unchanged.
    TurnResult(Box<TurnResult>),
    SessionCleared(ClearSessionResult),
    // Appended for the same reason `ClearSession` was appended to `Request`.
    // Boxed because it carries one entry per live session and every other
    // response would otherwise pay for a pool-sized variant on every move.
    Diagnosis(Box<DaemonDiagnosis>),
    // Boxed for the same reason as `TurnResult` above: a variant that carries a
    // whole answer must not make every cheap response pay for its size on every
    // move. `Box` is serde-transparent, so the wire shape is unchanged.
    StatelessResult(Box<StatelessResult>),
    // Appended in the order their methods were appended to `Request`. Each is
    // boxed for the reason `SessionSnapshot` and `TurnResult` are: an
    // `AgentDescriptor` carries a whole `ClaudeLaunchConfig` plus an
    // environment map, and no cheap response should pay for that on every move.
    //
    // FOUR VARIANTS, NOT ONE SHARED `agent`, and the constraint is worth
    // stating because it is not obvious: the shared golden corpus asserts that
    // each method's result type is DISTINCT (`v1_golden.rs` inserts into a
    // `BTreeSet` and asserts the insert succeeded). `create_agent` and
    // `get_agent` both answering `agent` would redden it, and collapsing them
    // would mean changing that invariant -- a worse trade than three extra
    // variants.
    AgentCreated(Box<AgentDescriptor>),
    Agent(Box<AgentDescriptor>),
    AgentList(Box<AgentList>),
    AgentUpdated(Box<AgentDescriptor>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pong {
    pub server_version: String,
    pub protocol_version: u16,
}

/// The coarse, foldable result of one check.
///
/// Three values, not two, because two cannot distinguish "I checked and it was
/// fine" from "I did not check". A boolean forces the second into the first,
/// and that is precisely how a health report comes to assert health over
/// machinery it never touched.
///
/// The declaration order IS the severity order, and [`Ord`] is derived from it
/// so that folding is `max`. `Fail` outranks `Unproven`, which outranks `Pass`:
/// evidence of a fault is more actionable than absence of evidence, and absence
/// of evidence must never be reported as evidence of health.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeOutcome {
    /// The check ran to completion and found what it was looking for.
    Pass,
    /// The check could not be completed, so its subject is neither proven
    /// healthy nor proven faulty.
    Unproven,
    /// The check ran to completion and found a fault.
    Fail,
}

impl ProbeOutcome {
    /// Folds a set of outcomes into the worst one present.
    ///
    /// An empty set folds to [`Self::Pass`]. That is deliberate and is the one
    /// place this type says something a caller must agree with: a daemon
    /// holding no sessions is not unhealthy for holding none.
    ///
    /// It is a rule about an empty set of OUTCOMES, and it is not a licence to
    /// fold every empty subject to pass. Whether an empty subject is a capacity
    /// fact or a fault is the producing layer's question and not this one's --
    /// a pool holding no instances is idle when no warm floor was declared and
    /// [`LayerFinding::Faulted`] when one was -- and by the time an outcome
    /// reaches this fold, that question has already been answered.
    #[must_use]
    pub fn fold(outcomes: impl IntoIterator<Item = Self>) -> Self {
        outcomes.into_iter().max().unwrap_or(Self::Pass)
    }
}

/// What the daemon found when it last completed a real operation against its
/// own private runtime.
///
/// This type is the answer to a question `Ping` structurally cannot answer.
/// `Ping` is served from the accept loop without touching the private runtime,
/// the session registry, the launch broker or the rmux sidecar, so a daemon
/// whose sidecar has been killed, stopped, or wedged answers it perfectly.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonDiagnosis {
    /// One entry per layer of the health tree, in [`HealthLayer`]'s declaration
    /// order.
    ///
    /// **Health is a proof tree, not a boolean.** Each layer either proves
    /// itself by exercising it, reports a failure, or reports NOT ESTABLISHED,
    /// and the third is a distinct answer rather than a shading of the first.
    /// [`Self::outcome`] folds every layer, and it treats a layer that is
    /// ABSENT from this list as `Unproven` too -- so a daemon that forgets to
    /// report a layer cannot report health for it by omission, which is the
    /// exact failure this whole type replaced.
    ///
    /// Appended after `runtime` and `sessions`, which stay: they are pinned by
    /// the shared conformance corpus and by both shipped clients, and this list
    /// carries the same facts in a layered form plus the layers neither of them
    /// could express.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<HealthLayer>,
    /// The one control-plane operation this probe performs.
    pub runtime: RuntimeProbe,
    /// One entry per session the registry held when the probe began, in stable
    /// session-id order.
    ///
    /// Deliberately a list and not a count or a summary boolean. "Healthy" is
    /// not a property a pool has; it is a property each instance has, and a
    /// supervisor whose classes are independently warm, cold and quarantined
    /// cannot recover per-instance answers from a fold. The fold is published
    /// too, as a convenience, but this list is the report.
    pub sessions: Vec<SessionProbe>,
}

impl DaemonDiagnosis {
    /// The worst outcome anywhere in this report, INCLUDING every layer that is
    /// missing from it.
    ///
    /// The missing-layer clause is the load-bearing one. `ProbeOutcome::fold`
    /// over an empty set is `Pass`, which is correct for a daemon holding no
    /// sessions -- holding none is a capacity fact, not a fault -- and wrong for
    /// a daemon reporting no layers, because every layer is always applicable.
    /// A fold over `self.layers` alone would therefore report a daemon that
    /// established nothing as healthy, which is the sentence this type exists
    /// to make unsayable.
    #[must_use]
    pub fn outcome(&self) -> ProbeOutcome {
        ProbeOutcome::fold(
            std::iter::once(self.runtime.outcome)
                .chain(self.sessions.iter().map(|session| session.outcome))
                .chain(self.layers.iter().map(|layer| layer.outcome))
                .chain(
                    self.missing_layers()
                        .into_iter()
                        .map(|_| ProbeOutcome::Unproven),
                ),
        )
    }

    /// Layers this report does not carry an entry for, in declaration order.
    ///
    /// Published rather than merely folded, so a reader can name what was not
    /// established instead of inferring it from a worse total.
    #[must_use]
    pub fn missing_layers(&self) -> Vec<HealthLayerName> {
        HealthLayerName::ALL
            .iter()
            .copied()
            .filter(|name| !self.layers.iter().any(|layer| layer.layer == *name))
            .collect()
    }

    /// The layer entry for one name, when this report carries one.
    #[must_use]
    pub fn layer(&self, name: HealthLayerName) -> Option<&HealthLayer> {
        self.layers.iter().find(|layer| layer.layer == name)
    }
}

/// One layer of the health tree.
///
/// The layers are not a severity ladder and are not ordered by importance; they
/// are the distinct things that can independently be true or false about a
/// running pmux. A daemon whose control plane answers and whose pool has halted
/// is neither healthy nor wholly broken, and a single verdict cannot say that.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthLayerName {
    /// The configuration the daemon booted on, including whether the stateless
    /// engine is configured at all.
    Configuration,
    /// A connection to the private rmux socket.
    ControlPlane,
    /// A completed request/response exchange with the private rmux sidecar --
    /// one that takes its dispatch state lock, so answering it is evidence the
    /// sidecar is serving and not merely accepting.
    PrivateRuntime,
    /// The launch broker's accept loop, which every session start goes through.
    LaunchBroker,
    /// Whether any Claude compatibility cell is admitted, which is what decides
    /// whether a minified cell -- and therefore the whole stateless engine --
    /// can start at all.
    CompatibilityProfile,
    /// The stateless pool: capacity, in-flight work, leaked slots, halt state.
    Pool,
    /// The registered sessions, as [`DaemonDiagnosis::sessions`] reports them.
    Sessions,
    /// Measured latency against the envelopes this daemon was sized on.
    Performance,
}

impl HealthLayerName {
    /// Every layer, in declaration order.
    ///
    /// Derived from an exhaustive `match` rather than written out beside the
    /// enum. A hand-written array is the exact shape of the defect this file
    /// has now produced twice: the array is what a new variant is invisible to,
    /// and [`DaemonDiagnosis::missing_layers`] reads this array to decide what
    /// was not established. A layer absent from here would be a layer nothing
    /// ever notices is missing.
    pub const ALL: &'static [Self] = &{
        const fn exhaustive(name: HealthLayerName) -> HealthLayerName {
            match name {
                HealthLayerName::Configuration
                | HealthLayerName::ControlPlane
                | HealthLayerName::PrivateRuntime
                | HealthLayerName::LaunchBroker
                | HealthLayerName::CompatibilityProfile
                | HealthLayerName::Pool
                | HealthLayerName::Sessions
                | HealthLayerName::Performance => name,
            }
        }
        [
            exhaustive(Self::Configuration),
            exhaustive(Self::ControlPlane),
            exhaustive(Self::PrivateRuntime),
            exhaustive(Self::LaunchBroker),
            exhaustive(Self::CompatibilityProfile),
            exhaustive(Self::Pool),
            exhaustive(Self::Sessions),
            exhaustive(Self::Performance),
        ]
    };
}

/// What one layer of the tree established.
///
/// Three values, mirroring [`ProbeOutcome`], because the same argument applies
/// one level down: two cannot distinguish "I exercised this and it was fine"
/// from "I did not exercise this". [`Self::NotEstablished`] is the one that
/// must never roll up as healthy.
///
/// # Why there are four and not three
///
/// Three conflated two different sentences under `not_established`:
///
/// - **"I tried and could not."** A prerequisite failed, or the probe did not
///   complete. Nothing is known. This must never roll up as healthy, and
///   [`Self::NotEstablished`] is it.
/// - **"There was nothing to try."** The subject is an empty set THAT NOTHING
///   DECLARED SHOULD BE OCCUPIED, or the layer does not apply to how this
///   daemon was configured. Nothing is *wrong*, and nothing ever will be until
///   the set is non-empty. This is [`Self::NothingToExercise`].
///
/// The second is the ordinary, permanent state of a correct daemon:
/// [`ProbeOutcome::fold`] already says so for an empty set of sessions -- "a
/// daemon holding no sessions is not unhealthy for holding none" -- and a pure
/// Path B daemon holds none by construction, because a pool instance's session
/// id is the one name no client may learn and pool instances are therefore
/// deliberately absent from [`DaemonDiagnosis::sessions`].
///
/// MEASURED before the split: a daemon with a warm pool of two idle instances,
/// every layer `pass` and every counter as designed, reported `unproven`
/// overall and `pmux doctor` exited 1 -- permanently, on every invocation, for
/// the life of the daemon. The detail string the pool layer emitted while doing
/// it read "holding none is a capacity fact rather than a fault", which is the
/// defect stated in the report itself: the message said "not a fault" and the
/// encoding said "worse than pass". A surface that cries wolf on every healthy
/// daemon makes a genuine `unproven` unreadable, which is the same failure as
/// the boolean `healthy` this tree replaced, one level along.
///
/// # The question is not "is the set empty?"
///
/// It is "is the set empty when something declared it should not be?", and a
/// producer that asks the first question instead of the second has this defect
/// whichever arm it picks. An empty set is vacuous only when nothing promised
/// to fill it: a pool holding no instances is a capacity fact if the operator
/// declared no warm floor and a fault if they declared one, and the emptiness
/// alone cannot tell those apart.
///
/// Both wrong encodings shipped, and they are the same error rather than
/// opposite ones. The first answered `not_established` to every empty set and
/// made a correct daemon permanently unprovable. The second answered
/// `nothing_to_exercise` to every empty set and made a daemon holding none of
/// an operator-declared warm floor -- MEASURED refusing six consecutive `pmux
/// ask` calls -- report `healthy` and exit 0.
///
/// A `detail` may state only what its own predicate tested. The second
/// encoding's pool detail closed with "and the next call of any class mints
/// one": a promise nothing in that predicate tested, and one that was false in
/// the state that produced it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerFinding {
    /// The layer was EXERCISED and the exercise succeeded. Not "looked
    /// plausible", not "was configured": a real operation completed.
    Exercised,
    /// The layer was exercised and reported a fault.
    Faulted,
    /// The layer had nothing to exercise, and that is not a fault.
    ///
    /// VACUOUSLY FINE, not unproven. Reached when the layer's subject is an
    /// empty set that nothing declared should be occupied (a registry holding
    /// no sessions; a pool with no declared warm floor holding no instances) or
    /// when the layer does not apply to how this daemon was configured (the
    /// pool on a daemon with no `--pool-parent`; the compatibility profile on
    /// a daemon with no pool, which is what makes a promoted cell mandatory).
    /// In every case the layer was reached, evaluated, and found to have no
    /// subject.
    ///
    /// An empty set the daemon's own configuration DECLARED should be occupied
    /// is not this: a pool holding none of an operator-declared warm floor is
    /// [`Self::Faulted`], because the declaration is what gives the emptiness a
    /// subject. See the type's own docs for why that distinction is the whole
    /// question.
    ///
    /// Folds to [`ProbeOutcome::Pass`] for the same reason
    /// [`ProbeOutcome::fold`] over an empty set is `Pass`: absence of a thing
    /// is a capacity fact, not a fault, and a daemon that is idle is not a
    /// daemon that is broken. The `detail` is still required and still has to
    /// say what was absent, so "pass" here is never a bare claim of health.
    ///
    /// The distinction from [`Self::NotEstablished`] is the load-bearing one
    /// and is not a shade of it: this says the question has no subject, that
    /// one says the question has a subject and no answer.
    NothingToExercise,
    /// The layer was not exercised, so nothing is claimed about it either way.
    ///
    /// Reached when a prerequisite layer failed -- there is no point asking the
    /// sidecar what it holds when no connection was made -- or when the probe
    /// itself could not complete. This is the finding that must never roll up
    /// as healthy, and it is the reason this enum exists.
    ///
    /// It is NOT the finding for "there was nothing here to look at"; that is
    /// [`Self::NothingToExercise`]. Conflating them is what made a correct
    /// daemon permanently unprovable.
    NotEstablished,
}

impl LayerFinding {
    #[must_use]
    pub const fn outcome(self) -> ProbeOutcome {
        match self {
            Self::Exercised | Self::NothingToExercise => ProbeOutcome::Pass,
            Self::Faulted => ProbeOutcome::Fail,
            Self::NotEstablished => ProbeOutcome::Unproven,
        }
    }
}

/// One layer's entry in the tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthLayer {
    pub layer: HealthLayerName,
    pub outcome: ProbeOutcome,
    pub finding: LayerFinding,
    /// What was exercised, what failed, or what was not established -- in that
    /// layer's own words.
    ///
    /// Required, not optional, and for every finding including
    /// [`LayerFinding::Exercised`]. "Pass" without a statement of what was
    /// exercised is the boolean this type replaced, one level down.
    pub detail: String,
    /// The observation the finding was derived from, as opaque JSON.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub evidence: serde_json::Value,
}

impl HealthLayer {
    /// The only constructor. `outcome` is derived from `finding` rather than
    /// accepted, for the reason [`RuntimeProbe::new`] states.
    #[must_use]
    pub fn new(
        layer: HealthLayerName,
        finding: LayerFinding,
        detail: impl Into<String>,
        evidence: serde_json::Value,
    ) -> Self {
        Self {
            layer,
            outcome: finding.outcome(),
            finding,
            detail: detail.into(),
            evidence,
        }
    }

    /// The [`HealthLayerName::Sessions`] entry for a set of session probes.
    ///
    /// # Why this lives in the protocol crate and not in the daemon
    ///
    /// It is a pure fold over [`SessionProbe`], which is a protocol type, and
    /// its result is entirely determined by [`DaemonDiagnosis::sessions`],
    /// which every client already receives. Keeping it here makes it the ONE
    /// producer: the daemon calls it to build the layer, and a test that needs
    /// a realistic tree calls it instead of writing a layer entry by hand.
    ///
    /// That is not a tidiness argument. `bin/pmux`'s process-boundary fixture
    /// asserted `status == "healthy"` on a reply carrying `sessions: []`
    /// together with a hand-built `sessions` layer whose finding was
    /// `exercised` -- a combination the daemon cannot produce for an empty set,
    /// under either the old encoding or the new one. A fixture assembled to
    /// satisfy an assertion rather than to model the producer proves nothing
    /// about the producer, and it is what let the encoding defect above ship
    /// past a green suite. A fixture that CALLS the producer cannot state an
    /// unreachable combination.
    ///
    /// The severity fold is [`ProbeOutcome::fold`]'s and not a second `match`
    /// here, for the same reason: a local table is free to drift from the one
    /// the probes derived their outcomes from.
    #[must_use]
    pub fn for_sessions(sessions: &[SessionProbe]) -> Self {
        let count = |wanted: ProbeOutcome| {
            sessions
                .iter()
                .filter(|session| session.outcome == wanted)
                .count()
        };
        let evidence = serde_json::json!({
            "registered": sessions.len(),
            "pass": count(ProbeOutcome::Pass),
            "unproven": count(ProbeOutcome::Unproven),
            "fail": count(ProbeOutcome::Fail),
        });
        if sessions.is_empty() {
            // NOT `NotEstablished`. Holding no sessions is a capacity fact, and
            // it is the PERMANENT shape of a daemon that serves only the
            // stateless engine: pool instances are deliberately absent from
            // `DaemonDiagnosis::sessions`, so this list is empty on every probe
            // of a correct Path B daemon, forever.
            return Self::new(
                HealthLayerName::Sessions,
                LayerFinding::NothingToExercise,
                "the registry holds no sessions, so there was no session to exercise; holding \
                 none is a capacity fact and not a fault, and this daemon may be serving \
                 stateless turns, whose instances are never named in a diagnosis",
                evidence,
            );
        }
        let (finding, detail) =
            match ProbeOutcome::fold(sessions.iter().map(|session| session.outcome)) {
                ProbeOutcome::Pass => (
                    LayerFinding::Exercised,
                    format!(
                        "the private rmux sidecar reports a terminal for every one of the {} \
                     registered session(s)",
                        sessions.len()
                    ),
                ),
                ProbeOutcome::Unproven => (
                    LayerFinding::NotEstablished,
                    format!(
                        "{} of {} session(s) could not be proven either way; see the per-session \
                     entries for which and why",
                        count(ProbeOutcome::Unproven),
                        sessions.len()
                    ),
                ),
                ProbeOutcome::Fail => (
                    LayerFinding::Faulted,
                    format!(
                        "{} of {} session(s) are ones pmux would still accept work for and whose \
                     terminal the sidecar does not report",
                        count(ProbeOutcome::Fail),
                        sessions.len()
                    ),
                ),
            };
        Self::new(HealthLayerName::Sessions, finding, detail, evidence)
    }
}

/// The result of exercising the private runtime: the rmux sidecar's dispatch
/// path, and the launch broker every session start has to go through.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeProbe {
    pub outcome: ProbeOutcome,
    pub finding: RuntimeFinding,
    /// Wall time the probe request took, including connection setup.
    #[serde(with = "safe_u64")]
    pub elapsed_ms: u64,
    /// How many private terminals the sidecar itself reported.
    ///
    /// Reported as a fact and deliberately not folded into any outcome. A
    /// terminal the sidecar knows about and the registry does not is the normal,
    /// transient shape of every in-flight start: pmux publishes a session only
    /// after its terminal exists. A rule that called that a leak would hold for
    /// an idle daemon and fire on a busy one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_private_terminals: Option<u32>,
}

/// Why [`RuntimeProbe::outcome`] is what it is.
///
/// The control plane is evaluated before the launch broker, so a report whose
/// finding names the broker is also asserting that the sidecar answered. That
/// order is deliberate: a dead sidecar makes every session unusable now, while
/// a stopped broker makes only the *next* start unusable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeFinding {
    /// The sidecar completed a request that takes its dispatch state lock, and
    /// the launch broker is still accepting.
    PrivateRuntimeResponsive,
    /// No connection could be established to the private socket.
    ControlPlaneUnreachable,
    /// A connection was established and the request did not complete within
    /// the same deadline every session operation is held to.
    ControlPlaneUnresponsive,
    /// The sidecar answered, and the answer was an error.
    ControlPlaneRefused,
    /// The sidecar answered, and the launch broker's accept loop has ended.
    ///
    /// The socket file survives the loop, so nothing about the endpoint looks
    /// wrong; the listener does not, so every later session start is refused at
    /// the launcher's connect. MEASURED in `pseudomux_service::launch_broker`'s
    /// `a_broker_whose_accept_loop_has_ended_reports_itself_not_accepting`,
    /// which corrected the earlier claim here that such a start would block in
    /// the handshake.
    LaunchBrokerStopped,
}

impl RuntimeFinding {
    #[must_use]
    pub const fn outcome(self) -> ProbeOutcome {
        match self {
            Self::PrivateRuntimeResponsive => ProbeOutcome::Pass,
            Self::ControlPlaneUnreachable
            | Self::ControlPlaneUnresponsive
            | Self::ControlPlaneRefused
            | Self::LaunchBrokerStopped => ProbeOutcome::Fail,
        }
    }
}

impl RuntimeProbe {
    /// The only constructor. `outcome` is derived from `finding` rather than
    /// accepted from a caller, so the coarse state and the fine state cannot
    /// come to disagree -- a report whose summary promises more than its
    /// finding tested is a false report with a confession attached.
    #[must_use]
    pub fn new(
        finding: RuntimeFinding,
        elapsed_ms: u64,
        live_private_terminals: Option<u32>,
    ) -> Self {
        Self {
            outcome: finding.outcome(),
            finding,
            elapsed_ms,
            live_private_terminals,
        }
    }
}

/// What the daemon found behind one registered session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionProbe {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub session_id: SessionId,
    pub generation_id: SessionGenerationId,
    pub outcome: ProbeOutcome,
    pub finding: SessionFinding,
    /// The state the actor reported while the probe ran, when it answered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<SessionState>,
    /// Whether the sidecar reported this session's private terminal.
    ///
    /// Present whenever the control-plane probe completed, including for
    /// sessions whose outcome is [`ProbeOutcome::Unproven`], so the observation
    /// is never withheld merely because no claim is being made from it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private_terminal_present: Option<bool>,
}

/// Why [`SessionProbe::outcome`] is what it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionFinding {
    /// The session is one pmux would still accept work for, and the sidecar
    /// reports the private terminal behind it.
    TerminalPresent,
    /// The session is one pmux would still accept work for, and the sidecar
    /// does not report the private terminal behind it. Nothing in pmux notices
    /// this on its own: no code polls an idle session's terminal, so a session
    /// whose Claude process died between turns keeps reporting `ready`.
    TerminalMissing,
    /// pmux has already declared this session unusable, so the presence or
    /// absence of its terminal proves nothing about the runtime. It is
    /// reported, and it is deliberately not counted as healthy: an
    /// undertaker's session still holds a registry slot and may still hold a
    /// Claude process.
    SessionDeclaredUnusable,
    /// The session's actor did not answer within the probe's bound. A busy
    /// actor is not a fault; it is an actor whose state this probe declined to
    /// guess at.
    SessionActorUnresponsive,
    /// The session left the registry while the probe was running.
    SessionClosedDuringProbe,
    /// The control-plane probe did not complete, so no terminal was looked for.
    NotProbed,
}

impl SessionFinding {
    #[must_use]
    pub const fn outcome(self) -> ProbeOutcome {
        match self {
            Self::TerminalPresent => ProbeOutcome::Pass,
            Self::TerminalMissing => ProbeOutcome::Fail,
            Self::SessionDeclaredUnusable
            | Self::SessionActorUnresponsive
            | Self::SessionClosedDuringProbe
            | Self::NotProbed => ProbeOutcome::Unproven,
        }
    }
}

impl SessionProbe {
    /// The only constructor; see [`RuntimeProbe::new`] for why.
    #[must_use]
    pub const fn new(
        session_id: SessionId,
        generation_id: SessionGenerationId,
        finding: SessionFinding,
        state: Option<SessionState>,
        private_terminal_present: Option<bool>,
    ) -> Self {
        Self {
            session_id,
            generation_id,
            outcome: finding.outcome(),
            finding,
            state,
            private_terminal_present,
        }
    }
}

/// An independently streamable event. Events are ordered within one session.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EventEnvelope {
    pub schema_version: u16,
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub session_id: SessionId,
    pub generation_id: SessionGenerationId,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_canonical_uuid"
    )]
    pub turn_id: Option<TurnId>,
    #[serde(with = "safe_u64")]
    pub sequence: u64,
    #[serde(with = "safe_u64")]
    pub timestamp_ms: TimestampMs,
    pub event: EventPayload,
}

impl<'de> Deserialize<'de> for EventEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireEventEnvelope {
            schema_version: u16,
            #[serde(deserialize_with = "deserialize_canonical_uuid")]
            session_id: SessionId,
            generation_id: SessionGenerationId,
            #[serde(default, deserialize_with = "deserialize_optional_canonical_uuid")]
            turn_id: Option<TurnId>,
            #[serde(deserialize_with = "safe_u64::deserialize")]
            sequence: u64,
            #[serde(deserialize_with = "safe_u64::deserialize")]
            timestamp_ms: TimestampMs,
            event: EventPayload,
        }

        let value = Value::deserialize(deserializer)?;
        validate_opaque_json(&value).map_err(serde::de::Error::custom)?;
        let wire =
            serde_json::from_value::<WireEventEnvelope>(value).map_err(serde::de::Error::custom)?;
        Ok(Self {
            schema_version: wire.schema_version,
            session_id: wire.session_id,
            generation_id: wire.generation_id,
            turn_id: wire.turn_id,
            sequence: wire.sequence,
            timestamp_ms: wire.timestamp_ms,
            event: wire.event,
        })
    }
}

impl EventEnvelope {
    #[must_use]
    pub fn new(
        session_id: SessionId,
        generation_id: SessionGenerationId,
        turn_id: Option<TurnId>,
        sequence: u64,
        timestamp_ms: TimestampMs,
        event: EventPayload,
    ) -> Self {
        Self {
            schema_version: PROTOCOL_VERSION,
            session_id,
            generation_id,
            turn_id,
            sequence,
            timestamp_ms,
            event,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum EventPayload {
    SessionStateChanged(SessionStateChanged),
    PromptAcknowledged(PromptAcknowledged),
    LogicalMessage(LogicalMessage),
    ToolStarted(ToolStarted),
    ToolCompleted(ToolCompleted),
    RateLimit(RateLimitEvent),
    NeedsInput(NeedsInput),
    TerminalCandidate(TerminalCandidate),
    TurnCompleted(Box<TurnResult>),
    TurnCancelled(TurnCancelledEvent),
    TurnFailed(ErrorBody),
    Warning(ProtocolWarning),
    ReplayGap(ReplayGap),
    Heartbeat(Heartbeat),
}

// ---- Launch and policy types -------------------------------------------------

/// One interactive Claude session start.
///
/// **`Serialize` and `Deserialize` are written out below rather than derived**,
/// because both have to answer a question the derive cannot: which fields the
/// caller actually WROTE. The five defaulted launch-policy fields are
/// non-`Option`, so once a request is typed, "omitted" and "sent at exactly the
/// default" are the same value -- and the both-modes refusal is a statement
/// about presence, not about equality to a default. Every `skip_serializing_if`
/// and `default` the derive used to carry is preserved by those two impls, with
/// its own argument written where it applies.
#[derive(Clone, Debug, PartialEq)]
pub struct StartSessionRequest {
    pub identity: SessionIdentity,
    /// Absolute, normalized working directory.
    ///
    /// ALWAYS PER-SESSION, and never taken from an agent. A cwd is not a
    /// preference: it is one of exactly two directories a live session BINDS,
    /// it is where the transcript slug comes from and where the file tools
    /// work, and a stored value that silently redirected where an agent
    /// operates would be the ambient resolution this product refuses
    /// everywhere else. An [`AgentSpec`] may only BOUND it, via
    /// [`AgentContainment::workspace_root`].
    pub cwd: String,
    /// The inline launch configuration, or `None` when [`Self::agent`] names a
    /// stored one.
    ///
    /// `Option` only so that "named an agent instead" is representable. Exactly
    /// one of this and [`Self::agent`] is present in every admitted request,
    /// and a present value serializes byte-identically to every release before
    /// the agent resource existed. Omitted from the wire when absent.
    pub claude: Option<ClaudeLaunchConfig>,
    /// A stored [`AgentSpec`] supplying this session's launch policy.
    ///
    /// Omitted from the wire when absent, for the same reason [`Self::cell`] is
    /// when it is the default: request DTOs are `deny_unknown_fields`, so a
    /// daemon that predates this field REFUSES any request carrying it, and
    /// omitting it keeps every pre-existing caller's bytes identical.
    ///
    /// Exactly one of this and the inline launch fields may be present. A
    /// request carrying both is refused by name -- see
    /// [`agent_supplied_start_paths`] for the derivation and the argument.
    pub agent: Option<AgentRef>,
    pub environment: EnvironmentSpec,
    pub auth_policy: AuthPolicy,
    /// A pmux-owned Claude configuration root for this session.
    ///
    /// Absent means "inherit the caller's root", which is the behaviour of
    /// every v1 release to date and the reason this is an `Option` rather than
    /// a defaulted enum: absence already carries the meaning, so nothing has to
    /// round-trip a `{"mode":"default"}` spelling. Omitted from the wire when
    /// absent.
    pub config_isolation: Option<ConfigIsolation>,
    pub terminal: TerminalSpec,
    pub lifecycle: LifecycleMode,
    pub retention: RetentionPolicy,
    pub compatibility: CompatibilityPolicy,
    /// Which cell this session is driven as, for its whole life.
    ///
    /// Chosen here rather than by a later request because the only real guard
    /// on the choice -- a tested compatibility profile -- is resolvable before
    /// a Claude process exists. Deciding at start means an inadmissible cell
    /// refuses without ever spawning a child, and means no turn can change the
    /// proof it is allowed to finish on midway through.
    ///
    /// Omitted from the wire when it is the default. Request DTOs are
    /// `deny_unknown_fields`, so a daemon that predates this field REFUSES any
    /// request carrying it -- and a `cell` that is `full` asks such a daemon for
    /// exactly what it already does. Serializing it unconditionally would have
    /// broken every pre-existing caller's request against a pre-existing daemon
    /// at `version: 1`, which is not a compatible evolution in any direction.
    pub cell: SessionCell,
}

/// The marker that separates a decode refusal **pmux composed** from one serde
/// produced out of the payload.
///
/// It is a prefix on every `serde::de::Error::custom` message this module
/// writes, and it exists so the transport can forward those and only those.
pub const DECODE_REFUSAL_MARKER: &str = "pmux-v1: ";

/// The caller-actionable half of a typed decode failure, when pmux wrote it.
///
/// **A DECODE FAILURE'S RENDERED TEXT IS NOT SAFE TO RETURN IN GENERAL**, and
/// that is why the daemon's transport answers `"request does not match protocol
/// v1"` with a content-free classification for everything else. MEASURED
/// against this crate's own decoder, `{"environment":{"set":{"SECRET":42}}}`
/// renders as ``invalid type: integer `42`, expected a string`` -- and a
/// request frame carries environment values, inline settings and MCP documents,
/// and system prompts.
///
/// This function is the one exception, and it is bounded by construction: it
/// returns only the span between [`DECODE_REFUSAL_MARKER`] and serde's own
/// ` at line N column M` suffix, so what reaches a caller is text this module
/// composed out of field paths and its own argument, never a value the caller
/// sent. A refusal that wants to be actionable adds the marker; one that does
/// not, is not forwarded.
///
/// # Errors
///
/// None. `None` means "pmux did not write this one".
#[must_use]
pub fn caller_actionable_decode_refusal(error: &serde_json::Error) -> Option<String> {
    let text = error.to_string();
    let start = text.find(DECODE_REFUSAL_MARKER)? + DECODE_REFUSAL_MARKER.len();
    let tail = &text[start..];
    let end = tail.rfind(" at line ").unwrap_or(tail.len());
    Some(tail[..end].to_owned())
}

/// The wire shape of a start, with PRESENCE preserved for every field a stored
/// agent could also supply.
///
/// This exists because the public DTO cannot answer the only question the
/// both-modes refusal needs answered. `terminal`, `lifecycle`, `retention`,
/// `compatibility`, `cell`, `auth_policy` and `environment` are all `#[serde(default)]`
/// on [`StartSessionRequest`], so by the time a request is typed, "the caller
/// omitted it" and "the caller sent exactly the default" are the same value.
/// A refusal written over the typed DTO would therefore have to say "must be at
/// its default", which is a different sentence: it silently accepts a caller
/// who wrote `"cell": "full"` beside an agent whose cell is `minified`, runs
/// the agent's, and never mentions it. That is the accepted-and-ignored field
/// this whole design refuses to ship.
///
/// `deny_unknown_fields` lives here rather than on the public struct so the
/// strict-object guarantee the shared corpus pins is unchanged.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireStartSessionRequest {
    identity: SessionIdentity,
    cwd: String,
    #[serde(default)]
    claude: Option<ClaudeLaunchConfig>,
    #[serde(default)]
    agent: Option<AgentRef>,
    #[serde(default)]
    environment: Option<EnvironmentSpec>,
    #[serde(default)]
    auth_policy: Option<AuthPolicy>,
    #[serde(default)]
    config_isolation: Option<ConfigIsolation>,
    #[serde(default)]
    terminal: Option<TerminalSpec>,
    #[serde(default)]
    lifecycle: Option<LifecycleMode>,
    #[serde(default)]
    retention: Option<RetentionPolicy>,
    #[serde(default)]
    compatibility: Option<CompatibilityPolicy>,
    #[serde(default)]
    cell: Option<SessionCell>,
}

/// Every start-request wire path a stored [`AgentSpec`] supplies.
///
/// This is one half of the both-modes refusal: a request that names an
/// [`AgentRef`] and also carries any path in this list is refused, by name,
/// with the colliding path in the message.
///
/// **THE LIST IS DERIVED, NOT REVIEWED.** Hand-listing is how the pool census
/// listed six of seven constructors, and it is the defect this repository has
/// now found thirty-three times -- in the PRACTICE (twenty-six hand-run mutation
/// campaigns sampling a space `cargo-mutants` enumerates), then in the pass
/// that fixed it (seven false claims about the tool; `current-state.md` §9.25),
/// then in the tool that checks for the class: `verify_calibration.py` held
/// 22 product line-range citations and 16 were wrong, 5 of them in strings it
/// PRINTS, one of those under a self-test named for that very citation which
/// was grading a comment instead (§9.26); and then in the SURVIVOR LIST, where
/// eighteen mutants were left open under one reason -- "needs a live pool actor
/// under `tokio` ... a harness this pass did not build" -- that was false for
/// thirteen of them, because that harness had been in the tree the whole time
/// (§9.27); and then in the GATE DRIVER's own preflight, whose docstring cited
/// the staleness proof in `pool_concurrency.rs` and whose code checked only
/// that proof's precondition, so three cells in two phases went red against a
/// stale `target/release` and not one of them said so (§9.28); and then in the
/// LATENCY DECOMPOSITION written to find the class, which reported a gate
/// reordering "verified gate-equivalent" against a run deciding a different
/// property, and proposed spending Gate 1's 250ms window on a screen `revision`
/// it never checked was a mutation counter -- it is a per-capture transition
/// counter, measured at two increments per turn (§9.29).
///
/// THIS COUNTER IS ITSELF DERIVED-AGAINST NOW. It is one number restated in
/// four places, which is the defect it describes, and
/// `tools/dev/tests/test_workflow.py::LivingDocs.test_bug_class_ordinal_matches_current_state`
/// is the thing that compares them -- against the last
/// `THE BUG CLASS, instance …` heading in `docs/current-state.md`, which is the
/// document that decides it. It lives beside the living check, not in a mutant
/// copy of this crate, because a test that reads `docs/` under `cargo-mutants`
/// is a bet on that tool's copy rules.
/// Its first run over the files those campaigns had covered found **69
/// survivors in 478 decided mutants**, including a whole guard that deletes with
/// the suite green and a re-export whose doc says "never a copy" that nothing
/// compared against the list it re-exports -- `supplied_start_paths`, which is
/// the accessor for THIS list and which returned `["xyzzy"]` with the suite
/// green.
/// Two mechanisms hold it:
///
/// * The `..`-free destructuring below. A field added to [`AgentSpec`] stops
///   this function compiling until it is written down here.
/// * `the_agent_supplied_start_paths_are_exactly_the_serialized_leaf_collision`
///   in `v1_wire`, which computes this same list from the SERIALIZERS -- the
///   leaf paths of a fully populated `AgentSpec` intersected with those of a
///   fully populated `StartSessionRequest`, reduced to the maximal paths whose
///   every leaf collides -- and asserts it equals this array. Both fixtures are
///   `..`-free struct literals, so a field added to either type forces the
///   fixture to set it, which changes the intersection, which reddens the
///   assertion. That is the same serializer-backed technique
///   [`validate_v1_serializable`] states for itself: "deliberately
///   serializer-backed rather than a second field inventory".
///
/// `environment.snapshot` is deliberately ABSENT and needs no exception: it is
/// a leaf of `StartSessionRequest` and not of `AgentSpec`, because
/// [`AgentEnvironmentSpec`] deletes the field rather than documenting that it
/// must be empty. A caller's snapshot is a fact about the calling process at
/// call time and survives alongside an agent untouched.
#[must_use]
pub fn agent_supplied_start_paths() -> &'static [&'static str] {
    // A destructuring without `..`: a field added to `AgentSpec` stops this
    // compiling until it is classified, and a field added to
    // `AgentEnvironmentSpec` stops it too.
    #[allow(unused)]
    fn exhaustive(spec: AgentSpec) {
        let AgentSpec {
            // The agent's own identity and its narrowing rules. None of these
            // is a `StartSessionRequest` field, and none can collide with one.
            name: _,
            description: _,
            containment: _,
            // Launch policy. Each supplies the start path of the same name.
            claude: _,
            environment: AgentEnvironmentSpec { set: _, unset: _ },
            auth_policy: _,
            terminal: _,
            lifecycle: _,
            retention: _,
            compatibility: _,
            cell: _,
        } = spec;
    }

    &AGENT_SUPPLIED_START_PATHS
}

/// The one array [`agent_supplied_start_paths`] returns, and the one every
/// both-modes guard is indexed against.
///
/// It is a `const` with a LENGTH because that length is the type of what both
/// presence functions return. `WireStartSessionRequest::first_agent_conflict`
/// and `StartSessionRequest::agent_supplied_presence` each hand back
/// `[(&'static str, bool); AGENT_SUPPLIED_START_PATHS.len()]`, so a path added
/// here stops both of them compiling until each says whether the request in
/// front of it carries that path. That is what makes "the serializer checks
/// every derived path" a compile-time fact instead of a review note: the
/// serializer's guard used to hand-write FIVE `emit_policy` calls where this
/// list supplies NINE, and MEASURED, an in-process caller sending
/// `cell: "minified"` beside an agent silently got the agent's `full`.
const AGENT_SUPPLIED_START_PATHS: [&str; 9] = [
    "claude",
    "environment.set",
    "environment.unset",
    "auth_policy",
    "terminal",
    "lifecycle",
    "retention",
    "compatibility",
    "cell",
];

/// One entry per path in [`AGENT_SUPPLIED_START_PATHS`], in that order.
type AgentSuppliedPresence = [(&'static str, bool); AGENT_SUPPLIED_START_PATHS.len()];

/// The first path a presence table reports as carried.
fn first_carried_path(presence: &AgentSuppliedPresence) -> Option<&'static str> {
    presence
        .iter()
        .find_map(|(path, present)| present.then_some(*path))
}

impl WireStartSessionRequest {
    /// Whether this WIRE request carries each agent-supplied path, by
    /// PRESENCE.
    ///
    /// Destructured without `..` for the same reason
    /// [`agent_supplied_start_paths`] is: a field added to the start request
    /// stops this compiling until it is classified as agent-supplied or as
    /// per-session. The return type is fixed to
    /// [`AGENT_SUPPLIED_START_PATHS`]'s length, so a path added there stops it
    /// compiling too.
    fn agent_supplied_presence(&self) -> AgentSuppliedPresence {
        let Self {
            // PER-SESSION, structurally. `identity` IS the session's name;
            // `cwd` and `config_isolation` name directories pmux CLAIMS, and an
            // agent that named one would make an agent id a contention key;
            // `agent` is the reference itself.
            identity: _,
            cwd: _,
            config_isolation: _,
            agent: _,
            // AGENT-SUPPLIED. Every path below is in
            // `agent_supplied_start_paths`, and the test named there proves the
            // two lists are the same list.
            claude,
            environment,
            auth_policy,
            terminal,
            lifecycle,
            retention,
            compatibility,
            cell,
        } = self;
        [
            ("claude", claude.is_some()),
            (
                "environment.set",
                environment
                    .as_ref()
                    .is_some_and(|environment| !environment.set.is_empty()),
            ),
            (
                "environment.unset",
                environment
                    .as_ref()
                    .is_some_and(|environment| !environment.unset.is_empty()),
            ),
            ("auth_policy", auth_policy.is_some()),
            ("terminal", terminal.is_some()),
            ("lifecycle", lifecycle.is_some()),
            ("retention", retention.is_some()),
            ("compatibility", compatibility.is_some()),
            ("cell", cell.is_some()),
        ]
    }

    /// The first path this request carries that a named agent also supplies.
    fn first_agent_conflict(&self) -> Option<&'static str> {
        first_carried_path(&self.agent_supplied_presence())
    }
}

impl StartSessionRequest {
    /// Whether this TYPED request carries each agent-supplied path.
    ///
    /// **THE TYPED TEST IS NOT THE WIRE TEST, AND CANNOT BE.** Five of these
    /// fields are non-`Option` with `#[serde(default)]`, so once a request is
    /// typed, "the caller omitted it" and "the caller sent exactly the default"
    /// are one value; only [`WireStartSessionRequest`] can tell them apart, and
    /// it is what refuses `"cell": "full"` beside a `minified` agent. What this
    /// answers is the strongest question the typed DTO admits: does the request
    /// carry a value that would be DISCARDED by resolution.
    ///
    /// Both functions return the same fixed-length table, keyed by
    /// [`AGENT_SUPPLIED_START_PATHS`], and
    /// `both_agent_presence_tables_name_every_derived_path_in_order` asserts
    /// their names are that array in that order. The serializer used to
    /// hand-write five of these nine arms and justify the omission by saying
    /// the other four were "refused by name" -- in `Deserialize`, which no
    /// in-process caller runs.
    fn agent_supplied_presence(&self) -> AgentSuppliedPresence {
        let Self {
            // PER-SESSION, structurally: see
            // `WireStartSessionRequest::agent_supplied_presence`.
            identity: _,
            cwd: _,
            config_isolation: _,
            agent: _,
            // AGENT-SUPPLIED.
            claude,
            environment,
            auth_policy,
            terminal,
            lifecycle,
            retention,
            compatibility,
            cell,
        } = self;
        [
            ("claude", claude.is_some()),
            ("environment.set", !environment.set.is_empty()),
            ("environment.unset", !environment.unset.is_empty()),
            ("auth_policy", *auth_policy != AuthPolicy::default()),
            ("terminal", *terminal != TerminalSpec::default()),
            ("lifecycle", *lifecycle != LifecycleMode::default()),
            ("retention", *retention != RetentionPolicy::default()),
            (
                "compatibility",
                *compatibility != CompatibilityPolicy::default(),
            ),
            ("cell", !cell.is_default()),
        ]
    }
}

impl Serialize for StartSessionRequest {
    /// Emits exactly the fields the wire form carries, and refuses a value the
    /// wire form cannot represent.
    ///
    /// SYMMETRIC WITH [`Deserialize`], and it has to be. `terminal`,
    /// `lifecycle`, `retention`, `compatibility` and `auth_policy` are
    /// non-`Option` and unconditionally serialized, so a request naming an
    /// agent would otherwise emit five fields the daemon then refuses -- which
    /// would make a `start_session` carrying `agent` fail every time, for a
    /// launch policy the caller never wrote. Omitting them when they are at their type
    /// default is what makes the DTO expressible at all.
    ///
    /// A NON-DEFAULT VALUE BESIDE AN AGENT IS AN ERROR, not a silent drop. A
    /// Rust embedder can construct that combination -- a UDS caller cannot,
    /// because `Deserialize` refuses it by name -- and dropping it here would
    /// be the accepted-and-ignored field this whole design exists not to ship.
    /// `validate_v1_serializable` runs this serializer before any side effect,
    /// so the refusal reaches an embedder as `invalid_config` on the same call.
    ///
    /// **THE GUARD IS DERIVED FROM `AGENT_SUPPLIED_START_PATHS`, NOT WRITTEN
    /// OUT.** It used to be five hand-written arms where that list supplies
    /// nine, and the omission was justified by saying `claude`,
    /// `environment.set`/`unset` and `cell` were "refused by name" -- which is
    /// true only in `Deserialize`, and no in-process caller runs it.
    /// `validate_v1_serializable` only SERIALIZES. MEASURED against that
    /// version: an embedder sending `cell: "minified"` beside an agent whose
    /// cell was `full` was accepted and launched `full`, an embedder's
    /// `environment.set` was silently replaced by the agent's, and an embedder
    /// naming its own `claude` executable and model got the agent's.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeStruct;

        let names_agent = self.agent.is_some();
        if names_agent && let Some(path) = first_carried_path(&self.agent_supplied_presence()) {
            return Err(serde::ser::Error::custom(format!(
                "a start naming `agent` may not also carry `{path}`: an agent supplies the whole \
                 launch policy, and merging is refused rather than resolved"
            )));
        }

        // Every one of the five defaulted policy fields is emitted exactly when
        // this request does not name an agent, and that is not a second rule:
        // a request that names one has just been proven, by the loop above, to
        // hold all nine agent-supplied paths at their omitted value.
        let emit_policy = !names_agent;

        // THE COUNT IS A SECOND STATEMENT OF THE EMISSION RULES BELOW, and it
        // is checked against them rather than trusted. `serialize_struct`'s
        // length is a HINT that `serde_json` ignores and that a
        // non-self-describing format -- bincode, postcard, MessagePack's
        // compact struct encoding -- writes as the frame's own element count,
        // so a wrong number there is a corrupt frame rather than a wrong one. A
        // full-scope mutation run made every term of this arithmetic wrong in
        // turn (`+=` as `-=` and as `*=`, `4 *` as `4 +`, the `!` deleted) and
        // no test in the workspace could tell, because the only serializer this
        // tree runs it through is the one that discards it.
        let mut fields = 2; // identity, cwd
        fields += usize::from(self.claude.is_some());
        fields += usize::from(names_agent);
        fields += 1; // environment
        fields += usize::from(emit_policy); // auth_policy
        fields += usize::from(self.config_isolation.is_some());
        fields += 4 * usize::from(emit_policy); // terminal, lifecycle, retention, compatibility
        fields += usize::from(!self.cell.is_default());

        let mut state = serializer.serialize_struct("StartSessionRequest", fields)?;
        let mut emitted = 0_usize;
        // One mutation site rather than nine, and no emission that forgets to
        // count itself: every field goes out through here.
        macro_rules! emit {
            ($name:literal, $value:expr) => {{
                state.serialize_field($name, $value)?;
                emitted += 1;
            }};
        }
        emit!("identity", &self.identity);
        emit!("cwd", &self.cwd);
        if let Some(claude) = &self.claude {
            emit!("claude", claude);
        }
        if let Some(agent) = &self.agent {
            emit!("agent", agent);
        }
        emit!("environment", &self.environment);
        if emit_policy {
            emit!("auth_policy", &self.auth_policy);
        }
        if let Some(config_isolation) = &self.config_isolation {
            emit!("config_isolation", config_isolation);
        }
        if emit_policy {
            emit!("terminal", &self.terminal);
            emit!("lifecycle", &self.lifecycle);
            emit!("retention", &self.retention);
            emit!("compatibility", &self.compatibility);
        }
        if !self.cell.is_default() {
            emit!("cell", &self.cell);
        }
        if emitted != fields {
            return Err(serde::ser::Error::custom(format!(
                "StartSessionRequest declared {fields} fields to the serializer and emitted \
                 {emitted}; a format that writes the declared length would have produced a \
                 frame no decoder can read"
            )));
        }
        state.end()
    }
}

impl<'de> Deserialize<'de> for StartSessionRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WireStartSessionRequest::deserialize(deserializer)?;
        match (&wire.agent, wire.first_agent_conflict()) {
            (Some(_), Some(path)) => {
                return Err(serde::de::Error::custom(format!(
                    "{DECODE_REFUSAL_MARKER}a start naming `agent` may not also carry `{path}`: \
                     an agent supplies the whole launch policy, and merging is refused rather \
                     than resolved, because a merge surface needs one documented rule per field \
                     and nothing derives that list. Drop `{path}`, or drop `agent` and send the \
                     inline launch fields"
                )));
            }
            (None, _) if wire.claude.is_none() => {
                return Err(serde::de::Error::custom(format!(
                    "{DECODE_REFUSAL_MARKER}a start must carry either `claude` (the inline launch \
                     configuration) or `agent` (an id and an exact stored version)"
                )));
            }
            _ => {}
        }
        Ok(Self {
            identity: wire.identity,
            cwd: wire.cwd,
            claude: wire.claude,
            agent: wire.agent,
            environment: wire.environment.unwrap_or_default(),
            auth_policy: wire.auth_policy.unwrap_or_default(),
            config_isolation: wire.config_isolation,
            terminal: wire.terminal.unwrap_or_default(),
            lifecycle: wire.lifecycle.unwrap_or_default(),
            retention: wire.retention.unwrap_or_default(),
            compatibility: wire.compatibility.unwrap_or_default(),
            cell: wire.cell.unwrap_or_default(),
        })
    }
}

/// Which cell one session is driven as.
///
/// A property of a session rather than an operation on one: it is chosen once
/// at start and there is deliberately no request that changes it mid-session,
/// because a cell change mid-flight would mean a turn could finish on a proof
/// it did not start under.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionCell {
    /// The ordinary interactive cell: the full tool surface, no clearing, and
    /// what every caller that omits the field gets.
    #[default]
    Full,
    /// Path B: no tool surface, cleared between turns via
    /// [`Request::ClearSession`], and admitted only on a tested compatibility
    /// profile. It narrows what a session may do and changes nothing about how
    /// a turn is proven finished -- the transcript remains the sole completion
    /// authority for both cells.
    Minified,
}

impl SessionCell {
    /// Named for the property that makes omitting the field sound, rather than
    /// for the variant that happens to satisfy it: a request that omits `cell`
    /// asks a daemon of any age for exactly what it already does.
    #[must_use]
    pub const fn is_default(&self) -> bool {
        matches!(self, Self::Full)
    }

    /// The ONE spelling of this cell: the wire value, and the word to put in a
    /// message beside a literal `cell`.
    ///
    /// Exists for the same reason [`EffortLevel::as_str`] does, and against the
    /// same defect: `{cell:?}` renders `Minified`, which is a Rust identifier
    /// the wire does not accept. Derived from an exhaustive `match`, so a
    /// variant added to this enum is a compile error here rather than a
    /// message that silently renders a spelling the field it names would
    /// reject. Pinned against `Serialize` in `v1_wire`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Minified => "minified",
        }
    }
}

impl std::fmt::Display for SessionCell {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionIdentity {
    New {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_optional_canonical_uuid"
        )]
        session_id: Option<SessionId>,
    },
    Resume {
        #[serde(deserialize_with = "deserialize_canonical_uuid")]
        session_id: SessionId,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeLaunchConfig {
    /// Absolute path to the Claude executable. The service rejects relative paths.
    pub executable: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<EffortLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub denied_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<ConfigSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_configs: Vec<ConfigSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugin_dirs: Vec<String>,
    #[serde(default)]
    pub system_prompt: SystemPromptPolicy,
    /// Validated compatibility arguments. Driver-owned flags are always rejected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_args: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConfigSource {
    File {
        path: String,
    },
    Inline {
        #[serde(with = "safe_json_value")]
        document: Value,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffortLevel {
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
    Max,
}

impl EffortLevel {
    /// The ONE spelling of this tier: the wire value, the `--effort` value
    /// `pmux run` accepts, and the word to put in a message beside a literal
    /// `--effort`.
    ///
    /// Exists because `{effort:?}` does not spell any of those. `XHigh` is the
    /// Rust identifier and nothing accepts it: `pmux run --effort XHigh` is
    /// refused by clap, and `"XHigh"` is refused by this type's own `Deserialize`. A
    /// refusal that renders the tier with `Debug` therefore names a spelling
    /// that is rejected by the very flag the same sentence tells the operator
    /// to use -- which is what `pseudomux_service::pool::class`'s
    /// `ModelEffortRefusal` did.
    ///
    /// Derived from an exhaustive `match`, so a variant added to this enum is a
    /// compile error here rather than a message that silently renders wrong.
    /// Pinned against `Serialize` in `v1_wire`, so the two cannot diverge.
    ///
    /// This is a spelling of a tier, NOT a licence to render argv from an
    /// `EffortLevel`: which tiers a given model admits is a separate question,
    /// answered only by `pseudomux_service::pool::class`'s table.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }
}

impl std::fmt::Display for EffortLevel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    Default,
    AcceptEdits,
    Plan,
    Auto,
    BypassPermissions,
    DontAsk,
    /// Launches Claude with `--dangerously-skip-permissions`, which is a single
    /// flag rather than a `--permission-mode` value. Every turn of a session
    /// launched this way carries the `dangerous_permission_bypass` warning.
    DangerouslySkipPermissions,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum SystemPromptPolicy {
    #[default]
    Default,
    Append {
        prompt: String,
    },
    Replace {
        prompt: String,
    },
}

impl<'de> Deserialize<'de> for SystemPromptPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
        enum WireSystemPromptPolicy {
            Default {},
            Append { prompt: String },
            Replace { prompt: String },
        }

        Ok(match WireSystemPromptPolicy::deserialize(deserializer)? {
            WireSystemPromptPolicy::Default {} => Self::Default,
            WireSystemPromptPolicy::Append { prompt } => Self::Append { prompt },
            WireSystemPromptPolicy::Replace { prompt } => Self::Replace { prompt },
        })
    }
}

/// A complete caller snapshot plus deterministic changes applied by pmux.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentSpec {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub snapshot: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub unset: BTreeSet<String>,
}

/// A pmux-owned Claude configuration root for one session.
///
/// This answers *whose configuration*, which is a different question from
/// [`AuthPolicy`]'s *whose credentials*. The two are deliberately separate, and
/// this is not a third `AuthPolicy` variant: `AuthPolicy` is consumed only by
/// code that decides which credential names survive to the child, and the
/// honest answer for config isolation is "exactly the same ones as before".
/// Folding them together would also make isolation non-composable with
/// [`AuthPolicy::Inherit`], which is a real combination.
///
/// An isolated session shares the caller's credential store **by
/// construction**: pmux pins `CLAUDE_SECURESTORAGE_CONFIG_DIR` to the config
/// root the same request would have resolved *without* isolation, so no value
/// of this field changes which account the session authenticates as. That pin
/// is computed by the service and is not expressible here, because a caller who
/// set `CLAUDE_CONFIG_DIR` by hand and forgot it would silently get a login
/// screen instead of a session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigIsolation {
    /// Absolute path to an existing, owner-only (`0700`) directory that pmux
    /// owns for the life of this session. Delivered to the child as
    /// `CLAUDE_CONFIG_DIR` in canonical form. pmux never creates it: giving
    /// `start_session` the authority to `mkdir -p` a caller-named tree would be
    /// a filesystem-write capability on the admission path that nothing else in
    /// v1 has.
    pub root: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthPolicy {
    #[default]
    Subscription,
    Inherit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalSpec {
    pub rows: u16,
    pub cols: u16,
    #[serde(default)]
    pub profile: TerminalProfile,
    #[serde(default)]
    pub input_transport: InputTransport,
}

impl Default for TerminalSpec {
    fn default() -> Self {
        Self {
            rows: 24,
            cols: 120,
            profile: TerminalProfile::default(),
            input_transport: InputTransport::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalProfile {
    #[default]
    Transparent,
    RmuxStandard,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputTransport {
    #[default]
    Auto,
    Sdk,
    AttachedStream,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum LifecycleMode {
    #[default]
    Transcript,
    Hybrid {
        #[serde(default = "default_hook_timeout_ms", with = "safe_u64")]
        hook_timeout_ms: u64,
    },
}

impl<'de> Deserialize<'de> for LifecycleMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
        enum WireLifecycleMode {
            Transcript {},
            Hybrid {
                #[serde(default = "default_hook_timeout_ms", with = "safe_u64")]
                hook_timeout_ms: u64,
            },
        }

        Ok(match WireLifecycleMode::deserialize(deserializer)? {
            WireLifecycleMode::Transcript {} => Self::Transcript,
            WireLifecycleMode::Hybrid { hook_timeout_ms } => Self::Hybrid { hook_timeout_ms },
        })
    }
}

const fn default_hook_timeout_ms() -> u64 {
    5_000
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum RetentionPolicy {
    OneShot,
    Persistent {
        #[serde(default = "default_idle_ttl_ms", with = "safe_u64")]
        idle_ttl_ms: u64,
    },
}

impl<'de> Deserialize<'de> for RetentionPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
        enum WireRetentionPolicy {
            OneShot {},
            Persistent {
                #[serde(default = "default_idle_ttl_ms", with = "safe_u64")]
                idle_ttl_ms: u64,
            },
        }

        Ok(match WireRetentionPolicy::deserialize(deserializer)? {
            WireRetentionPolicy::OneShot {} => Self::OneShot,
            WireRetentionPolicy::Persistent { idle_ttl_ms } => Self::Persistent { idle_ttl_ms },
        })
    }
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self::Persistent {
            idle_ttl_ms: default_idle_ttl_ms(),
        }
    }
}

const fn default_idle_ttl_ms() -> u64 {
    30 * 60 * 1_000
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompatibilityPolicy {
    #[default]
    RequireTested,
    AllowUntested,
}

/// Exact runtime compatibility cell selected for one Claude process.
///
/// `tested == true` means the daemon matched every field against an admitted
/// evidence profile. `false` is only possible for an explicit
/// `allow_untested` request and uses the daemon's conservative fallback drain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatibilityReport {
    pub claude_version: String,
    pub os: String,
    pub arch: String,
    pub terminal_profile: TerminalProfile,
    #[serde(
        serialize_with = "serialize_resolved_input_transport",
        deserialize_with = "deserialize_resolved_input_transport"
    )]
    pub input_transport: InputTransport,
    pub tested: bool,
    #[serde(
        serialize_with = "serialize_transcript_drain_ms",
        deserialize_with = "deserialize_transcript_drain_ms"
    )]
    pub transcript_drain_ms: u64,
}

fn serialize_resolved_input_transport<S>(
    value: &InputTransport,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if *value != InputTransport::Sdk {
        return Err(serde::ser::Error::custom(
            "compatibility input_transport must be the resolved sdk transport",
        ));
    }
    value.serialize(serializer)
}

fn deserialize_resolved_input_transport<'de, D>(deserializer: D) -> Result<InputTransport, D::Error>
where
    D: Deserializer<'de>,
{
    match InputTransport::deserialize(deserializer)? {
        InputTransport::Sdk => Ok(InputTransport::Sdk),
        _ => Err(serde::de::Error::custom(
            "compatibility input_transport must be the resolved sdk transport",
        )),
    }
}

fn deserialize_transcript_drain_ms<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = safe_u64::deserialize(deserializer)?;
    if !(1..=60_000).contains(&value) {
        return Err(serde::de::Error::custom(
            "transcript_drain_ms must be between 1 and 60000",
        ));
    }
    Ok(value)
}

fn serialize_transcript_drain_ms<S>(value: &u64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if !(1..=60_000).contains(value) {
        return Err(serde::ser::Error::custom(
            "transcript_drain_ms must be between 1 and 60000",
        ));
    }
    safe_u64::serialize(value, serializer)
}

// ---- Session and turn DTOs --------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionHandle {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub session_id: SessionId,
    pub generation_id: SessionGenerationId,
    pub state: SessionState,
    pub compatibility: CompatibilityReport,
    #[serde(with = "safe_u64")]
    pub created_at_ms: TimestampMs,
    #[serde(with = "safe_u64")]
    pub last_sequence: u64,
    /// The stored agent version this session resolved and pinned, when it named
    /// one. Absent for an inline start, which is every start before the agent
    /// resource existed -- so an existing caller's response bytes are
    /// unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<SessionAgentPin>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunTurnRequest {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub session_id: SessionId,
    pub generation_id: SessionGenerationId,
    pub turn: TurnRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnRequest {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub turn_id: TurnId,
    pub prompt: String,
    /// Absolute Unix deadline. Omit to use the server policy.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_safe_u64"
    )]
    pub deadline_unix_ms: Option<TimestampMs>,
    #[serde(default)]
    pub lease: TurnLeasePolicy,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnLeasePolicy {
    #[serde(default)]
    pub on_disconnect: DisconnectAction,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_safe_u64"
    )]
    pub heartbeat_timeout_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisconnectAction {
    #[default]
    Continue,
    CancelTurn,
    CloseSession,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnAccepted {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub session_id: SessionId,
    pub generation_id: SessionGenerationId,
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub turn_id: TurnId,
    pub replayed: bool,
    pub state: SessionState,
    #[serde(with = "safe_u64")]
    pub next_sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TurnResult {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub session_id: SessionId,
    pub generation_id: SessionGenerationId,
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub turn_id: TurnId,
    pub outcome: TurnOutcome,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub final_blocks: Vec<MessageBlock>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
    pub usage: UsageBreakdown,
    /// How many rows this turn appended on a sidechain, of any kind.
    ///
    /// A COUNT and not a token total, because the two answer different
    /// questions and `usage.sidechain` already answers the second. A `Task`
    /// subagent whose every model call reported zero tokens leaves
    /// `usage.sidechain` at its default and this field at a positive number,
    /// and on a cell launched with its tool surface denied that number is the
    /// only evidence the isolation claim was broken.
    ///
    /// Absent on the wire when zero, which is the common case and keeps every
    /// existing golden byte-identical. Zero is not a "not measured" value: the
    /// transcript analysis counts these rows on every turn it commits.
    #[serde(default, with = "safe_u64", skip_serializing_if = "is_zero_u64")]
    pub sidechain_rows: u64,
    pub timings: TurnTimings,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<ProtocolWarning>,
    pub claude_version: String,
    pub compatibility: CompatibilityReport,
    pub completion: CompletionProvenance,
    #[serde(with = "safe_u64")]
    pub final_sequence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(with = "safe_json_value")]
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        #[serde(with = "safe_json_value")]
        content: Value,
        is_error: bool,
    },
    Unknown {
        block_type: String,
        #[serde(with = "safe_json_value")]
        data: Value,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolRecord {
    pub tool_use_id: String,
    pub name: String,
    #[serde(with = "safe_json_value")]
    pub input: Value,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_safe_json_value"
    )]
    pub output: Option<Value>,
    pub status: ToolStatus,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_safe_u64"
    )]
    pub started_at_ms: Option<TimestampMs>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_safe_u64"
    )]
    pub completed_at_ms: Option<TimestampMs>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Requested,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    #[serde(with = "safe_u64")]
    pub input_tokens: u64,
    #[serde(with = "safe_u64")]
    pub output_tokens: u64,
    #[serde(with = "safe_u64")]
    pub cache_creation_input_tokens: u64,
    #[serde(with = "safe_u64")]
    pub cache_read_input_tokens: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageBreakdown {
    pub main: TokenUsage,
    pub sidechain: TokenUsage,
    pub combined: TokenUsage,
    /// Absent for subscription execution. pmux never fabricates a cost.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopReason {
    pub kind: StopReasonKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReasonKind {
    EndTurn,
    StopSequence,
    MaxTokens,
    ToolUse,
    PauseTurn,
    Refusal,
    Error,
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnTimings {
    #[serde(with = "safe_u64")]
    pub submitted_at_ms: TimestampMs,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_safe_u64"
    )]
    pub prompt_acknowledged_at_ms: Option<TimestampMs>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_safe_u64"
    )]
    pub terminal_candidate_at_ms: Option<TimestampMs>,
    #[serde(with = "safe_u64")]
    pub completed_at_ms: TimestampMs,
    /// How long the transcript had been byte-for-byte unchanged at the instant
    /// the drain gate was satisfied — the stability duration reported by the
    /// poll that admitted the commit.
    ///
    /// This is **not** `completed_at_ms - terminal_candidate_at_ms`. The two
    /// coincide only when nothing is appended after the terminal-looking
    /// message, which is why the pair alone cannot calibrate
    /// `CompatibilityReport::transcript_drain_ms`; see
    /// `last_transcript_activity_at_ms`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_safe_u64"
    )]
    pub drain_ms: Option<u64>,
    /// When pmux last observed the transcript grow before it committed this
    /// turn, in the same wall-clock domain as every other `*_at_ms` field here.
    ///
    /// Derived at commit as `completed_at_ms - drain_ms`: an absolute anchor
    /// for the stability duration, which on its own says only how long pmux
    /// waited. The calibration quantity is the **signed** difference
    /// `last_transcript_activity_at_ms - terminal_candidate_at_ms` — how much
    /// later than the terminal-looking message the transcript last changed.
    /// Producers must not publish that difference pre-subtracted: it can be
    /// negative, and a duration field would have to clamp exactly the boundary
    /// that distinguishes "the candidate row was the last row" from "one more
    /// row landed a millisecond later".
    ///
    /// Boundary, stated precisely. `terminal_candidate_at_ms` is stamped after
    /// the batch carrying the terminal row has been read and analyzed, and that
    /// same read is what last moved the transcript. So when no row arrives
    /// afterwards the difference straddles zero by a few milliseconds at most:
    /// negative by the parse-and-analyze interval, positive by the interval
    /// between the confirming poll's stability measurement (a monotonic
    /// duration) and the completion timestamp read (a wall clock). Read a
    /// difference within one actor poll interval of zero as "no late rows";
    /// only a clearly positive difference means the drain window did work.
    ///
    /// Absent on any turn that never reached the drain gate (cancelled, timed
    /// out, failed), and absent rather than clamped in the degenerate case
    /// where the reported stability exceeds `completed_at_ms` itself.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_safe_u64"
    )]
    pub last_transcript_activity_at_ms: Option<TimestampMs>,
    /// When Claude's `Stop` (or `StopFailure`) lifecycle hook was observed for
    /// this turn, in the same wall-clock domain as every other `*_at_ms` field
    /// here. Pure measurement: nothing in pmux decides completion from it.
    ///
    /// It exists to answer one question, and only by comparison. pmux proves a
    /// turn finished by waiting for the transcript to stop growing, because
    /// Claude appends incrementally with no end-of-stream marker; that bounded
    /// drain dominates per-turn overhead. A hook-based fast path — complete as
    /// soon as the Stop hook arrives, keeping the drain as a fallback — is
    /// sound only if Claude flushes the transcript *before* it fires Stop.
    ///
    /// The deciding quantity is the **signed** difference
    /// `stop_hook_at_ms - last_transcript_activity_at_ms`. A consistently
    /// positive difference means the hook arrived after the final write, so
    /// completing on the hook could only ever be faster, never wrong. **Any**
    /// negative observation means Stop can precede the last write, so the fast
    /// path would commit a truncated turn and must not be built.
    ///
    /// For the same reason as `last_transcript_activity_at_ms`, producers must
    /// publish the instant and not the pre-subtracted difference: the sign is
    /// the entire answer, and a duration field would clamp exactly the negative
    /// case this field exists to catch.
    ///
    /// Absent on any turn where no Stop hook was observed — including every
    /// session that runs without the Hybrid lifecycle hook installed, and any
    /// turn that ended before one arrived.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_safe_u64"
    )]
    pub stop_hook_at_ms: Option<TimestampMs>,
    /// When pmux **observed** the active chain's `turn_duration` marker row: the
    /// instant the transcript read that first carried it returned, in the same
    /// wall-clock domain as every other `*_at_ms` field here.
    ///
    /// It is deliberately *not* the `timestamp` the row itself carries. That
    /// string says when Claude wrote the row; pmux can only act on when the
    /// bytes reached its reader, and only the reader's instant can answer
    /// whether acting at the marker would have been premature.
    ///
    /// Pure measurement. Nothing in pmux reads this field, and no other field,
    /// warning, or state transition changes because it is present. It exists to
    /// justify or condemn one candidate optimization: pmux proves a turn
    /// finished by waiting for the transcript to stop growing, and that bounded
    /// drain is the overwhelming majority of pmux's own per-turn cost. If
    /// `turn_duration` is a true end-of-stream marker, completion could be
    /// admitted as soon as it is observed *or* the drain is satisfied — a
    /// disjunction, so the worst case stays exactly today's.
    ///
    /// Unlike a hook-based instrument, this one cannot contaminate what it
    /// measures: it installs nothing, writes nothing, mutates no settings, and
    /// alters no bytes Claude reads. It only stamps a clock read against a read
    /// pmux was already going to perform.
    ///
    /// Granularity, stated precisely. pmux reads the transcript in batches and
    /// can only admit completion after a batch has been fully ingested, so the
    /// batch is the finest grain at which the question has meaning. This is the
    /// instant of the first batch that carried the marker; rows delivered in
    /// that same batch were already in hand and could not have been missed.
    ///
    /// Absent on any turn that observed no `turn_duration` row — the row is
    /// written by recent Claude CLI versions and is no part of protocol v1's
    /// contract — and absent rather than clamped when the clock reading fell
    /// outside the safe-integer domain.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_safe_u64"
    )]
    pub turn_duration_observed_at_ms: Option<TimestampMs>,
    /// When pmux observed the first analysis-changing transcript row that
    /// arrived **strictly after** the batch carrying `turn_duration`.
    ///
    /// This is the deciding half of the pair. Absent, with
    /// `turn_duration_observed_at_ms` present, means nothing the analysis reads
    /// followed the marker, so completing at the marker would have committed the
    /// same result sooner. Present means the drain window did real work on this
    /// turn: completing at the marker would have dropped that row.
    ///
    /// Never published without `turn_duration_observed_at_ms`, so the pair can
    /// never be read as "something arrived late" without saying late relative to
    /// what.
    ///
    /// "Analysis-changing" is `ParsedRow::is_analysis_changing`: a row of a kind
    /// and scope the transcript engine admits anywhere — which is everything
    /// except the enumerated off-graph metadata records. That predicate is
    /// deliberately conservative and structural. It asks whether a row like this
    /// one is read at all, not whether this one in fact altered the committed
    /// result, because a false "something followed" merely declines to justify a
    /// faster path while a false "nothing followed" would justify an unsound
    /// one, and returning before the work is done is the single unacceptable
    /// outcome.
    ///
    /// Also pure measurement, on the same batch granularity, and equally unable
    /// to contaminate what it measures.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_safe_u64"
    )]
    pub post_turn_duration_row_observed_at_ms: Option<TimestampMs>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionProvenance {
    pub authority: CompletionAuthority,
    pub prompt_acknowledged: bool,
    pub terminal_message_observed: bool,
    pub terminal_prompt_observed: bool,
    pub terminal_quiet_observed: bool,
    pub transcript_drained: bool,
    pub lifecycle_hook_observed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionAuthority {
    Transcript,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProtocolWarning {
    pub code: String,
    pub message: String,
    #[serde(
        default,
        skip_serializing_if = "value_is_null",
        with = "safe_json_value"
    )]
    pub details: Value,
}

fn value_is_null(value: &Value) -> bool {
    value.is_null()
}

// ---- Session operations and snapshots --------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InspectSessionRequest {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub session_id: SessionId,
    pub generation_id: SessionGenerationId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub session_id: SessionId,
    pub generation_id: SessionGenerationId,
    /// The transcript this session's turns are currently proven from.
    ///
    /// Equal to `session_id` until a [`Request::ClearSession`] rotates it.
    /// Public `inspect_session` / `clear_session` refuse
    /// (`session_surface_removed`); the field remains on the wire type.
    /// Living recovery for a Messages lease is `x-pmux-cell` /
    /// `pmux doctor` conversation leases, not this snapshot.
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub transcript_session_id: SessionId,
    /// Which cell this session is driven as. Fixed at start and never mutated,
    /// so this is the caller's authoritative answer to "may I clear this
    /// session, and which completion proof do its turns run under".
    pub cell: SessionCell,
    pub state: SessionState,
    pub cwd: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_canonical_uuid"
    )]
    pub active_turn_id: Option<TurnId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_version: Option<String>,
    pub compatibility: CompatibilityReport,
    #[serde(with = "safe_u64")]
    pub created_at_ms: TimestampMs,
    #[serde(with = "safe_u64")]
    pub updated_at_ms: TimestampMs,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_safe_u64"
    )]
    pub idle_deadline_ms: Option<TimestampMs>,
    pub resumable: bool,
    #[serde(with = "safe_u64")]
    pub last_sequence: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_turn: Option<TurnSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs_input: Option<NeedsInput>,
    /// The stored agent version this session resolved and pinned at start.
    ///
    /// It does not move when the agent is updated, and that is the point: an
    /// update mints a NEW immutable version and a running session holds the one
    /// it started under, by value. Reading this after an `update_agent` is how
    /// a caller proves that.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<SessionAgentPin>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Creating,
    Booting,
    Ready,
    Submitting,
    AwaitingPromptAck,
    Running,
    NeedsInput,
    TerminalCandidate,
    Draining,
    Cancelling,
    Tainted,
    Closing,
    Closed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnSummary {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub turn_id: TurnId,
    pub outcome: TurnOutcome,
    #[serde(with = "safe_u64")]
    pub completed_at_ms: TimestampMs,
    #[serde(with = "safe_u64")]
    pub final_sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelTurnRequest {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub session_id: SessionId,
    pub generation_id: SessionGenerationId,
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub turn_id: TurnId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelTurnResult {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub session_id: SessionId,
    pub generation_id: SessionGenerationId,
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub turn_id: TurnId,
    pub outcome: CancelOutcome,
    pub session_state: SessionState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelOutcome {
    Cancelled,
    AlreadyTerminal,
    RecoveryFailed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachSessionRequest {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub session_id: SessionId,
    pub generation_id: SessionGenerationId,
    #[serde(default)]
    pub read_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<TerminalSize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalSize {
    pub rows: u16,
    pub cols: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachCapability {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub session_id: SessionId,
    pub generation_id: SessionGenerationId,
    pub token: String,
    pub endpoint: String,
    #[serde(with = "safe_u64")]
    pub expires_at_ms: TimestampMs,
    pub read_only: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloseSessionRequest {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub session_id: SessionId,
    pub generation_id: SessionGenerationId,
    #[serde(default)]
    pub policy: ClosePolicy,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClosePolicy {
    #[default]
    Graceful,
    Force,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloseSessionResult {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub session_id: SessionId,
    pub generation_id: SessionGenerationId,
    pub already_closed: bool,
    /// True only after the backend positively observed the owned process
    /// boundary empty and saw no descendant escape that would invalidate the
    /// proof. A kill/session-removal acknowledgement alone is insufficient.
    pub process_reaped: bool,
}

/// Clears one minified-cell session's context between turns.
///
/// `/clear` abandons the bound transcript -- same inode, same length, no further
/// appends -- and opens a new one under a session id Claude rotates to. Nothing
/// the caller holds changes: `session_id` and `generation_id` name the same pmux
/// session and the same process incarnation before and after.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClearSessionRequest {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub session_id: SessionId,
    pub generation_id: SessionGenerationId,
    /// The transcript the caller believes this session is bound to.
    ///
    /// A fence, not a routing key, and the same idiom as `generation_id`: a
    /// clear rotates this id, so only a caller whose view is current may ask for
    /// another rotation. Any value that is not the currently bound transcript is
    /// [`ErrorCode::IdConflict`], including one that is stale by exactly one
    /// rotation. Nothing here is ever answered as "your clear already landed".
    ///
    /// At start this equals `session_id`; afterwards it is whatever the previous
    /// [`ClearSessionResult`] returned, or whatever
    /// [`SessionSnapshot::transcript_session_id`] currently reports. The start
    /// value is why there is no already-cleared answer: a caller that lost its
    /// fence and reconstructed it as `session_id` presents exactly the bytes a
    /// *different* caller's first-ever fence carries, so any rule that answered
    /// one would answer the other, and telling a second caller that a transcript
    /// it never cleared is empty is how its turn lands behind the first caller's
    /// prompt.
    ///
    /// Public `inspect_session` / `clear_session` refuse
    /// (`session_surface_removed`). Living recovery for a Messages lease is
    /// `x-pmux-cell` / `pmux doctor` conversation leases. The pool's internal
    /// `/clear` still fences on this field.
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub expected_transcript_session_id: SessionId,
    /// Absolute Unix deadline for submitting the command to the TUI. Omit to
    /// use the server policy. It does not bound the rebind: the wait for the
    /// transcript `/clear` opens has its own fixed refusal deadline, because a
    /// caller must not be able to shorten a correctness deadline by asking
    /// nicely.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_safe_u64"
    )]
    pub deadline_unix_ms: Option<TimestampMs>,
}

/// What one clear changed, and nothing else.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClearSessionResult {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub session_id: SessionId,
    pub generation_id: SessionGenerationId,
    /// The transcript this session's turns are now proven from.
    ///
    /// Claude's id, not the caller's. It is disclosed because the caller needs
    /// it to fence its next clear, and because it names the same Claude process
    /// the caller already holds.
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub transcript_session_id: SessionId,
    /// Always `true`. A `ClearSessionResult` is reachable only by having typed
    /// `/clear` into the TUI and bound the transcript it opened, so a successful
    /// clear always rotated.
    ///
    /// The field is retained rather than removed because it is pinned by the
    /// shared conformance golden and read by both shipped clients, and a caller
    /// asserting `rotated` still gets what it always got. The `false` arm was
    /// RETIRED: it answered "your clear already landed and that transcript is
    /// still empty" for a transcript another caller could have mutated in the
    /// meantime, and two attempts to bound it by session state both leaked.
    /// There is deliberately no replacement derived from session state; see
    /// [`ClearSessionRequest::expected_transcript_session_id`] for the recovery
    /// a caller uses instead.
    pub rotated: bool,
    pub state: SessionState,
}

/// The Path B token engine: `(model, effort, prompt) -> tokens`.
///
/// It names no resource. There is no session id, no generation, no turn id, no
/// cwd, no config root, no environment, no tool list, no permission mode, no
/// terminal geometry, no lease, no retention policy and no system prompt. Every
/// one of those is a pmux-wide default owned by daemon configuration, because a
/// name a caller can write is a name two callers can write -- which is exactly
/// how `environment.set["CLAUDE_CONFIG_DIR"] = <a live cell's root>` was once
/// admitted into a live minified cell. A caller who cannot name a resource
/// cannot alias one.
///
/// `deny_unknown_fields` refuses each absent field BY NAME rather than ignoring
/// it. A caller that believes it set a system prompt and silently did not is
/// worse than one that gets [`ErrorCode::InvalidConfig`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunStatelessRequest {
    /// A Claude model alias or exact id. REQUIRED.
    ///
    /// Required rather than optional for two independent reasons. It is half the
    /// pool's class key, so an absent model would silently partition the pool on
    /// whatever the operator's `.claude.json` default resolves to; and effort
    /// cannot be validated against a model pmux was never told.
    ///
    /// A `String` rather than a closed enum deliberately: a closed enum makes
    /// every Anthropic model release a three-language protocol event against a
    /// requirement that reads "all Claude models are supported". The admitted
    /// set is daemon configuration, so a new model is an operator change.
    pub model: String,
    /// Omit for the resolved model's own default depth. Validated against the
    /// RESOLVED model, never against this enum alone: effort tiers are not
    /// uniform across Claude models, and a model with no admitted tier refuses
    /// every value of this field rather than dropping it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<EffortLevel>,
    /// Bounded and normalized before any instance is acquired: non-empty, under
    /// the service prompt limit, no leading solidus past invisibles, no unsafe
    /// control characters.
    pub prompt: String,
    /// Absolute Unix deadline. Omit for daemon policy. It may only SHORTEN
    /// pmux's wait; nothing here lengthens a correctness deadline. Same idiom
    /// and same reason as [`ClearSessionRequest::deadline_unix_ms`].
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_safe_u64"
    )]
    pub deadline_unix_ms: Option<TimestampMs>,
}

/// One completed stateless turn. Nothing here names a resource a second call
/// could reach, and nothing here is constant across the reachable value space.
///
/// Deliberately absent, each because omissions are reversible and inclusions are
/// not: `session_id`, `generation_id`, `turn_id` and `final_sequence` (a caller
/// that cannot name a resource cannot share one, and every session-addressed
/// method takes an id this response never emits, so those capabilities are
/// unconstructible rather than merely refused); `outcome` (constant --
/// `cancelled` is unreachable and `failed` returns an [`ErrorBody`] instead);
/// `final_blocks` and `tools` (structurally empty on a cell launched with the
/// tool surface denied, and a populated `tools` array here would be evidence of
/// a leak, which belongs in a quarantine diagnostic rather than in a field
/// callers learn to read); `timings` (measurement about pmux's internals);
/// `compatibility` (a property of an instance the caller did not choose and
/// cannot name); `warnings` (the only one Path B could carry is
/// `dangerous_permission_bypass`, and Path B does not launch that way);
/// `completion` (every field is `true` on every value a caller can receive,
/// because a turn that did not satisfy them was never committed); and
/// `replayed` (there is no idempotency store, deliberately: a stored result
/// keyed by a caller-supplied id is a caller-nameable resource).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatelessResult {
    /// The canonical `--model` argv value pmux launched the answering instance
    /// with. Always known: it is the pool's class key, resolved before checkout.
    pub model: String,
    /// The model the TRANSCRIPT reported, when it carried one.
    ///
    /// Deliberately a second field rather than a narrowing of `model`. `model`
    /// is what pmux asked for; this is what replied. Conflating them is how a
    /// probe measures the wrong thing, and [`TurnResult::model`] is already
    /// optional because the row is not guaranteed. pmux does not fabricate the
    /// missing case.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reported_model: Option<String>,
    /// The effort actually rendered into argv, or absent when the resolved model
    /// takes none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<EffortLevel>,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
    /// The product. Reused rather than flattened to [`TokenUsage`] because both
    /// shipped clients already validate this exact shape, so reusing it is
    /// strictly less new validation code in three languages. The risk it names
    /// -- three copies of one number on a cell that structurally has no
    /// sidechain -- is closed by refusing to commit a turn whose transcript
    /// carried a sidechain row at all, not by deleting the field.
    pub usage: UsageBreakdown,
    pub claude_version: String,
}

// ---- The agent resource ------------------------------------------------------
//
// ONE INVARIANT GOVERNS EVERY FIELD BELOW:
//
//     An agent may narrow what a session may name.
//     It may never name a resource on the session's behalf.
//
// It is a deduplication, pinning and auditability resource and it is NOT a
// security boundary: the daemon and its clients run as the same uid, so
// anything an agent would refuse the caller can send directly as an inline
// DTO. Every rule here is a NARROWING of what one request may say, composed
// with `AND` against the checks that already run, and never a capability.
//
// The launch stays a pure function of the request because the request PINS the
// version: `AgentRef::version` is required, a stored version is immutable, and
// resolution is a pure function run once at admission whose output is a
// `StartSessionRequest` nothing downstream can distinguish from one a caller
// typed inline.

/// Identity of one stored agent. Daemon-minted, canonical hyphenated on the
/// wire, and used verbatim as the store's directory name.
///
/// A UUID rather than the human `name` deliberately: a name is a wire string a
/// caller chooses, and the moment one becomes a path component, `..` is a
/// directory traversal. Minting a UUID makes traversal UNCONSTRUCTIBLE rather
/// than filtered, which is the same move `CONFIG_ROOT_ENV_DOORS` makes for the
/// config-root environment names.
pub type AgentId = Uuid;

/// Monotonic revision of one agent's stored configuration, starting at 1.
///
/// A counter and deliberately NOT a timestamp: two updates inside one clock
/// tick would share a timestamp, and a clock that steps backwards would order
/// them wrongly. This field ORDERS. Identity is
/// [`AgentDescriptor::config_digest`], which is what a caller actually wants to
/// compare -- two versions with equal digests are the same configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct AgentVersion(#[serde(with = "safe_u64")] u64);

impl<'de> Deserialize<'de> for AgentVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Same idiom as [`SessionGenerationId`]: the newtype owns its own
        // domain check, so no caller has to remember one.
        let value = safe_u64::deserialize(deserializer)?;
        if value == 0 {
            return Err(serde::de::Error::custom(format!(
                "{DECODE_REFUSAL_MARKER}agent version starts at 1; there is no version 0"
            )));
        }
        Ok(Self(value))
    }
}

impl AgentVersion {
    /// The version every `create_agent` mints.
    pub const FIRST: Self = Self(1);

    /// # Errors
    ///
    /// Returns the input when it is zero, which is not a version.
    pub const fn new(value: u64) -> Result<Self, u64> {
        if value == 0 {
            return Err(value);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The next revision after this one.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl std::fmt::Display for AgentVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

/// The environment policy an agent carries. Deliberately NOT
/// [`EnvironmentSpec`].
///
/// `EnvironmentSpec::snapshot` is "a complete caller snapshot" -- a fact about
/// the calling process at call time -- and there is no version of "an agent
/// stores one" that is not either stale the moment the caller's shell changes
/// or a file full of environment values at rest. Reusing `EnvironmentSpec` here
/// and documenting that `snapshot` must be empty would be a rule enforced by
/// prose; deleting the field makes the sentence unsayable, and makes
/// `environment.snapshot` survive the both-modes refusal with no exception
/// list.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentEnvironmentSpec {
    /// Names delivered to the child verbatim. This is the one channel the
    /// launch allowlist does not filter, which is why the store is owner-only
    /// from birth and why `get_agent` returns these values as digests.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub unset: BTreeSet<String>,
}

/// What an agent may say about the resources a session names.
///
/// EVERY FIELD HERE NARROWS. There is no value of any of them that makes an
/// otherwise-refused start admissible; each is composed with `AND` against the
/// checks that already run, and each runs BEFORE them so the existing rules
/// then run unchanged.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentContainment {
    /// Absolute directory every session's `cwd` must resolve INSIDE.
    ///
    /// The agent never supplies a cwd; the caller still writes one on every
    /// call, so the command a caller typed still contains the directory it will
    /// operate in. Tested with the service's own resolving containment
    /// predicate and never with `Path::starts_with`, which is wrong under
    /// symlinks and under the `/tmp` -> `/private/tmp` rewrite this host
    /// performs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
    /// Whether a session started from this agent MUST name a `config_isolation`
    /// root. It does not name one, and cannot.
    ///
    /// An agent that NAMED a root would make an agent id into a contention key:
    /// N sessions from one agent would all claim one root, and the seed
    /// disposition would start refusing starts as a function of how popular the
    /// agent is.
    ///
    /// `false` beside `cell: minified` is REFUSED at `create_agent` rather than
    /// silently overridden by the minified cell's own requirement. A field that
    /// is accepted and ignored is the defect this whole design exists not to
    /// ship.
    #[serde(default)]
    pub require_config_isolation: bool,
}

/// Everything an agent stores.
///
/// The complete difference between this and [`StartSessionRequest`] is the
/// invariant at the top of this section, and that difference IS the design:
/// `cwd`, `config_isolation` and `identity` name resources and stay
/// per-session; `environment.snapshot` is a fact about the calling process and
/// stays per-session structurally; everything else is launch policy a caller
/// retypes identically on every call.
///
/// AN AGENT CARRIES PREFERENCES AND NO EVIDENCE. It cannot claim its cell is
/// tested, cannot widen the launch-environment allowlist, cannot admit an
/// untested compatibility profile the registry would refuse, and cannot reach
/// anything in the Path B pool's settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSpec {
    /// A human label. No filesystem role, no uniqueness requirement, and
    /// deliberately not the id.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub claude: ClaudeLaunchConfig,
    #[serde(default)]
    pub environment: AgentEnvironmentSpec,
    #[serde(default)]
    pub auth_policy: AuthPolicy,
    #[serde(default)]
    pub terminal: TerminalSpec,
    #[serde(default)]
    pub lifecycle: LifecycleMode,
    #[serde(default)]
    pub retention: RetentionPolicy,
    #[serde(default)]
    pub compatibility: CompatibilityPolicy,
    #[serde(default, skip_serializing_if = "SessionCell::is_default")]
    pub cell: SessionCell,
    #[serde(default)]
    pub containment: AgentContainment,
}

/// The exact stored configuration one session runs.
///
/// There is deliberately no "omit for latest" shorthand. "Latest at start time"
/// makes the launch a function of WHEN the request arrived, which is precisely
/// the impurity `docs/spec.md` Sec. 4.4 forbids -- and it is the same refusal
/// [`RunStatelessRequest::model`] already makes for the same stated reason. A
/// caller wanting the head does one `get_agent` and gets a value it can log.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRef {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub agent_id: AgentId,
    /// REQUIRED. See the type doc.
    pub version: AgentVersion,
}

/// What a running session pinned at start, published so a caller can check what
/// it actually launched rather than trust that resolution did what it said.
///
/// A session resolves and COPIES its `AgentSpec` at start and holds
/// `(agent_id, version, config_digest)` for life. This is the same rule, for
/// the same reason, as [`SessionCell`]: a configuration change mid-flight would
/// mean a turn could finish under a policy it did not start under. It is also
/// why no `delete` method is needed to avoid stranding anyone -- a running
/// session never reads the store again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionAgentPin {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub agent_id: AgentId,
    pub version: AgentVersion,
    /// Lowercase hex SHA-256 over the canonical serialization of the
    /// UNREDACTED spec this session resolved. Identity, where `version` is only
    /// order.
    pub config_digest: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateAgentRequest {
    pub spec: AgentSpec,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetAgentRequest {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub agent_id: AgentId,
    /// Omit for the current head. Absent means "whatever is current NOW", which
    /// is honest for a READ and is exactly what [`AgentRef`] refuses for a
    /// LAUNCH: a read reports, a launch commits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<AgentVersion>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListAgentsRequest {}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateAgentRequest {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub agent_id: AgentId,
    /// The version the caller believes is current. REQUIRED, and a fence rather
    /// than a routing key -- the same idiom and the same argument as
    /// [`ClearSessionRequest::expected_transcript_session_id`]. Any value that
    /// is not the current head is [`ErrorCode::IdConflict`], including one
    /// stale by exactly one revision, and nothing here is ever answered as
    /// "your update already landed".
    pub expected_version: AgentVersion,
    /// The COMPLETE replacement spec. There is deliberately no partial update:
    /// a patch surface has one merge rule per field and nothing derives that
    /// list. Read, edit, write.
    pub spec: AgentSpec,
}

/// One stored agent version, as read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentDescriptor {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub agent_id: AgentId,
    pub version: AgentVersion,
    /// Lowercase hex SHA-256 over the canonical serialization of the UNREDACTED
    /// spec.
    ///
    /// This is identity; `version` is only order. It is also what makes the
    /// argv-purity claim checkable without disclosing an environment value or
    /// an inline settings document: [`Self::spec`] is redacted on the wire and
    /// this digest is not computed from the redacted form.
    pub config_digest: String,
    /// The stored spec, with every environment VALUE and every inline
    /// settings/MCP document body replaced by `sha256:<hex>` of its bytes.
    ///
    /// `system_prompt` is deliberately NOT redacted. The deleted `pmux probe`
    /// command redacted it because `probe` printed to a terminal; an agent's
    /// system prompt is the single most important thing about it and an
    /// inspection surface that hides it is useless.
    ///
    /// **OPAQUE ON THE RESPONSE, and typed with [`Self::typed_spec`].** It is
    /// an echoed REQUEST document, and the two halves of the wire contract pull
    /// in opposite directions on one: request DTOs are `deny_unknown_fields`,
    /// so `{"auth_polcy": "inherit"}` can never be stored as a silent default;
    /// response DTOs must accept unknown fields, so a newer daemon can add one
    /// without breaking an older client. A single strict type on a response
    /// would force every client in all three languages to keep two decoders for
    /// one type, and none of them does -- so the RESPONSE carries the document
    /// and the caller decodes it with [`AgentSpec`], which is strict, exactly
    /// where strictness is what is wanted. This is the same treatment
    /// [`ConfigSource::Inline`]'s `document` already gets, and for the same
    /// reason.
    #[serde(with = "safe_json_value")]
    pub spec: Value,
    #[serde(with = "safe_u64")]
    pub created_at_ms: TimestampMs,
    #[serde(with = "safe_u64")]
    pub updated_at_ms: TimestampMs,
}

impl AgentDescriptor {
    /// The echoed configuration, decoded with the STRICT request type.
    ///
    /// This is where a caller gets `deny_unknown_fields` back: a document
    /// carrying a field this build does not understand is refused here, by
    /// name, rather than silently ignored -- while the frame that carried it
    /// still decoded, which is what lets an older client read a newer daemon's
    /// response at all.
    ///
    /// # Errors
    ///
    /// The decoder's own error, naming the offending field.
    pub fn typed_spec(&self) -> Result<AgentSpec, serde_json::Error> {
        serde_json::from_value(self.spec.clone())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentList {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<AgentSummary>,
    /// Every stored record the listing could not read, by id, with the reason.
    ///
    /// **A LIST REPORTS WHAT IT COULD NOT READ, AND NEVER LOSES WHAT IT
    /// COULD.** The daemon used to answer the whole listing with the first
    /// record's refusal, which made `no agent <id>`'s own recommendation --
    /// "list the stored agents" -- unreachable in
    /// precisely the state it was offered. Dropping the bad record instead
    /// would have been worse: a stored agent that stopped appearing without a
    /// word is the accepted-and-ignored shape this protocol refuses.
    ///
    /// Omitted from the wire when empty, so the ordinary listing's bytes are
    /// exactly what every release before this field sent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unreadable: Vec<AgentListFailure>,
}

/// One stored record a listing could not read.
///
/// `agent_id` is present because the directory name is the one thing that was
/// still legible -- a record is only reported here at all if its name parsed as
/// the canonical UUID this store mints. `reason` is the same sentence
/// `get_agent` would have answered for it, so the two surfaces cannot drift.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentListFailure {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub agent_id: AgentId,
    pub reason: String,
}

/// Deliberately not `Vec<AgentDescriptor>`: a list is a directory read, and
/// returning every agent's full spec would make `list_agents` the most
/// expensive request on the socket and would spray every stored environment key
/// across one frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentSummary {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub agent_id: AgentId,
    pub version: AgentVersion,
    pub config_digest: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub cell: SessionCell,
    #[serde(with = "safe_u64")]
    pub updated_at_ms: TimestampMs,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscribeEventsRequest {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub session_id: SessionId,
    pub generation_id: SessionGenerationId,
    #[serde(default, with = "safe_u64")]
    pub after_sequence: u64,
    /// Long-poll duration. Zero requests the currently available batch only.
    #[serde(
        default,
        serialize_with = "safe_u64::serialize",
        deserialize_with = "deserialize_subscribe_wait_ms"
    )]
    pub wait_ms: u64,
    /// Zero lets the server apply its bounded default.
    #[serde(default, deserialize_with = "deserialize_subscribe_events")]
    pub max_events: u32,
}

fn deserialize_subscribe_wait_ms<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = safe_u64::deserialize(deserializer)?;
    if value > MAX_SUBSCRIBE_WAIT_MS {
        return Err(serde::de::Error::custom(format!(
            "wait_ms must not exceed {MAX_SUBSCRIBE_WAIT_MS}"
        )));
    }
    Ok(value)
}

fn deserialize_subscribe_events<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let value = u32::deserialize(deserializer)?;
    if value > MAX_SUBSCRIBE_EVENTS {
        return Err(serde::de::Error::custom(format!(
            "max_events must not exceed {MAX_SUBSCRIBE_EVENTS}"
        )));
    }
    Ok(value)
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EventBatch {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<EventEnvelope>,
    #[serde(with = "safe_u64")]
    pub next_sequence: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay_gap: Option<ReplayGap>,
}

impl<'de> Deserialize<'de> for EventBatch {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireEventBatch {
            #[serde(default)]
            events: Vec<EventEnvelope>,
            #[serde(deserialize_with = "safe_u64::deserialize")]
            next_sequence: u64,
            replay_gap: Option<ReplayGap>,
        }

        let wire = WireEventBatch::deserialize(deserializer)?;
        if let Some(gap) = &wire.replay_gap {
            if !wire.events.is_empty() {
                return Err(serde::de::Error::custom(
                    "a replay-gap batch cannot contain ordinary events",
                ));
            }
            let expected_next = gap
                .snapshot
                .last_sequence
                .checked_add(1)
                .ok_or_else(|| serde::de::Error::custom("replay-gap cursor overflow"))?;
            let first_requested = gap
                .requested_after
                .checked_add(1)
                .ok_or_else(|| serde::de::Error::custom("replay-gap request cursor overflow"))?;
            if gap.next_sequence != expected_next || wire.next_sequence != expected_next {
                return Err(serde::de::Error::custom(
                    "replay-gap, snapshot, and batch cursors must agree exactly",
                ));
            }
            if first_requested >= gap.oldest_available || gap.oldest_available > expected_next {
                return Err(serde::de::Error::custom(
                    "replay-gap retained range does not prove that requested events were lost",
                ));
            }
        }

        Ok(Self {
            events: wire.events,
            next_sequence: wire.next_sequence,
            replay_gap: wire.replay_gap,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReplayGap {
    #[serde(with = "safe_u64")]
    pub requested_after: u64,
    #[serde(with = "safe_u64")]
    pub oldest_available: u64,
    #[serde(with = "safe_u64")]
    pub next_sequence: u64,
    /// Current state from which a client can resume after losing replay history.
    pub snapshot: Box<SessionSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunOnceRequest {
    pub session: StartSessionRequest,
    pub turn: TurnRequest,
}

// ---- Event payloads ---------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStateChanged {
    pub previous: SessionState,
    pub current: SessionState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptAcknowledged {
    pub prompt_uuid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_id: Option<String>,
    #[serde(with = "safe_u64")]
    pub transcript_offset: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogicalMessage {
    pub message_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub scope: MessageScope,
    pub blocks: Vec<MessageBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
    pub terminal: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageScope {
    Main,
    Sidechain,
    Team,
    Metadata,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolStarted {
    pub tool_use_id: String,
    pub name: String,
    #[serde(with = "safe_json_value")]
    pub input: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCompleted {
    pub tool_use_id: String,
    #[serde(with = "safe_json_value")]
    pub output: Value,
    pub is_error: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimitEvent {
    pub status: RateLimitStatus,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_safe_u64"
    )]
    pub resets_at_ms: Option<TimestampMs>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitStatus {
    Allowed,
    Rejected,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NeedsInput {
    pub kind: NeedsInputKind,
    pub message: String,
    #[serde(
        default,
        skip_serializing_if = "value_is_null",
        with = "safe_json_value"
    )]
    pub details: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NeedsInputKind {
    Trust,
    Login,
    Permission,
    Update,
    Quota,
    UnknownModal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalCandidate {
    pub message_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnCancelledEvent {
    pub outcome: CancelOutcome,
    pub recovered_to_ready: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heartbeat {
    pub session_state: SessionState,
}

// ---- Errors -----------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    #[serde(
        default,
        skip_serializing_if = "value_is_null",
        with = "safe_json_value"
    )]
    pub details: Value,
}

impl ErrorBody {
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
            details: Value::Null,
        }
    }

    #[must_use]
    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    #[must_use]
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }

    /// Write this refusal's advice to [`RECOMMENDATION_KEY`], keeping every
    /// other detail already on the body.
    ///
    /// The key is spelled ONCE, here, for the writer and for both readers. It
    /// used to be spelled at every site that writes advice and again at every
    /// site that renders it, which is how `bin/pmux-mcp` came to render neither
    /// -- a reader looking for a key nothing forced it to agree about.
    ///
    /// Merging rather than replacing is the whole reason this is not
    /// `with_details(json!({...}))` at the call site: every refusal that has
    /// advice also has a `violation`, and a builder that dropped one to add the
    /// other would be a silent regression in whichever half was written second.
    #[must_use]
    pub fn advising(mut self, recommendation: impl Into<String>) -> Self {
        let recommendation = Value::String(recommendation.into());
        match self.details.as_object_mut() {
            Some(details) => {
                details.insert(RECOMMENDATION_KEY.to_owned(), recommendation);
            }
            None => {
                let mut details = serde_json::Map::new();
                details.insert(RECOMMENDATION_KEY.to_owned(), recommendation);
                self.details = Value::Object(details);
            }
        }
        self
    }

    /// This refusal's advice, or `None` when it carries none.
    ///
    /// The one key inside `details` that is written to be read by a person, so
    /// the one key a surface may render without deciding, per field, whether it
    /// is safe. `details` at large is a general diagnostic channel that also
    /// carries attach capability tokens and backend matcher text.
    #[must_use]
    pub fn recommendation(&self) -> Option<&str> {
        self.details.get(RECOMMENDATION_KEY)?.as_str()
    }
}

/// The advice channel inside [`ErrorBody::details`]: what a caller should do
/// next, in the daemon's own words.
pub const RECOMMENDATION_KEY: &str = "recommendation";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidConfig,
    UnsupportedFeature,
    UnsupportedClaudeVersion,
    ClaudeNotFound,
    RmuxUnavailable,
    RmuxIncompatible,
    PersistenceDisabled,
    TranscriptUnavailable,
    SchemaDrift,
    PromptNotAcknowledged,
    ResultTooLarge,
    TurnHistoryCapacityExceeded,
    SessionBusy,
    IdConflict,
    IdCollision,
    SessionNotFound,
    StaleSessionGeneration,
    NeedsTrust,
    NeedsLogin,
    NeedsPermission,
    NeedsUpdate,
    NeedsInput,
    RateLimited,
    AuthenticationFailed,
    BillingFailed,
    PermissionDenied,
    TurnTimeout,
    Cancelled,
    RecoveryFailed,
    ClaudeExited,
    DaemonLost,
    ReplayGap,
    ProtocolVersionMismatch,
    Internal,
}
