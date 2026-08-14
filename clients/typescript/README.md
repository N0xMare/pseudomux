# pmux-client

Native protocol-v1 client for a caller-owned pmux daemon socket. It targets Node.js 18+ and uses
only Node built-ins at runtime. It does not discover or start a daemon, invoke Claude, translate
CLI flags, or depend on the `claude-p` compatibility facade. The socket path must be absolute.

## Native client

```ts
import { randomUUID } from "node:crypto";
import { PmuxClient } from "pmux-client";

const client = new PmuxClient("/run/user/1000/pmux/pmuxd.sock");
const session = await client.startSession({
  identity: { mode: "new" },
  cwd: "/work/project",
  claude: {
    executable: "/usr/local/bin/claude",
    model: "sonnet",
    settings: [
      // Existing settings and hooks remain data for pmuxd to compose safely.
      { source: "inline", document: { hooks: { PostToolUse: [] } } },
    ],
  },
  auth_policy: "subscription",
  lifecycle: { mode: "transcript" },
  retention: { mode: "persistent", idle_ttl_ms: 1_800_000 },
});

const accepted = await client.runTurn(session.session_id, session.generation_id, {
  turn_id: randomUUID(),
  prompt: "Review this repository",
});

for await (const item of client.events(session.session_id, session.generation_id, {
  afterSequence: accepted.next_sequence - 1,
})) {
  if (item.kind === "replay_gap") {
    console.warn("resynchronize from", item.gap.snapshot);
    continue;
  }
  if (item.event.event.type === "turn_completed") {
    console.log(item.event.event.data.text);
    break;
  }
}
```

Every request accepts `{ signal }`. Event subscriptions retry transport failures from their last
validated `after_sequence`; protocol, server, and sequence errors remain visible to the caller.
Known v1 response and event discriminants and their required fields are validated while additive
minor-version object fields remain accepted.
Protocol integers are nonnegative safe integers; integer-valued numbers inside opaque JSON use the
signed safe-integer range. Outbound values outside those domains and nonfinite numbers fail before
I/O; inbound values are rejected before they are exposed to the caller.
Session handles, snapshots, and turn results include an exact `compatibility` report with tested
status and the selected transcript drain. Callers should surface `tested: false` rather than
treating it as release evidence.
Persist `session_id` and `generation_id` together. A resumed Claude UUID receives a new generation
fence; delayed work carrying an older fence fails instead of targeting the replacement process.

## Smithers transport

```ts
import { PmuxClaudeAgentTransport, PmuxClient } from "pmux-client";

const transport = new PmuxClaudeAgentTransport(
  new PmuxClient(process.env.PMUX_SOCKET!),
);

const result = await transport.runTurn({
  sessionId: persistedClaudeSessionId,
  generationId: persistedPmuxGenerationId,
  durableTaskAttemptId: task.attemptId,
  prompt: task.prompt,
  signal: task.signal,
  onEvent(event) {
    // Map logical-message/tool/rate-limit events to Smithers lifecycle events.
  },
});
```

`durableTaskAttemptId` maps deterministically to an RFC 4122 UUIDv5 `TurnId`, so retrying the same
Smithers attempt exercises pmux idempotency instead of starting duplicate work. Smithers must
persist the complete pmux session handle, including `generationId`. Aborting the task
requests native pmux cancellation. A replay gap is surfaced as `PmuxReplayGapError` with the
recovery snapshot; Smithers should persist event cursors and treat this as an explicit
reconciliation boundary.
