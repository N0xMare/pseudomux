/** Messages listener helpers. Harnesses (Pi) still POST /v1/messages themselves. */

import { request as httpRequest } from "node:http";

export const PMUX_CONVERSATION_HEADER = "x-pmux-conversation";

export const PMUX_CONVERSATION_HEADER_ALIASES = [
  "x-pmux-conversation",
  "x-session-id",
  "x-session-affinity",
] as const;

/** Exact host names; other 127/8 addresses such as `127.0.0.2` are refused. */
const LOOPBACK_HOSTS = new Set(["127.0.0.1", "localhost", "::1"]);
const DEFAULT_TIMEOUT_MS = 10_000;
const MAX_RESPONSE_BYTES = 2 * 1024 * 1024;

function pathSafeConversationId(conversationId: string): string {
  const id = conversationId.trim();
  if (!id) {
    throw new TypeError("conversation id must not be empty");
  }
  if (/[/?#]/.test(id) || /\s/.test(id)) {
    throw new TypeError("conversation id is not path-safe");
  }
  return id;
}

export function setConversationHeader(
  headers: Record<string, string>,
  conversationId: string,
): void {
  headers[PMUX_CONVERSATION_HEADER] = pathSafeConversationId(conversationId);
}

export interface PmuxMessagesOptions {
  baseUrl: string;
  apiKey?: string;
  timeoutMs?: number;
}

function parseMessagesBaseUrl(baseUrl: string): {
  origin: string;
  host: string;
  port: number;
} {
  const raw = baseUrl.trim().replace(/\/+$/, "");
  if (!raw) {
    throw new TypeError("baseUrl must not be empty");
  }
  if (!raw.startsWith("http://")) {
    throw new TypeError("Messages URL must be http://HOST:PORT");
  }
  let parsed: URL;
  try {
    parsed = new URL(raw);
  } catch {
    throw new TypeError("Messages URL must be http://HOST:PORT");
  }
  if (
    parsed.protocol !== "http:" ||
    parsed.username !== "" ||
    parsed.password !== "" ||
    parsed.port === ""
  ) {
    throw new TypeError("Messages URL must be http://HOST:PORT");
  }
  const host = parsed.hostname.replace(/^\[|\]$/g, "").toLowerCase();
  if (!LOOPBACK_HOSTS.has(host)) {
    throw new TypeError("Messages client is loopback-only");
  }
  const port = Number(parsed.port);
  const origin = host === "::1" ? `http://[::1]:${port}` : `http://${host}:${port}`;
  return { origin, host, port };
}

function abortError(signal?: AbortSignal): Error {
  if (signal?.reason instanceof Error) {
    return signal.reason;
  }
  return new Error("This operation was aborted");
}

export class PmuxMessages {
  readonly baseUrl: string;
  readonly apiKey: string;
  private readonly host: string;
  private readonly port: number;
  private readonly timeoutMs: number;

  constructor(options: PmuxMessagesOptions) {
    const parsed = parseMessagesBaseUrl(options.baseUrl);
    this.baseUrl = parsed.origin;
    this.host = parsed.host;
    this.port = parsed.port;
    this.apiKey = (options.apiKey ?? "pmux").trim() || "pmux";
    if (/[\r\n\0]/.test(this.apiKey)) {
      throw new TypeError("api_key contains CR, LF, or NUL");
    }
    const timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS;
    if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
      throw new TypeError("timeoutMs must be greater than zero");
    }
    this.timeoutMs = timeoutMs;
  }

  async release(
    conversationId: string,
    init?: { keepalive?: boolean; signal?: AbortSignal },
  ): Promise<void> {
    const id = pathSafeConversationId(conversationId);
    await this.exchange("POST", `/v1/conversations/${id}/release`, init?.signal);
  }

  async models(init?: { signal?: AbortSignal }): Promise<unknown> {
    return this.getJson("/v1/models", init?.signal);
  }

  async capabilities(init?: { signal?: AbortSignal }): Promise<unknown> {
    return this.getJson("/v1/capabilities", init?.signal);
  }

  private authHeaders(): Record<string, string> {
    return {
      "x-api-key": this.apiKey,
      authorization: `Bearer ${this.apiKey}`,
      connection: "close",
    };
  }

  private async getJson(path: string, signal?: AbortSignal): Promise<unknown> {
    const { body } = await this.exchange("GET", path, signal);
    return JSON.parse(body) as unknown;
  }

  private exchange(
    method: string,
    path: string,
    signal?: AbortSignal,
  ): Promise<{ status: number; body: string }> {
    return new Promise((resolve, reject) => {
      if (signal?.aborted) {
        reject(abortError(signal));
        return;
      }

      let settled = false;
      const chunks: Buffer[] = [];
      let received = 0;
      let req: ReturnType<typeof httpRequest> | undefined;

      const settle = (error?: Error, value?: { status: number; body: string }) => {
        if (settled) {
          return;
        }
        settled = true;
        clearTimeout(timer);
        signal?.removeEventListener("abort", onAbort);
        if (error) {
          reject(error);
        } else {
          resolve(value as { status: number; body: string });
        }
      };

      const onAbort = () => {
        req?.destroy();
        settle(abortError(signal));
      };

      const timer = setTimeout(() => {
        req?.destroy();
        settle(new Error("Messages HTTP timed out"));
      }, this.timeoutMs);

      req = httpRequest(
        {
          hostname: this.host,
          port: this.port,
          method,
          path,
          headers: this.authHeaders(),
          family: this.host === "::1" ? 6 : undefined,
        },
        (res) => {
          res.on("data", (chunk: Buffer | string) => {
            const buf = typeof chunk === "string" ? Buffer.from(chunk) : chunk;
            received += buf.length;
            if (received > MAX_RESPONSE_BYTES) {
              req?.destroy();
              res.destroy();
              settle(
                new Error(
                  `Messages HTTP response exceeds ${MAX_RESPONSE_BYTES} bytes`,
                ),
              );
              return;
            }
            chunks.push(buf);
          });
          res.on("end", () => {
            if (settled) {
              return;
            }
            const body = Buffer.concat(chunks).toString("utf8");
            const status = res.statusCode ?? 0;
            if (status !== 200) {
              const label = method === "POST" ? "release" : path;
              settle(new Error(`pmux ${label} ${status}: ${body}`));
              return;
            }
            settle(undefined, { status, body });
          });
          res.on("error", (error) => {
            settle(error);
          });
        },
      );

      signal?.addEventListener("abort", onAbort, { once: true });
      req.on("error", (error) => {
        settle(error);
      });
      req.end();
    });
  }
}
