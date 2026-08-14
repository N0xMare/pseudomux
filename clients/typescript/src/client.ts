import { randomUUID } from "node:crypto";
import { createConnection, type Socket } from "node:net";
import { isAbsolute } from "node:path";
import { performance } from "node:perf_hooks";
import { TextDecoder } from "node:util";

import {
  CANCEL_OUTCOMES,
  COMPLETION_AUTHORITIES,
  EFFORT_LEVELS,
  HEALTH_LAYER_NAMES,
  LAYER_FINDINGS,
  MAX_NATIVE_FRAME_BYTES,
  MESSAGE_SCOPES,
  NEEDS_INPUT_KINDS,
  PMUX_ERROR_CODES,
  PROBE_OUTCOMES,
  PROTOCOL_VERSION,
  RATE_LIMIT_STATUSES,
  RUNTIME_FINDINGS,
  SESSION_CELLS,
  SESSION_FINDINGS,
  SESSION_STATES,
  STOP_REASON_KINDS,
  TERMINAL_PROFILES,
  TOOL_STATUSES,
  TURN_OUTCOMES,
  V1_TAGGED_UNIONS,
  type AgentDescriptor,
  type AgentList,
  type AgentSpec,
  type AttachCapability,
  type AttachSessionRequest,
  type CancelTurnResult,
  type ClosePolicy,
  type ClearSessionRequest,
  type ClearSessionResult,
  type CloseSessionResult,
  type DaemonDiagnosis,
  type ErrorBody,
  type EventBatch,
  type EventEnvelope,
  type Pong,
  type PmuxRequest,
  type ReplayGap,
  type RequestEnvelope,
  type ResponseEnvelope,
  type ResponseResult,
  type RunOnceRequest,
  type RunStatelessRequest,
  type SessionHandle,
  type SessionGenerationId,
  type SessionId,
  type SessionSnapshot,
  type StartSessionRequest,
  type StatelessResult,
  type SubscribeEventsRequest,
  type TurnAccepted,
  type TurnId,
  type TurnRequest,
  type TurnResult,
} from "./protocol.js";

export const DEFAULT_MAX_FRAME_BYTES = MAX_NATIVE_FRAME_BYTES;
export const DEFAULT_CONNECT_TIMEOUT_MS = 5_000;
export const DEFAULT_REQUEST_TIMEOUT_MS = 45_000;
/** Contains the default startup, turn, recovery, drain, and reap budgets. */
export const DEFAULT_RUN_ONCE_TIMEOUT_MS = 15 * 60_000;
/** Lets pmuxd publish a bounded terminal outcome after a turn deadline. */
export const RUN_ONCE_RESPONSE_MARGIN_MS = 120_000;

// Node coerces larger delays to 1ms. Long protocol deadlines therefore need
// bounded chunks rather than a single overflowing timer.
const MAX_NODE_TIMER_DELAY_MS = 2_147_483_647;

function scheduleLongTimeout(callback: () => void, delayMs: number): () => void {
  const startedAt = performance.now();
  let cancelled = false;
  let timer: ReturnType<typeof setTimeout> | undefined;

  const arm = (): void => {
    if (cancelled) return;
    const remaining = delayMs - (performance.now() - startedAt);
    if (remaining <= 0) {
      cancelled = true;
      timer = undefined;
      callback();
      return;
    }
    timer = setTimeout(arm, Math.min(Math.ceil(remaining), MAX_NODE_TIMER_DELAY_MS));
  };

  // Keep zero-delay behavior asynchronous, matching setTimeout(callback, 0).
  timer = setTimeout(arm, Math.min(Math.max(0, Math.ceil(delayMs)), MAX_NODE_TIMER_DELAY_MS));
  return () => {
    cancelled = true;
    if (timer !== undefined) clearTimeout(timer);
  };
}

export interface PmuxClientOptions {
  maxFrameBytes?: number;
  connectTimeoutMs?: number;
  requestTimeoutMs?: number;
}

export interface RequestOptions {
  signal?: AbortSignal;
}

interface ResolvedClientOptions {
  maxFrameBytes: number;
  connectTimeoutMs: number;
  requestTimeoutMs: number;
}

export class PmuxClientError extends Error {
  constructor(message: string, options?: { cause?: unknown }) {
    super(message);
    this.name = new.target.name;
    if (options && "cause" in options) {
      Object.defineProperty(this, "cause", {
        configurable: true,
        value: options.cause,
      });
    }
  }
}

export class PmuxTransportError extends PmuxClientError {}
export class PmuxProtocolError extends PmuxClientError {}
export class PmuxAbortError extends PmuxClientError {}
export class PmuxTimeoutError extends PmuxTransportError {
  readonly operation: "connect" | "request";
  readonly timeoutMs: number;

  constructor(operation: "connect" | "request", timeoutMs: number) {
    super(`${operation} timed out after ${timeoutMs}ms`);
    this.operation = operation;
    this.timeoutMs = timeoutMs;
  }
}

export class PmuxFrameTooLargeError extends PmuxProtocolError {
  readonly advertised: number;
  readonly maximum: number;

  constructor(advertised: number, maximum: number) {
    super(`frame size ${advertised} exceeds configured maximum ${maximum}`);
    this.advertised = advertised;
    this.maximum = maximum;
  }
}

export class PmuxVersionError extends PmuxProtocolError {
  readonly expected: number;
  readonly actual: unknown;

  constructor(actual: unknown) {
    super(`unsupported protocol version ${String(actual)}; expected ${PROTOCOL_VERSION}`);
    this.expected = PROTOCOL_VERSION;
    this.actual = actual;
  }
}

export class PmuxRequestIdMismatchError extends PmuxProtocolError {
  readonly expected: string;
  readonly actual: unknown;

  constructor(expected: string, actual: unknown) {
    super(`response request id ${String(actual)} does not match ${expected}`);
    this.expected = expected;
    this.actual = actual;
  }
}

export class PmuxUnexpectedResultError extends PmuxProtocolError {
  constructor(
    readonly expected: ResponseResult["type"],
    readonly actual: string,
  ) {
    super(`pmuxd returned ${actual}, expected ${expected}`);
  }
}

export class PmuxServerError extends PmuxClientError {
  readonly body: ErrorBody;

  constructor(body: ErrorBody) {
    super(`pmuxd error ${body.code}: ${body.message}`);
    this.body = body;
  }
}

export class PmuxSequenceError extends PmuxProtocolError {
  constructor(
    readonly expected: number,
    readonly actual: number,
  ) {
    super(`invalid event sequence ${actual}; expected ${expected}`);
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function requireSafeSequence(value: unknown, field: string): number {
  if (!Number.isSafeInteger(value) || (value as number) < 0) {
    throw new PmuxProtocolError(`${field} must be a non-negative safe integer`);
  }
  return value as number;
}

function validateJsonNumericDomain(value: unknown, field: string): void {
  if (typeof value === "number") {
    if (!Number.isFinite(value)) {
      throw new PmuxProtocolError(`${field} must not contain a non-finite number`);
    }
    if (Number.isInteger(value) && !Number.isSafeInteger(value)) {
      throw new PmuxProtocolError(`${field} integer is outside the signed safe-integer range`);
    }
    return;
  }
  if (Array.isArray(value)) {
    value.forEach((item, index) => validateJsonNumericDomain(item, `${field}[${index}]`));
    return;
  }
  if (isRecord(value)) {
    for (const [key, item] of Object.entries(value)) {
      validateJsonNumericDomain(item, `${field}.${key}`);
    }
  }
}

function validateOptionalSequence(
  record: Record<string, unknown>,
  key: string,
  field: string,
): void {
  if (Object.prototype.hasOwnProperty.call(record, key) && record[key] !== undefined) {
    requireSafeSequence(record[key], `${field}.${key}`);
  }
}

function nextSequence(cursor: number, field: string): number {
  return requireSafeSequence(cursor + 1, field);
}

function requireRecord(value: unknown, field: string): Record<string, unknown> {
  if (!isRecord(value)) throw new PmuxProtocolError(`${field} must be an object`);
  return value;
}

function requireField(record: Record<string, unknown>, key: string, field: string): unknown {
  if (!Object.prototype.hasOwnProperty.call(record, key)) {
    throw new PmuxProtocolError(`${field}.${key} is required`);
  }
  return record[key];
}

function requireStringField(record: Record<string, unknown>, key: string, field: string): string {
  const value = requireField(record, key, field);
  if (typeof value !== "string") throw new PmuxProtocolError(`${field}.${key} must be a string`);
  return value;
}

const CANONICAL_UUID_PATTERN =
  /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;

function requireUuid(value: unknown, field: string): string {
  if (typeof value !== "string" || !CANONICAL_UUID_PATTERN.test(value)) {
    throw new PmuxProtocolError(`${field} must be a canonical UUID`);
  }
  return value;
}

function requireUuidField(record: Record<string, unknown>, key: string, field: string): string {
  return requireUuid(requireField(record, key, field), `${field}.${key}`);
}

function sameUuid(left: string, right: string): boolean {
  return left.toLowerCase() === right.toLowerCase();
}

const UTF8_DECODER = new TextDecoder("utf-8", { fatal: true });

function requireBooleanField(record: Record<string, unknown>, key: string, field: string): boolean {
  const value = requireField(record, key, field);
  if (typeof value !== "boolean") {
    throw new PmuxProtocolError(`${field}.${key} must be a boolean`);
  }
  return value;
}

function requireSequenceField(record: Record<string, unknown>, key: string, field: string): number {
  return requireSafeSequence(requireField(record, key, field), `${field}.${key}`);
}

function requireArrayField(record: Record<string, unknown>, key: string, field: string): unknown[] {
  const value = requireField(record, key, field);
  if (!Array.isArray(value)) throw new PmuxProtocolError(`${field}.${key} must be an array`);
  return value;
}

/**
 * Generic in the admitted values so that the returned discriminant is the
 * union `values` names rather than `string`. That is what lets a `switch` on
 * the result be checked for exhaustiveness: see `validateMessageBlock`.
 */
function requireEnumField<Value extends string>(
  record: Record<string, unknown>,
  key: string,
  field: string,
  values: readonly Value[],
): Value {
  const value = requireStringField(record, key, field);
  if (!(values as readonly string[]).includes(value)) {
    throw new PmuxProtocolError(`${field}.${key} has an unknown discriminant`);
  }
  return value as Value;
}

function validateCompatibilityReport(value: unknown, field: string): void {
  const report = requireRecord(value, field);
  requireStringField(report, "claude_version", field);
  requireStringField(report, "os", field);
  requireStringField(report, "arch", field);
  requireEnumField(report, "terminal_profile", field, TERMINAL_PROFILES);
  requireEnumField(report, "input_transport", field, ["sdk"]);
  requireBooleanField(report, "tested", field);
  const drainMs = requireSequenceField(report, "transcript_drain_ms", field);
  if (drainMs === 0 || drainMs > 60_000) {
    throw new PmuxProtocolError(`${field}.transcript_drain_ms must be between 1 and 60000`);
  }
}

function validateSessionSnapshot(value: unknown, field: string): void {
  const data = requireRecord(value, field);
  requireUuidField(data, "session_id", field);
  requireUuidField(data, "generation_id", field);
  requireUuidField(data, "transcript_session_id", field);
  requireEnumField(data, "cell", field, SESSION_CELLS);
  requireEnumField(data, "state", field, SESSION_STATES);
  requireStringField(data, "cwd", field);
  validateCompatibilityReport(
    requireField(data, "compatibility", field),
    `${field}.compatibility`,
  );
  requireSequenceField(data, "created_at_ms", field);
  requireSequenceField(data, "updated_at_ms", field);
  validateOptionalSequence(data, "idle_deadline_ms", field);
  requireBooleanField(data, "resumable", field);
  requireSequenceField(data, "last_sequence", field);
  if (Object.prototype.hasOwnProperty.call(data, "active_turn_id")) {
    requireUuidField(data, "active_turn_id", field);
  }
  if (Object.prototype.hasOwnProperty.call(data, "last_turn")) {
    const lastTurn = requireRecord(data.last_turn, `${field}.last_turn`);
    requireUuidField(lastTurn, "turn_id", `${field}.last_turn`);
    requireEnumField(lastTurn, "outcome", `${field}.last_turn`, TURN_OUTCOMES);
    requireSequenceField(lastTurn, "completed_at_ms", `${field}.last_turn`);
    requireSequenceField(lastTurn, "final_sequence", `${field}.last_turn`);
  }
  if (Object.prototype.hasOwnProperty.call(data, "needs_input")) {
    validateNeedsInput(data.needs_input, `${field}.needs_input`);
  }
}

function validateErrorBody(value: unknown, field: string): ErrorBody {
  const data = requireRecord(value, field);
  requireEnumField(data, "code", field, PMUX_ERROR_CODES);
  requireStringField(data, "message", field);
  requireBooleanField(data, "retryable", field);
  return data as unknown as ErrorBody;
}

function validateTokenUsage(value: unknown, field: string): void {
  const usage = requireRecord(value, field);
  for (const key of [
    "input_tokens", "output_tokens", "cache_creation_input_tokens", "cache_read_input_tokens",
  ]) {
    requireSequenceField(usage, key, field);
  }
}

function validateStopReason(value: unknown, field: string): void {
  const reason = requireRecord(value, field);
  requireEnumField(reason, "kind", field, STOP_REASON_KINDS);
  if (Object.prototype.hasOwnProperty.call(reason, "raw")) {
    requireStringField(reason, "raw", field);
  }
}

function validateMessageBlock(value: unknown, field: string): void {
  const block = requireRecord(value, field);
  const kind = requireEnumField(block, "kind", field, V1_TAGGED_UNIONS.MessageBlock.variants);
  switch (kind) {
    case "text":
      requireStringField(block, "text", field);
      break;
    case "tool_use":
      requireStringField(block, "id", field);
      requireStringField(block, "name", field);
      requireField(block, "input", field);
      break;
    case "tool_result":
      requireStringField(block, "tool_use_id", field);
      requireField(block, "content", field);
      requireBooleanField(block, "is_error", field);
      break;
    case "unknown":
      requireStringField(block, "block_type", field);
      requireField(block, "data", field);
      break;
    default: {
      // The domain above is `MessageBlock`'s own variant list, so a variant
      // added there and not here reaches this arm -- and `kind` is no longer
      // `never`, which is a compile error rather than a block admitted with
      // none of its fields checked.
      const unhandled: never = kind;
      throw new PmuxProtocolError(`${field}.kind ${String(unhandled)} is not validated`);
    }
  }
}

function validateProtocolWarning(value: unknown, field: string): void {
  const warning = requireRecord(value, field);
  requireStringField(warning, "code", field);
  requireStringField(warning, "message", field);
}

function validateNeedsInput(value: unknown, field: string): void {
  const data = requireRecord(value, field);
  requireEnumField(data, "kind", field, NEEDS_INPUT_KINDS);
  requireStringField(data, "message", field);
}

/**
 * A diagnosis is only useful if its coarse outcome and its fine finding agree,
 * so this checks that relationship rather than merely checking that both fields
 * are members of their enums. A report whose summary promises more than its
 * finding tested is a false report with a confession attached.
 */
const RUNTIME_FINDING_OUTCOMES: Readonly<Record<string, string>> = {
  private_runtime_responsive: "pass",
  control_plane_unreachable: "fail",
  control_plane_unresponsive: "fail",
  control_plane_refused: "fail",
  launch_broker_stopped: "fail",
};

const SESSION_FINDING_OUTCOMES: Readonly<Record<string, string>> = {
  terminal_present: "pass",
  terminal_missing: "fail",
  session_declared_unusable: "unproven",
  session_actor_unresponsive: "unproven",
  session_closed_during_probe: "unproven",
  not_probed: "unproven",
};

function requireDerivedOutcome(
  record: Record<string, unknown>,
  field: string,
  findings: readonly string[],
  outcomes: Readonly<Record<string, string>>,
): void {
  requireEnumField(record, "outcome", field, PROBE_OUTCOMES);
  const finding = requireEnumField(record, "finding", field, findings);
  if (record.outcome !== outcomes[finding]) {
    throw new PmuxProtocolError(`${field}.outcome contradicts ${field}.finding`);
  }
}

/** The same derived-outcome relationship, one level down, for the health tree. */
const LAYER_FINDING_OUTCOMES: Readonly<Record<string, string>> = {
  exercised: "pass",
  faulted: "fail",
  // A layer with no subject is vacuously fine, and a layer whose subject could
  // not be reached is not. The daemon derives `outcome` from `finding`; this
  // table is what lets a client REFUSE a report where the two disagree, so it
  // has to carry the same mapping and not a summary of it.
  nothing_to_exercise: "pass",
  not_established: "unproven",
};

function validateDaemonDiagnosis(value: unknown, field: string): void {
  const data = requireRecord(value, field);
  if (Object.prototype.hasOwnProperty.call(data, "layers")) {
    const named = new Set<string>();
    for (const [index, entry] of requireArrayField(data, "layers", field).entries()) {
      const scope = `${field}.layers[${index}]`;
      const layer = requireRecord(entry, scope);
      const name = requireEnumField(layer, "layer", scope, HEALTH_LAYER_NAMES);
      if (named.has(name)) {
        throw new PmuxProtocolError(`${field}.layers reports ${name} twice`);
      }
      named.add(name);
      requireDerivedOutcome(layer, scope, LAYER_FINDINGS, LAYER_FINDING_OUTCOMES);
      // Required for every finding, `exercised` included. A layer that passed
      // without saying what it exercised is the boolean this tree replaced.
      const detail = requireStringField(layer, "detail", scope);
      if (detail.length === 0) {
        throw new PmuxProtocolError(`${scope}.detail must not be empty`);
      }
    }
  }
  const runtime = requireRecord(requireField(data, "runtime", field), `${field}.runtime`);
  requireDerivedOutcome(runtime, `${field}.runtime`, RUNTIME_FINDINGS, RUNTIME_FINDING_OUTCOMES);
  requireSequenceField(runtime, "elapsed_ms", `${field}.runtime`);
  validateOptionalSequence(runtime, "live_private_terminals", `${field}.runtime`);
  for (const [index, entry] of requireArrayField(data, "sessions", field).entries()) {
    const scope = `${field}.sessions[${index}]`;
    const session = requireRecord(entry, scope);
    requireUuidField(session, "session_id", scope);
    requireUuidField(session, "generation_id", scope);
    requireDerivedOutcome(session, scope, SESSION_FINDINGS, SESSION_FINDING_OUTCOMES);
    if (Object.prototype.hasOwnProperty.call(session, "state")) {
      requireEnumField(session, "state", scope, SESSION_STATES);
    }
    if (Object.prototype.hasOwnProperty.call(session, "private_terminal_present")) {
      requireBooleanField(session, "private_terminal_present", scope);
    }
  }
}

function validateTurnResult(value: unknown, field: string): void {
  const data = requireRecord(value, field);
  requireUuidField(data, "session_id", field);
  requireUuidField(data, "generation_id", field);
  requireUuidField(data, "turn_id", field);
  requireEnumField(data, "outcome", field, TURN_OUTCOMES);
  requireStringField(data, "text", field);
  requireStringField(data, "claude_version", field);
  validateCompatibilityReport(
    requireField(data, "compatibility", field),
    `${field}.compatibility`,
  );
  requireSequenceField(data, "final_sequence", field);
  const usage = requireRecord(requireField(data, "usage", field), `${field}.usage`);
  for (const key of ["main", "sidechain", "combined"]) {
    validateTokenUsage(requireField(usage, key, `${field}.usage`), `${field}.usage.${key}`);
  }
  if (Object.prototype.hasOwnProperty.call(usage, "cost_usd")) {
    requireStringField(usage, "cost_usd", `${field}.usage`);
  }
  const timings = requireRecord(requireField(data, "timings", field), `${field}.timings`);
  requireSequenceField(timings, "submitted_at_ms", `${field}.timings`);
  validateOptionalSequence(timings, "prompt_acknowledged_at_ms", `${field}.timings`);
  validateOptionalSequence(timings, "terminal_candidate_at_ms", `${field}.timings`);
  requireSequenceField(timings, "completed_at_ms", `${field}.timings`);
  validateOptionalSequence(timings, "drain_ms", `${field}.timings`);
  validateOptionalSequence(timings, "last_transcript_activity_at_ms", `${field}.timings`);
  validateOptionalSequence(timings, "stop_hook_at_ms", `${field}.timings`);
  const completion = requireRecord(
    requireField(data, "completion", field),
    `${field}.completion`,
  );
  requireEnumField(completion, "authority", `${field}.completion`, COMPLETION_AUTHORITIES);
  for (const key of [
    "prompt_acknowledged", "terminal_message_observed", "terminal_prompt_observed",
    "terminal_quiet_observed", "transcript_drained", "lifecycle_hook_observed",
  ]) {
    requireBooleanField(completion, key, `${field}.completion`);
  }
  if (Object.prototype.hasOwnProperty.call(data, "model")) {
    requireStringField(data, "model", field);
  }
  if (Object.prototype.hasOwnProperty.call(data, "stop_reason")) {
    validateStopReason(data.stop_reason, `${field}.stop_reason`);
  }
  if (Object.prototype.hasOwnProperty.call(data, "final_blocks")) {
    requireArrayField(data, "final_blocks", field).forEach((block, index) =>
      validateMessageBlock(block, `${field}.final_blocks[${index}]`),
    );
  }
  if (Object.prototype.hasOwnProperty.call(data, "tools")) {
    const tools = requireArrayField(data, "tools", field);
    tools.forEach((tool, index) => {
      const record = requireRecord(tool, `${field}.tools[${index}]`);
      requireStringField(record, "tool_use_id", `${field}.tools[${index}]`);
      requireStringField(record, "name", `${field}.tools[${index}]`);
      requireField(record, "input", `${field}.tools[${index}]`);
      requireEnumField(record, "status", `${field}.tools[${index}]`, TOOL_STATUSES);
      validateOptionalSequence(record, "started_at_ms", `${field}.tools[${index}]`);
      validateOptionalSequence(record, "completed_at_ms", `${field}.tools[${index}]`);
    });
  }
  if (Object.prototype.hasOwnProperty.call(data, "warnings")) {
    requireArrayField(data, "warnings", field).forEach((warning, index) =>
      validateProtocolWarning(warning, `${field}.warnings[${index}]`),
    );
  }
}

function validateReplayGap(value: unknown, field: string): void {
  const gap = requireRecord(value, field);
  requireSequenceField(gap, "requested_after", field);
  requireSequenceField(gap, "oldest_available", field);
  requireSequenceField(gap, "next_sequence", field);
  validateSessionSnapshot(requireField(gap, "snapshot", field), `${field}.snapshot`);
}

function validateEventEnvelope(value: unknown, field: string): void {
  const envelope = requireRecord(value, field);
  if (requireSequenceField(envelope, "schema_version", field) !== PROTOCOL_VERSION) {
    throw new PmuxVersionError(envelope.schema_version);
  }
  requireUuidField(envelope, "session_id", field);
  requireUuidField(envelope, "generation_id", field);
  requireSequenceField(envelope, "sequence", field);
  requireSequenceField(envelope, "timestamp_ms", field);
  if (Object.prototype.hasOwnProperty.call(envelope, "turn_id")) {
    requireUuidField(envelope, "turn_id", field);
  }
  const event = requireRecord(requireField(envelope, "event", field), `${field}.event`);
  const type = requireStringField(event, "type", `${field}.event`);
  const data = requireRecord(requireField(event, "data", `${field}.event`), `${field}.event.data`);
  const dataField = `${field}.event.data`;
  switch (type) {
    case "session_state_changed":
      requireEnumField(data, "previous", dataField, SESSION_STATES);
      requireEnumField(data, "current", dataField, SESSION_STATES);
      break;
    case "prompt_acknowledged":
      requireStringField(data, "prompt_uuid", dataField);
      requireSequenceField(data, "transcript_offset", dataField);
      break;
    case "logical_message":
      requireStringField(data, "message_id", dataField);
      requireEnumField(data, "scope", dataField, MESSAGE_SCOPES);
      requireArrayField(data, "blocks", dataField).forEach((block, index) =>
        validateMessageBlock(block, `${dataField}.blocks[${index}]`),
      );
      requireBooleanField(data, "terminal", dataField);
      if (Object.prototype.hasOwnProperty.call(data, "request_id")) {
        requireStringField(data, "request_id", dataField);
      }
      if (Object.prototype.hasOwnProperty.call(data, "model")) {
        requireStringField(data, "model", dataField);
      }
      if (Object.prototype.hasOwnProperty.call(data, "stop_reason")) {
        validateStopReason(data.stop_reason, `${dataField}.stop_reason`);
      }
      if (Object.prototype.hasOwnProperty.call(data, "usage")) {
        validateTokenUsage(data.usage, `${dataField}.usage`);
      }
      break;
    case "tool_started":
      requireStringField(data, "tool_use_id", dataField);
      requireStringField(data, "name", dataField);
      requireField(data, "input", dataField);
      break;
    case "tool_completed":
      requireStringField(data, "tool_use_id", dataField);
      requireField(data, "output", dataField);
      requireBooleanField(data, "is_error", dataField);
      break;
    case "rate_limit":
      requireEnumField(data, "status", dataField, RATE_LIMIT_STATUSES);
      validateOptionalSequence(data, "resets_at_ms", dataField);
      break;
    case "needs_input":
      validateNeedsInput(data, dataField);
      break;
    case "terminal_candidate":
      requireStringField(data, "message_id", dataField);
      if (Object.prototype.hasOwnProperty.call(data, "stop_reason")) {
        validateStopReason(data.stop_reason, `${dataField}.stop_reason`);
      }
      break;
    case "turn_completed":
      validateTurnResult(data, dataField);
      break;
    case "turn_cancelled":
      requireEnumField(data, "outcome", dataField, CANCEL_OUTCOMES);
      requireBooleanField(data, "recovered_to_ready", dataField);
      break;
    case "turn_failed":
      validateErrorBody(data, dataField);
      break;
    case "warning":
      validateProtocolWarning(data, dataField);
      break;
    case "replay_gap":
      validateReplayGap(data, dataField);
      break;
    case "heartbeat":
      requireEnumField(data, "session_state", dataField, SESSION_STATES);
      break;
    default:
      throw new PmuxProtocolError(`${field}.event.type has an unknown discriminant`);
  }
}

function validateEventBatch(value: unknown, field: string): void {
  const batch = requireRecord(value, field);
  const actualNextSequence = requireSequenceField(batch, "next_sequence", field);
  const events = Object.prototype.hasOwnProperty.call(batch, "events")
    ? requireArrayField(batch, "events", field)
    : [];
  if (Object.prototype.hasOwnProperty.call(batch, "events")) {
    events.forEach((event, index) =>
      validateEventEnvelope(event, `${field}.events[${index}]`));
  }
  if (Object.prototype.hasOwnProperty.call(batch, "replay_gap")) {
    validateReplayGap(batch.replay_gap, `${field}.replay_gap`);
    if (events.length !== 0) {
      throw new PmuxProtocolError("a replay-gap batch cannot contain ordinary events");
    }
    const gap = batch.replay_gap as ReplayGap;
    const expectedNext = nextSequence(
      gap.snapshot.last_sequence,
      `${field}.replay_gap.snapshot.next_sequence`,
    );
    const firstRequested = nextSequence(
      gap.requested_after,
      `${field}.replay_gap.requested_after.next_sequence`,
    );
    if (gap.next_sequence !== expectedNext || actualNextSequence !== expectedNext) {
      throw new PmuxProtocolError(
        "replay-gap, snapshot, and batch cursors must agree exactly",
      );
    }
    if (firstRequested >= gap.oldest_available || gap.oldest_available > expectedNext) {
      throw new PmuxProtocolError(
        "replay-gap retained range does not prove that requested events were lost",
      );
    }
  }
}

function validateResponseResult(value: unknown): ResponseResult {
  const result = requireRecord(value, "response.result");
  const type = requireStringField(result, "type", "response.result");
  const data = requireRecord(requireField(result, "data", "response.result"), "response.result.data");
  const field = "response.result.data";
  switch (type) {
    case "pong":
      requireStringField(data, "server_version", field);
      requireSequenceField(data, "protocol_version", field);
      break;
    case "session_started":
      requireUuidField(data, "session_id", field);
      requireUuidField(data, "generation_id", field);
      requireEnumField(data, "state", field, SESSION_STATES);
      validateCompatibilityReport(
        requireField(data, "compatibility", field),
        `${field}.compatibility`,
      );
      requireSequenceField(data, "created_at_ms", field);
      requireSequenceField(data, "last_sequence", field);
      break;
    case "turn_accepted":
      requireUuidField(data, "session_id", field);
      requireUuidField(data, "generation_id", field);
      requireUuidField(data, "turn_id", field);
      requireBooleanField(data, "replayed", field);
      requireEnumField(data, "state", field, SESSION_STATES);
      requireSequenceField(data, "next_sequence", field);
      break;
    case "turn_cancelled":
      requireUuidField(data, "session_id", field);
      requireUuidField(data, "generation_id", field);
      requireUuidField(data, "turn_id", field);
      requireEnumField(data, "outcome", field, CANCEL_OUTCOMES);
      requireEnumField(data, "session_state", field, SESSION_STATES);
      break;
    case "session_snapshot":
      validateSessionSnapshot(data, field);
      break;
    case "attach_capability":
      requireUuidField(data, "session_id", field);
      requireUuidField(data, "generation_id", field);
      requireStringField(data, "token", field);
      requireStringField(data, "endpoint", field);
      requireSequenceField(data, "expires_at_ms", field);
      requireBooleanField(data, "read_only", field);
      break;
    case "session_closed":
      requireUuidField(data, "session_id", field);
      requireUuidField(data, "generation_id", field);
      requireBooleanField(data, "already_closed", field);
      requireBooleanField(data, "process_reaped", field);
      break;
    case "events":
      validateEventBatch(data, field);
      break;
    case "turn_result":
      validateTurnResult(data, field);
      break;
    case "session_cleared":
      requireUuidField(data, "session_id", field);
      requireUuidField(data, "generation_id", field);
      requireUuidField(data, "transcript_session_id", field);
      requireBooleanField(data, "rotated", field);
      requireEnumField(data, "state", field, SESSION_STATES);
      break;
    case "diagnosis":
      validateDaemonDiagnosis(data, field);
      break;
    case "stateless_result":
      validateStatelessResult(data, field);
      break;
    case "agent_created":
    case "agent":
    case "agent_updated":
      validateAgentDescriptor(data, field);
      break;
    case "agent_list":
      validateAgentList(data, field);
      break;
    default:
      throw new PmuxProtocolError("response.result.type has an unknown discriminant");
  }
  return result as unknown as ResponseResult;
}

interface ResponseDataMap {
  pong: Pong;
  session_started: SessionHandle;
  turn_accepted: TurnAccepted;
  turn_cancelled: CancelTurnResult;
  session_snapshot: SessionSnapshot;
  attach_capability: AttachCapability;
  session_closed: CloseSessionResult;
  events: EventBatch;
  turn_result: TurnResult;
  session_cleared: ClearSessionResult;
  diagnosis: DaemonDiagnosis;
  stateless_result: StatelessResult;
  agent_created: AgentDescriptor;
  agent: AgentDescriptor;
  agent_list: AgentList;
  agent_updated: AgentDescriptor;
}

/**
 * One stored agent version.
 *
 * `config_digest` is checked as a required string because it is IDENTITY: a
 * descriptor without one names nothing, and it is the value a caller compares
 * to answer "is this the configuration I wrote". The spec's own optional
 * launch-policy fields are not required here for the reason they are optional
 * on the wire -- the daemon omits a field the stored agent left at its default.
 */
function validateAgentDescriptor(data: Record<string, unknown>, field: string): void {
  requireUuidField(data, "agent_id", field);
  requireAgentVersionField(data, "version", field);
  requireStringField(data, "config_digest", field);
  requireSequenceField(data, "created_at_ms", field);
  requireSequenceField(data, "updated_at_ms", field);
  // `spec` is checked for being an object and NOTHING ELSE. It is the stored
  // document echoed back, opaque on a response for the reason the daemon's own
  // type states: a request must refuse an unknown field and a response must
  // tolerate one, and no client in any language keeps two decoders for one
  // type. Decode it with an `AgentSpec` where strictness is what you want.
  requireRecord(requireField(data, "spec", field), `${field}.spec`);
}

/**
 * A listing, and the records it could not read.
 *
 * Both arrays are optional on the wire and omitted when empty, so an ordinary
 * listing's bytes are what every release before `unreadable` existed sent. It
 * is validated rather than passed through for the reason it exists: a caller
 * who cannot see that a record was unreadable sees a stored agent simply stop
 * appearing.
 */
function validateAgentList(data: Record<string, unknown>, field: string): void {
  if (Object.prototype.hasOwnProperty.call(data, "agents")) {
    const agents = requireField(data, "agents", field);
    if (!Array.isArray(agents)) {
      throw new PmuxProtocolError(`${field}.agents must be an array`);
    }
    agents.forEach((entry, index) => {
      const scope = `${field}.agents[${index}]`;
      const summary = requireRecord(entry, scope);
      requireUuidField(summary, "agent_id", scope);
      requireAgentVersionField(summary, "version", scope);
      requireStringField(summary, "config_digest", scope);
      requireStringField(summary, "name", scope);
      requireEnumField(summary, "cell", scope, SESSION_CELLS);
      requireSequenceField(summary, "updated_at_ms", scope);
    });
  }
  if (!Object.prototype.hasOwnProperty.call(data, "unreadable")) {
    return;
  }
  const unreadable = requireField(data, "unreadable", field);
  if (!Array.isArray(unreadable)) {
    throw new PmuxProtocolError(`${field}.unreadable must be an array`);
  }
  unreadable.forEach((entry, index) => {
    const scope = `${field}.unreadable[${index}]`;
    const failure = requireRecord(entry, scope);
    requireUuidField(failure, "agent_id", scope);
    requireStringField(failure, "reason", scope);
  });
}

/**
 * An agent version starts at 1.
 *
 * Checked in the client for the same reason the daemon's newtype checks it: a
 * zero is not a version, and a caller that pinned one would be naming a stored
 * object that cannot exist.
 */
function requireAgentVersionField(
  data: Record<string, unknown>,
  key: string,
  field: string,
): void {
  const value = requireSequenceField(data, key, field);
  if (value < 1) {
    throw new PmuxProtocolError(`${field}.${key} must be at least 1; there is no version 0`);
  }
}

/**
 * The Path B answer, validated for what it must and must NOT carry.
 *
 * The absence check is not decoration. `session_id` is the one field whose
 * presence would mean a pool instance had been named on the wire, and a caller
 * that can name a resource is one step from aliasing one. It is asserted here,
 * in the client, because the client is where a daemon that regressed would be
 * caught by somebody who did not deploy it.
 */
function validateStatelessResult(data: Record<string, unknown>, field: string): void {
  requireStringField(data, "model", field);
  requireStringField(data, "text", field);
  requireStringField(data, "claude_version", field);
  if (Object.prototype.hasOwnProperty.call(data, "reported_model")) {
    requireStringField(data, "reported_model", field);
  }
  if (Object.prototype.hasOwnProperty.call(data, "effort")) {
    requireEnumField(data, "effort", field, EFFORT_LEVELS);
  }
  // FOUND BY GIVING `run_stateless` A GOLDEN PAIR. This validator was the only
  // one of the three that reads a `stop_reason` and did not check it: the
  // corpus's required-field inventory deletes `stop_reason/kind` from every
  // result that carries one and requires the client to reject the frame, and
  // this client accepted it, because `run_stateless` was the one method with no
  // golden pair in any language.
  if (Object.prototype.hasOwnProperty.call(data, "stop_reason")) {
    validateStopReason(data.stop_reason, `${field}.stop_reason`);
  }
  const usage = requireRecord(requireField(data, "usage", field), `${field}.usage`);
  for (const key of ["main", "sidechain", "combined"]) {
    validateTokenUsage(requireField(usage, key, `${field}.usage`), `${field}.usage.${key}`);
  }
  if (Object.prototype.hasOwnProperty.call(usage, "cost_usd")) {
    requireStringField(usage, "cost_usd", `${field}.usage`);
  }
  for (const named of [
    "session_id",
    "generation_id",
    "cwd",
    "config_root",
    "system_prompt",
  ]) {
    if (Object.prototype.hasOwnProperty.call(data, named)) {
      throw new PmuxProtocolError(
        `${field} carries ${named}, which would name a pool resource on the wire`,
      );
    }
  }
}

function expectResult<K extends keyof ResponseDataMap>(
  result: ResponseResult,
  expected: K,
): ResponseDataMap[K] {
  if (result.type !== expected) {
    throw new PmuxUnexpectedResultError(expected, result.type);
  }
  return result.data as ResponseDataMap[K];
}

function decodeResponse(payload: Buffer, requestId: string): ResponseResult {
  let parsed: unknown;
  try {
    parsed = JSON.parse(UTF8_DECODER.decode(payload));
  } catch (error) {
    throw new PmuxProtocolError("pmuxd returned invalid JSON", { cause: error });
  }
  validateJsonNumericDomain(parsed, "response");
  if (!isRecord(parsed)) {
    throw new PmuxProtocolError("pmuxd response must be an object");
  }

  // Validate the major before interpreting version-specific payloads.
  if (parsed.version !== PROTOCOL_VERSION) {
    throw new PmuxVersionError(parsed.version);
  }
  const responseRequestId = requireUuidField(parsed, "request_id", "response");
  if (!sameUuid(responseRequestId, requestId)) {
    throw new PmuxRequestIdMismatchError(requestId, responseRequestId);
  }

  // Intentionally do not exact-key-check v1 responses. Servers may add object
  // fields in a minor evolution; required fields, result/error exclusivity,
  // discriminants used by each typed method, and the major stay strict.
  const hasResult = Object.prototype.hasOwnProperty.call(parsed, "result");
  const hasError = Object.prototype.hasOwnProperty.call(parsed, "error");
  if (hasResult === hasError) {
    throw new PmuxProtocolError("response must contain exactly one of result or error");
  }
  if (hasError) {
    const body = validateErrorBody(parsed.error, "response.error");
    throw new PmuxServerError(body);
  }

  return validateResponseResult(parsed.result);
}

function requestTimeoutFor(request: PmuxRequest, configured: number): number {
  if (request.method === "subscribe_events") {
    return Math.max(configured, (request.params.wait_ms ?? 0) + 5_000);
  }
  if (request.method === "run_once") {
    const deadline = request.params.turn.deadline_unix_ms;
    const turnWindow = deadline === undefined
      ? DEFAULT_RUN_ONCE_TIMEOUT_MS
      : Math.max(0, deadline - Date.now()) + RUN_ONCE_RESPONSE_MARGIN_MS;
    return Math.max(configured, turnWindow);
  }
  // A caller-supplied submission deadline widens the client's patience the same
  // way `run_once` does, so asking for a longer input window cannot make the
  // client give up while the daemon is still typing.
  if (request.method === "clear_session") {
    const deadline = request.params.deadline_unix_ms;
    if (deadline === undefined) return configured;
    return Math.max(configured, Math.max(0, deadline - Date.now()) + RUN_ONCE_RESPONSE_MARGIN_MS);
  }
  return configured;
}

function validateStartRequestIdentities(request: StartSessionRequest, field: string): void {
  const identity = requireRecord(request.identity, `${field}.identity`);
  if (Object.prototype.hasOwnProperty.call(identity, "session_id")) {
    requireUuid(identity.session_id, `${field}.identity.session_id`);
  } else if (identity.mode === "resume") {
    throw new PmuxProtocolError(`${field}.identity.session_id is required`);
  }
}

function validateTurnRequestIdentity(request: TurnRequest, field: string): void {
  requireUuid(request.turn_id, `${field}.turn_id`);
}

function validateTurnRequestNumbers(request: TurnRequest, field: string): void {
  const turn = requireRecord(request, field);
  validateOptionalSequence(turn, "deadline_unix_ms", field);
  if (turn.lease !== undefined) {
    const lease = requireRecord(turn.lease, `${field}.lease`);
    validateOptionalSequence(lease, "heartbeat_timeout_ms", `${field}.lease`);
  }
}

function validateStartRequestNumbers(request: StartSessionRequest, field: string): void {
  const start = requireRecord(request, field);
  if (start.terminal !== undefined) {
    const terminal = requireRecord(start.terminal, `${field}.terminal`);
    requireSequenceField(terminal, "rows", `${field}.terminal`);
    requireSequenceField(terminal, "cols", `${field}.terminal`);
  }
  if (start.lifecycle !== undefined) {
    const lifecycle = requireRecord(start.lifecycle, `${field}.lifecycle`);
    validateOptionalSequence(lifecycle, "hook_timeout_ms", `${field}.lifecycle`);
  }
  if (start.retention !== undefined) {
    const retention = requireRecord(start.retention, `${field}.retention`);
    validateOptionalSequence(retention, "idle_ttl_ms", `${field}.retention`);
  }
}

function validateRequestIdentities(request: PmuxRequest): void {
  switch (request.method) {
    case "ping":
      return;
    case "start_session":
      validateStartRequestIdentities(request.params, "request.params");
      validateStartRequestNumbers(request.params, "request.params");
      return;
    case "run_turn":
      requireUuid(request.params.session_id, "request.params.session_id");
      requireUuid(request.params.generation_id, "request.params.generation_id");
      validateTurnRequestIdentity(request.params.turn, "request.params.turn");
      validateTurnRequestNumbers(request.params.turn, "request.params.turn");
      return;
    case "cancel_turn":
      requireUuid(request.params.session_id, "request.params.session_id");
      requireUuid(request.params.generation_id, "request.params.generation_id");
      requireUuid(request.params.turn_id, "request.params.turn_id");
      return;
    case "inspect_session":
    case "close_session":
      requireUuid(request.params.session_id, "request.params.session_id");
      requireUuid(request.params.generation_id, "request.params.generation_id");
      return;
    case "attach_session":
      requireUuid(request.params.session_id, "request.params.session_id");
      requireUuid(request.params.generation_id, "request.params.generation_id");
      if (request.params.size !== undefined) {
        const size = requireRecord(request.params.size, "request.params.size");
        requireSequenceField(size, "rows", "request.params.size");
        requireSequenceField(size, "cols", "request.params.size");
      }
      return;
    case "subscribe_events": {
      requireUuid(request.params.session_id, "request.params.session_id");
      requireUuid(request.params.generation_id, "request.params.generation_id");
      const params = requireRecord(request.params, "request.params");
      validateOptionalSequence(params, "after_sequence", "request.params");
      validateOptionalSequence(params, "wait_ms", "request.params");
      validateOptionalSequence(params, "max_events", "request.params");
      return;
    }
    case "run_once":
      validateStartRequestIdentities(request.params.session, "request.params.session");
      validateStartRequestNumbers(request.params.session, "request.params.session");
      validateTurnRequestIdentity(request.params.turn, "request.params.turn");
      validateTurnRequestNumbers(request.params.turn, "request.params.turn");
      return;
  }
}

function requireMatchingIdentity(actual: string, expected: string, field: string): void {
  if (!sameUuid(actual, expected)) {
    throw new PmuxProtocolError(`${field} ${actual} does not match request ${expected}`);
  }
}

function expectedStartSessionId(request: StartSessionRequest): string | undefined {
  return request.identity.session_id;
}

function validateResultForRequest(request: PmuxRequest, result: ResponseResult): void {
  switch (request.method) {
    case "ping":
      if (result.type === "pong" && result.data.protocol_version !== PROTOCOL_VERSION) {
        throw new PmuxVersionError(result.data.protocol_version);
      }
      return;
    case "start_session":
      if (result.type === "session_started") {
        const expected = expectedStartSessionId(request.params);
        if (expected !== undefined) {
          requireMatchingIdentity(result.data.session_id, expected, "result session_id");
        }
      }
      return;
    case "run_turn":
      if (result.type === "turn_accepted") {
        requireMatchingIdentity(
          result.data.session_id,
          request.params.session_id,
          "result session_id",
        );
        requireMatchingIdentity(
          result.data.generation_id,
          request.params.generation_id,
          "result generation_id",
        );
        requireMatchingIdentity(
          result.data.turn_id,
          request.params.turn.turn_id,
          "result turn_id",
        );
      }
      return;
    case "cancel_turn":
      if (result.type === "turn_cancelled") {
        requireMatchingIdentity(
          result.data.session_id,
          request.params.session_id,
          "result session_id",
        );
        requireMatchingIdentity(
          result.data.generation_id,
          request.params.generation_id,
          "result generation_id",
        );
        requireMatchingIdentity(result.data.turn_id, request.params.turn_id, "result turn_id");
      }
      return;
    case "inspect_session":
      if (result.type === "session_snapshot") {
        requireMatchingIdentity(
          result.data.session_id,
          request.params.session_id,
          "result session_id",
        );
        requireMatchingIdentity(
          result.data.generation_id,
          request.params.generation_id,
          "result generation_id",
        );
      }
      return;
    case "attach_session":
      if (result.type === "attach_capability") {
        requireMatchingIdentity(
          result.data.session_id,
          request.params.session_id,
          "result session_id",
        );
        requireMatchingIdentity(
          result.data.generation_id,
          request.params.generation_id,
          "result generation_id",
        );
      }
      return;
    case "close_session":
      if (result.type === "session_closed") {
        requireMatchingIdentity(
          result.data.session_id,
          request.params.session_id,
          "result session_id",
        );
        requireMatchingIdentity(
          result.data.generation_id,
          request.params.generation_id,
          "result generation_id",
        );
      }
      return;
    case "run_once":
      if (result.type === "turn_result") {
        const expectedSession = expectedStartSessionId(request.params.session);
        if (expectedSession !== undefined) {
          requireMatchingIdentity(
            result.data.session_id,
            expectedSession,
            "result session_id",
          );
        }
        requireMatchingIdentity(
          result.data.turn_id,
          request.params.turn.turn_id,
          "result turn_id",
        );
      }
      return;
    case "clear_session":
      if (result.type === "session_cleared") {
        requireMatchingIdentity(
          result.data.session_id,
          request.params.session_id,
          "result session_id",
        );
        requireMatchingIdentity(
          result.data.generation_id,
          request.params.generation_id,
          "result generation_id",
        );
      }
      return;
    case "subscribe_events":
      return;
  }
}

/** Native protocol-v1 client bound to one explicit Unix-domain socket. */
export class PmuxClient {
  readonly socketPath: string;
  readonly options: Readonly<ResolvedClientOptions>;

  constructor(socketPath: string, options: PmuxClientOptions = {}) {
    if (!socketPath) throw new TypeError("socketPath must not be empty");
    if (!isAbsolute(socketPath)) throw new TypeError("socketPath must be absolute");
    const resolved = {
      maxFrameBytes: options.maxFrameBytes ?? DEFAULT_MAX_FRAME_BYTES,
      connectTimeoutMs: options.connectTimeoutMs ?? DEFAULT_CONNECT_TIMEOUT_MS,
      requestTimeoutMs: options.requestTimeoutMs ?? DEFAULT_REQUEST_TIMEOUT_MS,
    };
    if (
      !Number.isSafeInteger(resolved.maxFrameBytes) ||
      resolved.maxFrameBytes < 1 ||
      resolved.maxFrameBytes > MAX_NATIVE_FRAME_BYTES
    ) {
      throw new RangeError(`maxFrameBytes must be between 1 and ${MAX_NATIVE_FRAME_BYTES}`);
    }
    if (!Number.isFinite(resolved.connectTimeoutMs) || resolved.connectTimeoutMs <= 0) {
      throw new RangeError("connectTimeoutMs must be greater than zero");
    }
    if (!Number.isFinite(resolved.requestTimeoutMs) || resolved.requestTimeoutMs <= 0) {
      throw new RangeError("requestTimeoutMs must be greater than zero");
    }
    this.socketPath = socketPath;
    this.options = Object.freeze(resolved);
  }

  async request(request: PmuxRequest, options: RequestOptions = {}): Promise<ResponseResult> {
    validateRequestIdentities(request);
    const requestId = randomUUID();
    const envelope: RequestEnvelope = {
      version: PROTOCOL_VERSION,
      request_id: requestId,
      ...request,
    };
    let payload: Buffer;
    try {
      payload = Buffer.from(
        JSON.stringify(envelope, (_key, value: unknown) => {
          if (typeof value === "number") {
            if (!Number.isFinite(value)) {
              throw new TypeError("non-finite numbers are not valid JSON");
            }
            if (Number.isInteger(value) && !Number.isSafeInteger(value)) {
              throw new TypeError("integer is outside the signed safe-integer range");
            }
          }
          return value;
        }),
        "utf8",
      );
    } catch (error) {
      throw new PmuxProtocolError("request is not JSON serializable", { cause: error });
    }
    this.ensureFrameSize(payload.length);
    const response = await this.exchange(
      payload,
      requestTimeoutFor(request, this.options.requestTimeoutMs),
      options.signal,
    );
    const result = decodeResponse(response, requestId);
    validateResultForRequest(request, result);
    return result;
  }

  async ping(options?: RequestOptions): Promise<Pong> {
    return expectResult(await this.request({ method: "ping" }, options), "pong");
  }

  async startSession(
    request: StartSessionRequest,
    options?: RequestOptions,
  ): Promise<SessionHandle> {
    return expectResult(
      await this.request({ method: "start_session", params: request }, options),
      "session_started",
    );
  }

  async inspectSession(
    sessionId: SessionId,
    generationId: SessionGenerationId,
    options?: RequestOptions,
  ): Promise<SessionSnapshot> {
    return expectResult(
      await this.request(
        {
          method: "inspect_session",
          params: { session_id: sessionId, generation_id: generationId },
        },
        options,
      ),
      "session_snapshot",
    );
  }

  async runTurn(
    sessionId: SessionId,
    generationId: SessionGenerationId,
    turn: TurnRequest,
    options?: RequestOptions,
  ): Promise<TurnAccepted> {
    return expectResult(
      await this.request(
        {
          method: "run_turn",
          params: { session_id: sessionId, generation_id: generationId, turn },
        },
        options,
      ),
      "turn_accepted",
    );
  }

  async cancelTurn(
    sessionId: SessionId,
    generationId: SessionGenerationId,
    turnId: TurnId,
    options?: RequestOptions,
  ): Promise<CancelTurnResult> {
    return expectResult(
      await this.request(
        {
          method: "cancel_turn",
          params: {
            session_id: sessionId,
            generation_id: generationId,
            turn_id: turnId,
          },
        },
        options,
      ),
      "turn_cancelled",
    );
  }

  async attachSession(
    request: AttachSessionRequest,
    options?: RequestOptions,
  ): Promise<AttachCapability> {
    return expectResult(
      await this.request({ method: "attach_session", params: request }, options),
      "attach_capability",
    );
  }

  async closeSession(
    sessionId: SessionId,
    generationId: SessionGenerationId,
    policy: ClosePolicy = "graceful",
    options?: RequestOptions,
  ): Promise<CloseSessionResult> {
    return expectResult(
      await this.request(
        {
          method: "close_session",
          params: { session_id: sessionId, generation_id: generationId, policy },
        },
        options,
      ),
      "session_closed",
    );
  }

  /**
   * Clears one minified-cell session's context between turns.
   *
   * `expectedTranscriptSessionId` is the transcript the caller believes is
   * bound: at start it is the session id, and afterwards it is whatever the
   * previous result returned, or whatever `inspectSession` reports as
   * `transcript_session_id`. It is a compare-and-swap fence and every stale
   * value is refused, including one that is stale by exactly one rotation:
   * there is no "your clear already landed" answer, because the one-behind
   * value is indistinguishable from the fence a session starts with, which is
   * what a second caller holds. To recover a lost response, re-read
   * `transcript_session_id` and clear again on it if certainty is wanted;
   * clearing an already-empty cell is semantically idempotent.
   */
  async clearSession(
    sessionId: SessionId,
    generationId: SessionGenerationId,
    expectedTranscriptSessionId: SessionId,
    deadlineUnixMs?: number,
    options?: RequestOptions,
  ): Promise<ClearSessionResult> {
    const params: ClearSessionRequest = {
      session_id: sessionId,
      generation_id: generationId,
      expected_transcript_session_id: expectedTranscriptSessionId,
    };
    if (deadlineUnixMs !== undefined) params.deadline_unix_ms = deadlineUnixMs;
    return expectResult(
      await this.request({ method: "clear_session", params }, options),
      "session_cleared",
    );
  }

  /**
   * Asks the daemon to complete one real operation against its private runtime
   * and report what it found, per session.
   *
   * Costs one rmux round trip in the daemon regardless of how many sessions it
   * holds, and never starts a Claude turn. `ping` cannot answer any of this:
   * it is served without touching the private runtime, the session registry,
   * the launch broker or the rmux sidecar.
   */
  async diagnose(options?: RequestOptions): Promise<DaemonDiagnosis> {
    return expectResult(await this.request({ method: "diagnose" }, options), "diagnosis");
  }

  /**
   * One stateless call: `(model, effort, prompt)` in, text and usage out.
   *
   * THE CALLER NAMES NO RESOURCE. {@link RunStatelessRequest} carries a model,
   * an optional effort, a prompt and an optional deadline, and nothing else.
   * The daemon mints every path, environment variable and system prompt from
   * its own configuration plus a slot identity.
   */
  async runStateless(
    request: RunStatelessRequest,
    options?: RequestOptions,
  ): Promise<StatelessResult> {
    return expectResult(
      await this.request({ method: "run_stateless", params: request }, options),
      "stateless_result",
    );
  }

  /**
   * Stores one reusable launch configuration and returns it at version 1.
   *
   * The daemon mints the id. An agent carries LAUNCH POLICY and never a
   * resource: no cwd, no configuration root, no session identity, no prompt and
   * no environment snapshot, because each is per-session and is named on every
   * `startSession`.
   */
  async createAgent(spec: AgentSpec, options?: RequestOptions): Promise<AgentDescriptor> {
    return expectResult(
      await this.request({ method: "create_agent", params: { spec } }, options),
      "agent_created",
    );
  }

  /**
   * Reads one stored agent version, or the current head when `version` is
   * omitted.
   *
   * Environment values and inline settings/MCP document bodies come back as
   * `sha256:` digests and never in the clear; `config_digest` still identifies
   * the configuration exactly.
   */
  async getAgent(
    agentId: string,
    version?: number,
    options?: RequestOptions,
  ): Promise<AgentDescriptor> {
    const params = version === undefined ? { agent_id: agentId } : { agent_id: agentId, version };
    return expectResult(await this.request({ method: "get_agent", params }, options), "agent");
  }

  /**
   * Lists every stored agent's id, current version, digest, name and cell.
   * Deliberately not full specs.
   */
  async listAgents(options?: RequestOptions): Promise<AgentList> {
    return expectResult(
      await this.request({ method: "list_agents", params: {} }, options),
      "agent_list",
    );
  }

  /**
   * Stores a new immutable version of one agent and returns it.
   *
   * `expectedVersion` is a fence: any value that is not the current head is
   * refused with `id_conflict`, including one stale by exactly one revision,
   * and no update is ever answered as "already landed". `spec` is a COMPLETE
   * replacement. Running sessions are unaffected -- each pinned its version at
   * start.
   */
  async updateAgent(
    agentId: string,
    expectedVersion: number,
    spec: AgentSpec,
    options?: RequestOptions,
  ): Promise<AgentDescriptor> {
    return expectResult(
      await this.request(
        {
          method: "update_agent",
          params: { agent_id: agentId, expected_version: expectedVersion, spec },
        },
        options,
      ),
      "agent_updated",
    );
  }

  async runOnce(request: RunOnceRequest, options?: RequestOptions): Promise<TurnResult> {
    return expectResult(
      await this.request({ method: "run_once", params: request }, options),
      "turn_result",
    );
  }

  async subscribeEvents(
    request: SubscribeEventsRequest,
    options?: RequestOptions,
  ): Promise<EventBatch> {
    const normalized = {
      ...request,
      after_sequence: request.after_sequence ?? 0,
      wait_ms: request.wait_ms ?? 0,
      max_events: request.max_events ?? 0,
    };
    const batch = expectResult(
      await this.request({ method: "subscribe_events", params: normalized }, options),
      "events",
    );
    validateBatch(
      normalized.session_id,
      normalized.generation_id,
      normalized.after_sequence,
      batch,
    );
    return batch;
  }

  /** Long-poll subscription with durable cursor and transport reconnection. */
  events(
    sessionId: SessionId,
    generationId: SessionGenerationId,
    options: EventSubscriptionOptions = {},
  ): EventSubscription {
    return new EventSubscription(this, sessionId, generationId, options);
  }

  private ensureFrameSize(length: number): void {
    if (length > this.options.maxFrameBytes) {
      throw new PmuxFrameTooLargeError(length, this.options.maxFrameBytes);
    }
  }

  private exchange(
    payload: Buffer,
    requestTimeoutMs: number,
    signal?: AbortSignal,
  ): Promise<Buffer> {
    if (signal?.aborted) {
      return Promise.reject(new PmuxAbortError("request aborted", { cause: signal.reason }));
    }

    return new Promise<Buffer>((resolve, reject) => {
      let settled = false;
      let connected = false;
      let headerOffset = 0;
      const header = Buffer.allocUnsafe(4);
      let body: Buffer | undefined;
      let bodyOffset = 0;
      let socket: Socket;

      const cancelConnectTimer = scheduleLongTimeout(
        () => fail(new PmuxTimeoutError("connect", this.options.connectTimeoutMs)),
        this.options.connectTimeoutMs,
      );
      const cancelRequestTimer = scheduleLongTimeout(
        () => fail(new PmuxTimeoutError("request", requestTimeoutMs)),
        requestTimeoutMs,
      );

      const cleanup = (): void => {
        cancelConnectTimer();
        cancelRequestTimer();
        signal?.removeEventListener("abort", onAbort);
        socket?.removeAllListeners();
      };
      const fail = (error: unknown): void => {
        if (settled) return;
        settled = true;
        cleanup();
        socket?.destroy();
        reject(error);
      };
      const succeed = (value: Buffer): void => {
        if (settled) return;
        settled = true;
        cleanup();
        socket.destroy();
        resolve(value);
      };
      const onAbort = (): void =>
        fail(new PmuxAbortError("request aborted", { cause: signal?.reason }));

      socket = createConnection({ path: this.socketPath });
      signal?.addEventListener("abort", onAbort, { once: true });
      socket.once("connect", () => {
        connected = true;
        cancelConnectTimer();
        const prefix = Buffer.allocUnsafe(4);
        prefix.writeUInt32BE(payload.length);
        socket.cork();
        socket.write(prefix);
        socket.write(payload);
        socket.uncork();
      });
      socket.on("data", (chunk: Buffer) => {
        let offset = 0;
        while (offset < chunk.length && !settled) {
          if (headerOffset < 4) {
            const copied = chunk.copy(header, headerOffset, offset, offset + (4 - headerOffset));
            headerOffset += copied;
            offset += copied;
            if (headerOffset < 4) continue;
            const advertised = header.readUInt32BE(0);
            if (advertised > this.options.maxFrameBytes) {
              fail(new PmuxFrameTooLargeError(advertised, this.options.maxFrameBytes));
              return;
            }
            body = Buffer.allocUnsafe(advertised);
            if (advertised === 0) {
              if (offset !== chunk.length) {
                fail(new PmuxProtocolError("response contained trailing bytes"));
              } else {
                succeed(body);
              }
              return;
            }
          }

          const response = body as Buffer;
          const copied = chunk.copy(
            response,
            bodyOffset,
            offset,
            offset + (response.length - bodyOffset),
          );
          bodyOffset += copied;
          offset += copied;
          if (bodyOffset === response.length) {
            if (offset !== chunk.length) {
              fail(new PmuxProtocolError("response contained trailing bytes"));
            } else {
              succeed(response);
            }
            return;
          }
        }
      });
      socket.once("error", (error) =>
        fail(new PmuxTransportError(`socket error: ${error.message}`, { cause: error })),
      );
      socket.once("close", () => {
        if (!settled) {
          fail(
            new PmuxTransportError(
              connected
                ? "socket closed before a complete response"
                : "socket closed before connecting",
            ),
          );
        }
      });
    });
  }
}

export interface EventSubscriptionOptions extends RequestOptions {
  afterSequence?: number;
  waitMs?: number;
  maxEvents?: number;
  reconnectDelayMs?: number;
  maxReconnectAttempts?: number;
  emptyBatchDelayMs?: number;
  onReconnect?: (error: PmuxTransportError, attempt: number, afterSequence: number) => void;
}

export type EventStreamItem =
  | { kind: "event"; event: EventEnvelope }
  | { kind: "replay_gap"; gap: ReplayGap };

export class EventSubscription implements AsyncIterable<EventStreamItem> {
  private cursor: number;
  private running = false;
  private readonly options: Required<
    Pick<
      EventSubscriptionOptions,
      | "waitMs"
      | "maxEvents"
      | "reconnectDelayMs"
      | "maxReconnectAttempts"
      | "emptyBatchDelayMs"
    >
  > &
    Pick<EventSubscriptionOptions, "signal" | "onReconnect">;

  constructor(
    private readonly client: PmuxClient,
    readonly sessionId: SessionId,
    readonly generationId: SessionGenerationId,
    options: EventSubscriptionOptions,
  ) {
    this.cursor = requireSafeSequence(options.afterSequence ?? 0, "afterSequence");
    this.options = {
      signal: options.signal,
      onReconnect: options.onReconnect,
      waitMs: options.waitMs ?? 30_000,
      maxEvents: options.maxEvents ?? 128,
      reconnectDelayMs: options.reconnectDelayMs ?? 250,
      maxReconnectAttempts: options.maxReconnectAttempts ?? Number.POSITIVE_INFINITY,
      emptyBatchDelayMs: options.emptyBatchDelayMs ?? 25,
    };
  }

  get afterSequence(): number {
    return this.cursor;
  }

  async *[Symbol.asyncIterator](): AsyncGenerator<EventStreamItem> {
    if (this.running) throw new PmuxProtocolError("event subscription is already being consumed");
    this.running = true;
    let reconnectAttempts = 0;
    try {
      while (true) {
        throwIfAborted(this.options.signal);
        let batch: EventBatch;
        try {
          batch = await this.client.subscribeEvents(
            {
              session_id: this.sessionId,
              generation_id: this.generationId,
              after_sequence: this.cursor,
              wait_ms: this.options.waitMs,
              max_events: this.options.maxEvents,
            },
            { signal: this.options.signal },
          );
          reconnectAttempts = 0;
        } catch (error) {
          if (
            !(error instanceof PmuxTransportError) ||
            reconnectAttempts >= this.options.maxReconnectAttempts
          ) {
            throw error;
          }
          reconnectAttempts += 1;
          this.options.onReconnect?.(error, reconnectAttempts, this.cursor);
          const backoff = Math.min(
            this.options.reconnectDelayMs * 2 ** Math.min(reconnectAttempts - 1, 8),
            30_000,
          );
          await abortableDelay(backoff, this.options.signal);
          continue;
        }

        const events = batch.events ?? [];
        if (batch.replay_gap) {
          this.cursor = batch.replay_gap.snapshot.last_sequence;
          yield { kind: "replay_gap", gap: batch.replay_gap };
        }
        for (const event of events) {
          this.cursor = event.sequence;
          yield { kind: "event", event };
        }
        if (!batch.replay_gap && events.length === 0 && this.options.emptyBatchDelayMs > 0) {
          await abortableDelay(this.options.emptyBatchDelayMs, this.options.signal);
        }
      }
    } finally {
      this.running = false;
    }
  }
}

function validateBatch(
  sessionId: SessionId,
  generationId: SessionGenerationId,
  requestedAfter: number,
  batch: EventBatch,
): void {
  let cursor = requestedAfter;
  const events = batch.events ?? [];
  if (!Array.isArray(events)) throw new PmuxProtocolError("events must be an array");

  if (batch.replay_gap) {
    const gap = batch.replay_gap;
    if (events.length !== 0) {
      throw new PmuxProtocolError("a replay-gap batch cannot contain ordinary events");
    }
    if (gap.requested_after !== requestedAfter) {
      throw new PmuxProtocolError(
        `replay gap cursor ${gap.requested_after} does not match ${requestedAfter}`,
      );
    }
    if (!sameUuid(gap.snapshot.session_id, sessionId)) {
      throw new PmuxProtocolError("replay gap snapshot belongs to another session");
    }
    if (!sameUuid(gap.snapshot.generation_id, generationId)) {
      throw new PmuxProtocolError("replay gap snapshot belongs to another process generation");
    }
    const snapshotLast = requireSafeSequence(
      gap.snapshot.last_sequence,
      "replay_gap.snapshot.last_sequence",
    );
    const expectedNext = nextSequence(snapshotLast, "replay_gap.snapshot.next_sequence");
    if (gap.next_sequence !== expectedNext || batch.next_sequence !== expectedNext) {
      throw new PmuxProtocolError(
        "replay-gap, snapshot, and batch cursors must agree exactly",
      );
    }
    cursor = snapshotLast;
  }

  for (const event of events) {
    if (event.schema_version !== PROTOCOL_VERSION) {
      throw new PmuxVersionError(event.schema_version);
    }
    if (!sameUuid(event.session_id, sessionId)) {
      throw new PmuxProtocolError("event belongs to another session");
    }
    if (!sameUuid(event.generation_id, generationId)) {
      throw new PmuxProtocolError("event belongs to another process generation");
    }
    const sequence = requireSafeSequence(event.sequence, "event.sequence");
    const expected = nextSequence(cursor, "event.next_sequence");
    if (sequence !== expected) throw new PmuxSequenceError(expected, sequence);
    cursor = sequence;
  }
  const actualNextSequence = requireSafeSequence(batch.next_sequence, "batch.next_sequence");
  const expectedNext = nextSequence(cursor, "batch.next_sequence");
  if (actualNextSequence !== expectedNext) {
    throw new PmuxSequenceError(expectedNext, actualNextSequence);
  }
}

function throwIfAborted(signal?: AbortSignal): void {
  if (signal?.aborted) {
    throw new PmuxAbortError("operation aborted", { cause: signal.reason });
  }
}

function abortableDelay(milliseconds: number, signal?: AbortSignal): Promise<void> {
  throwIfAborted(signal);
  return new Promise((resolve, reject) => {
    const cancelTimer = scheduleLongTimeout(() => {
      signal?.removeEventListener("abort", onAbort);
      resolve();
    }, milliseconds);
    const onAbort = (): void => {
      cancelTimer();
      signal?.removeEventListener("abort", onAbort);
      reject(new PmuxAbortError("operation aborted", { cause: signal?.reason }));
    };
    signal?.addEventListener("abort", onAbort, { once: true });
  });
}
