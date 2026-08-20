# pmux-client

Python client for the pmux token engine. Python 3.11+, no extra runtime
dependencies.

- **Harnesses** (Pi): `PmuxMessages` + `set_conversation_header`. You still POST
  Anthropic Messages; this package only pins, releases, and reads the catalog.
- **One-shot**: `PmuxClient.run_stateless` on the owner-only Unix socket.

Interactive session methods (`start_session`, `run_turn`) remain in the
package for protocol completeness and are refused by current daemons. Do
not build new callers on them. Session and agent DTOs remain on
`pmux_client.protocol` for goldens.

```python
from pmux_client import PmuxMessages, set_conversation_header

messages = PmuxMessages("http://127.0.0.1:8765")
set_conversation_header(headers, session_id)
messages.release(session_id)
```

`PmuxMessages` is loopback-only (`http://127.0.0.1`, `http://localhost`,
`http://[::1]`).

```python
from pmux_client import PmuxClient

client = PmuxClient("/absolute/path/pmux.sock")
result = client.run_stateless({
    "model": "claude-sonnet-5",
    "effort": "low",
    "prompt": "Name the three largest moons of Saturn.",
})
```

Session methods remain compiled and are refused by current daemons. The
caller still supplies an exact absolute socket path; the package performs no
daemon discovery or startup.
