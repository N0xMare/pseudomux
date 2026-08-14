//! Native Rust client for pmux protocol v1.
//!
//! A client is configured with one explicit Unix-domain socket path. It never
//! discovers or starts a daemon. Each request uses a fresh connection, making
//! retries and event-stream reconnection deterministic and avoiding hidden
//! shared connection state.

#![cfg(unix)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsString;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_core::Stream;
use pseudomux_protocol::v1::{
    self, AgentDescriptor, AgentId, AgentList, AgentSpec, AgentVersion, AttachCapability,
    AttachSessionRequest, CancelTurnRequest, CancelTurnResult, ClearSessionRequest,
    ClearSessionResult, ClosePolicy, CloseSessionRequest, CloseSessionResult, CreateAgentRequest,
    DaemonDiagnosis, EnvironmentSpec, ErrorBody, EventBatch, EventEnvelope, GetAgentRequest,
    InspectSessionRequest, ListAgentsRequest, Pong, ReplayGap, Request, RequestEnvelope,
    ResponseEnvelope, ResponsePayload, ResponseResult, RunOnceRequest, RunStatelessRequest,
    RunTurnRequest, SessionGenerationId, SessionHandle, SessionId, SessionIdentity,
    SessionSnapshot, StartSessionRequest, StatelessResult, SubscribeEventsRequest, TurnAccepted,
    TurnId, TurnRequest, TurnResult, UpdateAgentRequest,
};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::time::timeout;
use uuid::Uuid;

pub mod agent_profile;
pub mod prompt;

pub use agent_profile::{
    AGENT_PROFILE_VERSION, AgentProfile, AgentProfileError, MAX_AGENT_CHAIN_DEPTH,
    load_agent_profile, verify_required_environment,
};
pub use prompt::normalize_cli_prompt;

pub const DEFAULT_MAX_FRAME_BYTES: usize = v1::MAX_NATIVE_FRAME_BYTES;
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(45);
/// Transport ceiling for a no-deadline one-shot. This contains pmuxd's
/// default startup, ten-minute turn, maximum transcript stabilization,
/// cancellation recovery, and process-reap budgets with scheduling margin.
pub const DEFAULT_RUN_ONCE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Extra response window after an explicit absolute turn deadline. The turn
/// deadline remains authoritative; this margin only lets pmuxd publish its
/// bounded cancellation, transcript-drain, and cleanup outcome.
pub const RUN_ONCE_RESPONSE_MARGIN: Duration = Duration::from_secs(120);

pub type ClientResult<T> = Result<T, ClientError>;

/// Captures the process environment exactly or refuses non-UTF-8 entries.
///
/// Protocol v1 carries UTF-8 JSON. Silently dropping or lossy-converting an
/// entry would change the requested Claude launch, while `std::env::vars()`
/// can panic. Every native Rust entry point shares this explicit conversion.
pub fn exact_environment_snapshot() -> Result<EnvironmentSpec, EnvironmentSnapshotError> {
    let mut snapshot = BTreeMap::new();
    for (key, value) in std::env::vars_os() {
        let key = environment_os_string_to_utf8(key, "environment variable name")?;
        let value =
            environment_os_string_to_utf8(value, &format!("value of environment variable {key}"))?;
        snapshot.insert(key, value);
    }
    Ok(EnvironmentSpec {
        snapshot,
        set: BTreeMap::new(),
        unset: BTreeSet::new(),
    })
}

fn environment_os_string_to_utf8(
    value: OsString,
    label: &str,
) -> Result<String, EnvironmentSnapshotError> {
    value
        .into_string()
        .map_err(|_| EnvironmentSnapshotError(label.to_owned()))
}

#[derive(Debug, Error)]
#[error("{0} is not valid UTF-8; exact launch snapshot refused")]
pub struct EnvironmentSnapshotError(String);

#[derive(Clone, Debug)]
pub struct ClientOptions {
    pub max_frame_bytes: usize,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }
}

impl ClientOptions {
    fn validate(&self) -> ClientResult<()> {
        if self.max_frame_bytes == 0 || self.max_frame_bytes > v1::MAX_NATIVE_FRAME_BYTES {
            return Err(ClientError::InvalidOptions(format!(
                "max_frame_bytes must be between 1 and {}",
                v1::MAX_NATIVE_FRAME_BYTES
            )));
        }
        if self.connect_timeout.is_zero() {
            return Err(ClientError::InvalidOptions(
                "connect_timeout must be greater than zero".into(),
            ));
        }
        if self.request_timeout.is_zero() {
            return Err(ClientError::InvalidOptions(
                "request_timeout must be greater than zero".into(),
            ));
        }
        Ok(())
    }
}

/// A cloneable connector for one caller-selected pmuxd socket.
#[derive(Clone, Debug)]
pub struct PmuxClient {
    socket_path: Arc<PathBuf>,
    options: ClientOptions,
}

impl PmuxClient {
    /// Creates a client for exactly `socket_path`; no fallback or discovery is performed.
    pub fn new(socket_path: impl Into<PathBuf>) -> ClientResult<Self> {
        Self::with_options(socket_path, ClientOptions::default())
    }

    pub fn with_options(
        socket_path: impl Into<PathBuf>,
        options: ClientOptions,
    ) -> ClientResult<Self> {
        let socket_path = socket_path.into();
        if socket_path.as_os_str().is_empty() {
            return Err(ClientError::InvalidOptions(
                "socket_path must not be empty".into(),
            ));
        }
        if !socket_path.is_absolute() {
            return Err(ClientError::InvalidOptions(
                "socket_path must be absolute".into(),
            ));
        }
        options.validate()?;
        Ok(Self {
            socket_path: Arc::new(socket_path),
            options,
        })
    }

    #[must_use]
    pub fn socket_path(&self) -> &Path {
        self.socket_path.as_path()
    }

    #[must_use]
    pub const fn options(&self) -> &ClientOptions {
        &self.options
    }

    /// Sends a typed v1 request and returns its typed result.
    pub async fn request(&self, request: Request) -> ClientResult<ResponseResult> {
        let request_timeout = request_timeout_for(&request, self.options.request_timeout);
        let expected_request = request.clone();
        let request_id = Uuid::new_v4();
        let envelope = RequestEnvelope::new(request_id, request);
        let payload = serde_json::to_vec(&envelope)?;
        ensure_frame_size(payload.len(), self.options.max_frame_bytes)?;

        let mut stream = timeout(
            self.options.connect_timeout,
            UnixStream::connect(self.socket_path.as_path()),
        )
        .await
        .map_err(|_| ClientError::Timeout {
            operation: "connect",
            duration: self.options.connect_timeout,
        })??;

        timeout(
            request_timeout,
            write_frame(&mut stream, &payload, self.options.max_frame_bytes),
        )
        .await
        .map_err(|_| ClientError::Timeout {
            operation: "write request",
            duration: request_timeout,
        })??;

        let response_payload = timeout(
            request_timeout,
            read_frame(&mut stream, self.options.max_frame_bytes),
        )
        .await
        .map_err(|_| ClientError::Timeout {
            operation: "read response",
            duration: request_timeout,
        })??;
        let wire_value: serde_json::Value = serde_json::from_slice(&response_payload)?;
        let wire_version = wire_value
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .and_then(|version| u16::try_from(version).ok())
            .ok_or(ClientError::InvalidProtocolVersion)?;
        if !v1::is_supported_version(wire_version) {
            return Err(ClientError::UnsupportedProtocolVersion {
                expected: v1::PROTOCOL_VERSION,
                actual: wire_version,
            });
        }
        let response: ResponseEnvelope = serde_json::from_value(wire_value)?;
        if response.request_id != request_id {
            return Err(ClientError::MismatchedRequestId {
                expected: request_id,
                actual: response.request_id,
            });
        }

        match response.payload {
            ResponsePayload::Success(result) => {
                validate_result_for_request(&expected_request, &result)?;
                Ok(*result)
            }
            ResponsePayload::Failure(error) => Err(ClientError::Server(error)),
        }
    }

    pub async fn ping(&self) -> ClientResult<Pong> {
        match self.request(Request::Ping).await? {
            ResponseResult::Pong(result) => Ok(result),
            other => Err(unexpected_result("pong", &other)),
        }
    }

    /// Asks the daemon to complete one real operation against its private
    /// runtime and report what it found, per session.
    ///
    /// This costs one rmux request in the daemon regardless of how many
    /// sessions it holds, and it never starts a Claude turn.
    pub async fn diagnose(&self) -> ClientResult<DaemonDiagnosis> {
        match self.request(Request::Diagnose).await? {
            ResponseResult::Diagnosis(result) => Ok(*result),
            other => Err(unexpected_result("diagnosis", &other)),
        }
    }

    /// One stateless call: `(model, effort, prompt) -> text + usage`.
    ///
    /// The request DTO is the whole surface. It carries no cwd, no config root,
    /// no system prompt and no session id, and it is `deny_unknown_fields`, so a
    /// caller that believes it set one of those gets `invalid_config` rather
    /// than silently not having set it.
    pub async fn run_stateless(
        &self,
        request: RunStatelessRequest,
    ) -> ClientResult<StatelessResult> {
        match self.request(Request::RunStateless(request)).await? {
            ResponseResult::StatelessResult(result) => Ok(*result),
            other => Err(unexpected_result("stateless_result", &other)),
        }
    }

    pub async fn start_session(&self, request: StartSessionRequest) -> ClientResult<SessionHandle> {
        match self.request(Request::StartSession(request)).await? {
            ResponseResult::SessionStarted(result) => Ok(result),
            other => Err(unexpected_result("session_started", &other)),
        }
    }

    pub async fn inspect_session(
        &self,
        session_id: SessionId,
        generation_id: SessionGenerationId,
    ) -> ClientResult<SessionSnapshot> {
        match self
            .request(Request::InspectSession(InspectSessionRequest {
                session_id,
                generation_id,
            }))
            .await?
        {
            ResponseResult::SessionSnapshot(result) => Ok(*result),
            other => Err(unexpected_result("session_snapshot", &other)),
        }
    }

    pub async fn run_turn(
        &self,
        session_id: SessionId,
        generation_id: SessionGenerationId,
        turn: TurnRequest,
    ) -> ClientResult<TurnAccepted> {
        match self
            .request(Request::RunTurn(RunTurnRequest {
                session_id,
                generation_id,
                turn,
            }))
            .await?
        {
            ResponseResult::TurnAccepted(result) => Ok(result),
            other => Err(unexpected_result("turn_accepted", &other)),
        }
    }

    pub async fn cancel_turn(
        &self,
        session_id: SessionId,
        generation_id: SessionGenerationId,
        turn_id: TurnId,
    ) -> ClientResult<CancelTurnResult> {
        match self
            .request(Request::CancelTurn(CancelTurnRequest {
                session_id,
                generation_id,
                turn_id,
            }))
            .await?
        {
            ResponseResult::TurnCancelled(result) => Ok(result),
            other => Err(unexpected_result("turn_cancelled", &other)),
        }
    }

    pub async fn attach_session(
        &self,
        request: AttachSessionRequest,
    ) -> ClientResult<AttachCapability> {
        match self.request(Request::AttachSession(request)).await? {
            ResponseResult::AttachCapability(result) => Ok(result),
            other => Err(unexpected_result("attach_capability", &other)),
        }
    }

    pub async fn close_session(
        &self,
        session_id: SessionId,
        generation_id: SessionGenerationId,
        policy: ClosePolicy,
    ) -> ClientResult<CloseSessionResult> {
        match self
            .request(Request::CloseSession(CloseSessionRequest {
                session_id,
                generation_id,
                policy,
            }))
            .await?
        {
            ResponseResult::SessionClosed(result) => Ok(result),
            other => Err(unexpected_result("session_closed", &other)),
        }
    }

    /// Clears one minified-cell session's context between turns.
    ///
    /// `expected_transcript_session_id` is the transcript the caller believes is
    /// bound: at start it is the session id, and afterwards it is whatever the
    /// previous [`ClearSessionResult`] returned, or whatever
    /// [`SessionSnapshot::transcript_session_id`] currently reports. It is a
    /// compare-and-swap fence: any value other than the currently bound
    /// transcript is refused, including one that is stale by exactly one
    /// rotation. There is no "your clear already landed" answer, because the
    /// one-behind value is indistinguishable from the fence a session starts
    /// with, which is what a second caller holds.
    ///
    /// To recover a lost response, read the fence back from
    /// [`SessionSnapshot::transcript_session_id`] and, if certainty about the
    /// cell's contents is wanted, clear again on it; clearing an already-empty
    /// cell is semantically idempotent.
    pub async fn clear_session(
        &self,
        session_id: SessionId,
        generation_id: SessionGenerationId,
        expected_transcript_session_id: SessionId,
        deadline_unix_ms: Option<u64>,
    ) -> ClientResult<ClearSessionResult> {
        match self
            .request(Request::ClearSession(ClearSessionRequest {
                session_id,
                generation_id,
                expected_transcript_session_id,
                deadline_unix_ms,
            }))
            .await?
        {
            ResponseResult::SessionCleared(result) => Ok(result),
            other => Err(unexpected_result("session_cleared", &other)),
        }
    }

    pub async fn run_once(&self, request: RunOnceRequest) -> ClientResult<TurnResult> {
        match self.request(Request::RunOnce(request)).await? {
            ResponseResult::TurnResult(result) => Ok(*result),
            other => Err(unexpected_result("turn_result", &other)),
        }
    }

    /// Stores one reusable launch configuration and returns it at version 1.
    ///
    /// The daemon mints the id. An agent never carries a `cwd`, a
    /// `config_isolation` root, a session identity, a prompt or an environment
    /// snapshot: those are per-session and are named on every `start_session`.
    pub async fn create_agent(&self, spec: AgentSpec) -> ClientResult<AgentDescriptor> {
        match self
            .request(Request::CreateAgent(CreateAgentRequest { spec }))
            .await?
        {
            ResponseResult::AgentCreated(result) => Ok(*result),
            other => Err(unexpected_result("agent_created", &other)),
        }
    }

    /// Reads one stored agent version, or the current head when `version` is
    /// `None`.
    ///
    /// Environment values and inline settings/MCP document bodies come back as
    /// `sha256:` digests and never in the clear; `config_digest` still
    /// identifies the configuration exactly.
    pub async fn get_agent(
        &self,
        agent_id: AgentId,
        version: Option<AgentVersion>,
    ) -> ClientResult<AgentDescriptor> {
        match self
            .request(Request::GetAgent(GetAgentRequest { agent_id, version }))
            .await?
        {
            ResponseResult::Agent(result) => Ok(*result),
            other => Err(unexpected_result("agent", &other)),
        }
    }

    /// Lists every stored agent's id, current version, digest, name and cell.
    /// Deliberately not full specs.
    pub async fn list_agents(&self) -> ClientResult<AgentList> {
        match self
            .request(Request::ListAgents(ListAgentsRequest {}))
            .await?
        {
            ResponseResult::AgentList(result) => Ok(*result),
            other => Err(unexpected_result("agent_list", &other)),
        }
    }

    /// Stores a new immutable version of one agent and returns it.
    ///
    /// `expected_version` is a fence: any value that is not the current head is
    /// refused with `id_conflict`, including one stale by exactly one revision,
    /// and no update is ever answered as "already landed". `spec` is a COMPLETE
    /// replacement, not a patch. Running sessions are unaffected -- they pinned
    /// their version at start.
    pub async fn update_agent(
        &self,
        agent_id: AgentId,
        expected_version: AgentVersion,
        spec: AgentSpec,
    ) -> ClientResult<AgentDescriptor> {
        match self
            .request(Request::UpdateAgent(UpdateAgentRequest {
                agent_id,
                expected_version,
                spec,
            }))
            .await?
        {
            ResponseResult::AgentUpdated(result) => Ok(*result),
            other => Err(unexpected_result("agent_updated", &other)),
        }
    }

    pub async fn subscribe_events(
        &self,
        request: SubscribeEventsRequest,
    ) -> ClientResult<EventBatch> {
        let session_id = request.session_id;
        let generation_id = request.generation_id;
        let after_sequence = request.after_sequence;
        match self.request(Request::SubscribeEvents(request)).await? {
            ResponseResult::Events(batch) => {
                validate_event_batch(session_id, generation_id, after_sequence, &batch)?;
                Ok(batch)
            }
            other => Err(unexpected_result("events", &other)),
        }
    }

    /// Creates a reconnectable long-poll event stream beginning after a known sequence.
    #[must_use]
    pub fn event_stream(
        &self,
        session_id: SessionId,
        generation_id: SessionGenerationId,
        after_sequence: u64,
        options: EventStreamOptions,
    ) -> SequencedEventStream {
        SequencedEventStream {
            client: self.clone(),
            session_id,
            generation_id,
            after_sequence,
            options,
            pending: VecDeque::new(),
            in_flight: None,
        }
    }
}

fn validate_result_for_request(request: &Request, result: &ResponseResult) -> ClientResult<()> {
    match (request, result) {
        (Request::Ping, ResponseResult::Pong(pong)) => {
            if !v1::is_supported_version(pong.protocol_version) {
                return Err(ClientError::UnsupportedProtocolVersion {
                    expected: v1::PROTOCOL_VERSION,
                    actual: pong.protocol_version,
                });
            }
        }
        (Request::StartSession(request), ResponseResult::SessionStarted(result)) => {
            let expected = match &request.identity {
                SessionIdentity::New { session_id } => *session_id,
                SessionIdentity::Resume { session_id } => Some(*session_id),
            };
            if let Some(expected) = expected {
                validate_result_session(expected, result.session_id)?;
            }
        }
        (Request::RunTurn(request), ResponseResult::TurnAccepted(result)) => {
            validate_result_session(request.session_id, result.session_id)?;
            validate_result_generation(request.generation_id, result.generation_id)?;
            validate_result_turn(request.turn.turn_id, result.turn_id)?;
        }
        (Request::CancelTurn(request), ResponseResult::TurnCancelled(result)) => {
            validate_result_session(request.session_id, result.session_id)?;
            validate_result_generation(request.generation_id, result.generation_id)?;
            validate_result_turn(request.turn_id, result.turn_id)?;
        }
        (Request::InspectSession(request), ResponseResult::SessionSnapshot(result)) => {
            validate_result_session(request.session_id, result.session_id)?;
            validate_result_generation(request.generation_id, result.generation_id)?;
        }
        (Request::AttachSession(request), ResponseResult::AttachCapability(result)) => {
            validate_result_session(request.session_id, result.session_id)?;
            validate_result_generation(request.generation_id, result.generation_id)?;
        }
        (Request::CloseSession(request), ResponseResult::SessionClosed(result)) => {
            validate_result_session(request.session_id, result.session_id)?;
            validate_result_generation(request.generation_id, result.generation_id)?;
        }
        (Request::ClearSession(request), ResponseResult::SessionCleared(result)) => {
            validate_result_session(request.session_id, result.session_id)?;
            validate_result_generation(request.generation_id, result.generation_id)?;
        }
        (Request::RunOnce(request), ResponseResult::TurnResult(result)) => {
            let expected = match &request.session.identity {
                SessionIdentity::New { session_id } => *session_id,
                SessionIdentity::Resume { session_id } => Some(*session_id),
            };
            if let Some(expected) = expected {
                validate_result_session(expected, result.session_id)?;
            }
            validate_result_turn(request.turn.turn_id, result.turn_id)?;
        }
        _ => {}
    }
    Ok(())
}

fn validate_result_session(expected: SessionId, actual: SessionId) -> ClientResult<()> {
    if actual != expected {
        return Err(ClientError::ResultSessionMismatch { expected, actual });
    }
    Ok(())
}

fn validate_result_generation(
    expected: SessionGenerationId,
    actual: SessionGenerationId,
) -> ClientResult<()> {
    if actual != expected {
        return Err(ClientError::ResultGenerationMismatch { expected, actual });
    }
    Ok(())
}

fn validate_result_turn(expected: TurnId, actual: TurnId) -> ClientResult<()> {
    if actual != expected {
        return Err(ClientError::ResultTurnMismatch { expected, actual });
    }
    Ok(())
}

fn request_timeout_for(request: &Request, configured: Duration) -> Duration {
    let now_ms: u64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX);
    request_timeout_for_at(request, configured, now_ms)
}

fn request_timeout_for_at(request: &Request, configured: Duration, now_ms: u64) -> Duration {
    match request {
        Request::SubscribeEvents(params) => configured
            .max(Duration::from_millis(params.wait_ms).saturating_add(Duration::from_secs(5))),
        // The daemon may still be inside the input gate when the default
        // request timeout would fire. A caller-supplied deadline widens the
        // client's patience the same way `RunOnce` does, so asking for a longer
        // submission window cannot make the client give up first.
        Request::ClearSession(params) => {
            let clear_window = params.deadline_unix_ms.map_or(configured, |deadline| {
                Duration::from_millis(deadline.saturating_sub(now_ms))
                    .saturating_add(RUN_ONCE_RESPONSE_MARGIN)
            });
            configured.max(clear_window)
        }
        Request::RunOnce(params) => {
            let turn_window =
                params
                    .turn
                    .deadline_unix_ms
                    .map_or(DEFAULT_RUN_ONCE_TIMEOUT, |deadline| {
                        Duration::from_millis(deadline.saturating_sub(now_ms))
                            .saturating_add(RUN_ONCE_RESPONSE_MARGIN)
                    });
            configured.max(turn_window)
        }
        // Same shape as `RunOnce`, and for a stronger reason. A stateless call
        // is answered by the pool, and the pool may have to MINT: a cold class
        // pays a TUI launch (~4.4s measured) before the model is even asked. On
        // the default 45s request timeout the client gave up first for any
        // answer that took longer, and a client that gives up first turns a
        // completed turn into a transport error the caller cannot retry
        // idempotently.
        Request::RunStateless(params) => {
            let answer_window =
                params
                    .deadline_unix_ms
                    .map_or(DEFAULT_RUN_ONCE_TIMEOUT, |deadline| {
                        Duration::from_millis(deadline.saturating_sub(now_ms))
                            .saturating_add(RUN_ONCE_RESPONSE_MARGIN)
                    });
            configured.max(answer_window)
        }
        _ => configured,
    }
}

#[cfg(test)]
mod timeout_tests {
    use super::*;
    use pseudomux_protocol::v1::{
        AuthPolicy, ClaudeLaunchConfig, CompatibilityPolicy, EnvironmentSpec, LifecycleMode,
        RetentionPolicy, SessionCell, SessionIdentity, StartSessionRequest, SystemPromptPolicy,
        TerminalSpec, TurnLeasePolicy,
    };

    fn run_once(deadline_unix_ms: Option<u64>) -> Request {
        Request::RunOnce(RunOnceRequest {
            session: StartSessionRequest {
                identity: SessionIdentity::New { session_id: None },
                cwd: "/work".into(),
                agent: None,
                claude: Some(ClaudeLaunchConfig {
                    executable: "/usr/local/bin/claude".into(),
                    model: None,
                    effort: None,
                    permission_mode: None,
                    allowed_tools: Vec::new(),
                    denied_tools: Vec::new(),
                    settings: Vec::new(),
                    mcp_configs: Vec::new(),
                    plugin_dirs: Vec::new(),
                    system_prompt: SystemPromptPolicy::Default,
                    extra_args: Vec::new(),
                }),
                environment: EnvironmentSpec::default(),
                auth_policy: AuthPolicy::Inherit,
                config_isolation: None,
                terminal: TerminalSpec::default(),
                lifecycle: LifecycleMode::Transcript,
                retention: RetentionPolicy::OneShot,
                compatibility: CompatibilityPolicy::AllowUntested,
                cell: SessionCell::Full,
            },
            turn: TurnRequest {
                turn_id: TurnId::new_v4(),
                prompt: "short".into(),
                deadline_unix_ms,
                lease: TurnLeasePolicy::default(),
            },
        })
    }

    #[test]
    fn explicit_run_once_deadline_preserves_full_terminal_outcome_margin() {
        assert_eq!(
            request_timeout_for_at(&run_once(Some(11_000)), Duration::from_secs(1), 10_000,),
            Duration::from_secs(121)
        );
        assert_eq!(
            request_timeout_for_at(&run_once(Some(9_000)), Duration::from_secs(1), 10_000,),
            RUN_ONCE_RESPONSE_MARGIN
        );
    }

    #[test]
    fn no_deadline_run_once_contains_default_server_lifecycle_budget() {
        assert_eq!(
            request_timeout_for_at(&run_once(None), Duration::from_secs(1), 10_000),
            DEFAULT_RUN_ONCE_TIMEOUT
        );
    }

    fn run_stateless(deadline_unix_ms: Option<u64>) -> Request {
        Request::RunStateless(RunStatelessRequest {
            model: "claude-sonnet-5".into(),
            effort: None,
            prompt: "hello".into(),
            deadline_unix_ms,
        })
    }

    /// A stateless call gets the same patience a one-shot gets, and for a
    /// stronger reason: the pool may have to MINT before the model is asked.
    ///
    /// The table this reads had a `_ => configured` arm, so `run_stateless`
    /// silently got the 45-second default. A cold class pays a TUI launch
    /// (~4.4 s measured) plus a real model turn, and a client that gives up
    /// first turns a completed, billed turn into a transport error the caller
    /// cannot retry idempotently -- the answer is gone and the tokens are spent.
    #[test]
    fn a_stateless_call_gets_the_full_lifecycle_budget_and_not_the_default() {
        // No deadline: the whole one-shot ceiling, not `configured`.
        assert_eq!(
            request_timeout_for_at(&run_stateless(None), Duration::from_secs(45), 10_000),
            DEFAULT_RUN_ONCE_TIMEOUT
        );
        assert_ne!(
            request_timeout_for_at(&run_stateless(None), Duration::from_secs(45), 10_000),
            Duration::from_secs(45),
            "the wildcard arm handed a mint-and-turn call the default request timeout"
        );
        // An explicit deadline widens to it plus the response margin.
        assert_eq!(
            request_timeout_for_at(&run_stateless(Some(11_000)), Duration::from_secs(1), 10_000),
            Duration::from_secs(1) + RUN_ONCE_RESPONSE_MARGIN
        );
        // And a deadline shorter than the configured timeout never SHORTENS
        // the client's patience below it.
        assert_eq!(
            request_timeout_for_at(
                &run_stateless(Some(10_001)),
                Duration::from_secs(300),
                10_000
            ),
            Duration::from_secs(300)
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_environment_entries_are_explicit_errors_not_panics() {
        use std::os::unix::ffi::OsStringExt;

        let error = environment_os_string_to_utf8(
            OsString::from_vec(vec![b'V', b'A', b'L', 0xff]),
            "environment value",
        )
        .unwrap_err();
        assert!(error.to_string().contains("not valid UTF-8"));
    }
}

#[derive(Clone, Debug)]
pub struct EventStreamOptions {
    pub wait_ms: u64,
    pub max_events: u32,
}

impl Default for EventStreamOptions {
    fn default() -> Self {
        Self {
            wait_ms: 30_000,
            max_events: 128,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum EventStreamItem {
    Event(Box<EventEnvelope>),
    ReplayGap(ReplayGap),
}

type BatchFuture = Pin<Box<dyn Future<Output = ClientResult<EventBatch>> + Send + 'static>>;

/// A sequence-validating stream that reconnects for every long-poll batch.
///
/// Transport failures are yielded to the caller without advancing the cursor.
/// Polling again retries from the last validated sequence. Replay loss is a
/// first-class [`EventStreamItem::ReplayGap`] carrying a recovery snapshot.
pub struct SequencedEventStream {
    client: PmuxClient,
    session_id: SessionId,
    generation_id: SessionGenerationId,
    after_sequence: u64,
    options: EventStreamOptions,
    pending: VecDeque<(EventStreamItem, u64)>,
    in_flight: Option<BatchFuture>,
}

impl SequencedEventStream {
    #[must_use]
    pub const fn after_sequence(&self) -> u64 {
        self.after_sequence
    }

    fn ingest(&mut self, batch: EventBatch) -> ClientResult<()> {
        validate_event_batch(
            self.session_id,
            self.generation_id,
            self.after_sequence,
            &batch,
        )?;

        let mut pending = VecDeque::new();
        if let Some(gap) = batch.replay_gap {
            let cursor = gap.snapshot.last_sequence;
            pending.push_back((EventStreamItem::ReplayGap(gap), cursor));
        }
        for event in batch.events {
            let cursor = event.sequence;
            pending.push_back((EventStreamItem::Event(Box::new(event)), cursor));
        }

        self.pending.extend(pending);
        Ok(())
    }
}

impl Stream for SequencedEventStream {
    type Item = ClientResult<EventStreamItem>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if let Some((item, cursor)) = self.pending.pop_front() {
                self.after_sequence = cursor;
                return Poll::Ready(Some(Ok(item)));
            }

            if self.in_flight.is_none() {
                let client = self.client.clone();
                let request = SubscribeEventsRequest {
                    session_id: self.session_id,
                    generation_id: self.generation_id,
                    after_sequence: self.after_sequence,
                    wait_ms: self.options.wait_ms,
                    max_events: self.options.max_events,
                };
                self.in_flight = Some(Box::pin(
                    async move { client.subscribe_events(request).await },
                ));
            }

            let result = match self.in_flight.as_mut() {
                Some(future) => match future.as_mut().poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(result) => result,
                },
                None => unreachable!("event stream future was initialized"),
            };
            self.in_flight = None;

            match result {
                Ok(batch) => {
                    if let Err(error) = self.ingest(batch) {
                        return Poll::Ready(Some(Err(error)));
                    }
                    if self.pending.is_empty() {
                        cx.waker().wake_by_ref();
                        return Poll::Pending;
                    }
                }
                Err(error) => return Poll::Ready(Some(Err(error))),
            }
        }
    }
}

fn validate_event_batch(
    session_id: SessionId,
    generation_id: SessionGenerationId,
    requested_after: u64,
    batch: &EventBatch,
) -> ClientResult<()> {
    let mut cursor = requested_after;
    if let Some(gap) = &batch.replay_gap {
        if gap.requested_after != requested_after {
            return Err(ClientError::ReplayGapRequestMismatch {
                expected: requested_after,
                actual: gap.requested_after,
            });
        }
        if gap.snapshot.session_id != session_id {
            return Err(ClientError::EventSessionMismatch {
                expected: session_id,
                actual: gap.snapshot.session_id,
            });
        }
        if gap.snapshot.generation_id != generation_id {
            return Err(ClientError::EventGenerationMismatch {
                expected: generation_id,
                actual: gap.snapshot.generation_id,
            });
        }
        let snapshot_next = next_event_sequence(gap.snapshot.last_sequence)?;
        let first_requested = next_event_sequence(gap.requested_after)?;
        if !batch.events.is_empty()
            || gap.next_sequence != snapshot_next
            || batch.next_sequence != snapshot_next
            || first_requested >= gap.oldest_available
            || gap.oldest_available > snapshot_next
        {
            return Err(ClientError::InvalidReplayGap {
                gap_next_sequence: gap.next_sequence,
                snapshot_last_sequence: gap.snapshot.last_sequence,
                batch_next_sequence: batch.next_sequence,
            });
        }
        cursor = gap.snapshot.last_sequence;
    }

    for event in &batch.events {
        validate_event(session_id, generation_id, event)?;
        let expected = next_event_sequence(cursor)?;
        if event.sequence != expected {
            return Err(ClientError::InvalidEventSequence {
                expected,
                actual: event.sequence,
            });
        }
        cursor = event.sequence;
    }
    let expected_next = next_event_sequence(cursor)?;
    if batch.next_sequence != expected_next {
        return Err(ClientError::InvalidBatchCursor {
            expected: expected_next,
            actual: batch.next_sequence,
        });
    }
    Ok(())
}

fn next_event_sequence(cursor: u64) -> ClientResult<u64> {
    cursor
        .checked_add(1)
        .filter(|next| *next <= v1::MAX_SAFE_JSON_INTEGER)
        .ok_or(ClientError::EventCursorOverflow { cursor })
}

fn validate_event(
    session_id: SessionId,
    generation_id: SessionGenerationId,
    event: &EventEnvelope,
) -> ClientResult<()> {
    if !v1::is_supported_version(event.schema_version) {
        return Err(ClientError::UnsupportedProtocolVersion {
            expected: v1::PROTOCOL_VERSION,
            actual: event.schema_version,
        });
    }
    if event.session_id != session_id {
        return Err(ClientError::EventSessionMismatch {
            expected: session_id,
            actual: event.session_id,
        });
    }
    if event.generation_id != generation_id {
        return Err(ClientError::EventGenerationMismatch {
            expected: generation_id,
            actual: event.generation_id,
        });
    }
    Ok(())
}

#[cfg(test)]
mod protocol_properties {
    use super::*;
    use proptest::prelude::*;
    use proptest::test_runner::{Config as ProptestConfig, RngAlgorithm, RngSeed};
    use pseudomux_protocol::v1::{
        CompatibilityReport, EventPayload, InputTransport, ProtocolWarning, SessionState,
        TerminalProfile,
    };

    fn deterministic_config() -> ProptestConfig {
        ProptestConfig {
            max_shrink_iters: 10_000,
            failure_persistence: None,
            rng_algorithm: RngAlgorithm::ChaCha,
            rng_seed: RngSeed::Fixed(0x504d_5558_434c_4e54),
            ..ProptestConfig::default()
        }
    }

    #[test]
    fn deterministic_config_preserves_the_gate_requested_case_count() {
        assert_eq!(
            deterministic_config().cases,
            ProptestConfig::default().cases
        );
    }

    fn compatibility() -> CompatibilityReport {
        CompatibilityReport {
            claude_version: "test".into(),
            os: "test".into(),
            arch: "test".into(),
            terminal_profile: TerminalProfile::Transparent,
            input_transport: InputTransport::Sdk,
            tested: false,
            transcript_drain_ms: 1,
        }
    }

    fn snapshot(
        session_id: SessionId,
        generation_id: SessionGenerationId,
        last_sequence: u64,
    ) -> SessionSnapshot {
        SessionSnapshot {
            session_id,
            generation_id,
            transcript_session_id: session_id,
            cell: pseudomux_protocol::v1::SessionCell::Full,
            state: SessionState::Ready,
            cwd: "/test".into(),
            active_turn_id: None,
            claude_version: Some("test".into()),
            compatibility: compatibility(),
            created_at_ms: 0,
            updated_at_ms: last_sequence,
            idle_deadline_ms: None,
            resumable: true,
            last_sequence,
            last_turn: None,
            needs_input: None,
            agent: None,
        }
    }

    fn event(
        session_id: SessionId,
        generation_id: SessionGenerationId,
        sequence: u64,
    ) -> EventEnvelope {
        EventEnvelope::new(
            session_id,
            generation_id,
            None,
            sequence,
            sequence,
            EventPayload::Warning(ProtocolWarning {
                code: "model_event".into(),
                message: "model event".into(),
                details: serde_json::Value::Null,
            }),
        )
    }

    fn contiguous_batch(
        session_id: SessionId,
        generation_id: SessionGenerationId,
        after_sequence: u64,
        event_count: usize,
    ) -> EventBatch {
        let events = (1..=event_count)
            .map(|offset| {
                event(
                    session_id,
                    generation_id,
                    after_sequence + u64::try_from(offset).expect("small model offset"),
                )
            })
            .collect::<Vec<_>>();
        EventBatch {
            events,
            next_sequence: after_sequence
                + u64::try_from(event_count).expect("small model count")
                + 1,
            replay_gap: None,
        }
    }

    proptest! {
        #![proptest_config(deterministic_config())]

        #[test]
        fn partitioned_contiguous_delivery_matches_reference_cursor_model(
            initial_cursor in 0_u64..=(v1::MAX_SAFE_JSON_INTEGER - 1_024),
            batch_lengths in prop::collection::vec(1_u8..=16, 1..32),
        ) {
            let session_id = Uuid::from_u128(1);
            let generation_id = SessionGenerationId::from_u128(2);
            let mut cursor = initial_cursor;

            for length in batch_lengths {
                let length = usize::from(length);
                let batch = contiguous_batch(session_id, generation_id, cursor, length);
                prop_assert!(validate_event_batch(
                    session_id,
                    generation_id,
                    cursor,
                    &batch,
                ).is_ok());
                cursor += u64::try_from(length).expect("small model count");
            }
        }

        #[test]
        fn any_sequence_discontinuity_is_rejected(
            requested_after in 0_u64..=(v1::MAX_SAFE_JSON_INTEGER - 1_024),
            event_count in 1_usize..33,
            selected in any::<usize>(),
        ) {
            let session_id = Uuid::from_u128(3);
            let generation_id = SessionGenerationId::from_u128(4);
            let mut batch = contiguous_batch(
                session_id,
                generation_id,
                requested_after,
                event_count,
            );
            let index = selected % event_count;
            batch.events[index].sequence += 1;

            let rejected = matches!(
                validate_event_batch(
                    session_id,
                    generation_id,
                    requested_after,
                    &batch,
                ),
                Err(ClientError::InvalidEventSequence { .. })
            );
            prop_assert!(rejected);
        }

        #[test]
        fn any_batch_cursor_disagreement_is_rejected(
            requested_after in 0_u64..=(v1::MAX_SAFE_JSON_INTEGER - 1_024),
            event_count in 0_usize..33,
            cursor_delta in 1_u64..64,
        ) {
            let session_id = Uuid::from_u128(5);
            let generation_id = SessionGenerationId::from_u128(6);
            let mut batch = contiguous_batch(
                session_id,
                generation_id,
                requested_after,
                event_count,
            );
            batch.next_sequence += cursor_delta;

            let rejected = matches!(
                validate_event_batch(
                    session_id,
                    generation_id,
                    requested_after,
                    &batch,
                ),
                Err(ClientError::InvalidBatchCursor { .. })
            );
            prop_assert!(rejected);
        }

        #[test]
        fn coherent_replay_gap_reconciles_to_the_snapshot_cursor(
            requested_after in 0_u64..=(v1::MAX_SAFE_JSON_INTEGER - 1_024),
            retained_distance in 2_u64..128,
            retained_tail in 0_u64..128,
        ) {
            let session_id = Uuid::from_u128(7);
            let generation_id = SessionGenerationId::from_u128(8);
            let oldest_available = requested_after + retained_distance;
            let snapshot_last = oldest_available + retained_tail;
            let next_sequence = snapshot_last + 1;
            let batch = EventBatch {
                events: Vec::new(),
                next_sequence,
                replay_gap: Some(ReplayGap {
                    requested_after,
                    oldest_available,
                    next_sequence,
                    snapshot: Box::new(snapshot(
                        session_id,
                        generation_id,
                        snapshot_last,
                    )),
                }),
            };

            prop_assert!(validate_event_batch(
                session_id,
                generation_id,
                requested_after,
                &batch,
            ).is_ok());
        }
    }

    #[test]
    fn replay_gap_mutations_fail_closed_without_advancing_the_stream() {
        let session_id = Uuid::from_u128(9);
        let generation_id = SessionGenerationId::from_u128(10);
        let requested_after = 4;
        let gap = ReplayGap {
            requested_after,
            oldest_available: 8,
            next_sequence: 13,
            snapshot: Box::new(snapshot(session_id, generation_id, 12)),
        };
        let valid = EventBatch {
            events: Vec::new(),
            next_sequence: 13,
            replay_gap: Some(gap),
        };
        assert!(validate_event_batch(session_id, generation_id, requested_after, &valid,).is_ok());

        let mut wrong_request = valid.clone();
        wrong_request
            .replay_gap
            .as_mut()
            .expect("gap")
            .requested_after += 1;
        assert!(matches!(
            validate_event_batch(session_id, generation_id, requested_after, &wrong_request,),
            Err(ClientError::ReplayGapRequestMismatch { .. })
        ));

        let mut wrong_session = valid.clone();
        wrong_session
            .replay_gap
            .as_mut()
            .expect("gap")
            .snapshot
            .session_id = Uuid::from_u128(11);
        assert!(matches!(
            validate_event_batch(session_id, generation_id, requested_after, &wrong_session,),
            Err(ClientError::EventSessionMismatch { .. })
        ));

        let mut wrong_generation = valid.clone();
        wrong_generation
            .replay_gap
            .as_mut()
            .expect("gap")
            .snapshot
            .generation_id = SessionGenerationId::from_u128(12);
        assert!(matches!(
            validate_event_batch(
                session_id,
                generation_id,
                requested_after,
                &wrong_generation,
            ),
            Err(ClientError::EventGenerationMismatch { .. })
        ));

        let mut mixed = valid.clone();
        mixed.events.push(event(session_id, generation_id, 13));
        assert!(matches!(
            validate_event_batch(session_id, generation_id, requested_after, &mixed),
            Err(ClientError::InvalidReplayGap { .. })
        ));

        let mut divergent_cursor = valid;
        divergent_cursor.next_sequence += 1;
        assert!(matches!(
            validate_event_batch(
                session_id,
                generation_id,
                requested_after,
                &divergent_cursor,
            ),
            Err(ClientError::InvalidReplayGap { .. })
        ));

        let mut no_actual_loss = EventBatch {
            events: Vec::new(),
            next_sequence: 13,
            replay_gap: Some(ReplayGap {
                requested_after,
                oldest_available: requested_after + 1,
                next_sequence: 13,
                snapshot: Box::new(snapshot(session_id, generation_id, 12)),
            }),
        };
        assert!(matches!(
            validate_event_batch(session_id, generation_id, requested_after, &no_actual_loss,),
            Err(ClientError::InvalidReplayGap { .. })
        ));

        no_actual_loss
            .replay_gap
            .as_mut()
            .expect("gap")
            .oldest_available = 14;
        assert!(matches!(
            validate_event_batch(session_id, generation_id, requested_after, &no_actual_loss,),
            Err(ClientError::InvalidReplayGap { .. })
        ));
    }

    #[test]
    fn safe_integer_ceiling_never_wraps_or_reuses_a_cursor() {
        assert_eq!(
            next_event_sequence(v1::MAX_SAFE_JSON_INTEGER - 1).expect("last cursor is valid"),
            v1::MAX_SAFE_JSON_INTEGER
        );
        assert!(matches!(
            next_event_sequence(v1::MAX_SAFE_JSON_INTEGER),
            Err(ClientError::EventCursorOverflow { cursor })
                if cursor == v1::MAX_SAFE_JSON_INTEGER
        ));
        assert!(matches!(
            next_event_sequence(u64::MAX),
            Err(ClientError::EventCursorOverflow { cursor }) if cursor == u64::MAX
        ));
    }
}

fn unexpected_result(expected: &'static str, actual: &ResponseResult) -> ClientError {
    ClientError::UnexpectedResult {
        expected,
        actual: response_result_name(actual),
    }
}

const fn response_result_name(result: &ResponseResult) -> &'static str {
    match result {
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
    }
}

async fn write_frame<W>(writer: &mut W, payload: &[u8], maximum: usize) -> ClientResult<()>
where
    W: AsyncWrite + Unpin,
{
    ensure_frame_size(payload.len(), maximum)?;
    let length = u32::try_from(payload.len()).map_err(|_| ClientError::FrameTooLarge {
        advertised: payload.len(),
        maximum,
    })?;
    writer.write_all(&length.to_be_bytes()).await?;
    writer.write_all(payload).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_frame<R>(reader: &mut R, maximum: usize) -> ClientResult<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut header = [0_u8; 4];
    reader.read_exact(&mut header).await?;
    let advertised = u32::from_be_bytes(header) as usize;
    ensure_frame_size(advertised, maximum)?;
    let mut payload = vec![0; advertised];
    reader.read_exact(&mut payload).await?;
    Ok(payload)
}

fn ensure_frame_size(advertised: usize, maximum: usize) -> ClientResult<()> {
    if advertised > maximum {
        return Err(ClientError::FrameTooLarge {
            advertised,
            maximum,
        });
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("invalid client options: {0}")]
    InvalidOptions(String),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid JSON frame: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{operation} timed out after {duration:?}")]
    Timeout {
        operation: &'static str,
        duration: Duration,
    },
    #[error("frame size {advertised} exceeds configured maximum {maximum}")]
    FrameTooLarge { advertised: usize, maximum: usize },
    #[error("unsupported protocol version {actual}; expected {expected}")]
    UnsupportedProtocolVersion { expected: u16, actual: u16 },
    #[error("response is missing a valid integer protocol version")]
    InvalidProtocolVersion,
    #[error("response request id {actual} does not match {expected}")]
    MismatchedRequestId { expected: Uuid, actual: Uuid },
    #[error("pmuxd returned {actual}, expected {expected}")]
    UnexpectedResult {
        expected: &'static str,
        actual: &'static str,
    },
    #[error("result belongs to session {actual}, expected {expected}")]
    ResultSessionMismatch {
        expected: SessionId,
        actual: SessionId,
    },
    #[error("result belongs to process generation {actual}, expected {expected}")]
    ResultGenerationMismatch {
        expected: SessionGenerationId,
        actual: SessionGenerationId,
    },
    #[error("result belongs to turn {actual}, expected {expected}")]
    ResultTurnMismatch { expected: TurnId, actual: TurnId },
    #[error(
        "pmuxd error code={code:?} message={message:?} retryable={retryable}",
        code = .0.code,
        message = .0.message,
        retryable = .0.retryable
    )]
    Server(ErrorBody),
    #[error("event belongs to session {actual}, expected {expected}")]
    EventSessionMismatch {
        expected: SessionId,
        actual: SessionId,
    },
    #[error("event belongs to process generation {actual}, expected {expected}")]
    EventGenerationMismatch {
        expected: SessionGenerationId,
        actual: SessionGenerationId,
    },
    #[error("invalid event sequence {actual}; expected {expected}")]
    InvalidEventSequence { expected: u64, actual: u64 },
    #[error("event cursor {cursor} cannot advance within protocol-v1's safe-integer domain")]
    EventCursorOverflow { cursor: u64 },
    #[error("invalid batch next_sequence {actual}; expected {expected}")]
    InvalidBatchCursor { expected: u64, actual: u64 },
    #[error("replay gap is for cursor {actual}; expected {expected}")]
    ReplayGapRequestMismatch { expected: u64, actual: u64 },
    #[error(
        "invalid replay gap: gap next {gap_next_sequence}, snapshot last {snapshot_last_sequence}, batch next {batch_next_sequence}"
    )]
    InvalidReplayGap {
        gap_next_sequence: u64,
        snapshot_last_sequence: u64,
        batch_next_sequence: u64,
    },
}

#[cfg(test)]
mod error_display_tests {
    use super::*;
    use pseudomux_protocol::v1::ErrorCode;

    #[test]
    fn server_error_display_preserves_public_fields_and_redacts_details() {
        let error = ClientError::Server(
            ErrorBody::new(ErrorCode::RateLimited, "quota exhausted\nnext line")
                .retryable(true)
                .with_details(serde_json::json!({
                    "backend_matcher": "backend-matcher-secret",
                    "attach_token": "attach-capability-token-secret",
                })),
        );

        let rendered = error.to_string();
        assert!(rendered.contains("RateLimited"));
        assert!(rendered.contains("quota exhausted"));
        assert!(rendered.contains("retryable=true"));
        assert!(rendered.contains(r"\nnext line"));
        assert!(!rendered.contains('\n'));
        assert!(!rendered.contains("backend-matcher-secret"));
        assert!(!rendered.contains("attach-capability-token-secret"));

        let ClientError::Server(body) = error else {
            unreachable!()
        };
        assert_eq!(body.details["backend_matcher"], "backend-matcher-secret");
    }
}
