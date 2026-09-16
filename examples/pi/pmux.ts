/**
 * pmux provider for Pi.
 *
 * /model lists the recommended warm-set families (not the full pool table):
 *   claude-sonnet-5-{low,medium,high,xhigh,max}
 *   claude-opus-5-{low,medium,high,xhigh,max}
 *   claude-fable-5-1-{low,medium,high,xhigh,max}
 *
 * Effort is in the model id. The Messages facade splits it before the pool.
 * Requires pmuxd with --messages-bind 127.0.0.1:8765.
 *
 * Cell lifetime contract:
 *   - Every request carries x-pmux-conversation = Pi session id
 *     (or a process UUID for `pi -p --no-session`).
 *   - Optional PMUX_ACCOUNT sets x-pmux-account (a --pool-account name).
 *   - session_start that switches sessions releases the previous id first.
 *   - session_shutdown POSTs /v1/conversations/<id>/release so the cell
 *     /clear's and returns to the pool instead of waiting for idle TTL.
 *     keepalive=true so the POST can outlive process teardown.
 *   - Compaction / rewind / class change is a prefix break; pmux repriming.
 *   - pi-subagents 0.68+ foreground children disable ambient extensions, so
 *     this file registers itself as a required child extension (conversation
 *     pin / account header would otherwise be absent and Messages 400s).
 */
import { homedir } from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { PmuxMessages, setAccountHeader, setConversationHeader } from "pmux-client";

const SELF = fileURLToPath(import.meta.url);

const BASE_URL = process.env.PMUX_MESSAGES_URL ?? "http://127.0.0.1:8765";
const ACCOUNT = process.env.PMUX_ACCOUNT?.trim();
const messages = new PmuxMessages({ baseUrl: BASE_URL, apiKey: "pmux" });

const FAMILIES = [
	{ id: "claude-sonnet-5", name: "Sonnet 5" },
	{ id: "claude-opus-5", name: "Opus 5" },
	{ id: "claude-fable-5-1", name: "Fable 5.1" },
] as const;

const CONTEXT_WINDOW = 200_000;
const MAX_OUTPUT = 128_000;
const EFFORTS = ["low", "medium", "high", "xhigh", "max"] as const;

type ChildExtensionRegistration = { dispose(): void };

async function registerOnChildren(sessionId: string): Promise<ChildExtensionRegistration> {
	const specs = [
		"pi-subagents/required-child-extensions",
		pathToFileURL(
			path.join(
				homedir(),
				".pi/agent/npm/node_modules/pi-subagents/src/api/required-child-extensions.ts",
			),
		).href,
	];
	for (const spec of specs) {
		try {
			const mod = (await import(spec)) as {
				registerRequiredChildExtensions?: (input: {
					sessionId: string;
					extensions: { id: string; path: string }[];
				}) => ChildExtensionRegistration;
			};
			if (typeof mod.registerRequiredChildExtensions !== "function") continue;
			return mod.registerRequiredChildExtensions({
				sessionId,
				extensions: [{ id: "pmux", path: SELF }],
			});
		} catch {
			continue;
		}
	}
	return { dispose() {} };
}

export default function (pi: ExtensionAPI) {
	let conversationId = crypto.randomUUID();
	let childExtensions: ChildExtensionRegistration | undefined;

	const release = (id: string) =>
		messages.release(id, { keepalive: true }).catch(() => {
			/* idle TTL is the backstop */
		});

	const bindSession = (next: string) => {
		if (next !== conversationId) {
			void release(conversationId);
			childExtensions?.dispose();
			childExtensions = undefined;
			conversationId = next;
		}
		void registerOnChildren(conversationId)
			.then((registration) => {
				childExtensions = registration;
			})
			.catch(() => {
				/* parent still works; children without the pin 400 */
			});
	};

	pi.on("session_start", (_event, ctx) => {
		const next = ctx.sessionManager.getSessionId();
		if (next) bindSession(next);
	});

	pi.on("before_provider_headers", (event) => {
		setConversationHeader(event.headers, conversationId);
		if (ACCOUNT) {
			setAccountHeader(event.headers, ACCOUNT);
		}
	});

	pi.on("session_shutdown", async () => {
		const id = conversationId;
		childExtensions?.dispose();
		childExtensions = undefined;
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
