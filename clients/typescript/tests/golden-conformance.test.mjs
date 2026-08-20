import assert from "node:assert/strict";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { clientModuleUrl } from "./dist-stage.mjs";

const {
  V1_TAGGED_UNIONS,
  V1_VALUE_ENUMS,
  PmuxClient,
  PmuxProtocolError,
  PmuxRequestIdMismatchError,
  PmuxSequenceError,
  PmuxServerError,
  PmuxVersionError,
} = await import((await clientModuleUrl(import.meta.url)).href);

const GOLDEN = JSON.parse(
  await readFile(new URL("../../../tests/conformance/v1/golden.json", import.meta.url), "utf8"),
);
const CASES = JSON.parse(
  await readFile(new URL("../../../tests/conformance/v1/cases.json", import.meta.url), "utf8"),
);
const MANIFEST = JSON.parse(
  await readFile(new URL("../../../tests/conformance/v1/manifest.json", import.meta.url), "utf8"),
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

async function withFakeServer(handler, body) {
  const directory = await mkdtemp(join(tmpdir(), "pmux-ts-golden-"));
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

function replaceRequestId(value, requestId) {
  if (value === "$REQUEST_ID") return requestId;
  if (Array.isArray(value)) return value.map((item) => replaceRequestId(item, requestId));
  if (value !== null && typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value).map(([key, item]) => [key, replaceRequestId(item, requestId)]),
    );
  }
  return value;
}

function removePointer(value, pointer) {
  const parts = pointer.slice(1).split("/");
  const field = parts.pop();
  let parent = value;
  for (const part of parts) parent = parent[part];
  assert.ok(Object.hasOwn(parent, field), `shared deletion pointer ${pointer}`);
  delete parent[field];
}

function objectAtPointer(value, pointer) {
  if (pointer === "") return value;
  return pointer
    .slice(1)
    .split("/")
    .map((part) => part.replaceAll("~1", "/").replaceAll("~0", "~"))
    .reduce((parent, part) => parent[part], value);
}

function insertAdditiveField(value, pointer) {
  const target = objectAtPointer(value, pointer);
  assert.ok(target !== null && typeof target === "object" && !Array.isArray(target), pointer);
  assert.ok(!Object.hasOwn(target, "future_minor_field"), pointer);
  target.future_minor_field = { opaque: true };
}

function objectPointers(value) {
  const pointers = [];
  const escape = (part) => part.replaceAll("~", "~0").replaceAll("/", "~1");
  const visit = (current, pointer) => {
    if (Array.isArray(current)) {
      current.forEach((child, index) => visit(child, `${pointer}/${index}`));
      return;
    }
    if (current !== null && typeof current === "object") {
      pointers.push(pointer);
      for (const [key, child] of Object.entries(current)) {
        visit(child, `${pointer}/${escape(key)}`);
      }
    }
  };
  visit(value, "");
  return pointers;
}

function requestFor(method) {
  return structuredClone(
    GOLDEN.requests_and_results.find((exchange) => exchange.method === method).request,
  );
}

async function invokeTyped(client, request) {
  const params = request.params;
  switch (request.method) {
    case "ping":
      return { type: "pong", data: await client.ping() };
    case "start_session":
      return { type: "session_started", data: await client.startSession(params) };
    case "run_turn":
      return {
        type: "turn_accepted",
        data: await client.runTurn(params.session_id, params.generation_id, params.turn),
      };
    case "cancel_turn":
      return {
        type: "turn_cancelled",
        data: await client.cancelTurn(params.session_id, params.generation_id, params.turn_id),
      };
    case "inspect_session":
      return {
        type: "session_snapshot",
        data: await client.inspectSession(params.session_id, params.generation_id),
      };
    case "attach_session":
      return { type: "attach_capability", data: await client.attachSession(params) };
    case "close_session":
      return {
        type: "session_closed",
        data: await client.closeSession(params.session_id, params.generation_id, params.policy),
      };
    case "subscribe_events":
      return { type: "events", data: await client.subscribeEvents(params) };
    case "run_once":
      return { type: "turn_result", data: await client.runOnce(params) };
    case "clear_session":
      return {
        type: "session_cleared",
        data: await client.clearSession(
          params.session_id,
          params.generation_id,
          params.expected_transcript_session_id,
          params.deadline_unix_ms,
        ),
      };
    case "diagnose":
      return { type: "diagnosis", data: await client.diagnose() };
    case "run_stateless":
      return { type: "stateless_result", data: await client.runStateless(params) };
    case "create_agent":
      return { type: "agent_created", data: await client.createAgent(params.spec) };
    case "get_agent":
      return { type: "agent", data: await client.getAgent(params.agent_id, params.version) };
    case "list_agents":
      return { type: "agent_list", data: await client.listAgents() };
    case "update_agent":
      return {
        type: "agent_updated",
        data: await client.updateAgent(params.agent_id, params.expected_version, params.spec),
      };
    default:
      throw new Error(`unknown golden method ${request.method}`);
  }
}

test("every typed method sends exact shared golden requests and accepts matching results", async () => {
  // DERIVED FROM THE MANIFEST, never written out. A hand-written count freezes
  // the corpus at the size it had the day it was typed: deleting an entry
  // reddens it, failing to ADD one does not, and MEASURED, `run_stateless` --
  // all of Path B and the only producer of `stateless_result` -- had no golden
  // pair in any of the three languages while this client implemented and
  // validated it. Compared by NAME so the failure says which method is
  // uncovered.
  assert.deepEqual(
    GOLDEN.requests_and_results.map((exchange) => exchange.method).sort(),
    [...MANIFEST.methods].sort(),
    "golden.json must carry one complete request/result pair for every manifest method",
  );
  await withFakeServer(
    async (socket, index) => {
      const exchange = GOLDEN.requests_and_results[index];
      const actual = await readRequest(socket);
      assert.match(
        actual.request_id,
        /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i,
      );
      assert.deepEqual(
        { ...actual, request_id: GOLDEN.ids.request_id },
        exchange.request,
        exchange.method,
      );
      writeJson(socket, { ...exchange.response, request_id: actual.request_id });
    },
    async (socketPath) => {
      const client = new PmuxClient(socketPath);
      for (const exchange of GOLDEN.requests_and_results) {
        const actual = await invokeTyped(client, structuredClone(exchange.request));
        assert.deepEqual(actual, exchange.response.result, exchange.method);
      }
    },
  );
});

test("shared negative identity, schema, sequence, cursor, gap, and safe-max matrix fails closed", async () => {
  assert.equal(CASES.client_negative_matrix.length, 17);
  await withFakeServer(
    async (socket, index) => {
      const request = await readRequest(socket);
      const response = replaceRequestId(
        structuredClone(CASES.client_negative_matrix[index].response),
        request.request_id,
      );
      writeJson(socket, response);
    },
    async (socketPath) => {
      const client = new PmuxClient(socketPath);
      for (const vector of CASES.client_negative_matrix) {
        const request = requestFor(vector.operation);
        if (vector.operation === "subscribe_events") {
          request.params.after_sequence = vector.after_sequence;
        }
        let error;
        try {
          await invokeTyped(client, request);
        } catch (caught) {
          error = caught;
        }
        assert.ok(error instanceof Error, `${vector.id} must reject`);
        switch (vector.error_category) {
          case "response_identity":
            assert.ok(error instanceof PmuxRequestIdMismatchError, vector.id);
            break;
          case "schema_version":
            assert.ok(error instanceof PmuxVersionError, vector.id);
            break;
          case "schema":
            assert.ok(error instanceof PmuxProtocolError, vector.id);
            break;
          case "result_session":
            assert.match(error.message, /result session_id .* does not match request/, vector.id);
            break;
          case "result_generation":
            assert.match(error.message, /result generation_id .* does not match request/, vector.id);
            break;
          case "result_turn":
            assert.match(error.message, /result turn_id .* does not match request/, vector.id);
            break;
          case "event_session":
            assert.match(error.message, /(event|snapshot) belongs to another session/, vector.id);
            break;
          case "event_generation":
            assert.match(
              error.message,
              /(event|snapshot) belongs to another process generation/,
              vector.id,
            );
            break;
          case "event_sequence":
          case "batch_cursor":
            assert.ok(error instanceof PmuxSequenceError, vector.id);
            break;
          case "replay_gap":
            assert.match(error.message, /replay-gap|replay gap/, vector.id);
            break;
          case "cursor_exhaustion":
            assert.match(error.message, /safe integer/, vector.id);
            break;
          default:
            assert.fail(`unknown shared category ${vector.error_category}`);
        }
      }
    },
  );
});

test("shared required-field inventory rejects every nested result, event, and error deletion", async () => {
  const deletions = CASES.client_required_field_deletions;
  const resultCases = deletions.results.flatMap((fields) =>
    [...deletions.result_envelope, ...fields.pointers].map((pointer) => ({
      method: fields.method,
      pointer,
    })),
  );
  const eventCases = deletions.events.flatMap((fields) =>
    [...deletions.event_envelope, ...fields.pointers].map((pointer) => ({
      event_type: fields.event_type,
      pointer,
    })),
  );
  // 187 before `diagnose`; its nine required result pointers plus the five
  // shared envelope pointers add fourteen. 201 before `run_stateless`, whose
  // twenty required result pointers plus the same five add twenty-five. 226
  // before the four agent methods: three descriptors of nine plus five, and
  // `agent_list`'s six plus five.
  assert.equal(deletions.results.length, GOLDEN.requests_and_results.length);
  assert.equal(deletions.events.length, GOLDEN.events.length);
  assert.equal(resultCases.length, 270);
  assert.equal(eventCases.length, 223);
  assert.equal(deletions.error.length, 6);
  await withFakeServer(
    async (socket, index) => {
      const request = await readRequest(socket);
      if (index < resultCases.length) {
        const deletion = resultCases[index];
        const exchange = GOLDEN.requests_and_results.find(
          (candidate) => candidate.method === deletion.method,
        );
        const response = structuredClone(exchange.response);
        response.request_id = request.request_id;
        removePointer(response, deletion.pointer);
        writeJson(socket, response);
        return;
      }
      const eventIndex = index - resultCases.length;
      if (eventIndex < eventCases.length) {
        const deletion = eventCases[eventIndex];
        const frame = structuredClone(
          GOLDEN.events.find((candidate) => candidate.type === deletion.event_type).frame,
        );
        frame.sequence = 1;
        removePointer(frame, deletion.pointer);
        writeJson(socket, {
          version: 1,
          request_id: request.request_id,
          result: { type: "events", data: { events: [frame], next_sequence: 2 } },
        });
        return;
      }
      const errorPointer = deletions.error[eventIndex - eventCases.length];
      const response = structuredClone(GOLDEN.error);
      response.request_id = request.request_id;
      removePointer(response, errorPointer);
      writeJson(socket, response);
    },
    async (socketPath) => {
      const client = new PmuxClient(socketPath);
      for (const deletion of resultCases) {
        await assert.rejects(
          invokeTyped(client, requestFor(deletion.method)),
          PmuxProtocolError,
          `${deletion.method} ${deletion.pointer}`,
        );
      }
      const subscription = requestFor("subscribe_events");
      subscription.params.after_sequence = 0;
      for (const deletion of eventCases) {
        await assert.rejects(
          invokeTyped(client, structuredClone(subscription)),
          PmuxProtocolError,
          `${deletion.event_type} ${deletion.pointer}`,
        );
      }
      for (const pointer of deletions.error) {
        await assert.rejects(client.ping(), PmuxProtocolError, `error ${pointer}`);
      }
    },
  );
});

test("shared goldens accept additions at every result, event, and error object boundary", async () => {
  const successes = [];
  let resultBoundaries = 0;
  for (const exchange of GOLDEN.requests_and_results) {
    for (const pointer of objectPointers(exchange.response)) {
      resultBoundaries += 1;
      const response = structuredClone(exchange.response);
      insertAdditiveField(response, pointer);
      const entry = {
        label: `${exchange.method} ${JSON.stringify(pointer)}`,
        request: requestFor(exchange.method),
        response,
      };
      successes.push(entry);
    }
  }
  // 58 before `diagnose`; its exchange adds six object boundaries -- the
  // envelope, `result`, `result/data`, `result/data/runtime`, and one per entry
  // of `result/data/sessions` -- and every one must still tolerate an unknown
  // field, because response DTOs evolve additively. 64 before `run_stateless`,
  // whose exchange adds eight: the envelope, `result`, `result/data`,
  // `result/data/stop_reason`, `result/data/usage`, and one per usage scope.
  // 72 before the agent methods, whose four exchanges add 46. The echoed `spec`
  // is OPAQUE on a response, so its boundaries are additive like every other.
  assert.equal(resultBoundaries, 118, "review new result object boundaries");

  let eventBoundaries = 0;
  const subscription = requestFor("subscribe_events");
  subscription.params.after_sequence = 0;
  for (const event of GOLDEN.events) {
    const baseFrame = structuredClone(event.frame);
    baseFrame.sequence = 1;
    for (const pointer of objectPointers(baseFrame)) {
      eventBoundaries += 1;
      const frame = structuredClone(baseFrame);
      insertAdditiveField(frame, pointer);
      successes.push({
        label: `${event.type} ${JSON.stringify(pointer)}`,
        request: structuredClone(subscription),
        response: {
          version: 1,
          request_id: GOLDEN.ids.request_id,
          result: { type: "events", data: { events: [frame], next_sequence: 2 } },
        },
      });
    }
  }
  assert.equal(eventBoundaries, 67, "review new event object boundaries");

  const additiveErrors = objectPointers(GOLDEN.error).map((pointer) => {
    const response = structuredClone(GOLDEN.error);
    insertAdditiveField(response, pointer);
    return { label: `error ${JSON.stringify(pointer)}`, response };
  });
  assert.equal(additiveErrors.length, 3, "review new error object boundaries");

  await withFakeServer(
    async (socket, index) => {
      const request = await readRequest(socket);
      const source =
        index < successes.length
          ? successes[index].response
          : additiveErrors[index - successes.length].response;
      writeJson(socket, { ...structuredClone(source), request_id: request.request_id });
    },
    async (socketPath) => {
      const client = new PmuxClient(socketPath);
      for (const mutation of successes) {
        await assert.doesNotReject(
          invokeTyped(client, structuredClone(mutation.request)),
          mutation.label,
        );
      }
      for (const mutation of additiveErrors) {
        await assert.rejects(client.ping(), PmuxServerError, mutation.label);
      }
    },
  );
});

test("reserved turn leases are sent then surface stable unsupported_feature errors", async () => {
  const reserved = CASES.reserved_turn_lease_cases;
  assert.deepEqual(reserved.expected_error, { code: "unsupported_feature", retryable: false });
  assert.equal(reserved.cases.length, 6);
  const requests = reserved.cases.map((vector) => {
    const request = requestFor(vector.operation);
    request.params.turn.lease = structuredClone(vector.lease);
    return { ...vector, request };
  });

  await withFakeServer(
    async (socket, index) => {
      const actual = await readRequest(socket);
      assert.deepEqual(
        { ...actual, request_id: GOLDEN.ids.request_id },
        requests[index].request,
        requests[index].id,
      );
      writeJson(socket, {
        version: 1,
        request_id: actual.request_id,
        error: {
          ...reserved.expected_error,
          message: "reserved turn lease values require a future leased connection API",
        },
      });
    },
    async (socketPath) => {
      const client = new PmuxClient(socketPath);
      for (const vector of requests) {
        let error;
        try {
          await invokeTyped(client, structuredClone(vector.request));
        } catch (caught) {
          error = caught;
        }
        assert.ok(error instanceof PmuxServerError, vector.id);
        assert.equal(error.body.code, "unsupported_feature", vector.id);
        assert.equal(error.body.retryable, false, vector.id);
      }
    },
  );
});

test("durable UUIDv5 goldens are sourced from the same complete frame corpus", () => {
  assert.equal(GOLDEN.durable_ids.namespace, "7ec46f2d-5f29-5ebc-9ac1-925b0a76f76d");
  assert.ok(Array.isArray(GOLDEN.durable_ids.cases));
  assert.ok(GOLDEN.durable_ids.cases.length > 0);
  for (const vector of GOLDEN.durable_ids.cases) {
    assert.equal(typeof vector.attempt, "string");
    assert.match(
      vector.turn_id,
      /^[0-9a-f]{8}-[0-9a-f]{4}-5[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i,
    );
  }
});

test("golden.json carries one complete frame for every manifest event", () => {
  // DERIVED FROM THE MANIFEST, exactly as the method coverage above is. That
  // fix was applied to the method half and not to the event half: the Rust
  // checker kept two hand-written `14`s in the same file and the same commit
  // that derived the method count, and neither client asserted event coverage
  // at all -- both compared the corpus to itself. MEASURED, appending
  // `"future_event"` to `manifest.events` left every golden test in all three
  // languages green.
  assert.deepEqual(
    GOLDEN.events.map((event) => event.type).sort(),
    [...MANIFEST.events].sort(),
    "golden.json must carry one complete frame for every manifest event",
  );
});

test("shared manifest value enums match the TypeScript unions", () => {
  const actual = Object.fromEntries(
    Object.entries(V1_VALUE_ENUMS).map(([name, values]) => [name, [...values]]),
  );
  assert.deepEqual(actual, MANIFEST.value_enums);
});

test("shared manifest tagged unions match the TypeScript unions", () => {
  // The arrays behind this map are tied to the union types by `satisfies` in
  // `protocol.ts`, so `tsc` refuses a variant that is in one and not the other.
  // What `tsc` cannot see is order -- it compares sets -- and the manifest is
  // an ordered list, so this is the only check that the variants are pinned in
  // the declaration order Rust emits them in.
  const actual = Object.fromEntries(
    Object.entries(V1_TAGGED_UNIONS).map(([name, { tag, variants }]) => [
      name,
      { tag, variants: [...variants] },
    ]),
  );
  assert.deepEqual(actual, MANIFEST.tagged_unions);
});
