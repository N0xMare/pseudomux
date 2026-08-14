import { createHash } from "node:crypto";
import { readFileSync, realpathSync, statSync } from "node:fs";
import { createConnection } from "node:net";
import { isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import { TYPESCRIPT_DIST_FILES, verifyTypescriptDistStage } from "./dist-stage.mjs";

const SOURCE_MANIFEST_PATHS = [
  "package.json",
  "src/client.ts",
  "src/index.ts",
  "src/protocol.ts",
  "src/smithers.ts",
  "tests/actual_daemon_e2e.mjs",
  "tests/dist-stage.mjs",
];
const DIST_MANIFEST_PATHS = TYPESCRIPT_DIST_FILES.map((name) => `dist/${name}`);
const MANIFEST_PATHS = [...SOURCE_MANIFEST_PATHS, ...DIST_MANIFEST_PATHS];

const DIAGNOSTIC_MESSAGE_BYTES = 2 * 1024;
const DIAGNOSTIC_STACK_FRAMES = 12;
const DIAGNOSTIC_STACK_BYTES = 6 * 1024;
const diagnosticSecrets = new Set();

function registerDiagnosticSecret(value) {
  if (typeof value === "string" && value.length > 0) diagnosticSecrets.add(value);
}

function boundedUtf8(value, maxBytes) {
  const bytes = Buffer.from(value, "utf8");
  if (bytes.length <= maxBytes) return value;
  const marker = Buffer.from("<truncated>", "utf8");
  return `${bytes.subarray(0, maxBytes - marker.length).toString("utf8")}${marker.toString("utf8")}`;
}

function sanitizeDiagnostic(value, maxBytes) {
  let sanitized = String(value);
  for (const secret of [...diagnosticSecrets].sort((left, right) => right.length - left.length)) {
    sanitized = sanitized.replaceAll(secret, "<redacted>");
  }
  sanitized = sanitized.replaceAll(/PMUX_TEST_[A-Z0-9_]+/g, "<redacted-prompt>");
  return boundedUtf8(sanitized, maxBytes);
}

function diagnosticStack(error) {
  if (!(error instanceof Error) || typeof error.stack !== "string") return [];
  const frames = error.stack
    .split("\n")
    .slice(1)
    .filter((line) => /^\s*at\s/.test(line))
    .slice(0, DIAGNOSTIC_STACK_FRAMES);
  const bounded = sanitizeDiagnostic(frames.join("\n"), DIAGNOSTIC_STACK_BYTES);
  return bounded.length > 0 ? bounded.split("\n") : [];
}

function check(condition, label) {
  if (!condition) throw new Error(`cross-client assertion failed: ${label}`);
}

function digest(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function identity(path) {
  const canonical = realpathSync(path);
  const metadata = statSync(canonical);
  check(metadata.isFile(), "identity target is a regular file");
  return { path: canonical, sha256: digest(canonical) };
}

function sourceManifest(sourceRoot, distStage) {
  const source = SOURCE_MANIFEST_PATHS.map((name) => {
    const asset = identity(join(sourceRoot, name));
    check(relative(sourceRoot, asset.path) === name, `unexpected source path for ${name}`);
    return { relative_path: name, sha256: asset.sha256 };
  });
  const dist = distStage.manifest.map((record) => ({
    relative_path: `dist/${record.relative_path}`,
    sha256: record.sha256,
  }));
  check(source.length + dist.length === MANIFEST_PATHS.length, "complete source manifest");
  return [...source, ...dist];
}

function canonicalDirectoryArgument(index, label) {
  const supplied = process.argv[index];
  check(typeof supplied === "string" && isAbsolute(supplied), `${label} is absolute`);
  check(resolve(supplied) === supplied, `${label} is normalized`);
  const canonical = realpathSync(supplied);
  check(canonical === supplied, `${label} is canonical`);
  check(statSync(canonical).isDirectory(), `${label} is a directory`);
  return canonical;
}

function makeStart(config, identityValue, retention) {
  return {
    identity: identityValue,
    cwd: config.cwd,
    claude: {
      executable: config.claude_executable,
      model: "test-model",
      permission_mode: "default",
    },
    environment: config.environment,
    auth_policy: "subscription",
    terminal: {
      rows: 24,
      cols: 120,
      profile: "transparent",
      input_transport: "sdk",
    },
    lifecycle: { mode: "transcript" },
    retention,
    compatibility: "require_tested",
  };
}

function makeTurn(turnId, prompt) {
  return {
    turn_id: turnId,
    prompt,
    deadline_unix_ms: Date.now() + 30_000,
    lease: { on_disconnect: "continue" },
  };
}

function validateCompleted(result, sessionId, turnId) {
  check(result.session_id === sessionId, "completed session identity");
  check(result.turn_id === turnId, "completed turn identity");
  check(result.outcome === "completed", "completed outcome");
  check(result.text === "pmux-test-ok", "completed text");
  check(result.model === "pmux-test-model", "completed model");
  check(result.usage.main.input_tokens === 3, "completed input usage");
  check(result.usage.main.output_tokens === 1, "completed output usage");
  check(result.completion.authority === "transcript", "completion authority");
  check(result.completion.prompt_acknowledged, "prompt acknowledgement provenance");
  check(result.completion.terminal_message_observed, "terminal message provenance");
  check(result.completion.terminal_prompt_observed, "terminal prompt provenance");
  check(result.completion.terminal_quiet_observed, "terminal quiet provenance");
  check(result.completion.transcript_drained, "transcript drain provenance");
  check(result.compatibility.tested, "tested compatibility");
}

async function submitWhenReady(api, client, handle, turn) {
  for (let attempt = 0; attempt < 200; attempt += 1) {
    try {
      return await client.runTurn(handle.session_id, handle.generation_id, turn);
    } catch (error) {
      if (!(error instanceof api.PmuxServerError) || error.body.code !== "session_busy") throw error;
      await new Promise((resolveDelay) => setTimeout(resolveDelay, 25));
    }
  }
  throw new Error("session remained busy after bounded attach reconciliation");
}

async function observeUntil(client, handle, afterSequence, turnId, predicate, label) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 35_000);
  let cursor = afterSequence;
  let count = 0;
  let reconnects = 0;
  try {
    for await (const item of client.events(handle.session_id, handle.generation_id, {
      afterSequence,
      waitMs: 1_000,
      maxEvents: 128,
      reconnectDelayMs: 10,
      maxReconnectAttempts: 3,
      signal: controller.signal,
      onReconnect: () => {
        reconnects += 1;
      },
    })) {
      check(item.kind === "event", `${label} did not cross a replay gap`);
      const event = item.event;
      check(event.sequence === cursor + 1, `${label} sequence continuity`);
      cursor = event.sequence;
      count += 1;
      if (event.turn_id === turnId && predicate(event.event)) {
        return { event, first_sequence: afterSequence + 1, last_sequence: cursor, count, reconnects };
      }
    }
  } catch (error) {
    if (controller.signal.aborted) throw new Error(`${label} exceeded its bounded deadline`);
    throw error;
  } finally {
    clearTimeout(timer);
  }
  throw new Error(`${label} ended without the requested event`);
}

async function expectServerCode(api, operation, expected) {
  try {
    await operation();
  } catch (error) {
    if (error instanceof api.PmuxServerError && error.body.code === expected) return expected;
    throw error;
  }
  throw new Error(`operation unexpectedly succeeded instead of returning ${expected}`);
}

function attachExchange(endpoint, token, requireBytes) {
  return new Promise((resolveExchange, rejectExchange) => {
    let settled = false;
    let received = 0;
    const finish = (error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      socket.destroy();
      if (error) rejectExchange(error);
      else resolveExchange(received);
    };
    const socket = createConnection({ path: endpoint });
    const timer = setTimeout(() => {
      if (requireBytes) finish(new Error("attach stream timed out before terminal bytes"));
      else finish();
    }, 3_000);
    socket.once("connect", () => {
      const tokenBytes = Buffer.from(token, "utf8");
      const prefix = Buffer.allocUnsafe(4);
      prefix.writeUInt32BE(tokenBytes.length);
      socket.write(Buffer.concat([prefix, tokenBytes]));
    });
    socket.on("data", (chunk) => {
      received += chunk.length;
      if (requireBytes && received > 0) finish();
      else if (!requireBytes && received > 0) {
        finish(new Error("one-use attach capability returned bytes twice"));
      }
    });
    socket.once("end", () => {
      if (requireBytes && received === 0) finish(new Error("attach stream ended without bytes"));
      else finish();
    });
    socket.once("error", (error) => {
      if (requireBytes) finish(error);
      else finish();
    });
  });
}

async function consumeOneUseCapability(capability, handle) {
  check(capability.session_id === handle.session_id, "attach session identity");
  check(capability.generation_id === handle.generation_id, "attach generation identity");
  check(capability.read_only === false, "attach writable metadata");
  check(isAbsolute(capability.endpoint), "attach endpoint is absolute");
  check(capability.expires_at_ms > Date.now(), "attach expiry is in the future");
  registerDiagnosticSecret(capability.endpoint);
  registerDiagnosticSecret(capability.token);
  const firstBytes = await attachExchange(capability.endpoint, capability.token, true);
  check(firstBytes > 0, "first attach stream returned terminal bytes");
  await new Promise((resolveDelay) => setTimeout(resolveDelay, 50));
  const secondBytes = await attachExchange(capability.endpoint, capability.token, false);
  check(secondBytes === 0, "attach capability is one use");
  return { metadata_valid: true, first_stream_bytes: firstBytes, reuse_rejected: true };
}

async function main() {
  check(process.argv.length === 5, "expected config, client-root, and dist-root arguments");
  const configPath = realpathSync(process.argv[2]);
  const clientRoot = canonicalDirectoryArgument(3, "client root");
  check(isAbsolute(configPath), "configuration path is absolute");
  const workspaceRoot = realpathSync(join(clientRoot, "../.."));
  const distStage = await verifyTypescriptDistStage(process.argv[4], { outsideRoot: workspaceRoot });
  const distRoot = distStage.root;
  const config = JSON.parse(readFileSync(configPath, "utf8"));
  check(config.schema_version === 1, "configuration schema version");
  Object.values(config.prompts).forEach(registerDiagnosticSecret);

  const helperIdentity = identity(fileURLToPath(import.meta.url));
  check(
    helperIdentity.path === join(clientRoot, "tests/actual_daemon_e2e.mjs"),
    "helper loaded from the expected source root",
  );
  const entryPath = realpathSync(join(distRoot, "index.js"));
  const api = await import(pathToFileURL(entryPath).href);
  const packageMetadata = JSON.parse(readFileSync(join(clientRoot, "package.json"), "utf8"));
  check(api.PROTOCOL_VERSION === 1, "client protocol version");
  check(packageMetadata.name === "pmux-client", "client package name");

  const client = new api.PmuxClient(config.socket_path, {
    connectTimeoutMs: 5_000,
    requestTimeoutMs: 45_000,
  });
  const pong = await client.ping();
  check(pong.protocol_version === 1, "ping protocol version");

  const persistent = await client.startSession(
    makeStart(
      config,
      { mode: "new", session_id: config.ids.persistent_session },
      { mode: "persistent", idle_ttl_ms: 60_000 },
    ),
  );
  check(persistent.session_id === config.ids.persistent_session, "persistent session identity");
  check(persistent.compatibility.tested, "persistent tested compatibility");

  const firstTurnRequest = makeTurn(config.ids.first_turn, config.prompts.first);
  const firstAccepted = await submitWhenReady(api, client, persistent, firstTurnRequest);
  check(firstAccepted.replayed === false, "first turn was not replayed");
  const firstObserved = await observeUntil(
    client,
    persistent,
    firstAccepted.next_sequence - 1,
    config.ids.first_turn,
    (event) => event.type === "turn_completed" || event.type === "turn_failed",
    "first turn",
  );
  check(firstObserved.event.event.type === "turn_completed", "first terminal event completed");
  const firstResult = firstObserved.event.event.data;
  validateCompleted(firstResult, persistent.session_id, config.ids.first_turn);
  check(firstObserved.event.sequence === firstResult.final_sequence, "first final sequence");

  const replayAccepted = await client.runTurn(
    persistent.session_id,
    persistent.generation_id,
    firstTurnRequest,
  );
  check(replayAccepted.session_id === persistent.session_id, "replay session identity");
  check(replayAccepted.generation_id === persistent.generation_id, "replay generation identity");
  check(replayAccepted.turn_id === config.ids.first_turn, "replay turn identity");
  check(replayAccepted.replayed === true, "completed turn retry was replayed");
  check(replayAccepted.state === "ready", "replay preserved ready state");
  const replayObserved = await observeUntil(
    client,
    persistent,
    replayAccepted.next_sequence - 1,
    config.ids.first_turn,
    (event) => event.type === "turn_completed" || event.type === "turn_failed",
    "replayed first turn",
  );
  check(replayObserved.event.event.type === "turn_completed", "replay terminal event completed");
  const replayResult = replayObserved.event.event.data;
  validateCompleted(replayResult, persistent.session_id, config.ids.first_turn);
  check(replayObserved.event.sequence === replayResult.final_sequence, "replay final sequence");
  const conflictCode = await expectServerCode(
    api,
    () =>
      client.runTurn(persistent.session_id, persistent.generation_id, {
        ...firstTurnRequest,
        prompt: `${config.prompts.first}_CONFLICT`,
      }),
    "id_conflict",
  );

  const snapshot = await client.inspectSession(persistent.session_id, persistent.generation_id);
  check(snapshot.last_turn?.turn_id === config.ids.first_turn, "inspect last-turn identity");
  check(snapshot.last_sequence === replayResult.final_sequence, "inspect replay cursor");

  const capability = await client.attachSession({
    session_id: persistent.session_id,
    generation_id: persistent.generation_id,
    read_only: false,
  });
  const attach = await consumeOneUseCapability(capability, persistent);

  const cancelAccepted = await submitWhenReady(
    api,
    client,
    persistent,
    makeTurn(config.ids.cancel_turn, config.prompts.cancel),
  );
  const acknowledged = await observeUntil(
    client,
    persistent,
    cancelAccepted.next_sequence - 1,
    config.ids.cancel_turn,
    (event) => event.type === "prompt_acknowledged",
    "cancel prompt acknowledgement",
  );
  const cancelResult = await client.cancelTurn(
    persistent.session_id,
    persistent.generation_id,
    config.ids.cancel_turn,
  );
  check(
    cancelResult.outcome === "cancelled",
    `cancel result was ${cancelResult.outcome}`,
  );
  const cancelObserved = await observeUntil(
    client,
    persistent,
    acknowledged.last_sequence,
    config.ids.cancel_turn,
    (event) => event.type === "turn_cancelled" || event.type === "turn_failed",
    "cancel terminal event",
  );
  check(cancelObserved.event.event.type === "turn_cancelled", "cancel event type");
  check(cancelObserved.event.event.data.outcome === "cancelled", "cancel event outcome");
  check(cancelObserved.event.event.data.recovered_to_ready, "cancel recovery");

  const recoveryAccepted = await submitWhenReady(
    api,
    client,
    persistent,
    makeTurn(config.ids.recovery_turn, config.prompts.recovery),
  );
  const recoveryObserved = await observeUntil(
    client,
    persistent,
    recoveryAccepted.next_sequence - 1,
    config.ids.recovery_turn,
    (event) => event.type === "turn_completed" || event.type === "turn_failed",
    "recovery turn",
  );
  check(recoveryObserved.event.event.type === "turn_completed", "recovery terminal event");
  validateCompleted(recoveryObserved.event.event.data, persistent.session_id, config.ids.recovery_turn);

  const firstClose = await client.closeSession(
    persistent.session_id,
    persistent.generation_id,
    "graceful",
  );
  check(firstClose.process_reaped && !firstClose.already_closed, "persistent close proof");

  const resumed = await client.startSession(
    makeStart(
      config,
      { mode: "resume", session_id: persistent.session_id },
      { mode: "persistent", idle_ttl_ms: 60_000 },
    ),
  );
  check(resumed.generation_id !== persistent.generation_id, "resume generation changed");
  const staleCode = await expectServerCode(
    api,
    () => client.inspectSession(persistent.session_id, persistent.generation_id),
    "stale_session_generation",
  );
  const oldClose = await client.closeSession(
    persistent.session_id,
    persistent.generation_id,
    "force",
  );
  check(oldClose.already_closed && oldClose.process_reaped, "old close tombstone replay");

  const resumedAccepted = await submitWhenReady(
    api,
    client,
    resumed,
    makeTurn(config.ids.resumed_turn, config.prompts.resumed),
  );
  const resumedObserved = await observeUntil(
    client,
    resumed,
    resumedAccepted.next_sequence - 1,
    config.ids.resumed_turn,
    (event) => event.type === "turn_completed" || event.type === "turn_failed",
    "resumed turn",
  );
  check(resumedObserved.event.event.type === "turn_completed", "resumed terminal event");
  validateCompleted(resumedObserved.event.event.data, resumed.session_id, config.ids.resumed_turn);
  const resumedClose = await client.closeSession(
    resumed.session_id,
    resumed.generation_id,
    "graceful",
  );
  check(resumedClose.process_reaped, "resumed close proof");

  const onceResult = await client.runOnce({
    session: makeStart(
      config,
      { mode: "new", session_id: config.ids.once_session },
      { mode: "one_shot" },
    ),
    turn: makeTurn(config.ids.once_turn, config.prompts.once),
  });
  validateCompleted(onceResult, config.ids.once_session, config.ids.once_turn);

  const missingSocket = `${config.socket_path}.typescript-missing`;
  const transportClient = new api.PmuxClient(missingSocket, {
    connectTimeoutMs: 250,
    requestTimeoutMs: 250,
  });
  let transportError = false;
  try {
    await transportClient.ping();
  } catch (error) {
    transportError = error instanceof api.PmuxTransportError;
  }
  check(transportError, "missing socket maps to a transport error");

  const runtimePath = realpathSync(process.execPath);
  const report = {
    schema_version: 1,
    language: "typescript",
    runtime: { path: runtimePath, sha256: digest(runtimePath), version: process.version },
    helper: helperIdentity,
    client: {
      package_name: packageMetadata.name,
      package_version: packageMetadata.version,
      protocol_version: api.PROTOCOL_VERSION,
      source_root: clientRoot,
      dist_root: distRoot,
      dist_sha256: distStage.sha256,
      entry_path: entryPath,
      manifest: sourceManifest(clientRoot, distStage),
    },
    ping_protocol_version: pong.protocol_version,
    persistent: {
      session_id: persistent.session_id,
      generation_id: persistent.generation_id,
      first_turn_id: config.ids.first_turn,
      first_final_sequence: firstResult.final_sequence,
      first_event_count: firstObserved.count,
      reconnects: firstObserved.reconnects,
      inspected: true,
      closed_and_reaped: firstClose.process_reaped,
    },
    idempotency: {
      turn_id: config.ids.first_turn,
      initial_replayed: firstAccepted.replayed,
      replayed: replayAccepted.replayed,
      replay_final_sequence: replayResult.final_sequence,
      replay_event_count: replayObserved.count,
      reconnects: replayObserved.reconnects,
      conflict_error_code: conflictCode,
      conflict_preserved_cursor: snapshot.last_sequence === replayResult.final_sequence,
    },
    attach,
    cancellation: {
      turn_id: config.ids.cancel_turn,
      outcome: cancelResult.outcome,
      recovered_to_ready: cancelObserved.event.event.data.recovered_to_ready,
      recovery_turn_id: config.ids.recovery_turn,
      recovery_outcome: recoveryObserved.event.event.data.outcome,
    },
    resume: {
      generation_id: resumed.generation_id,
      stale_error_code: staleCode,
      old_close_replayed: oldClose.already_closed,
      turn_id: config.ids.resumed_turn,
      outcome: resumedObserved.event.event.data.outcome,
      closed_and_reaped: resumedClose.process_reaped,
    },
    run_once: {
      session_id: onceResult.session_id,
      generation_id: onceResult.generation_id,
      turn_id: onceResult.turn_id,
      outcome: onceResult.outcome,
      text: onceResult.text,
    },
    missing_socket_transport_error: transportError,
  };
  process.stdout.write(`${JSON.stringify(report)}\n`);
}

try {
  await main();
} catch (error) {
  const name = error instanceof Error ? error.name : "UnknownError";
  const code =
    typeof error === "object" &&
    error !== null &&
    "body" in error &&
    typeof error.body === "object" &&
    error.body !== null &&
    "code" in error.body
      ? error.body.code
      : "";
  const message = error instanceof Error ? error.message : String(error);
  const lines = [
    `pmux TypeScript cross-client E2E failed: ${sanitizeDiagnostic(name, 128)}:${sanitizeDiagnostic(String(code), 128)}`,
    `message: ${sanitizeDiagnostic(message, DIAGNOSTIC_MESSAGE_BYTES)}`,
    ...diagnosticStack(error),
  ];
  process.stderr.write(`${lines.join("\n")}\n`);
  process.exitCode = 1;
}
