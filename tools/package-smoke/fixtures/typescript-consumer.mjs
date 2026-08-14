import assert from "node:assert/strict";

import {
  MAX_NATIVE_FRAME_BYTES,
  MAX_SAFE_JSON_INTEGER,
  PROTOCOL_VERSION,
  PmuxClaudeAgentTransport,
  PmuxClient,
  turnIdForAttempt,
} from "pmux-client";

assert.equal(PROTOCOL_VERSION, 1);
assert.equal(MAX_NATIVE_FRAME_BYTES, 8 * 1024 * 1024);
assert.equal(MAX_SAFE_JSON_INTEGER, Number.MAX_SAFE_INTEGER);

const client = new PmuxClient("/tmp/pmux-package-smoke.sock");
assert.ok(client instanceof PmuxClient);
const transport = new PmuxClaudeAgentTransport(client);
assert.ok(transport instanceof PmuxClaudeAgentTransport);

const first = turnIdForAttempt("package-artifact-smoke");
const second = transport.turnIdForAttempt("package-artifact-smoke");
assert.equal(first, second);
assert.match(first, /^[0-9a-f]{8}-[0-9a-f]{4}-5[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
assert.throws(() => new PmuxClient("relative.sock"), TypeError);

process.stdout.write(
  `${JSON.stringify({
    api: "native_pmux_v1",
    client_constructed_without_io: true,
    protocol_version: PROTOCOL_VERSION,
    smithers_transport_constructed_without_io: true,
    turn_id: first,
  })}\n`,
);
