/**
 * Thin TypeScript client for the pseudomux HTTP API.
 *
 * Usage:
 * ```ts
 * import { PmuxClient } from "pmux-client";
 *
 * const client = new PmuxClient("http://localhost:8765");
 * // If pmuxd uses HTTP token auth:
 * // const client = new PmuxClient("http://localhost:8765", "dev-secret");
 *
 * // Mode 1: one-shot
 * const result = await client.run({
 *   text: "Review this repo",
 *   cwd: "/path/to/repo",
 *   args: ["--model", "opus", "--permission-mode", "bypassPermissions"],
 * });
 * console.log(result.text);
 *
 * // Mode 2: persistent orchestrator
 * const sid = await client.startSession({
 *   cwd: "/path/to/repo",
 *   args: ["--model", "opus", "--permission-mode", "bypassPermissions"],
 * });
 * await client.waitReady(sid);
 * const r1 = await client.prompt(sid, "Read main.rs and summarize");
 * const r2 = await client.prompt(sid, "Propose 3 improvements");
 * await client.stopSession(sid);
 * ```
 */

// ── Types ─────────────────────────────────────────────────────────────────

export interface ToolCall {
  name: string | null;
  duration_ms: number | null;
}

export interface PromptResult {
  session_id: string;
  text: string;
  duration_ms: number;
  state: string;
  tools: ToolCall[];
}

export interface RunOptions {
  text: string;
  agent?: string;
  cwd?: string;
  name?: string;
  args?: string[];
  timeoutSecs?: number;
  keepAlive?: boolean;
}

export interface StartSessionOptions {
  agent?: string;
  cwd?: string;
  name?: string;
  args?: string[];
}

export type PmuxErrorCode =
  | "timeout"
  | "agent_exited"
  | "auth_required"
  | "confirmation_required"
  | "unauthorized"
  | "transport"
  | (string & {});

// ── Errors ────────────────────────────────────────────────────────────────

export class PmuxError extends Error {
  readonly code: PmuxErrorCode;
  readonly sessionId: string | null;

  constructor(code: PmuxErrorCode, message: string, sessionId: string | null = null) {
    super(message);
    this.name = this.constructor.name;
    this.code = code;
    this.sessionId = sessionId;
  }
}

export class TimeoutError extends PmuxError {
  constructor(message: string, sessionId: string | null = null) {
    super("timeout", message, sessionId);
  }
}
export class AgentExitedError extends PmuxError {
  constructor(message: string, sessionId: string | null = null) {
    super("agent_exited", message, sessionId);
  }
}
export class AuthRequiredError extends PmuxError {
  constructor(message: string, sessionId: string | null = null) {
    super("auth_required", message, sessionId);
  }
}
export class ConfirmationRequiredError extends PmuxError {
  constructor(message: string, sessionId: string | null = null) {
    super("confirmation_required", message, sessionId);
  }
}
export class UnauthorizedError extends PmuxError {
  constructor(message: string, sessionId: string | null = null) {
    super("unauthorized", message, sessionId);
  }
}
export class TransportError extends PmuxError {
  constructor(
    message: string,
    sessionId: string | null = null,
    code: PmuxErrorCode = "transport",
  ) {
    super(code, message, sessionId);
  }
}

type ErrorBody = {
  error?: string | { code?: string; message?: string };
  message?: string;
  session_id?: string;
};

function raiseFromBody(body: ErrorBody): never {
  const error = body.error ?? "transport";
  const code = (
    typeof error === "object" ? (error.code ?? "transport") : error
  ) as PmuxErrorCode;
  const msg =
    (typeof error === "object" ? error.message : undefined) ??
    body.message ??
    "unknown pmux error";
  const sid = body.session_id ?? null;
  switch (code) {
    case "timeout":
      throw new TimeoutError(msg, sid);
    case "agent_exited":
      throw new AgentExitedError(msg, sid);
    case "auth_required":
      throw new AuthRequiredError(msg, sid);
    case "confirmation_required":
      throw new ConfirmationRequiredError(msg, sid);
    case "unauthorized":
      throw new UnauthorizedError(msg, sid);
    default:
      throw new TransportError(msg, sid, code);
  }
}

// ── Client ────────────────────────────────────────────────────────────────

export class PmuxClient {
  private readonly baseUrl: string;
  private readonly token: string | null;

  constructor(baseUrl: string = "http://localhost:8765", token: string | null = null) {
    this.baseUrl = baseUrl.replace(/\/+$/, "");
    this.token = token;
  }

  private async request(method: string, path: string, body?: unknown): Promise<any> {
    const url = `${this.baseUrl}${path}`;
    let resp: Response;
    const headers: Record<string, string> = { "Content-Type": "application/json" };
    if (this.token) {
      headers.Authorization = `Bearer ${this.token}`;
    }
    try {
      resp = await fetch(url, {
        method,
        headers,
        body: body === undefined ? undefined : JSON.stringify(body),
      });
    } catch (e) {
      throw new TransportError(`connection failed: ${e}`);
    }
    const text = await resp.text();
    let parsed: any = null;
    if (text) {
      try {
        parsed = JSON.parse(text);
      } catch {
        throw new TransportError(`invalid JSON response: ${text.slice(0, 200)}`);
      }
    }
    if (!resp.ok) {
      if (parsed && typeof parsed === "object" && "error" in parsed) {
        raiseFromBody(parsed);
      }
      throw new TransportError(`HTTP ${resp.status}: ${text}`);
    }
    return parsed;
  }

  /** Daemon health check. */
  async health(): Promise<{ status: string }> {
    return await this.request("GET", "/health");
  }

  /**
   * One-shot: start a session, send a prompt, return the result. Unless
   * `keepAlive` is true, the session is terminated automatically.
   */
  async run(opts: RunOptions): Promise<PromptResult> {
    const body = {
      text: opts.text,
      timeout_secs: opts.timeoutSecs ?? 120,
      keep_alive: opts.keepAlive ?? false,
      session: {
        agent: opts.agent ?? "claude-code",
        args: opts.args ?? [],
        env: [],
        cwd: opts.cwd ?? null,
        name: opts.name ?? null,
      },
    };
    return (await this.request("POST", "/run", body)) as PromptResult;
  }

  /**
   * Create a persistent session and return its UUID. Use `prompt()` for
   * follow-up turns and `stopSession()` when done.
   */
  async startSession(opts: StartSessionOptions = {}): Promise<string> {
    const body = {
      agent: opts.agent ?? "claude-code",
      args: opts.args ?? [],
      env: [],
      cwd: opts.cwd ?? null,
      name: opts.name ?? null,
    };
    const out = await this.request("POST", "/sessions", body);
    return out.session as string;
  }

  /**
   * Block until the session's agent reaches Ready state. Polls every 500ms.
   * Throws `TimeoutError` if boot takes longer than `timeoutSecs`.
   */
  async waitReady(sessionId: string, timeoutSecs = 30): Promise<void> {
    const deadline = Date.now() + timeoutSecs * 1000;
    // eslint-disable-next-line no-constant-condition
    while (true) {
      const out = await this.request("GET", `/sessions/${sessionId}/state`);
      if (out.state === "Ready") return;
      if (Date.now() >= deadline) {
        throw new TimeoutError(
          `session ${sessionId} did not reach Ready in ${timeoutSecs}s`,
          sessionId,
        );
      }
      await new Promise((r) => setTimeout(r, 500));
    }
  }

  /**
   * Send a prompt on an existing session, block until TurnComplete, and
   * return the result. The session stays alive.
   */
  async prompt(
    sessionId: string,
    text: string,
    opts: { timeoutSecs?: number } = {},
  ): Promise<PromptResult> {
    const body = { text, timeout_secs: opts.timeoutSecs ?? 120 };
    return (await this.request(
      "POST",
      `/sessions/${sessionId}/prompt-sync`,
      body,
    )) as PromptResult;
  }

  /** Terminate a session. */
  async stopSession(sessionId: string): Promise<void> {
    await this.request("DELETE", `/sessions/${sessionId}`);
  }

  /** List all active sessions. */
  async listSessions(): Promise<unknown[]> {
    const out = await this.request("GET", "/sessions");
    return out.sessions ?? [];
  }
}
