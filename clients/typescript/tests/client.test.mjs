import assert from "node:assert/strict";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { clientModuleUrl } from "./dist-stage.mjs";

const {
  DEFAULT_RUN_ONCE_TIMEOUT_MS,
  MAX_SAFE_JSON_INTEGER,
  PMUX_ERROR_CODES,
  PROTOCOL_VERSION,
  PmuxAbortError,
  PmuxClient,
  PmuxFrameTooLargeError,
  PmuxProtocolError,
  PmuxRequestIdMismatchError,
  PmuxSequenceError,
  PmuxServerError,
  PmuxVersionError,
  RUN_ONCE_RESPONSE_MARGIN_MS,
  requestTimeoutFor,
} = await import((await clientModuleUrl(import.meta.url)).href);

const SESSION_ID = "00000000-0000-4000-8000-000000000022";
const GENERATION_ID = "00000000-0000-4000-8000-000000000044";
const OTHER_ID = "00000000-0000-4000-8000-000000009999";
const CONFORMANCE_MANIFEST = JSON.parse(
  await readFile(new URL("../../../tests/conformance/v1/manifest.json", import.meta.url), "utf8"),
);
const CONFORMANCE_CASES = JSON.parse(
  await readFile(new URL("../../../tests/conformance/v1/cases.json", import.meta.url), "utf8"),
);

async function readFrame(socket) {
  return await new Promise((resolve, reject) => {
    let pending = Buffer.alloc(0);
    const cleanup = () => {
      socket.off("data", onData);
      socket.off("error", onError);
      socket.off("close", onClose);
    };
    const onData = (chunk) => {
      pending = Buffer.concat([pending, chunk]);
      if (pending.length < 4) return;
      const length = pending.readUInt32BE(0);
      if (pending.length < length + 4) return;
      cleanup();
      resolve(pending.subarray(4, length + 4));
    };
    const onError = (error) => {
      cleanup();
      reject(error);
    };
    const onClose = () => {
      cleanup();
      reject(new Error("socket closed before a complete request"));
    };
    socket.on("data", onData);
    socket.once("error", onError);
    socket.once("close", onClose);
  });
}

async function readRequest(socket) {
  return JSON.parse((await readFrame(socket)).toString("utf8"));
}

function writeJson(socket, value) {
  const body = Buffer.from(JSON.stringify(value));
  const header = Buffer.allocUnsafe(4);
  header.writeUInt32BE(body.length);
  socket.write(header);
  socket.write(body);
}

function writeRawJson(socket, value) {
  const body = Buffer.from(value, "utf8");
  const header = Buffer.allocUnsafe(4);
  header.writeUInt32BE(body.length);
  socket.write(header);
  socket.write(body);
}

function success(request, type, data, version = PROTOCOL_VERSION) {
  return {
    version,
    request_id: request.request_id,
    result: { type, data },
  };
}

async function withFakeServer(handler, body) {
  const directory = await mkdtemp(join(tmpdir(), "pmux-ts-client-"));
  const socketPath = join(directory, "pmuxd.sock");
  const active = new Set();
  let connection = 0;
  let handlerError;
  const server = createServer((socket) => {
    const index = connection++;
    const work = Promise.resolve(handler(socket, index)).catch((error) => {
      handlerError ??= error;
      socket.destroy();
    });
    active.add(work);
    work.finally(() => active.delete(work));
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(socketPath, resolve);
  });
  try {
    await body(socketPath);
  } finally {
    await new Promise((resolve) => server.close(resolve));
    await Promise.all(active);
    await rm(directory, { recursive: true, force: true });
  }
  if (handlerError) throw handlerError;
}

function startRequest() {
  return {
    identity: { mode: "new", session_id: SESSION_ID },
    cwd: "/work/project",
    claude: {
      executable: "/usr/local/bin/claude",
      settings: [
        {
          source: "inline",
          document: {
            hooks: {
              PostToolUse: [{ matcher: "*", hooks: [{ type: "command", command: "snapshot" }] }],
            },
          },
        },
      ],
    },
    auth_policy: "subscription",
    lifecycle: { mode: "transcript" },
    retention: { mode: "persistent", idle_ttl_ms: 1_800_000 },
  };
}

function compatibilityReport() {
  return {
    claude_version: "2.1.207",
    os: "macos",
    arch: "aarch64",
    terminal_profile: "transparent",
    input_transport: "sdk",
    tested: true,
    transcript_drain_ms: 750,
  };
}

function snapshot(lastSequence) {
  return {
    session_id: SESSION_ID,
    generation_id: GENERATION_ID,
    transcript_session_id: SESSION_ID,
    cell: "full",
    state: "ready",
    cwd: "/work/project",
    compatibility: compatibilityReport(),
    created_at_ms: 1,
    updated_at_ms: 2,
    resumable: true,
    last_sequence: lastSequence,
  };
}

function heartbeat(sequence) {
  return {
    schema_version: 1,
    session_id: SESSION_ID,
    generation_id: GENERATION_ID,
    sequence,
    timestamp_ms: 100 + sequence,
    event: { type: "heartbeat", data: { session_state: "ready" } },
  };
}

function turnResult(turnId) {
  const usage = {
    input_tokens: 10,
    output_tokens: 2,
    cache_creation_input_tokens: 0,
    cache_read_input_tokens: 0,
  };
  return {
    session_id: SESSION_ID,
    generation_id: GENERATION_ID,
    turn_id: turnId,
    outcome: "completed",
    text: "done",
    final_blocks: [{ kind: "text", text: "done" }],
    usage: { main: usage, sidechain: { ...usage, input_tokens: 0, output_tokens: 0 }, combined: usage },
    timings: { submitted_at_ms: 1, completed_at_ms: 2 },
    claude_version: "2.1.207",
    compatibility: compatibilityReport(),
    completion: {
      authority: "transcript",
      prompt_acknowledged: true,
      terminal_message_observed: true,
      terminal_prompt_observed: true,
      terminal_quiet_observed: true,
      transcript_drained: true,
      lifecycle_hook_observed: false,
    },
    final_sequence: 1,
  };
}

test("client rejects relative socket paths before connecting", () => {
  assert.throws(() => new PmuxClient("relative/pmux.sock"), /absolute/);
  assert.throws(
    () => new PmuxClient("/tmp/pmux.sock", { maxFrameBytes: 8 * 1024 * 1024 + 1 }),
    /maxFrameBytes/,
  );
});

test("outbound JSON rejects non-finite numbers before connecting", async () => {
  const request = startRequest();
  request.claude.settings = [{ source: "inline", document: { invalid: Number.NaN } }];
  await assert.rejects(
    new PmuxClient("/tmp/pmux-conformance-does-not-exist.sock").startSession(request),
    PmuxProtocolError,
  );
});

test("typed start uses explicit UDS and preserves settings/hooks as data", async () => {
  await withFakeServer(
    async (socket) => {
      const request = await readRequest(socket);
      assert.equal(request.version, 1);
      assert.equal(request.method, "start_session");
      assert.deepEqual(request.params.claude.settings, startRequest().claude.settings);
      writeJson(socket, success(request, "session_started", {
        session_id: SESSION_ID,
        generation_id: GENERATION_ID,
        state: "booting",
        compatibility: compatibilityReport(),
        created_at_ms: 1,
        last_sequence: 0,
      }));
    },
    async (socketPath) => {
      const client = new PmuxClient(socketPath);
      assert.equal(client.socketPath, socketPath);
      const session = await client.startSession(startRequest());
      assert.equal(session.session_id, SESSION_ID);
    },
  );
});

test("structured daemon errors remain typed", async () => {
  await withFakeServer(
    async (socket) => {
      const request = await readRequest(socket);
      writeJson(socket, {
        version: 1,
        request_id: request.request_id,
        error: {
          code: "rate_limited",
          message: "quota exhausted",
          retryable: true,
          details: { resets_at_ms: 42 },
        },
      });
    },
    async (socketPath) => {
      await assert.rejects(new PmuxClient(socketPath).ping(), (error) => {
        assert.ok(error instanceof PmuxServerError);
        assert.equal(error.body.code, "rate_limited");
        assert.equal(error.body.details.resets_at_ms, 42);
        return true;
      });
    },
  );
});

test("shared v1 manifest and durable-id vectors match the TypeScript surface", () => {
  assert.equal(CONFORMANCE_MANIFEST.schema_version, 1);
  assert.equal(CONFORMANCE_MANIFEST.protocol_version, PROTOCOL_VERSION);
  assert.deepEqual(CONFORMANCE_MANIFEST.error_codes, [...PMUX_ERROR_CODES]);
  assert.deepEqual(CONFORMANCE_MANIFEST.methods, [
    "ping", "start_session", "run_turn", "cancel_turn", "inspect_session",
    "attach_session", "close_session", "subscribe_events", "run_once", "clear_session",
    "diagnose", "run_stateless",
    "create_agent", "get_agent", "list_agents", "update_agent",
  ]);
  assert.deepEqual(CONFORMANCE_MANIFEST.results, [
    "pong", "session_started", "turn_accepted", "turn_cancelled", "session_snapshot",
    "attach_capability", "session_closed", "events", "turn_result", "session_cleared",
    "diagnosis", "stateless_result",
    "agent_created", "agent", "agent_list", "agent_updated",
  ]);
  assert.deepEqual(CONFORMANCE_MANIFEST.events, [
    "session_state_changed", "prompt_acknowledged", "logical_message", "tool_started",
    "tool_completed", "rate_limit", "needs_input", "terminal_candidate", "turn_completed",
    "turn_cancelled", "turn_failed", "warning", "replay_gap", "heartbeat",
  ]);
});

test("shared strict error-body vectors are enforced at the top-level response", async () => {
  await withFakeServer(
    async (socket, connection) => {
      const request = await readRequest(socket);
      const vector = CONFORMANCE_CASES.error_bodies[connection];
      writeJson(socket, {
        version: PROTOCOL_VERSION,
        request_id: request.request_id,
        error: vector.body,
      });
    },
    async (socketPath) => {
      const client = new PmuxClient(socketPath);
      for (const vector of CONFORMANCE_CASES.error_bodies) {
        if (vector.valid) {
          await assert.rejects(
            client.ping(),
            (error) => error instanceof PmuxServerError && error.body.code === vector.body.code,
            vector.id,
          );
        } else {
          await assert.rejects(client.ping(), PmuxProtocolError, vector.id);
        }
      }
    },
  );
});

test("shared replay-gap vectors require exclusivity and exact cursors", async () => {
  await withFakeServer(
    async (socket, connection) => {
      const request = await readRequest(socket);
      const vector = CONFORMANCE_CASES.replay_batches[connection];
      writeJson(socket, success(request, "events", {
        events: vector.event_sequences.map(heartbeat),
        next_sequence: vector.batch_next,
        replay_gap: {
          requested_after: vector.requested_after,
          oldest_available: vector.oldest_available,
          next_sequence: vector.gap_next,
          snapshot: snapshot(vector.snapshot_last),
        },
      }));
    },
    async (socketPath) => {
      const client = new PmuxClient(socketPath);
      for (const vector of CONFORMANCE_CASES.replay_batches) {
        const operation = client.subscribeEvents({
          session_id: SESSION_ID,
          generation_id: GENERATION_ID,
          after_sequence: vector.requested_after,
          max_events: 8,
        });
        if (vector.valid) {
          const batch = await operation;
          assert.equal(batch.replay_gap.snapshot.last_sequence, vector.snapshot_last);
        } else {
          await assert.rejects(operation, PmuxProtocolError, vector.id);
        }
      }
    },
  );
});

test("shared canonical UUID vectors are validated without rewriting", async () => {
  const valid = CONFORMANCE_CASES.identities.filter((vector) => vector.valid);
  await withFakeServer(
    async (socket, connection) => {
      const request = await readRequest(socket);
      const vector = valid[connection];
      assert.equal(request.params.session_id, vector.value);
      const response = success(request, "session_snapshot", {
        ...snapshot(0),
        session_id: vector.value,
      });
      if (vector.id === "canonical_upper") {
        response.request_id = response.request_id.toUpperCase();
      }
      writeJson(socket, response);
    },
    async (socketPath) => {
      const client = new PmuxClient(socketPath);
      for (const vector of valid) {
        const result = await client.inspectSession(vector.value, GENERATION_ID);
        assert.equal(result.session_id, vector.value);
      }
    },
  );

  const disconnected = new PmuxClient("/tmp/pmux-conformance-does-not-exist.sock");
  for (const vector of CONFORMANCE_CASES.identities.filter((item) => !item.valid)) {
    await assert.rejects(
      disconnected.inspectSession(vector.value, GENERATION_ID),
      PmuxProtocolError,
      vector.id,
    );
  }

  await withFakeServer(
    async (socket, connection) => {
      const request = await readRequest(socket);
      const vector = CONFORMANCE_CASES.identities.filter((item) => !item.valid)[connection];
      writeJson(socket, success(request, "session_started", {
        session_id: vector.value,
        generation_id: GENERATION_ID,
        state: "booting",
        compatibility: compatibilityReport(),
        created_at_ms: 1,
        last_sequence: 0,
      }));
    },
    async (socketPath) => {
      const client = new PmuxClient(socketPath);
      for (const vector of CONFORMANCE_CASES.identities.filter((item) => !item.valid)) {
        await assert.rejects(client.request({ method: "ping" }), PmuxProtocolError, vector.id);
      }
    },
  );
});

test("shared non-standard JSON constants are rejected", async () => {
  await withFakeServer(
    async (socket, connection) => {
      const request = await readRequest(socket);
      const constant = CONFORMANCE_CASES.nonstandard_json_constants[connection];
      writeRawJson(
        socket,
        `{"version":1,"request_id":"${request.request_id}","result":{"type":"pong","data":{"server_version":"test","protocol_version":${constant}}}}`,
      );
    },
    async (socketPath) => {
      const client = new PmuxClient(socketPath);
      for (const constant of CONFORMANCE_CASES.nonstandard_json_constants) {
        await assert.rejects(client.ping(), PmuxProtocolError, constant);
      }
    },
  );
});

test("shared safe-integer boundaries are enforced on public client input", async () => {
  assert.equal(MAX_SAFE_JSON_INTEGER, 9_007_199_254_740_991);
  const boundaries = CONFORMANCE_CASES.numeric_boundaries;
  await withFakeServer(
    async (socket, connection) => {
      const request = await readRequest(socket);
      const vector = boundaries[connection];
      const response = success(request, "session_started", {
        session_id: SESSION_ID,
        generation_id: GENERATION_ID,
        state: "booting",
        compatibility: compatibilityReport(),
        created_at_ms: "__PMUX_NUMBER__",
        last_sequence: 0,
      });
      writeRawJson(
        socket,
        JSON.stringify(response).replace('"__PMUX_NUMBER__"', vector.literal),
      );
    },
    async (socketPath) => {
      const client = new PmuxClient(socketPath);
      for (const vector of boundaries) {
        const operation = client.startSession(startRequest());
        if (vector.protocol_owned_valid) {
          const result = await operation;
          assert.equal(result.created_at_ms, Number(vector.literal), vector.id);
        } else {
          await assert.rejects(operation, PmuxProtocolError, vector.id);
        }
      }
    },
  );

  await withFakeServer(
    async (socket, connection) => {
      const request = await readRequest(socket);
      const vector = boundaries[connection];
      const response = {
        version: 1,
        request_id: request.request_id,
        error: {
          code: "internal",
          message: "synthetic",
          retryable: false,
          details: { nested: ["__PMUX_NUMBER__"] },
        },
      };
      writeRawJson(
        socket,
        JSON.stringify(response).replace('"__PMUX_NUMBER__"', vector.literal),
      );
    },
    async (socketPath) => {
      const client = new PmuxClient(socketPath);
      for (const vector of boundaries) {
        const operation = client.ping();
        if (vector.opaque_json_valid) {
          await assert.rejects(operation, PmuxServerError, vector.id);
        } else {
          await assert.rejects(operation, PmuxProtocolError, vector.id);
        }
      }
    },
  );
});

test("shared safe-integer boundaries are enforced before public client output", async () => {
  const boundaries = CONFORMANCE_CASES.numeric_boundaries;
  const turnId = "00000000-0000-4000-8000-000000000033";
  const validProtocol = boundaries.filter((vector) => vector.protocol_owned_valid);
  await withFakeServer(
    async (socket) => {
      const request = await readRequest(socket);
      writeJson(socket, success(request, "turn_accepted", {
        session_id: SESSION_ID,
        generation_id: GENERATION_ID,
        turn_id: turnId,
        replayed: false,
        state: "running",
        next_sequence: 1,
      }));
    },
    async (socketPath) => {
      const client = new PmuxClient(socketPath);
      for (const vector of boundaries) {
        const operation = client.runTurn(
          SESSION_ID,
          GENERATION_ID,
          {
            turn_id: turnId,
            prompt: "numeric boundary",
            deadline_unix_ms: Number(vector.literal),
          },
        );
        if (vector.protocol_owned_valid) {
          await operation;
        } else {
          await assert.rejects(operation, PmuxProtocolError, vector.id);
        }
      }
      assert.equal(validProtocol.length, 2);
    },
  );

  const validOpaque = boundaries.filter((vector) => vector.opaque_json_valid);
  await withFakeServer(
    async (socket) => {
      const request = await readRequest(socket);
      writeJson(socket, success(request, "session_started", {
        session_id: SESSION_ID,
        generation_id: GENERATION_ID,
        state: "booting",
        compatibility: compatibilityReport(),
        created_at_ms: 1,
        last_sequence: 0,
      }));
    },
    async (socketPath) => {
      const client = new PmuxClient(socketPath);
      for (const vector of boundaries) {
        const request = startRequest();
        request.claude.settings = [{
          source: "inline",
          document: { nested: [Number(vector.literal)] },
        }];
        const operation = client.startSession(request);
        if (vector.opaque_json_valid) {
          await operation;
        } else {
          await assert.rejects(operation, PmuxProtocolError, vector.id);
        }
      }
      assert.equal(validOpaque.length, 3);
    },
  );
});

test("invalid UTF-8 response bytes are rejected without replacement decoding", async () => {
  await withFakeServer(
    async (socket) => {
      await readRequest(socket);
      const body = Buffer.from([0xff]);
      const header = Buffer.allocUnsafe(4);
      header.writeUInt32BE(body.length);
      socket.write(header);
      socket.write(body);
    },
    async (socketPath) => {
      await assert.rejects(new PmuxClient(socketPath).ping(), PmuxProtocolError);
    },
  );
});

test("minor-v1 additive response and event fields are tolerated", async () => {
  await withFakeServer(
    async (socket, connection) => {
      const request = await readRequest(socket);
      if (connection === 0) {
        writeJson(socket, {
          ...success(request, "pong", {
            server_version: "future-minor",
            protocol_version: PROTOCOL_VERSION,
            future_pong_field: { opaque: true },
          }),
          future_envelope_field: true,
        });
        return;
      }
      writeJson(socket, success(request, "events", {
        events: [{
          ...heartbeat(1),
          future_event_envelope_field: true,
          event: {
            type: "heartbeat",
            future_event_wrapper_field: "opaque",
            data: { session_state: "ready", future_heartbeat_field: 1 },
          },
        }],
        next_sequence: 2,
        future_batch_field: { opaque: true },
      }));
    },
    async (socketPath) => {
      const client = new PmuxClient(socketPath);
      const pong = await client.ping();
      assert.equal(pong.server_version, "future-minor");
      assert.deepEqual(pong.future_pong_field, { opaque: true });

      const batch = await client.subscribeEvents({
        session_id: SESSION_ID,
        generation_id: GENERATION_ID,
        max_events: 8,
      });
      assert.deepEqual(batch.future_batch_field, { opaque: true });
      assert.equal(batch.events[0].event.data.future_heartbeat_field, 1);
    },
  );
});

test("known response and event payloads require their v1 fields", async () => {
  await withFakeServer(
    async (socket, connection) => {
      const request = await readRequest(socket);
      if (connection === 0) {
        writeJson(socket, success(request, "pong", { protocol_version: 1 }));
        return;
      }
      writeJson(socket, success(request, "events", {
        events: [{
          ...heartbeat(1),
          event: { type: "heartbeat", data: {} },
        }],
        next_sequence: 2,
      }));
    },
    async (socketPath) => {
      const client = new PmuxClient(socketPath);
      await assert.rejects(client.ping(), PmuxProtocolError);
      await assert.rejects(
        client.subscribeEvents({
          session_id: SESSION_ID,
          generation_id: GENERATION_ID,
          max_events: 8,
        }),
        PmuxProtocolError,
      );
    },
  );
});

test("compatibility reports are required, bounded, and contain the resolved transport", async () => {
  await withFakeServer(
    async (socket, connection) => {
      const request = await readRequest(socket);
      const data = {
        session_id: SESSION_ID,
        generation_id: GENERATION_ID,
        state: "booting",
        compatibility: compatibilityReport(),
        created_at_ms: 1,
        last_sequence: 0,
      };
      if (connection === 0) delete data.compatibility;
      if (connection === 1) data.compatibility.input_transport = "auto";
      if (connection === 2) data.compatibility.transcript_drain_ms = 60_001;
      writeJson(socket, success(request, "session_started", data));
    },
    async (socketPath) => {
      const client = new PmuxClient(socketPath);
      await assert.rejects(client.startSession(startRequest()), PmuxProtocolError);
      await assert.rejects(client.startSession(startRequest()), PmuxProtocolError);
      await assert.rejects(client.startSession(startRequest()), PmuxProtocolError);
    },
  );
});

test("optional turn timings are validated as protocol-owned integers", async () => {
  const turnId = "00000000-0000-4000-8000-000000000033";
  const fields = ["last_transcript_activity_at_ms", "stop_hook_at_ms"];
  const mutations = [1, "190", -1, MAX_SAFE_JSON_INTEGER + 1];
  await withFakeServer(
    async (socket, connection) => {
      const request = await readRequest(socket);
      const data = turnResult(turnId);
      data.timings.drain_ms = 10;
      const field = fields[Math.trunc(connection / mutations.length)];
      data.timings[field] = mutations[connection % mutations.length];
      writeJson(socket, success(request, "turn_result", data));
    },
    async (socketPath) => {
      const client = new PmuxClient(socketPath);
      const call = { session: startRequest(), turn: { turn_id: turnId, prompt: "timings" } };
      for (const field of fields) {
        const accepted = await client.runOnce(call);
        assert.equal(accepted.timings[field], 1);
        for (const other of fields) {
          if (other !== field) assert.ok(!(other in accepted.timings), `${other} must stay absent`);
        }
        for (let index = 1; index < mutations.length; index += 1) {
          await assert.rejects(
            client.runOnce(call),
            PmuxProtocolError,
            `${field} ${String(mutations[index])}`,
          );
        }
      }
    },
  );
});

test("unknown v1 result and event discriminants are rejected", async () => {
  await withFakeServer(
    async (socket, connection) => {
      const request = await readRequest(socket);
      if (connection === 0) {
        writeJson(socket, success(request, "future_result", {}));
        return;
      }
      writeJson(socket, success(request, "events", {
        events: [{ ...heartbeat(1), event: { type: "future_event", data: {} } }],
        next_sequence: 2,
      }));
    },
    async (socketPath) => {
      const client = new PmuxClient(socketPath);
      await assert.rejects(client.request({ method: "ping" }), PmuxProtocolError);
      await assert.rejects(
        client.subscribeEvents({
          session_id: SESSION_ID,
          generation_id: GENERATION_ID,
          max_events: 8,
        }),
        PmuxProtocolError,
      );
    },
  );
});

test("major version is validated before result decoding", async () => {
  await withFakeServer(
    async (socket) => {
      const request = await readRequest(socket);
      writeJson(socket, success(request, "future_result", { future: true }, 2));
    },
    async (socketPath) => {
      await assert.rejects(new PmuxClient(socketPath).ping(), PmuxVersionError);
    },
  );
});

test("request correlation rejects another request id", async () => {
  await withFakeServer(
    async (socket) => {
      await readRequest(socket);
      writeJson(socket, {
        version: 1,
        request_id: "00000000-0000-4000-8000-000000009999",
        result: { type: "pong", data: { server_version: "test", protocol_version: 1 } },
      });
    },
    async (socketPath) => {
      await assert.rejects(new PmuxClient(socketPath).ping(), PmuxRequestIdMismatchError);
    },
  );
});

test("every typed method rejects contextually mismatched results", async () => {
  const turnId = "00000000-0000-4000-8000-000000000033";
  const cases = [
    {
      name: "ping protocol version",
      response: (request) => success(request, "pong", {
        server_version: "test",
        protocol_version: 2,
      }),
      invoke: (client) => client.ping(),
    },
    {
      name: "start session id",
      response: (request) => success(request, "session_started", {
        session_id: OTHER_ID,
        generation_id: GENERATION_ID,
        state: "booting",
        compatibility: compatibilityReport(),
        created_at_ms: 1,
        last_sequence: 0,
      }),
      invoke: (client) => client.startSession(startRequest()),
    },
    {
      name: "inspect session id",
      response: (request) => success(request, "session_snapshot", {
        ...snapshot(0),
        session_id: OTHER_ID,
      }),
      invoke: (client) => client.inspectSession(SESSION_ID, GENERATION_ID),
    },
    {
      name: "run turn generation id",
      response: (request) => success(request, "turn_accepted", {
        session_id: SESSION_ID,
        generation_id: OTHER_ID,
        turn_id: turnId,
        replayed: false,
        state: "running",
        next_sequence: 1,
      }),
      invoke: (client) => client.runTurn(
        SESSION_ID,
        GENERATION_ID,
        { turn_id: turnId, prompt: "test" },
      ),
    },
    {
      name: "cancel turn id",
      response: (request) => success(request, "turn_cancelled", {
        session_id: SESSION_ID,
        generation_id: GENERATION_ID,
        turn_id: OTHER_ID,
        outcome: "cancelled",
        session_state: "ready",
      }),
      invoke: (client) => client.cancelTurn(SESSION_ID, GENERATION_ID, turnId),
    },
    {
      name: "attach session id",
      response: (request) => success(request, "attach_capability", {
        session_id: OTHER_ID,
        generation_id: GENERATION_ID,
        token: "opaque",
        endpoint: "/tmp/attach.sock",
        expires_at_ms: 10,
        read_only: true,
      }),
      invoke: (client) => client.attachSession({
        session_id: SESSION_ID,
        generation_id: GENERATION_ID,
        read_only: true,
      }),
    },
    {
      name: "close generation id",
      response: (request) => success(request, "session_closed", {
        session_id: SESSION_ID,
        generation_id: OTHER_ID,
        already_closed: false,
        process_reaped: true,
      }),
      invoke: (client) => client.closeSession(SESSION_ID, GENERATION_ID),
    },
    {
      name: "run once turn id",
      response: (request) => success(request, "turn_result", turnResult(OTHER_ID)),
      invoke: (client) => client.runOnce({
        session: startRequest(),
        turn: { turn_id: turnId, prompt: "test" },
      }),
    },
    {
      name: "subscribe event session id",
      response: (request) => success(request, "events", {
        events: [{ ...heartbeat(1), session_id: OTHER_ID }],
        next_sequence: 2,
      }),
      invoke: (client) => client.subscribeEvents({
        session_id: SESSION_ID,
        generation_id: GENERATION_ID,
        max_events: 8,
      }),
    },
  ];

  await withFakeServer(
    async (socket, connection) => {
      const request = await readRequest(socket);
      writeJson(socket, cases[connection].response(request));
    },
    async (socketPath) => {
      const client = new PmuxClient(socketPath);
      for (const contextCase of cases) {
        await assert.rejects(contextCase.invoke(client), PmuxProtocolError, contextCase.name);
      }
    },
  );
});

test("oversized advertised response is rejected before body allocation", async () => {
  await withFakeServer(
    async (socket) => {
      await readRequest(socket);
      const header = Buffer.allocUnsafe(4);
      header.writeUInt32BE(1_025);
      socket.write(header);
    },
    async (socketPath) => {
      const client = new PmuxClient(socketPath, { maxFrameBytes: 1_024 });
      await assert.rejects(client.ping(), PmuxFrameTooLargeError);
    },
  );
});

test("AbortSignal tears down an in-flight request", async () => {
  await withFakeServer(
    async (socket) => {
      await readRequest(socket);
      await new Promise((resolve) => socket.once("close", resolve));
    },
    async (socketPath) => {
      const controller = new AbortController();
      const pending = new PmuxClient(socketPath).ping({ signal: controller.signal });
      setTimeout(() => controller.abort("test abort"), 10);
      await assert.rejects(pending, PmuxAbortError);
    },
  );
});

test("safe-max runOnce deadlines use bounded rearming timers and remain abortable", async () => {
  const maxNodeTimerDelayMs = 2_147_483_647;
  const originalSetTimeout = globalThis.setTimeout;
  const originalClearTimeout = globalThis.clearTimeout;
  const timers = [];

  globalThis.setTimeout = (callback, delay, ...args) => {
    const timer = {
      active: true,
      delay: Number(delay),
      run() {
        if (!this.active) return;
        this.active = false;
        callback(...args);
      },
    };
    timers.push(timer);
    return timer;
  };
  globalThis.clearTimeout = (timer) => {
    if (timers.includes(timer)) {
      timer.active = false;
    } else {
      originalClearTimeout(timer);
    }
  };

  try {
    let markRequestSeen;
    const requestSeen = new Promise((resolve) => {
      markRequestSeen = resolve;
    });
    await withFakeServer(
      async (socket) => {
        const request = await readRequest(socket);
        assert.equal(request.params.turn.deadline_unix_ms, MAX_SAFE_JSON_INTEGER);
        markRequestSeen();
        await new Promise((resolve) => socket.once("close", resolve));
      },
      async (socketPath) => {
        const controller = new AbortController();
        let settled = false;
        const pending = new PmuxClient(socketPath).runOnce(
          {
            session: startRequest(),
            turn: {
              turn_id: "00000000-0000-4000-8000-000000000033",
              prompt: "wait for cancellation",
              deadline_unix_ms: MAX_SAFE_JSON_INTEGER,
            },
          },
          { signal: controller.signal },
        );
        void pending.then(
          () => { settled = true; },
          () => { settled = true; },
        );

        await requestSeen;
        const firstChunk = timers.find(
          (timer) => timer.active && timer.delay === maxNodeTimerDelayMs,
        );
        assert.ok(firstChunk, "long request timeout must be capped at Node's timer maximum");
        firstChunk.run();
        await Promise.resolve();

        assert.equal(settled, false, "one timer chunk must not expire the full deadline");
        assert.equal(
          timers.filter((timer) => timer.active && timer.delay === maxNodeTimerDelayMs).length,
          1,
          "the remaining timeout must be rearmed as another bounded chunk",
        );

        controller.abort("test abort");
        await assert.rejects(pending, PmuxAbortError);
        assert.equal(timers.some((timer) => timer.active), false);
      },
    );
  } finally {
    globalThis.setTimeout = originalSetTimeout;
    globalThis.clearTimeout = originalClearTimeout;
  }
});

test("runOnce derives its transport timeout from the turn window", async () => {
  const turnId = "00000000-0000-4000-8000-000000000033";
  await withFakeServer(
    async (socket) => {
      const request = await readRequest(socket);
      assert.equal(request.method, "run_once");
      await new Promise((resolve) => setTimeout(resolve, 75));
      writeJson(socket, success(request, "turn_result", turnResult(turnId)));
    },
    async (socketPath) => {
      const result = await new PmuxClient(socketPath, { requestTimeoutMs: 20 }).runOnce({
        session: startRequest(),
        turn: { turn_id: turnId, prompt: "wait past the RPC timeout" },
      });
      assert.equal(result.text, "done");
    },
  );
});

test("runOnce timeout constants contain the maximum recovery and drain path", () => {
  assert.equal(DEFAULT_RUN_ONCE_TIMEOUT_MS, 15 * 60_000);
  assert.equal(RUN_ONCE_RESPONSE_MARGIN_MS, 120_000);
});

function statelessResult() {
  const usage = {
    input_tokens: 10,
    output_tokens: 2,
    cache_creation_input_tokens: 0,
    cache_read_input_tokens: 0,
  };
  return {
    model: "claude-sonnet-5",
    text: "done",
    usage: { main: usage, sidechain: { ...usage, input_tokens: 0, output_tokens: 0 }, combined: usage },
    claude_version: "2.1.207",
  };
}

test("runStateless derives its transport timeout from the answer window", async () => {
  await withFakeServer(
    async (socket) => {
      const request = await readRequest(socket);
      assert.equal(request.method, "run_stateless");
      await new Promise((resolve) => setTimeout(resolve, 75));
      writeJson(socket, success(request, "stateless_result", statelessResult()));
    },
    async (socketPath) => {
      const result = await new PmuxClient(socketPath, { requestTimeoutMs: 20 }).runStateless({
        model: "claude-sonnet-5",
        prompt: "wait past the RPC timeout",
      });
      assert.equal(result.text, "done");
    },
  );
});

test("stateless_result resource fields refuse as a protocol error", async () => {
  for (const named of ["session_id", "generation_id", "cwd", "config_root", "system_prompt"]) {
    await withFakeServer(
      async (socket) => {
        const request = await readRequest(socket);
        writeJson(socket, success(request, "stateless_result", {
          ...statelessResult(),
          [named]: "named-pool-resource",
        }));
      },
      async (socketPath) => {
        await assert.rejects(
          new PmuxClient(socketPath).runStateless({
            model: "claude-sonnet-5",
            prompt: "hello",
          }),
          (error) => {
            assert.ok(
              error instanceof PmuxProtocolError,
              `${named} must be a protocol error, got ${error?.name}: ${error}`,
            );
            assert.equal(error.name, "PmuxProtocolError");
            assert.match(error.message, new RegExp(`carries ${named}`));
            return true;
          },
        );
      },
    );
  }
});

test("a stateless call gets the full lifecycle budget and not the default", () => {
  const none = { method: "run_stateless", params: { model: "claude-sonnet-5", prompt: "hello" } };
  assert.equal(requestTimeoutFor(none, 45_000, 10_000), DEFAULT_RUN_ONCE_TIMEOUT_MS);
  assert.notEqual(
    requestTimeoutFor(none, 45_000, 10_000),
    45_000,
    "the wildcard arm handed a mint-and-turn call the default request timeout",
  );
  const withDeadline = {
    method: "run_stateless",
    params: { model: "claude-sonnet-5", prompt: "hello", deadline_unix_ms: 11_000 },
  };
  assert.equal(requestTimeoutFor(withDeadline, 1_000, 10_000), 1_000 + RUN_ONCE_RESPONSE_MARGIN_MS);
  const shortDeadline = {
    method: "run_stateless",
    params: { model: "claude-sonnet-5", prompt: "hello", deadline_unix_ms: 10_001 },
  };
  assert.equal(requestTimeoutFor(shortDeadline, 300_000, 10_000), 300_000);
});

test("event subscription reconnects at its cursor and surfaces ReplayGap", async () => {
  await withFakeServer(
    async (socket, connection) => {
      const request = await readRequest(socket);
      if (connection === 0) {
        assert.equal(request.params.after_sequence, 0);
        socket.destroy();
        return;
      }
      if (connection === 1) {
        assert.equal(request.params.after_sequence, 0);
        writeJson(socket, success(request, "events", {
          events: [heartbeat(1), heartbeat(2)],
          next_sequence: 3,
        }));
        return;
      }
      assert.equal(request.params.after_sequence, 2);
      writeJson(socket, success(request, "events", {
        next_sequence: 10,
        replay_gap: {
          requested_after: 2,
          oldest_available: 8,
          next_sequence: 10,
          snapshot: snapshot(9),
        },
      }));
    },
    async (socketPath) => {
      const reconnects = [];
      const subscription = new PmuxClient(socketPath).events(SESSION_ID, GENERATION_ID, {
        reconnectDelayMs: 1,
        onReconnect: (_error, attempt, cursor) => reconnects.push([attempt, cursor]),
      });
      const iterator = subscription[Symbol.asyncIterator]();
      const first = (await iterator.next()).value;
      const second = (await iterator.next()).value;
      const gap = (await iterator.next()).value;
      assert.equal(first.event.sequence, 1);
      assert.equal(second.event.sequence, 2);
      assert.equal(gap.kind, "replay_gap");
      assert.equal(gap.gap.snapshot.last_sequence, 9);
      assert.equal(subscription.afterSequence, 9);
      assert.deepEqual(reconnects, [[1, 0]]);
      await iterator.return();
    },
  );
});

test("out-of-order event batches fail without advancing the cursor", async () => {
  await withFakeServer(
    async (socket) => {
      const request = await readRequest(socket);
      writeJson(socket, success(request, "events", {
        events: [heartbeat(2)],
        next_sequence: 3,
      }));
    },
    async (socketPath) => {
      const subscription = new PmuxClient(socketPath).events(SESSION_ID, GENERATION_ID, {
        maxReconnectAttempts: 0,
      });
      const iterator = subscription[Symbol.asyncIterator]();
      await assert.rejects(iterator.next(), PmuxSequenceError);
      assert.equal(subscription.afterSequence, 0);
    },
  );
});

test("safe-max event cursor fails closed instead of rounding or saturating", async () => {
  await withFakeServer(
    async (socket) => {
      const request = await readRequest(socket);
      writeJson(socket, success(request, "events", {
        events: [],
        next_sequence: MAX_SAFE_JSON_INTEGER,
      }));
    },
    async (socketPath) => {
      await assert.rejects(
        new PmuxClient(socketPath).subscribeEvents({
          session_id: SESSION_ID,
          generation_id: GENERATION_ID,
          after_sequence: MAX_SAFE_JSON_INTEGER,
        }),
        PmuxProtocolError,
      );
    },
  );
});

test("a listing surfaces the records the daemon could not read", async () => {
  // `unreadable` is omitted when empty, so the ordinary listing's bytes are
  // unchanged; a client that dropped the field when it IS present would show a
  // stored agent simply ceasing to exist, which is the reason pmuxd stopped
  // answering the whole listing with the first bad record's refusal.
  const frames = [
    { agents: [], unreadable: [{ agent_id: OTHER_ID, reason: "agent store ... has no head pointer" }] },
    { agents: [] },
    { unreadable: [{ agent_id: OTHER_ID }] },
    { unreadable: [{ agent_id: "not-a-uuid", reason: "x" }] },
    { unreadable: {} },
  ];
  await withFakeServer(
    async (socket, index) => {
      const request = await readRequest(socket);
      writeJson(socket, {
        version: PROTOCOL_VERSION,
        request_id: request.request_id,
        result: { type: "agent_list", data: frames[index] },
      });
    },
    async (socketPath) => {
      const client = new PmuxClient(socketPath);
      const reported = await client.listAgents();
      assert.deepEqual(reported.unreadable, [
        { agent_id: OTHER_ID, reason: "agent store ... has no head pointer" },
      ]);
      const empty = await client.listAgents();
      assert.equal(empty.unreadable, undefined);
      for (const label of ["missing reason", "non-canonical id", "not an array"]) {
        await assert.rejects(client.listAgents(), PmuxProtocolError, label);
      }
    },
  );
});
