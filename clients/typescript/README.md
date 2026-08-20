# pmux-client

TypeScript client for the pmux token engine. Node.js 18+, no extra runtime
dependencies.

- **Harnesses** (Pi): `PmuxMessages` + `setConversationHeader`. You still POST
  Anthropic Messages; this package only pins, releases, and reads the catalog.
- **One-shot**: `PmuxClient.runStateless` on the owner-only Unix socket.

Interactive session methods (`startSession`, `runTurn`) remain in the package
for protocol completeness and are refused by current daemons. Do not build
new callers on them. Session and agent DTOs for goldens import from
`pmux-client/goldens`.

```ts
import { PmuxMessages, setConversationHeader } from "pmux-client";

const messages = new PmuxMessages({ baseUrl: "http://127.0.0.1:8765" });
setConversationHeader(headers, sessionId);
await messages.release(sessionId);
```

`PmuxMessages` is loopback-only (`http://127.0.0.1`, `http://localhost`,
`http://[::1]`).

```ts
import { PmuxClient } from "pmux-client";

const client = new PmuxClient("/absolute/path/pmux.sock");
const result = await client.runStateless({
  model: "claude-sonnet-5",
  effort: "low",
  prompt: "Name the three largest moons of Saturn.",
});
```

Every request accepts `{ signal }`.
Protocol integers are nonnegative safe integers; integer-valued numbers inside opaque JSON use the
signed safe-integer range. Outbound values outside those domains and nonfinite numbers fail before
I/O; inbound values are rejected before they are exposed to the caller.
