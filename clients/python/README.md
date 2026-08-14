# pmux-client

Dependency-free Python 3.11+ client for the native pmux protocol-v1 Unix socket. The caller must
provide the exact absolute socket path. The package performs no daemon discovery or startup and has no
HTTP, subprocess, Claude CLI, or `claude-p` path.

```python
from uuid import uuid4

from pmux_client import PmuxClient

client = PmuxClient("/run/user/1000/pmux/pmuxd.sock")
session = client.start_session(
    {
        "identity": {"mode": "new"},
        "cwd": "/work/project",
        "claude": {
            "executable": "/usr/local/bin/claude",
            # Existing settings and hooks remain structured data for pmuxd.
            "settings": [
                {"source": "inline", "document": {"hooks": {"PostToolUse": []}}}
            ],
        },
        "auth_policy": "subscription",
        "lifecycle": {"mode": "transcript"},
        "retention": {"mode": "persistent", "idle_ttl_ms": 1_800_000},
    }
)

accepted = client.run_turn(
    session["session_id"],
    session["generation_id"],
    {"turn_id": str(uuid4()), "prompt": "Review this repository"},
)

events = client.events(
    session["session_id"],
    session["generation_id"],
    after_sequence=accepted["next_sequence"] - 1,
)
for item in events:
    if item.kind == "replay_gap":
        print("reconcile from", item.gap["snapshot"])
        continue
    if item.event["event"]["type"] == "turn_completed":
        print(item.event["event"]["data"]["text"])
        events.close()
        break
```

Each request uses a fresh connection. Event subscriptions reconnect transport failures from their
last delivered `after_sequence`, strictly validate sequence continuity, and return replay loss as
`ReplayGapItem` with the recovery snapshot.

Known v1 response and event discriminants and their required fields are validated while additive
minor-version object fields remain accepted.

Protocol integers are nonnegative safe integers; integer-valued numbers inside opaque JSON use the
signed safe-integer range. Outbound values outside those domains and nonfinite numbers fail before
I/O; inbound values are rejected before they are exposed to the caller.

Session handles, snapshots, and turn results include an exact `compatibility` report with tested
status and the selected transcript drain. Callers should surface `tested: false` rather than
treating it as release evidence.
Persist `session_id` and `generation_id` as one handle. Every resumed process gets a fresh
generation fence, so a delayed request cannot target a replacement with the same Claude UUID.

`turn_id_for_attempt(durable_attempt_id)` uses the same deterministic UUIDv5 namespace as the
TypeScript Smithers transport, allowing durable orchestrators to reuse pmux idempotency keys.
