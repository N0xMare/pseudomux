/**
 * pmux provider for Pi.
 *
 * /model lists the recommended warm-set families (not the full pool table):
 *   claude-sonnet-5-{low,medium,high,xhigh,max}
 *   claude-opus-5-{low,medium,high,xhigh,max}
 *   claude-fable-5-{low,medium,high,xhigh,max}
 *
 * Effort is in the model id. The Messages facade splits it before the pool.
 * Requires pmuxd with --messages-bind 127.0.0.1:8765.
 *
 * Cell lifetime contract:
 *   - Every request carries x-pmux-conversation = Pi session id
 *     (or a process UUID for `pi -p --no-session`).
 *   - session_start that switches sessions releases the previous id first.
 *   - session_shutdown POSTs /v1/conversations/<id>/release so the cell
 *     /clear's and returns to the pool instead of waiting for idle TTL.
 *     keepalive=true so the POST can outlive process teardown.
 *   - Compaction / rewind / class change is a prefix break; pmux repriming.
 */
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { PmuxMessages, setConversationHeader } from "pmux-client";

const BASE_URL = process.env.PMUX_MESSAGES_URL ?? "http://127.0.0.1:8765";
const messages = new PmuxMessages({ baseUrl: BASE_URL, apiKey: "pmux" });

const FAMILIES = [
	{ id: "claude-sonnet-5", name: "Sonnet 5" },
	{ id: "claude-opus-5", name: "Opus 5" },
	{ id: "claude-fable-5", name: "Fable 5" },
] as const;

const CONTEXT_WINDOW = 200_000;
const MAX_OUTPUT = 128_000;
const EFFORTS = ["low", "medium", "high", "xhigh", "max"] as const;

export default function (pi: ExtensionAPI) {
	let conversationId = crypto.randomUUID();

	const release = (id: string) =>
		messages.release(id, { keepalive: true }).catch(() => {
			/* idle TTL is the backstop */
		});

	pi.on("session_start", (_event, ctx) => {
		const next = ctx.sessionManager.getSessionId();
		if (next && next !== conversationId) {
			void release(conversationId);
			conversationId = next;
		} else if (next) {
			conversationId = next;
		}
	});

	pi.on("before_provider_headers", (event) => {
		setConversationHeader(event.headers, conversationId);
	});

	pi.on("session_shutdown", async () => {
		const id = conversationId;
		await release(id);
	});

	pi.registerProvider("pmux", {
		name: "pmux",
		baseUrl: BASE_URL,
		apiKey: "pmux",
		api: "anthropic-messages",
		compat: {
			supportsEagerToolInputStreaming: false,
			supportsLongCacheRetention: false,
			supportsCacheControlOnTools: false,
			supportsTemperature: false,
		},
		models: FAMILIES.flatMap((family) =>
			EFFORTS.map((effort) => ({
				id: `${family.id}-${effort}`,
				name: `${family.name} · ${effort}`,
				reasoning: false,
				input: ["text"] as ["text"],
				contextWindow: CONTEXT_WINDOW,
				maxTokens: MAX_OUTPUT,
				cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
			})),
		),
	});
}
