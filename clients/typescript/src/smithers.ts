import { createHash } from "node:crypto";

import {
  PmuxAbortError,
  PmuxClient,
  PmuxClientError,
  type EventStreamItem,
  type RequestOptions,
} from "./client.js";
import type {
  ErrorBody,
  EventEnvelope,
  ReplayGap,
  SessionHandle,
  SessionGenerationId,
  SessionId,
  StartSessionRequest,
  TurnId,
  TurnLeasePolicy,
  TurnResult,
} from "./protocol.js";

/** Stable namespace reserved for Smithers attempt identifiers. */
export const SMITHERS_TURN_NAMESPACE = "7ec46f2d-5f29-5ebc-9ac1-925b0a76f76d";

/** RFC 4122 UUIDv5, implemented with Node built-ins to keep the client dependency-free. */
export function turnIdForAttempt(
  durableTaskAttemptId: string,
  namespace = SMITHERS_TURN_NAMESPACE,
): TurnId {
  if (!durableTaskAttemptId) throw new TypeError("durableTaskAttemptId must not be empty");
  const namespaceBytes = uuidToBytes(namespace);
  const digest = createHash("sha1")
    .update(namespaceBytes)
    .update(Buffer.from(durableTaskAttemptId, "utf8"))
    .digest()
    .subarray(0, 16);
  digest[6] = (digest[6] & 0x0f) | 0x50;
  digest[8] = (digest[8] & 0x3f) | 0x80;
  return bytesToUuid(digest);
}

function uuidToBytes(uuid: string): Buffer {
  const compact = uuid.replaceAll("-", "");
  if (!/^[0-9a-fA-F]{32}$/.test(compact)) throw new TypeError("namespace must be a UUID");
  return Buffer.from(compact, "hex");
}

function bytesToUuid(bytes: Buffer): string {
  const hex = bytes.toString("hex");
  return [hex.slice(0, 8), hex.slice(8, 12), hex.slice(12, 16), hex.slice(16, 20), hex.slice(20)]
    .join("-");
}

export interface PmuxClaudeTurnInput {
  sessionId: SessionId;
  generationId: SessionGenerationId;
  durableTaskAttemptId: string;
  prompt: string;
  deadlineUnixMs?: number;
  lease?: TurnLeasePolicy;
  /** Override only when Smithers has persisted a later cursor for this same turn. */
  afterSequence?: number;
  signal?: AbortSignal;
  onEvent?: (event: EventEnvelope) => void | Promise<void>;
  onReplayGap?: (gap: ReplayGap) => void | Promise<void>;
}

export class PmuxReplayGapError extends PmuxClientError {
  constructor(readonly gap: ReplayGap) {
    super(
      `event history was lost after ${gap.requested_after}; recovery snapshot is at ${gap.snapshot.last_sequence}`,
    );
  }
}

export class PmuxTurnFailedError extends PmuxClientError {
  constructor(readonly body: ErrorBody) {
    super(`pmux turn failed (${body.code}): ${body.message}`);
  }
}

export class PmuxTurnCancelledError extends PmuxClientError {
  constructor(readonly event: EventEnvelope) {
    super(`pmux turn ${event.turn_id ?? "unknown"} was cancelled`);
  }
}

/**
 * Thin Smithers transport: no subprocess, CLI flags, or claude-p compatibility layer.
 * Smithers supplies native launch data and persists the returned Claude session UUID.
 */
export class PmuxClaudeAgentTransport {
  constructor(readonly client: PmuxClient) {}

  /** Passes settings, hook documents, environment, and launch policy through unchanged. */
  startSession(request: StartSessionRequest, options?: RequestOptions): Promise<SessionHandle> {
    return this.client.startSession(request, options);
  }

  turnIdForAttempt(durableTaskAttemptId: string): TurnId {
    return turnIdForAttempt(durableTaskAttemptId);
  }

  async runTurn(input: PmuxClaudeTurnInput): Promise<TurnResult> {
    const turnId = turnIdForAttempt(input.durableTaskAttemptId);
    let submitted = false;
    try {
      submitted = true;
      const accepted = await this.client.runTurn(
        input.sessionId,
        input.generationId,
        {
          turn_id: turnId,
          prompt: input.prompt,
          deadline_unix_ms: input.deadlineUnixMs,
          lease: input.lease,
        },
        { signal: input.signal },
      );

      const serverCursor = Math.max(0, accepted.next_sequence - 1);
      const afterSequence = Math.max(serverCursor, input.afterSequence ?? 0);
      const subscription = this.client.events(input.sessionId, input.generationId, {
        afterSequence,
        signal: input.signal,
      });

      for await (const item of subscription) {
        if (item.kind === "replay_gap") {
          await input.onReplayGap?.(item.gap);
          throw new PmuxReplayGapError(item.gap);
        }
        await input.onEvent?.(item.event);
        const result = terminalResultForTurn(item, turnId);
        if (result) return result;
      }
      throw new PmuxClientError(`event stream ended before turn ${turnId} completed`);
    } catch (error) {
      if (input.signal?.aborted) {
        if (submitted) {
          await this.bestEffortCancel(input.sessionId, input.generationId, turnId);
        }
        if (error instanceof PmuxAbortError) throw error;
        throw new PmuxAbortError("Smithers turn aborted", { cause: error });
      }
      throw error;
    }
  }

  private async bestEffortCancel(
    sessionId: SessionId,
    generationId: SessionGenerationId,
    turnId: TurnId,
  ): Promise<void> {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 5_000);
    try {
      await this.client.cancelTurn(sessionId, generationId, turnId, {
        signal: controller.signal,
      });
    } catch {
      // Preserve the original abort; Smithers can inspect/reconcile the session afterward.
    } finally {
      clearTimeout(timer);
    }
  }
}

function terminalResultForTurn(item: EventStreamItem, turnId: TurnId): TurnResult | undefined {
  if (item.kind !== "event") return undefined;
  const event = item.event;
  if (event.turn_id === undefined || event.turn_id.toLowerCase() !== turnId.toLowerCase()) {
    return undefined;
  }
  switch (event.event.type) {
    case "turn_completed":
      return event.event.data.turn_id.toLowerCase() === turnId.toLowerCase()
        ? event.event.data
        : undefined;
    case "turn_failed":
      throw new PmuxTurnFailedError(event.event.data);
    case "turn_cancelled":
      throw new PmuxTurnCancelledError(event);
    default:
      return undefined;
  }
}
