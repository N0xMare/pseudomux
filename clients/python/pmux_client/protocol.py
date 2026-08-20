"""Typed protocol-v1 wire shapes. Keys intentionally match native JSON."""

from __future__ import annotations

from typing import (
    Any,
    Final,
    Literal,
    NotRequired,
    TypeAlias,
    TypedDict,
    get_args,
    get_origin,
    get_type_hints,
)

PROTOCOL_VERSION = 1
MAX_NATIVE_FRAME_BYTES = 8 * 1024 * 1024
MAX_SAFE_JSON_INTEGER = 9_007_199_254_740_991

RequestId: TypeAlias = str
SessionId: TypeAlias = str
SessionGenerationId: TypeAlias = str
TurnId: TypeAlias = str
TimestampMs: TypeAlias = int
JsonValue: TypeAlias = None | bool | int | float | str | list["JsonValue"] | dict[str, "JsonValue"]


class NewSessionIdentity(TypedDict):
    mode: Literal["new"]
    session_id: NotRequired[SessionId]


class ResumeSessionIdentity(TypedDict):
    mode: Literal["resume"]
    session_id: SessionId


SessionIdentity: TypeAlias = NewSessionIdentity | ResumeSessionIdentity


class FileConfigSource(TypedDict):
    source: Literal["file"]
    path: str


class InlineConfigSource(TypedDict):
    source: Literal["inline"]
    document: JsonValue


ConfigSource: TypeAlias = FileConfigSource | InlineConfigSource
EffortLevel: TypeAlias = Literal["low", "medium", "high", "xhigh", "max"]
PermissionMode: TypeAlias = Literal[
    "default",
    "accept_edits",
    "plan",
    "auto",
    "bypass_permissions",
    "dont_ask",
    "dangerously_skip_permissions",
]


class DefaultSystemPrompt(TypedDict):
    mode: Literal["default"]


class CustomSystemPrompt(TypedDict):
    mode: Literal["append", "replace"]
    prompt: str


SystemPromptPolicy: TypeAlias = DefaultSystemPrompt | CustomSystemPrompt


class ClaudeLaunchConfig(TypedDict):
    executable: str
    model: NotRequired[str]
    effort: NotRequired[EffortLevel]
    permission_mode: NotRequired[PermissionMode]
    allowed_tools: NotRequired[list[str]]
    denied_tools: NotRequired[list[str]]
    settings: NotRequired[list[ConfigSource]]
    mcp_configs: NotRequired[list[ConfigSource]]
    plugin_dirs: NotRequired[list[str]]
    system_prompt: NotRequired[SystemPromptPolicy]
    extra_args: NotRequired[list[str]]


class EnvironmentSpec(TypedDict, total=False):
    snapshot: dict[str, str]
    set: dict[str, str]
    unset: list[str]


AuthPolicy: TypeAlias = Literal["subscription", "inherit"]
TerminalProfile: TypeAlias = Literal["transparent", "rmux_standard"]
InputTransport: TypeAlias = Literal["auto", "sdk", "attached_stream"]


class TerminalSpec(TypedDict):
    rows: int
    cols: int
    profile: NotRequired[TerminalProfile]
    input_transport: NotRequired[InputTransport]


class TranscriptLifecycle(TypedDict):
    mode: Literal["transcript"]


class HybridLifecycle(TypedDict):
    mode: Literal["hybrid"]
    hook_timeout_ms: NotRequired[int]


LifecycleMode: TypeAlias = TranscriptLifecycle | HybridLifecycle


class OneShotRetention(TypedDict):
    mode: Literal["one_shot"]


class PersistentRetention(TypedDict):
    mode: Literal["persistent"]
    idle_ttl_ms: NotRequired[int]


RetentionPolicy: TypeAlias = OneShotRetention | PersistentRetention
CompatibilityPolicy: TypeAlias = Literal["require_tested", "allow_untested"]
#: Which cell a session is driven as, chosen once at start.
#:
#: Protocol type kept for goldens. Current daemons refuse start_session /
#: clear_session / inspect_session / agent Requests with
#: ``session_surface_removed``.
#:
#: ``minified`` is the pool cell: no tool surface, admitted only on a tested
#: compatibility profile. Omitting it means ``full``. Living recovery of a
#: Messages pin is ``x-pmux-cell`` / ``pmux doctor`` conversation leases. Do
#: not teach inspect → fence → clear as a caller loop.
SessionCell: TypeAlias = Literal["full", "minified"]


class CompatibilityReport(TypedDict):
    claude_version: str
    os: str
    arch: str
    terminal_profile: TerminalProfile
    # ``auto`` is resolved before admission; attached-stream is not enabled in v1.
    input_transport: Literal["sdk"]
    tested: bool
    transcript_drain_ms: int


class ConfigIsolation(TypedDict):
    """A pmux-owned Claude configuration root for one session.

    Answers *whose configuration*, which is a different question from
    ``auth_policy``'s *whose credentials*: the daemon pins the credential store
    to the root the same request would have used without isolation, so an
    isolated session still authenticates as the same account. Omitting the field
    inherits the caller's root.
    """

    root: str


class AgentRef(TypedDict):
    """The exact stored agent version one session runs.

    Protocol type kept for goldens. Current daemons refuse start_session /
    clear_session / inspect_session / agent Requests with
    ``session_surface_removed``.

    ``version`` is REQUIRED and there is deliberately no "omit for latest":
    that would make the launch a function of WHEN the request arrived.
    """

    agent_id: str
    version: int


class SessionAgentPin(TypedDict):
    """What a running session pinned at start.

    It does not move when the agent is updated: an update mints a NEW immutable
    version and this session keeps the one it started under, by value.
    """

    agent_id: str
    version: int
    config_digest: str


class AgentEnvironmentSpec(TypedDict):
    """The environment policy an agent carries. Deliberately NOT
    ``EnvironmentSpec``: there is no ``snapshot``, because a stored caller
    snapshot is either stale the moment the caller's shell changes or a file of
    environment values at rest.
    """

    set: NotRequired[dict[str, str]]
    unset: NotRequired[list[str]]


class AgentContainment(TypedDict):
    """What an agent may say about the resources a session names.

    Every field NARROWS. There is no value of either that makes an
    otherwise-refused start admissible.
    """

    workspace_root: NotRequired[str]
    require_config_isolation: NotRequired[bool]


class AgentSpec(TypedDict):
    """Everything an agent stores: launch policy, and no resource.

    Protocol type kept for goldens. Current daemons refuse start_session /
    clear_session / inspect_session / agent Requests with
    ``session_surface_removed``.

    There is no ``cwd``, no ``config_isolation``, no session identity, no prompt
    and no environment snapshot: those were per-session fields on
    ``start_session``, which current daemons also refuse.
    """

    name: str
    description: NotRequired[str]
    claude: ClaudeLaunchConfig
    environment: NotRequired[AgentEnvironmentSpec]
    auth_policy: NotRequired[AuthPolicy]
    terminal: NotRequired[TerminalSpec]
    lifecycle: NotRequired[LifecycleMode]
    retention: NotRequired[RetentionPolicy]
    compatibility: NotRequired[CompatibilityPolicy]
    cell: NotRequired[SessionCell]
    containment: NotRequired[AgentContainment]


class AgentDescriptor(TypedDict):
    """One stored agent version, as read.

    ``spec`` is REDACTED -- every environment value and every inline
    settings/MCP document body is a ``sha256:`` digest -- and it is carried as
    an opaque document rather than a validated shape, because a request must
    refuse an unknown field and a response must tolerate one. Decode it with
    ``AgentSpec`` where strictness is what you want. ``config_digest`` is
    computed over the UNREDACTED spec, so it still identifies the configuration
    exactly.
    """

    agent_id: str
    version: int
    config_digest: str
    spec: dict[str, Any]
    created_at_ms: int
    updated_at_ms: int


class AgentSummary(TypedDict):
    agent_id: str
    version: int
    config_digest: str
    name: str
    description: NotRequired[str]
    cell: SessionCell
    updated_at_ms: int


class AgentListFailure(TypedDict):
    """One stored record ``list_agents`` could not read.

    Protocol type kept for goldens. Current daemons refuse start_session /
    clear_session / inspect_session / agent Requests with
    ``session_surface_removed``. Do not recommend ``pmux agent list``.

    The daemon reports these rather than dropping them, and rather than
    answering the whole listing with the first one's refusal.
    """

    agent_id: str
    reason: str


class AgentList(TypedDict):
    agents: NotRequired[list[AgentSummary]]
    unreadable: NotRequired[list[AgentListFailure]]


class CreateAgentRequest(TypedDict):
    spec: AgentSpec


class GetAgentRequest(TypedDict):
    agent_id: str
    version: NotRequired[int]


class UpdateAgentRequest(TypedDict):
    """A COMPLETE replacement, fenced on the version you believe is current.

    Any ``expected_version`` that is not the current head is refused with
    ``id_conflict``, including one stale by exactly one revision, and nothing is
    ever answered as "already landed".
    """

    agent_id: str
    expected_version: int
    spec: AgentSpec


class StartSessionRequest(TypedDict):
    """One interactive session start.

    Protocol type kept for goldens. Current daemons refuse start_session /
    clear_session / inspect_session / agent Requests with
    ``session_surface_removed``. Living recovery is ``x-pmux-cell`` /
    ``pmux doctor`` conversation leases. Do not teach inspect → fence →
    clear as a caller loop.

    Supply EITHER ``claude`` (the inline launch configuration) OR ``agent`` (a
    stored id and an EXACT version), never both and never neither. A request
    carrying both is refused by the daemon naming the colliding field, and
    merging is refused rather than resolved: a merge surface needs one
    documented rule per field and nothing derives that list.

    ``cwd`` is always required and is NEVER taken from the agent. An agent may
    only BOUND it, through ``containment.workspace_root``.
    """

    identity: SessionIdentity
    cwd: str
    claude: NotRequired[ClaudeLaunchConfig]
    agent: NotRequired[AgentRef]
    environment: NotRequired[EnvironmentSpec]
    auth_policy: NotRequired[AuthPolicy]
    config_isolation: NotRequired[ConfigIsolation]
    terminal: NotRequired[TerminalSpec]
    lifecycle: NotRequired[LifecycleMode]
    retention: NotRequired[RetentionPolicy]
    compatibility: NotRequired[CompatibilityPolicy]
    cell: NotRequired[SessionCell]


SessionState: TypeAlias = Literal[
    "creating",
    "booting",
    "ready",
    "submitting",
    "awaiting_prompt_ack",
    "running",
    "needs_input",
    "terminal_candidate",
    "draining",
    "cancelling",
    "tainted",
    "closing",
    "closed",
    "failed",
]


class SessionHandle(TypedDict):
    session_id: SessionId
    generation_id: SessionGenerationId
    state: SessionState
    compatibility: CompatibilityReport
    created_at_ms: TimestampMs
    last_sequence: int
    #: The stored agent version this session pinned, when it named one.
    agent: NotRequired[SessionAgentPin]


DisconnectAction: TypeAlias = Literal["continue", "cancel_turn", "close_session"]


class TurnLeasePolicy(TypedDict, total=False):
    on_disconnect: DisconnectAction
    heartbeat_timeout_ms: int


class TurnRequest(TypedDict):
    turn_id: TurnId
    prompt: str
    deadline_unix_ms: NotRequired[TimestampMs]
    lease: NotRequired[TurnLeasePolicy]


class TurnAccepted(TypedDict):
    session_id: SessionId
    generation_id: SessionGenerationId
    turn_id: TurnId
    replayed: bool
    state: SessionState
    next_sequence: int


TurnOutcome: TypeAlias = Literal["completed", "cancelled", "failed"]
ToolStatus: TypeAlias = Literal["requested", "completed", "failed", "cancelled"]


class TextBlock(TypedDict):
    kind: Literal["text"]
    text: str


class ToolUseBlock(TypedDict):
    kind: Literal["tool_use"]
    id: str
    name: str
    input: JsonValue


class ToolResultBlock(TypedDict):
    kind: Literal["tool_result"]
    tool_use_id: str
    content: JsonValue
    is_error: bool


class UnknownBlock(TypedDict):
    kind: Literal["unknown"]
    block_type: str
    data: JsonValue


MessageBlock: TypeAlias = TextBlock | ToolUseBlock | ToolResultBlock | UnknownBlock


class ToolRecord(TypedDict):
    tool_use_id: str
    name: str
    input: JsonValue
    output: NotRequired[JsonValue]
    status: ToolStatus
    started_at_ms: NotRequired[TimestampMs]
    completed_at_ms: NotRequired[TimestampMs]


class TokenUsage(TypedDict):
    input_tokens: int
    output_tokens: int
    cache_creation_input_tokens: int
    cache_read_input_tokens: int


class UsageBreakdown(TypedDict):
    main: TokenUsage
    sidechain: TokenUsage
    combined: TokenUsage
    cost_usd: NotRequired[str]


StopReasonKind: TypeAlias = Literal[
    "end_turn",
    "stop_sequence",
    "max_tokens",
    "tool_use",
    "pause_turn",
    "refusal",
    "error",
    "unknown",
]


class StopReason(TypedDict):
    kind: StopReasonKind
    raw: NotRequired[str]


class TurnTimings(TypedDict):
    submitted_at_ms: TimestampMs
    prompt_acknowledged_at_ms: NotRequired[TimestampMs]
    terminal_candidate_at_ms: NotRequired[TimestampMs]
    completed_at_ms: TimestampMs
    # How long the transcript had been byte-for-byte unchanged when the drain
    # gate admitted the commit. Not ``completed_at_ms - terminal_candidate_at_ms``.
    drain_ms: NotRequired[int]
    # When pmux last observed the transcript grow before committing the turn,
    # in the same wall-clock domain as the other ``*_at_ms`` fields. The drain
    # calibration quantity is the signed difference
    # ``last_transcript_activity_at_ms - terminal_candidate_at_ms``; a consumer
    # must compute it itself, because it can be negative by a few milliseconds
    # when no row arrived after the terminal-looking message.
    last_transcript_activity_at_ms: NotRequired[TimestampMs]
    # When Claude's ``Stop`` lifecycle hook was observed for this turn, in the
    # same wall-clock domain as the other ``*_at_ms`` fields. Measurement only:
    # nothing in pmux decides completion from it. The deciding quantity is the
    # signed difference ``stop_hook_at_ms - last_transcript_activity_at_ms``:
    # consistently positive means Claude flushed the transcript before firing
    # Stop, so a hook-based completion fast path could only ever be faster; a
    # single negative observation means the fast path would commit a truncated
    # turn and must not be built. A timestamp rather than a duration because
    # the sign is the answer and a duration would clamp the negative case.
    # Absent on any turn where no Stop hook was observed.
    stop_hook_at_ms: NotRequired[TimestampMs]


CompletionAuthority: TypeAlias = Literal["transcript"]


class CompletionProvenance(TypedDict):
    authority: CompletionAuthority
    prompt_acknowledged: bool
    terminal_message_observed: bool
    terminal_prompt_observed: bool
    terminal_quiet_observed: bool
    transcript_drained: bool
    lifecycle_hook_observed: bool


class ProtocolWarning(TypedDict):
    code: str
    message: str
    details: NotRequired[JsonValue]


class TurnResult(TypedDict):
    session_id: SessionId
    generation_id: SessionGenerationId
    turn_id: TurnId
    outcome: TurnOutcome
    text: str
    final_blocks: NotRequired[list[MessageBlock]]
    tools: NotRequired[list[ToolRecord]]
    model: NotRequired[str]
    stop_reason: NotRequired[StopReason]
    usage: UsageBreakdown
    timings: TurnTimings
    warnings: NotRequired[list[ProtocolWarning]]
    claude_version: str
    compatibility: CompatibilityReport
    completion: CompletionProvenance
    final_sequence: int


class RunStatelessRequest(TypedDict):
    """The whole Path B request surface.

    Every field a session start carries and this does not -- ``cwd``,
    ``config_isolation``, ``claude``, ``environment``, ``system_prompt``,
    ``identity`` -- is a resource the daemon mints from its own configuration.
    Their absence is the product statement, not an omission.
    """

    model: str
    effort: NotRequired[EffortLevel]
    prompt: str
    deadline_unix_ms: NotRequired[int]


class StatelessResult(TypedDict):
    """The Path B answer: text plus usage, naming no resource.

    ``model`` is what pmux ASKED for -- the pool's class key, resolved before
    checkout. ``reported_model`` is what the transcript said REPLIED, and it is
    a separate field rather than a narrowing of the first because conflating
    them is how a probe measures the wrong thing. It is absent when the
    transcript carried no ``message.model`` row; pmux does not fabricate it.
    """

    model: str
    reported_model: NotRequired[str]
    effort: NotRequired[EffortLevel]
    text: str
    stop_reason: NotRequired[StopReason]
    usage: UsageBreakdown
    claude_version: str


NeedsInputKind: TypeAlias = Literal[
    "trust", "login", "permission", "update", "quota", "unknown_modal"
]


class NeedsInput(TypedDict):
    kind: NeedsInputKind
    message: str
    details: NotRequired[JsonValue]


class TurnSummary(TypedDict):
    turn_id: TurnId
    outcome: TurnOutcome
    completed_at_ms: TimestampMs
    final_sequence: int


class SessionSnapshot(TypedDict):
    session_id: SessionId
    generation_id: SessionGenerationId
    #: Protocol type kept for goldens. Current daemons refuse start_session /
    #: clear_session / inspect_session / agent Requests with
    #: ``session_surface_removed``. Living recovery is ``x-pmux-cell`` /
    #: ``pmux doctor`` conversation leases. Do not teach inspect → fence →
    #: clear as a caller loop.
    transcript_session_id: SessionId
    #: Which cell this session is driven as. Fixed at start.
    cell: SessionCell
    state: SessionState
    cwd: str
    active_turn_id: NotRequired[TurnId]
    claude_version: NotRequired[str]
    compatibility: CompatibilityReport
    created_at_ms: TimestampMs
    updated_at_ms: TimestampMs
    idle_deadline_ms: NotRequired[TimestampMs]
    resumable: bool
    last_sequence: int
    last_turn: NotRequired[TurnSummary]
    needs_input: NotRequired[NeedsInput]
    #: The stored agent version this session pinned AT START. It does not move
    #: when the agent is updated.
    agent: NotRequired[SessionAgentPin]


CancelOutcome: TypeAlias = Literal["cancelled", "already_terminal", "recovery_failed"]
MessageScope: TypeAlias = Literal["main", "sidechain", "team", "metadata"]
RateLimitStatus: TypeAlias = Literal["allowed", "rejected", "unknown"]


class CancelTurnResult(TypedDict):
    session_id: SessionId
    generation_id: SessionGenerationId
    turn_id: TurnId
    outcome: CancelOutcome
    session_state: SessionState


ClosePolicy: TypeAlias = Literal["graceful", "force"]


class CloseSessionResult(TypedDict):
    session_id: SessionId
    generation_id: SessionGenerationId
    already_closed: bool
    process_reaped: bool


class ClearSessionRequest(TypedDict):
    """Clears one minified-cell session's context between turns.

    Protocol type kept for goldens. Current daemons refuse start_session /
    clear_session / inspect_session / agent Requests with
    ``session_surface_removed``. Living recovery of a Messages pin is
    ``x-pmux-cell`` / ``pmux doctor`` conversation leases. Do not teach
    inspect → fence → clear as a caller loop.

    ``expected_transcript_session_id`` is a compare-and-swap fence on the
    wire (every stale value is ``id_conflict``, including one stale by
    exactly one rotation). It is not a public recovery API.
    """

    session_id: SessionId
    generation_id: SessionGenerationId
    expected_transcript_session_id: SessionId
    deadline_unix_ms: NotRequired[int]


class ClearSessionResult(TypedDict):
    session_id: SessionId
    generation_id: SessionGenerationId
    #: Claude's rotated id, and the fence value for the caller's next clear.
    transcript_session_id: SessionId
    #: Always true: a result is only produced by a clear that actually ran.
    rotated: bool
    state: SessionState


class TerminalSize(TypedDict):
    rows: int
    cols: int


class AttachSessionRequest(TypedDict):
    session_id: SessionId
    generation_id: SessionGenerationId
    read_only: NotRequired[bool]
    size: NotRequired[TerminalSize]


class AttachCapability(TypedDict):
    session_id: SessionId
    generation_id: SessionGenerationId
    token: str
    endpoint: str
    expires_at_ms: TimestampMs
    read_only: bool


class ReplayGap(TypedDict):
    requested_after: int
    oldest_available: int
    next_sequence: int
    snapshot: SessionSnapshot


PmuxErrorCode: TypeAlias = Literal[
    "invalid_config",
    "unsupported_feature",
    "unsupported_claude_version",
    "claude_not_found",
    "rmux_unavailable",
    "rmux_incompatible",
    "persistence_disabled",
    "transcript_unavailable",
    "schema_drift",
    "prompt_not_acknowledged",
    "result_too_large",
    "turn_history_capacity_exceeded",
    "session_busy",
    "id_conflict",
    "id_collision",
    "session_not_found",
    "stale_session_generation",
    "needs_trust",
    "needs_login",
    "needs_permission",
    "needs_update",
    "needs_input",
    "rate_limited",
    "authentication_failed",
    "billing_failed",
    "permission_denied",
    "turn_timeout",
    "cancelled",
    "recovery_failed",
    "claude_exited",
    "daemon_lost",
    "replay_gap",
    "protocol_version_mismatch",
    "internal",
]


class ErrorBody(TypedDict):
    code: PmuxErrorCode
    message: str
    retryable: bool
    details: NotRequired[JsonValue]


class EventPayload(TypedDict):
    type: str
    data: JsonValue


class EventEnvelope(TypedDict):
    schema_version: int
    session_id: SessionId
    generation_id: SessionGenerationId
    turn_id: NotRequired[TurnId]
    sequence: int
    timestamp_ms: TimestampMs
    event: EventPayload


class EventBatch(TypedDict):
    events: NotRequired[list[EventEnvelope]]
    next_sequence: int
    replay_gap: NotRequired[ReplayGap]


class RunOnceRequest(TypedDict):
    session: StartSessionRequest
    turn: TurnRequest


class Pong(TypedDict):
    server_version: str
    protocol_version: int


#: The coarse, foldable result of one check.
#:
#: Three values, not two: a boolean cannot distinguish "I checked and it was
#: fine" from "I did not check", and forcing the second into the first is
#: exactly how a health report comes to assert health over machinery it never
#: touched. Declaration order is severity order -- ``fail`` outranks
#: ``unproven``, which outranks ``pass``.
ProbeOutcome: TypeAlias = Literal["pass", "unproven", "fail"]

#: Why ``RuntimeProbe.outcome`` is what it is. The control plane is evaluated
#: before the launch broker, so a finding naming the broker also asserts that
#: the sidecar answered.
RuntimeFinding: TypeAlias = Literal[
    "private_runtime_responsive",
    "control_plane_unreachable",
    "control_plane_unresponsive",
    "control_plane_refused",
    "launch_broker_stopped",
]

#: Why ``SessionProbe.outcome`` is what it is.
SessionFinding: TypeAlias = Literal[
    "terminal_present",
    "terminal_missing",
    "session_declared_unusable",
    "session_actor_unresponsive",
    "session_closed_during_probe",
    "not_probed",
]


class RuntimeProbe(TypedDict):
    outcome: ProbeOutcome
    finding: RuntimeFinding
    elapsed_ms: int
    #: How many private terminals the sidecar itself reported. A fact, folded
    #: into nothing: a terminal the sidecar knows and the registry does not is
    #: the normal, transient shape of every in-flight start.
    live_private_terminals: NotRequired[int]


class SessionProbe(TypedDict):
    session_id: SessionId
    generation_id: SessionGenerationId
    outcome: ProbeOutcome
    finding: SessionFinding
    state: NotRequired[SessionState]
    private_terminal_present: NotRequired[bool]


HealthLayerName: TypeAlias = Literal[
    "configuration",
    "control_plane",
    "private_runtime",
    "launch_broker",
    "compatibility_profile",
    "pool",
    "sessions",
    "performance",
]

LayerFinding: TypeAlias = Literal["exercised", "faulted", "nothing_to_exercise", "not_established"]


class HealthLayer(TypedDict):
    """One layer of the daemon's health proof tree.

    ``not_established`` is a third answer and not a shading of ``exercised``:
    a layer nobody exercised is neither proven healthy nor proven faulty, and
    a report that collapsed the two would assert health over machinery it never
    touched.

    ``nothing_to_exercise`` is a fourth and is NOT a shading of
    ``not_established``. It means the layer was reached, evaluated, and found to
    have no subject -- a registry holding no sessions, a pool with no declared
    warm floor holding no instances, or a pool the daemon was never configured
    to run. It folds to ``pass``, for the same reason folding an empty set of
    outcomes is a pass: absence is a capacity fact, not a fault. A daemon
    serving only stateless turns reports ``sessions: []`` on every probe
    forever, so encoding that as ``not_established`` made every correct such
    daemon permanently unprovable.

    An empty set the daemon's own configuration DECLARED should be occupied is
    ``faulted``, not this. The question is not "is the set empty?" but "is the
    set empty when something declared it should not be?": a pool holding none of
    an operator-declared ``--path-b-warm`` floor reports ``faulted``, and the
    same census with no floor declared reports ``nothing_to_exercise``. A client
    that treats ``nothing_to_exercise`` as "idle, therefore fine" is reading the
    finding correctly; one that treats every empty count as fine is not.
    """

    layer: HealthLayerName
    outcome: ProbeOutcome
    finding: LayerFinding
    #: What was exercised, what failed, or what was not established, in that
    #: layer's own words. Required for every finding, ``exercised`` included:
    #: "pass" without a statement of what was exercised is the boolean this
    #: type replaced, one level down.
    detail: str
    evidence: NotRequired[JsonValue]


class DaemonDiagnosis(TypedDict):
    """What the daemon found when it last completed a real operation against
    its own private runtime.

    ``sessions`` is a list and deliberately not a summary: "healthy" is a
    property each instance has, not one a pool has, and a supervisor whose
    classes are independently warm, cold and quarantined cannot recover
    per-instance answers from a fold.
    """

    #: One entry per layer of the health tree. A layer ABSENT from this list
    #: is ``not_established``, never healthy -- see ``missing_health_layers``.
    layers: NotRequired[list[HealthLayer]]
    runtime: RuntimeProbe
    sessions: list[SessionProbe]


class ResponseResult(TypedDict):
    type: str
    data: JsonValue


#: Every nested plain-string enum of the v1 wire surface, keyed by its protocol
#: type name and derived from the ``Literal`` alias above it, so this mapping is
#: the runtime image of the Python copy and
#: ``tests/conformance/v1/manifest.json#value_enums`` pins it against Rust.
#: ``PmuxErrorCode`` is deliberately absent: the manifest pins it as ``error_codes``.
V1_VALUE_ENUMS: Final[dict[str, tuple[str, ...]]] = {
    "AuthPolicy": get_args(AuthPolicy),
    "CancelOutcome": get_args(CancelOutcome),
    "ClosePolicy": get_args(ClosePolicy),
    "CompatibilityPolicy": get_args(CompatibilityPolicy),
    "CompletionAuthority": get_args(CompletionAuthority),
    "DisconnectAction": get_args(DisconnectAction),
    "EffortLevel": get_args(EffortLevel),
    "HealthLayerName": get_args(HealthLayerName),
    "InputTransport": get_args(InputTransport),
    "LayerFinding": get_args(LayerFinding),
    "MessageScope": get_args(MessageScope),
    "NeedsInputKind": get_args(NeedsInputKind),
    "PermissionMode": get_args(PermissionMode),
    "ProbeOutcome": get_args(ProbeOutcome),
    "RateLimitStatus": get_args(RateLimitStatus),
    "RuntimeFinding": get_args(RuntimeFinding),
    "SessionCell": get_args(SessionCell),
    "SessionFinding": get_args(SessionFinding),
    "SessionState": get_args(SessionState),
    "StopReasonKind": get_args(StopReasonKind),
    "TerminalProfile": get_args(TerminalProfile),
    "ToolStatus": get_args(ToolStatus),
    "TurnOutcome": get_args(TurnOutcome),
}


def _tagged_union(alias: Any) -> dict[str, Any]:
    """The wire image of one internally-tagged union, read off its own members.

    The discriminant key is derived rather than named: a candidate is a key that
    every member of the union annotates as a string ``Literal``, whose values no
    other member repeats. The real discriminant always qualifies -- every member
    carries it and two members cannot spell it the same -- so this either finds
    it alone or raises on the ambiguity. It never picks silently between two.

    The variant list is the members' own ``Literal`` values in declaration
    order, which is why one member may contribute more than one:
    ``CustomSystemPrompt`` is the Python spelling of two Rust variants.
    """
    members = get_args(alias)
    literals = [
        {
            key: get_args(hint)
            for key, hint in get_type_hints(member).items()
            if get_origin(hint) is Literal and all(isinstance(arg, str) for arg in get_args(hint))
        }
        for member in members
    ]
    candidates = [
        key
        for key in literals[0]
        if all(key in member for member in literals)
        and len({value for member in literals for value in member[key]})
        == sum(len(member[key]) for member in literals)
    ]
    if len(candidates) != 1:
        raise AssertionError(f"{alias} has no single discriminant key; candidates {candidates}")
    tag = candidates[0]
    return {"tag": tag, "variants": [value for member in literals for value in member[tag]]}


#: Every internally-tagged union of the v1 wire surface, keyed by its protocol
#: type name and derived from the ``TypedDict`` members of the alias above it,
#: so this mapping is the runtime image of the Python copy and
#: ``tests/conformance/v1/manifest.json#tagged_unions`` pins it against Rust.
#: These are the unions the manifest pinned by nothing until MEASURED, appending
#: a variant to each of the six Rust enums left every suite in all three
#: languages green -- and this client's ``_validate_message_block`` throws on a
#: ``kind`` it does not know.
V1_TAGGED_UNIONS: Final[dict[str, dict[str, Any]]] = {
    "ConfigSource": _tagged_union(ConfigSource),
    "LifecycleMode": _tagged_union(LifecycleMode),
    "MessageBlock": _tagged_union(MessageBlock),
    "RetentionPolicy": _tagged_union(RetentionPolicy),
    "SessionIdentity": _tagged_union(SessionIdentity),
    "SystemPromptPolicy": _tagged_union(SystemPromptPolicy),
}
