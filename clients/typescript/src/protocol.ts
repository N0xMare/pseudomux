/** Native pmux wire protocol v1. Field names intentionally match JSON. */

export const PROTOCOL_VERSION = 1 as const;
export const MAX_NATIVE_FRAME_BYTES = 8 * 1024 * 1024;
export const MAX_SAFE_JSON_INTEGER = Number.MAX_SAFE_INTEGER;

export type RequestId = string;
export type SessionId = string;
export type SessionGenerationId = string;
export type TurnId = string;
export type TimestampMs = number;

export type JsonValue =
  | null
  | boolean
  | number
  | string
  | JsonValue[]
  | { [key: string]: JsonValue };

export type SessionIdentity =
  | { mode: "new"; session_id?: SessionId }
  | { mode: "resume"; session_id: SessionId };

export type ConfigSource =
  | { source: "file"; path: string }
  | { source: "inline"; document: JsonValue };

export const EFFORT_LEVELS = ["low", "medium", "high", "xhigh", "max"] as const;
export type EffortLevel = (typeof EFFORT_LEVELS)[number];
export const PERMISSION_MODES = [
  "default",
  "accept_edits",
  "plan",
  "auto",
  "bypass_permissions",
  "dont_ask",
  "dangerously_skip_permissions",
] as const;
export type PermissionMode = (typeof PERMISSION_MODES)[number];

export type SystemPromptPolicy =
  | { mode: "default" }
  | { mode: "append"; prompt: string }
  | { mode: "replace"; prompt: string };

export interface ClaudeLaunchConfig {
  /** Absolute path; pmuxd rejects relative executables. */
  executable: string;
  model?: string;
  effort?: EffortLevel;
  permission_mode?: PermissionMode;
  allowed_tools?: string[];
  denied_tools?: string[];
  /** Settings and their hooks remain structured data; this client never rewrites them. */
  settings?: ConfigSource[];
  mcp_configs?: ConfigSource[];
  plugin_dirs?: string[];
  system_prompt?: SystemPromptPolicy;
  /** Server-validated compatibility arguments, never subprocess arguments in this client. */
  extra_args?: string[];
}

export interface EnvironmentSpec {
  snapshot?: Record<string, string>;
  set?: Record<string, string>;
  unset?: string[];
}

/**
 * A pmux-owned Claude configuration root for one session.
 *
 * Answers *whose configuration*, which is a different question from
 * `auth_policy`'s *whose credentials*: the daemon pins the credential store to
 * the root the same request would have used without isolation, so an isolated
 * session still authenticates as the same account. Omitting the field inherits
 * the caller's root, which is what every release to date does.
 */
export interface ConfigIsolation {
  /** Absolute path to an existing, owner-only directory. pmux never creates it. */
  root: string;
}

export const AUTH_POLICIES = ["subscription", "inherit"] as const;
export type AuthPolicy = (typeof AUTH_POLICIES)[number];
export const TERMINAL_PROFILES = ["transparent", "rmux_standard"] as const;
export type TerminalProfile = (typeof TERMINAL_PROFILES)[number];
export const INPUT_TRANSPORTS = ["auto", "sdk", "attached_stream"] as const;
export type InputTransport = (typeof INPUT_TRANSPORTS)[number];

export interface TerminalSpec {
  rows: number;
  cols: number;
  profile?: TerminalProfile;
  input_transport?: InputTransport;
}

export type LifecycleMode =
  | { mode: "transcript" }
  | { mode: "hybrid"; hook_timeout_ms?: number };

export type RetentionPolicy =
  | { mode: "one_shot" }
  | { mode: "persistent"; idle_ttl_ms?: number };

export const COMPATIBILITY_POLICIES = ["require_tested", "allow_untested"] as const;
export type CompatibilityPolicy = (typeof COMPATIBILITY_POLICIES)[number];

/**
 * Which cell a session is driven as, chosen once at start.
 *
 * Protocol type kept for goldens. Current daemons refuse start_session /
 * clear_session / inspect_session / agent Requests with
 * `session_surface_removed`.
 *
 * `minified` is the pool cell: no tool surface, admitted only on a tested
 * compatibility profile. Omitting the field means `full`. Living recovery of a
 * Messages pin is `x-pmux-cell` / `pmux doctor` conversation leases. Do not
 * teach inspect → fence → clear as a caller loop.
 */
export const SESSION_CELLS = ["full", "minified"] as const;
export type SessionCell = (typeof SESSION_CELLS)[number];

export interface CompatibilityReport {
  claude_version: string;
  os: string;
  arch: string;
  terminal_profile: TerminalProfile;
  /** `auto` is resolved before admission; attached-stream is not enabled in v1. */
  input_transport: "sdk";
  tested: boolean;
  transcript_drain_ms: number;
}

/**
 * One interactive session start.
 *
 * Protocol type kept for goldens. Current daemons refuse start_session /
 * clear_session / inspect_session / agent Requests with
 * `session_surface_removed`. Living recovery is `x-pmux-cell` /
 * `pmux doctor` conversation leases. Do not teach inspect → fence → clear as a
 * caller loop.
 *
 * Supply EITHER `claude` (the inline launch configuration) OR `agent` (a stored
 * id and an EXACT version), never both and never neither. A request carrying
 * both is refused by the daemon naming the colliding field, and merging is
 * refused rather than resolved: a merge surface needs one documented rule per
 * field and nothing derives that list.
 *
 * `cwd` is always required and is NEVER taken from the agent. An agent may only
 * BOUND it, through `containment.workspace_root`.
 */
export interface StartSessionRequest {
  identity: SessionIdentity;
  cwd: string;
  claude?: ClaudeLaunchConfig;
  agent?: AgentRef;
  environment?: EnvironmentSpec;
  auth_policy?: AuthPolicy;
  config_isolation?: ConfigIsolation;
  terminal?: TerminalSpec;
  lifecycle?: LifecycleMode;
  retention?: RetentionPolicy;
  compatibility?: CompatibilityPolicy;
  cell?: SessionCell;
}

/**
 * The exact stored agent version one session runs.
 *
 * Protocol type kept for goldens. Current daemons refuse start_session /
 * clear_session / inspect_session / agent Requests with
 * `session_surface_removed`.
 *
 * `version` is REQUIRED and there is deliberately no "omit for latest": that
 * would make the launch a function of WHEN the request arrived.
 */
export interface AgentRef {
  agent_id: string;
  version: number;
}

/**
 * What a running session pinned at start.
 *
 * It does not move when the agent is updated: an update mints a NEW immutable
 * version and this session keeps the one it started under, by value.
 */
export interface SessionAgentPin {
  agent_id: string;
  version: number;
  config_digest: string;
}

/**
 * The environment policy an agent carries. Deliberately NOT `EnvironmentSpec`:
 * there is no `snapshot`, because a stored caller snapshot is either stale the
 * moment the caller's shell changes or a file of environment values at rest.
 */
export interface AgentEnvironmentSpec {
  set?: Record<string, string>;
  unset?: string[];
}

/**
 * What an agent may say about the resources a session names.
 *
 * Every field NARROWS. There is no value of either that makes an
 * otherwise-refused start admissible.
 */
export interface AgentContainment {
  workspace_root?: string;
  require_config_isolation?: boolean;
}

/**
 * Protocol type kept for goldens. Current daemons refuse start_session /
 * clear_session / inspect_session / agent Requests with
 * `session_surface_removed`.
 *
 * Everything an agent stores: launch policy, and no resource.
 *
 * There is no `cwd`, no `config_isolation`, no session identity, no prompt and
 * no environment snapshot: those were per-session fields on startSession, which
 * current daemons also refuse.
 */
export interface AgentSpec {
  name: string;
  description?: string;
  claude: ClaudeLaunchConfig;
  environment?: AgentEnvironmentSpec;
  auth_policy?: AuthPolicy;
  terminal?: TerminalSpec;
  lifecycle?: LifecycleMode;
  retention?: RetentionPolicy;
  compatibility?: CompatibilityPolicy;
  cell?: SessionCell;
  containment?: AgentContainment;
}

/**
 * One stored agent version, as read.
 *
 * `spec` is REDACTED: every environment value and every inline settings/MCP
 * document body is a `sha256:` digest. `config_digest` is computed over the
 * UNREDACTED spec, so it still identifies the configuration exactly.
 */
export interface AgentDescriptor {
  agent_id: string;
  version: number;
  config_digest: string;
  spec: AgentSpec;
  created_at_ms: number;
  updated_at_ms: number;
}

export interface AgentSummary {
  agent_id: string;
  version: number;
  config_digest: string;
  name: string;
  description?: string;
  cell: SessionCell;
  updated_at_ms: number;
}

/**
 * One stored record `list_agents` could not read.
 *
 * Protocol type kept for goldens. Current daemons refuse start_session /
 * clear_session / inspect_session / agent Requests with
 * `session_surface_removed`. Do not recommend `pmux agent list`.
 *
 * The daemon reports these rather than dropping them, and rather than
 * answering the whole listing with the first one's refusal.
 */
export interface AgentListFailure {
  agent_id: string;
  reason: string;
}

export interface AgentList {
  agents?: AgentSummary[];
  unreadable?: AgentListFailure[];
}

export const SESSION_STATES = [
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
] as const;
export type SessionState = (typeof SESSION_STATES)[number];

export interface SessionHandle {
  session_id: SessionId;
  generation_id: SessionGenerationId;
  state: SessionState;
  compatibility: CompatibilityReport;
  created_at_ms: TimestampMs;
  last_sequence: number;
  /** The stored agent version this session pinned, when it named one. */
  agent?: SessionAgentPin;
}

export const DISCONNECT_ACTIONS = ["continue", "cancel_turn", "close_session"] as const;
export type DisconnectAction = (typeof DISCONNECT_ACTIONS)[number];

export interface TurnLeasePolicy {
  on_disconnect?: DisconnectAction;
  heartbeat_timeout_ms?: number;
}

export interface TurnRequest {
  turn_id: TurnId;
  prompt: string;
  deadline_unix_ms?: TimestampMs;
  lease?: TurnLeasePolicy;
}

export interface RunTurnRequest {
  session_id: SessionId;
  generation_id: SessionGenerationId;
  turn: TurnRequest;
}

export interface TurnAccepted {
  session_id: SessionId;
  generation_id: SessionGenerationId;
  turn_id: TurnId;
  replayed: boolean;
  state: SessionState;
  next_sequence: number;
}

export const TURN_OUTCOMES = ["completed", "cancelled", "failed"] as const;
export type TurnOutcome = (typeof TURN_OUTCOMES)[number];
export const TOOL_STATUSES = ["requested", "completed", "failed", "cancelled"] as const;
export type ToolStatus = (typeof TOOL_STATUSES)[number];

export type MessageBlock =
  | { kind: "text"; text: string }
  | { kind: "tool_use"; id: string; name: string; input: JsonValue }
  | {
      kind: "tool_result";
      tool_use_id: string;
      content: JsonValue;
      is_error: boolean;
    }
  | { kind: "unknown"; block_type: string; data: JsonValue };

export interface ToolRecord {
  tool_use_id: string;
  name: string;
  input: JsonValue;
  output?: JsonValue;
  status: ToolStatus;
  started_at_ms?: TimestampMs;
  completed_at_ms?: TimestampMs;
}

export interface TokenUsage {
  input_tokens: number;
  output_tokens: number;
  cache_creation_input_tokens: number;
  cache_read_input_tokens: number;
}

export interface UsageBreakdown {
  main: TokenUsage;
  sidechain: TokenUsage;
  combined: TokenUsage;
  /** Absent for subscription execution. */
  cost_usd?: string;
}

export const STOP_REASON_KINDS = [
  "end_turn",
  "stop_sequence",
  "max_tokens",
  "tool_use",
  "pause_turn",
  "refusal",
  "error",
  "unknown",
] as const;
export type StopReasonKind = (typeof STOP_REASON_KINDS)[number];

export interface StopReason {
  kind: StopReasonKind;
  raw?: string;
}

export interface TurnTimings {
  submitted_at_ms: TimestampMs;
  prompt_acknowledged_at_ms?: TimestampMs;
  terminal_candidate_at_ms?: TimestampMs;
  completed_at_ms: TimestampMs;
  /**
   * How long the transcript had been byte-for-byte unchanged when the drain
   * gate admitted the commit. Not `completed_at_ms - terminal_candidate_at_ms`.
   */
  drain_ms?: number;
  /**
   * When pmux last observed the transcript grow before committing the turn,
   * in the same wall-clock domain as the other `*_at_ms` fields. The drain
   * calibration quantity is the signed difference
   * `last_transcript_activity_at_ms - terminal_candidate_at_ms`; a consumer
   * must compute it itself, because it can be negative by a few milliseconds
   * when no row arrived after the terminal-looking message.
   */
  last_transcript_activity_at_ms?: TimestampMs;
  /**
   * When Claude's `Stop` lifecycle hook was observed for this turn, in the
   * same wall-clock domain as the other `*_at_ms` fields. Measurement only:
   * nothing in pmux decides completion from it. The deciding quantity is the
   * signed difference `stop_hook_at_ms - last_transcript_activity_at_ms`:
   * consistently positive means Claude flushed the transcript before firing
   * Stop, so a hook-based completion fast path could only ever be faster; a
   * single negative observation means the fast path would commit a truncated
   * turn and must not be built. A timestamp rather than a duration because
   * the sign is the answer and a duration would clamp the negative case.
   * Absent on any turn where no Stop hook was observed.
   */
  stop_hook_at_ms?: TimestampMs;
}

export const COMPLETION_AUTHORITIES = ["transcript"] as const;
export type CompletionAuthority = (typeof COMPLETION_AUTHORITIES)[number];

export interface CompletionProvenance {
  authority: CompletionAuthority;
  prompt_acknowledged: boolean;
  terminal_message_observed: boolean;
  terminal_prompt_observed: boolean;
  terminal_quiet_observed: boolean;
  transcript_drained: boolean;
  lifecycle_hook_observed: boolean;
}

export interface ProtocolWarning {
  code: string;
  message: string;
  details?: JsonValue;
}

export interface TurnResult {
  session_id: SessionId;
  generation_id: SessionGenerationId;
  turn_id: TurnId;
  outcome: TurnOutcome;
  text: string;
  final_blocks?: MessageBlock[];
  tools?: ToolRecord[];
  model?: string;
  stop_reason?: StopReason;
  usage: UsageBreakdown;
  timings: TurnTimings;
  warnings?: ProtocolWarning[];
  claude_version: string;
  compatibility: CompatibilityReport;
  completion: CompletionProvenance;
  final_sequence: number;
}

export interface TurnSummary {
  turn_id: TurnId;
  outcome: TurnOutcome;
  completed_at_ms: TimestampMs;
  final_sequence: number;
}

export const NEEDS_INPUT_KINDS = [
  "trust",
  "login",
  "permission",
  "update",
  "quota",
  "unknown_modal",
] as const;
export type NeedsInputKind = (typeof NEEDS_INPUT_KINDS)[number];

export interface NeedsInput {
  kind: NeedsInputKind;
  message: string;
  details?: JsonValue;
}

export interface SessionSnapshot {
  session_id: SessionId;
  generation_id: SessionGenerationId;
  /**
   * Protocol type kept for goldens. Current daemons refuse start_session /
   * clear_session / inspect_session / agent Requests with
   * `session_surface_removed`. Living recovery is `x-pmux-cell` /
   * `pmux doctor` conversation leases. Do not teach inspect → fence → clear as
   * a caller loop.
   */
  transcript_session_id: SessionId;
  /** Which cell this session is driven as. Fixed at start. */
  cell: SessionCell;
  state: SessionState;
  cwd: string;
  active_turn_id?: TurnId;
  claude_version?: string;
  compatibility: CompatibilityReport;
  created_at_ms: TimestampMs;
  updated_at_ms: TimestampMs;
  idle_deadline_ms?: TimestampMs;
  resumable: boolean;
  last_sequence: number;
  last_turn?: TurnSummary;
  needs_input?: NeedsInput;
  /**
   * The stored agent version this session pinned AT START. It does not move
   * when the agent is updated.
   */
  agent?: SessionAgentPin;
}

export interface CancelTurnRequest {
  session_id: SessionId;
  generation_id: SessionGenerationId;
  turn_id: TurnId;
}

export const CANCEL_OUTCOMES = ["cancelled", "already_terminal", "recovery_failed"] as const;
export type CancelOutcome = (typeof CANCEL_OUTCOMES)[number];

export interface CancelTurnResult {
  session_id: SessionId;
  generation_id: SessionGenerationId;
  turn_id: TurnId;
  outcome: CancelOutcome;
  session_state: SessionState;
}

export const CLOSE_POLICIES = ["graceful", "force"] as const;
export type ClosePolicy = (typeof CLOSE_POLICIES)[number];

export interface CloseSessionResult {
  session_id: SessionId;
  generation_id: SessionGenerationId;
  already_closed: boolean;
  process_reaped: boolean;
}

/**
 * Clears one minified-cell session's context between turns.
 *
 * Protocol type kept for goldens. Current daemons refuse start_session /
 * clear_session / inspect_session / agent Requests with
 * `session_surface_removed`. Living recovery of a Messages pin is
 * `x-pmux-cell` / `pmux doctor` conversation leases. Do not teach inspect →
 * fence → clear as a caller loop.
 *
 * `expected_transcript_session_id` is a compare-and-swap fence on the wire
 * (every stale value is `id_conflict`, including one stale by exactly one
 * rotation). It is not a public recovery API.
 */
export interface ClearSessionRequest {
  session_id: SessionId;
  generation_id: SessionGenerationId;
  expected_transcript_session_id: SessionId;
  deadline_unix_ms?: number;
}

export interface ClearSessionResult {
  session_id: SessionId;
  generation_id: SessionGenerationId;
  /** Claude's rotated id, and the fence value for the caller's next clear. */
  transcript_session_id: SessionId;
  /** Always true: a result is only produced by a clear that actually ran. */
  rotated: boolean;
  state: SessionState;
}

export interface TerminalSize {
  rows: number;
  cols: number;
}

export interface AttachSessionRequest {
  session_id: SessionId;
  generation_id: SessionGenerationId;
  read_only?: boolean;
  size?: TerminalSize;
}

export interface AttachCapability {
  session_id: SessionId;
  generation_id: SessionGenerationId;
  token: string;
  endpoint: string;
  expires_at_ms: TimestampMs;
  read_only: boolean;
}

export interface SubscribeEventsRequest {
  session_id: SessionId;
  generation_id: SessionGenerationId;
  after_sequence?: number;
  wait_ms?: number;
  max_events?: number;
}

export interface ReplayGap {
  requested_after: number;
  oldest_available: number;
  next_sequence: number;
  snapshot: SessionSnapshot;
}

export interface EventBatch {
  events?: EventEnvelope[];
  next_sequence: number;
  replay_gap?: ReplayGap;
}

export interface RunOnceRequest {
  session: StartSessionRequest;
  turn: TurnRequest;
}

export const MESSAGE_SCOPES = ["main", "sidechain", "team", "metadata"] as const;
export type MessageScope = (typeof MESSAGE_SCOPES)[number];

export interface LogicalMessage {
  message_id: string;
  request_id?: string;
  scope: MessageScope;
  blocks: MessageBlock[];
  model?: string;
  stop_reason?: StopReason;
  usage?: TokenUsage;
  terminal: boolean;
}

export const RATE_LIMIT_STATUSES = ["allowed", "rejected", "unknown"] as const;
export type RateLimitStatus = (typeof RATE_LIMIT_STATUSES)[number];

export type EventPayload =
  | {
      type: "session_state_changed";
      data: { previous: SessionState; current: SessionState; reason?: string };
    }
  | {
      type: "prompt_acknowledged";
      data: { prompt_uuid: string; prompt_id?: string; transcript_offset: number };
    }
  | { type: "logical_message"; data: LogicalMessage }
  | {
      type: "tool_started";
      data: { tool_use_id: string; name: string; input: JsonValue };
    }
  | {
      type: "tool_completed";
      data: { tool_use_id: string; output: JsonValue; is_error: boolean };
    }
  | {
      type: "rate_limit";
      data: { status: RateLimitStatus; resets_at_ms?: TimestampMs; message?: string };
    }
  | { type: "needs_input"; data: NeedsInput }
  | {
      type: "terminal_candidate";
      data: { message_id: string; stop_reason?: StopReason };
    }
  | { type: "turn_completed"; data: TurnResult }
  | {
      type: "turn_cancelled";
      data: { outcome: CancelOutcome; recovered_to_ready: boolean };
    }
  | { type: "turn_failed"; data: ErrorBody }
  | { type: "warning"; data: ProtocolWarning }
  | { type: "replay_gap"; data: ReplayGap }
  | { type: "heartbeat"; data: { session_state: SessionState } };

export interface EventEnvelope {
  schema_version: number;
  session_id: SessionId;
  generation_id: SessionGenerationId;
  turn_id?: TurnId;
  sequence: number;
  timestamp_ms: TimestampMs;
  event: EventPayload;
}

/** Closed protocol-v1 error discriminants. New codes require a versioned contract change. */
export const PMUX_ERROR_CODES = [
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
] as const;

export type PmuxErrorCode = (typeof PMUX_ERROR_CODES)[number];

export interface ErrorBody {
  code: PmuxErrorCode;
  message: string;
  retryable: boolean;
  details?: JsonValue;
}

export type PmuxRequest =
  | { method: "ping" }
  | { method: "start_session"; params: StartSessionRequest }
  | { method: "run_turn"; params: RunTurnRequest }
  | { method: "cancel_turn"; params: CancelTurnRequest }
  | {
      method: "inspect_session";
      params: { session_id: SessionId; generation_id: SessionGenerationId };
    }
  | { method: "attach_session"; params: AttachSessionRequest }
  | {
      method: "close_session";
      params: {
        session_id: SessionId;
        generation_id: SessionGenerationId;
        policy?: ClosePolicy;
      };
    }
  | { method: "subscribe_events"; params: SubscribeEventsRequest }
  | { method: "run_once"; params: RunOnceRequest }
  | { method: "clear_session"; params: ClearSessionRequest }
  | { method: "diagnose" }
  | { method: "run_stateless"; params: RunStatelessRequest }
  | { method: "create_agent"; params: CreateAgentRequest }
  | { method: "get_agent"; params: GetAgentRequest }
  | { method: "list_agents"; params: Record<string, never> }
  | { method: "update_agent"; params: UpdateAgentRequest };

export interface CreateAgentRequest {
  spec: AgentSpec;
}

export interface GetAgentRequest {
  agent_id: string;
  /** Omit for the current head: a read reports, a launch commits. */
  version?: number;
}

export interface UpdateAgentRequest {
  agent_id: string;
  /**
   * The version you believe is current. REQUIRED, and a fence: any value that
   * is not the current head is refused with `id_conflict`, including one stale
   * by exactly one revision, and nothing is ever answered as "already landed".
   */
  expected_version: number;
  /** A COMPLETE replacement, not a patch. */
  spec: AgentSpec;
}

export type RequestEnvelope = {
  version: typeof PROTOCOL_VERSION;
  request_id: RequestId;
} & PmuxRequest;

export interface Pong {
  server_version: string;
  protocol_version: number;
}

/**
 * The stateless request surface.
 *
 * Every field a session start carries and this does not -- `cwd`,
 * `config_isolation`, `claude`, `environment`, `system_prompt`, `identity` --
 * is a resource the daemon mints from its own configuration plus a slot
 * identity. Their absence is the product statement, not an omission: a caller
 * who cannot name a resource cannot alias one.
 */
export interface RunStatelessRequest {
  model: string;
  effort?: EffortLevel;
  prompt: string;
  deadline_unix_ms?: number;
}

/**
 * The Path B answer: text plus usage, naming no resource.
 *
 * `model` is what pmux ASKED for -- the pool's class key, resolved before
 * checkout. `reported_model` is what the transcript said REPLIED, and it is a
 * separate field rather than a narrowing of the first because conflating them
 * is how a probe measures the wrong thing. It is absent when the transcript
 * carried no `message.model` row; pmux does not fabricate it.
 */
export interface StatelessResult {
  model: string;
  reported_model?: string;
  effort?: EffortLevel;
  text: string;
  stop_reason?: StopReason;
  usage: UsageBreakdown;
  claude_version: string;
}

/**
 * The coarse, foldable result of one check.
 *
 * Three values, not two: a boolean cannot distinguish "I checked and it was
 * fine" from "I did not check", and forcing the second into the first is
 * exactly how a health report comes to assert health over machinery it never
 * touched. Declaration order is severity order -- `fail` outranks `unproven`,
 * which outranks `pass`.
 */
export const PROBE_OUTCOMES = ["pass", "unproven", "fail"] as const;
export type ProbeOutcome = (typeof PROBE_OUTCOMES)[number];

/**
 * Why `RuntimeProbe.outcome` is what it is. The control plane is evaluated
 * before the launch broker, so a finding that names the broker is also
 * asserting the sidecar answered.
 */
export const RUNTIME_FINDINGS = [
  "private_runtime_responsive",
  "control_plane_unreachable",
  "control_plane_unresponsive",
  "control_plane_refused",
  "launch_broker_stopped",
] as const;
export type RuntimeFinding = (typeof RUNTIME_FINDINGS)[number];

/** Why `SessionProbe.outcome` is what it is. */
export const SESSION_FINDINGS = [
  "terminal_present",
  "terminal_missing",
  "session_declared_unusable",
  "session_actor_unresponsive",
  "session_closed_during_probe",
  "not_probed",
] as const;
export type SessionFinding = (typeof SESSION_FINDINGS)[number];

export interface RuntimeProbe {
  outcome: ProbeOutcome;
  finding: RuntimeFinding;
  elapsed_ms: number;
  /**
   * How many private terminals the sidecar itself reported. A fact, folded
   * into nothing: a terminal the sidecar knows and the registry does not is
   * the normal, transient shape of every in-flight start.
   */
  live_private_terminals?: number;
}

export interface SessionProbe {
  session_id: SessionId;
  generation_id: SessionGenerationId;
  outcome: ProbeOutcome;
  finding: SessionFinding;
  state?: SessionState;
  private_terminal_present?: boolean;
}

/**
 * What the daemon found when it last completed a real operation against its own
 * private runtime.
 *
 * `sessions` is a list and deliberately not a summary: "healthy" is a property
 * each instance has, not one a pool has, and a supervisor whose classes are
 * independently warm, cold and quarantined cannot recover per-instance answers
 * from a fold.
 */
export const HEALTH_LAYER_NAMES = [
  "configuration",
  "control_plane",
  "private_runtime",
  "launch_broker",
  "compatibility_profile",
  "pool",
  "sessions",
  "performance",
] as const;
export type HealthLayerName = (typeof HEALTH_LAYER_NAMES)[number];

export const LAYER_FINDINGS = [
  "exercised",
  "faulted",
  "nothing_to_exercise",
  "not_established",
] as const;
export type LayerFinding = (typeof LAYER_FINDINGS)[number];

/**
 * One layer of the daemon's health proof tree.
 *
 * `not_established` is a third answer and not a shading of `exercised`: a layer
 * nobody exercised is neither proven healthy nor proven faulty, and a report
 * that collapsed the two would assert health over machinery it never touched.
 *
 * `nothing_to_exercise` is a fourth and is NOT a shading of `not_established`.
 * It means the layer was reached, evaluated, and found to have no subject -- a
 * registry holding no sessions, a pool with no declared warm floor holding no
 * instances, or a pool the daemon was never configured to run. It folds to
 * `pass`, for the same reason folding an empty set of outcomes is a pass:
 * absence is a capacity fact, not a fault. A daemon serving only stateless
 * turns reports `sessions: []` on every probe forever, so encoding that as
 * `not_established` made every correct such daemon permanently unprovable.
 *
 * An empty set the daemon's own configuration DECLARED should be occupied is
 * `faulted`, not this. The question is not "is the set empty?" but "is the set
 * empty when something declared it should not be?": a pool holding none of an
 * operator-declared `--pool-warm` floor reports `faulted`, and the same
 * census with no floor declared reports `nothing_to_exercise`. A client that
 * treats `nothing_to_exercise` as "idle, therefore fine" is reading the finding
 * correctly; one that treats every empty count as fine is not.
 */
export interface HealthLayer {
  layer: HealthLayerName;
  outcome: ProbeOutcome;
  finding: LayerFinding;
  /**
   * What was exercised, what failed, or what was not established, in that
   * layer's own words. Required for every finding, `exercised` included: "pass"
   * without a statement of what was exercised is the boolean this type
   * replaced, one level down.
   */
  detail: string;
  evidence?: JsonValue;
}

export interface DaemonDiagnosis {
  /**
   * One entry per layer of the health tree. A layer ABSENT from this list is
   * `not_established`, never healthy -- see {@link missingHealthLayers}.
   */
  layers?: HealthLayer[];
  runtime: RuntimeProbe;
  sessions: SessionProbe[];
}

/** Layers the report does not carry an entry for, in declaration order. */
export function missingHealthLayers(diagnosis: DaemonDiagnosis): HealthLayerName[] {
  const present = new Set((diagnosis.layers ?? []).map((layer) => layer.layer));
  return HEALTH_LAYER_NAMES.filter((name) => !present.has(name));
}

export type ResponseResult =
  | { type: "pong"; data: Pong }
  | { type: "session_started"; data: SessionHandle }
  | { type: "turn_accepted"; data: TurnAccepted }
  | { type: "turn_cancelled"; data: CancelTurnResult }
  | { type: "session_snapshot"; data: SessionSnapshot }
  | { type: "attach_capability"; data: AttachCapability }
  | { type: "session_closed"; data: CloseSessionResult }
  | { type: "events"; data: EventBatch }
  | { type: "turn_result"; data: TurnResult }
  | { type: "session_cleared"; data: ClearSessionResult }
  | { type: "diagnosis"; data: DaemonDiagnosis }
  | { type: "stateless_result"; data: StatelessResult }
  | { type: "agent_created"; data: AgentDescriptor }
  | { type: "agent"; data: AgentDescriptor }
  | { type: "agent_list"; data: AgentList }
  | { type: "agent_updated"; data: AgentDescriptor };

export type ResponseEnvelope =
  | { version: number; request_id: RequestId; result: ResponseResult }
  | { version: number; request_id: RequestId; error: ErrorBody };

/**
 * Every nested plain-string enum of the v1 wire surface, keyed by its protocol
 * type name. Each union above is derived from the array it names here, so this
 * map is the runtime image of the TypeScript copy and
 * `tests/conformance/v1/manifest.json#value_enums` pins it against Rust.
 * `PmuxErrorCode` is deliberately absent: the manifest pins it as `error_codes`.
 */
export const V1_VALUE_ENUMS = {
  AuthPolicy: AUTH_POLICIES,
  CancelOutcome: CANCEL_OUTCOMES,
  ClosePolicy: CLOSE_POLICIES,
  CompatibilityPolicy: COMPATIBILITY_POLICIES,
  CompletionAuthority: COMPLETION_AUTHORITIES,
  DisconnectAction: DISCONNECT_ACTIONS,
  EffortLevel: EFFORT_LEVELS,
  HealthLayerName: HEALTH_LAYER_NAMES,
  InputTransport: INPUT_TRANSPORTS,
  LayerFinding: LAYER_FINDINGS,
  MessageScope: MESSAGE_SCOPES,
  NeedsInputKind: NEEDS_INPUT_KINDS,
  PermissionMode: PERMISSION_MODES,
  ProbeOutcome: PROBE_OUTCOMES,
  RateLimitStatus: RATE_LIMIT_STATUSES,
  RuntimeFinding: RUNTIME_FINDINGS,
  SessionCell: SESSION_CELLS,
  SessionFinding: SESSION_FINDINGS,
  SessionState: SESSION_STATES,
  StopReasonKind: STOP_REASON_KINDS,
  TerminalProfile: TERMINAL_PROFILES,
  ToolStatus: TOOL_STATUSES,
  TurnOutcome: TURN_OUTCOMES,
} as const;

export const CONFIG_SOURCE_VARIANTS = ["file", "inline"] as const;
export const LIFECYCLE_MODE_VARIANTS = ["transcript", "hybrid"] as const;
export const MESSAGE_BLOCK_VARIANTS = ["text", "tool_use", "tool_result", "unknown"] as const;
export const RETENTION_POLICY_VARIANTS = ["one_shot", "persistent"] as const;
export const SESSION_IDENTITY_VARIANTS = ["new", "resume"] as const;
export const SYSTEM_PROMPT_POLICY_VARIANTS = ["default", "append", "replace"] as const;

/**
 * `true` when two string unions are the same set and `false` otherwise: each
 * side must extend the other, and the tuple wrapper stops the conditional
 * distributing over the union so that "same set" means what it says.
 *
 * `false` and not `never`, deliberately. MEASURED with the `never` spelling and
 * one variant deleted from `CONFIG_SOURCE_VARIANTS`, `tsc --noEmit` stayed
 * silent: `never extends true` is *true*, because `never` is assignable to
 * everything, so the mismatch selected the branch that accepts.
 */
type SameStrings<A extends string, B extends string> = [A] extends [B]
  ? [B] extends [A]
    ? true
    : false
  : false;

/**
 * One entry of {@link V1_TAGGED_UNIONS}, valid only when `Variants` names every
 * variant of `Union` and nothing else -- otherwise it resolves to `never` and
 * the `satisfies` below stops type-checking.
 */
type PinnedTaggedUnion<
  Union,
  Tag extends keyof Union & string,
  Variants extends readonly string[],
> = SameStrings<Union[Tag] & string, Variants[number]> extends true
  ? { readonly tag: Tag; readonly variants: Variants }
  : never;

/**
 * Every internally-tagged union of the v1 wire surface, keyed by its protocol
 * type name, with the discriminant key each one carries and the variants it
 * admits. This map is the runtime image of the TypeScript copy and
 * `tests/conformance/v1/manifest.json#tagged_unions` pins it against Rust.
 *
 * The `satisfies` is what ties the arrays to the unions: a variant added to a
 * union above and not to its array here (or the reverse) makes that entry
 * `never` and this file stops compiling. It exists because nothing pinned these
 * six at all -- MEASURED, appending a variant to each of the six Rust enums
 * left every suite in all three languages green, while `validateMessageBlock`
 * throws on a `kind` it does not know.
 */
export const V1_TAGGED_UNIONS = {
  ConfigSource: { tag: "source", variants: CONFIG_SOURCE_VARIANTS },
  LifecycleMode: { tag: "mode", variants: LIFECYCLE_MODE_VARIANTS },
  MessageBlock: { tag: "kind", variants: MESSAGE_BLOCK_VARIANTS },
  RetentionPolicy: { tag: "mode", variants: RETENTION_POLICY_VARIANTS },
  SessionIdentity: { tag: "mode", variants: SESSION_IDENTITY_VARIANTS },
  SystemPromptPolicy: { tag: "mode", variants: SYSTEM_PROMPT_POLICY_VARIANTS },
} as const satisfies {
  ConfigSource: PinnedTaggedUnion<ConfigSource, "source", typeof CONFIG_SOURCE_VARIANTS>;
  LifecycleMode: PinnedTaggedUnion<LifecycleMode, "mode", typeof LIFECYCLE_MODE_VARIANTS>;
  MessageBlock: PinnedTaggedUnion<MessageBlock, "kind", typeof MESSAGE_BLOCK_VARIANTS>;
  RetentionPolicy: PinnedTaggedUnion<RetentionPolicy, "mode", typeof RETENTION_POLICY_VARIANTS>;
  SessionIdentity: PinnedTaggedUnion<SessionIdentity, "mode", typeof SESSION_IDENTITY_VARIANTS>;
  SystemPromptPolicy: PinnedTaggedUnion<
    SystemPromptPolicy,
    "mode",
    typeof SYSTEM_PROMPT_POLICY_VARIANTS
  >;
};
