import assert from "node:assert/strict";
import { createServer } from "node:http";
import { createServer as createNetServer } from "node:net";
import { test } from "node:test";

import {
  PMUX_CONVERSATION_HEADER,
  PmuxMessages,
  setConversationHeader,
} from "../dist/messages.js";

test("setConversationHeader writes the pin", () => {
  const headers = {};
  setConversationHeader(headers, " sess-1 ");
  assert.equal(headers[PMUX_CONVERSATION_HEADER], "sess-1");
});

test("empty conversation id is rejected", async () => {
  const headers = {};
  assert.throws(() => setConversationHeader(headers, "  "), /must not be empty/);
  assert.throws(() => setConversationHeader(headers, ""), /must not be empty/);
  await assert.rejects(
    new PmuxMessages({ baseUrl: "http://127.0.0.1:8765" }).release(""),
    /must not be empty/,
  );
  await assert.rejects(
    new PmuxMessages({ baseUrl: "http://127.0.0.1:8765" }).release(" \t "),
    /must not be empty/,
  );
  assert.deepEqual(headers, {});
});

test("path-unsafe conversation ids are rejected", async () => {
  const headers = {};
  const client = new PmuxMessages({ baseUrl: "http://127.0.0.1:8765" });
  for (const id of ["a/b", "a b", "a?b", "a#b"]) {
    assert.throws(() => setConversationHeader(headers, id), /path-safe/);
    await assert.rejects(client.release(id), /path-safe/);
  }
  assert.deepEqual(headers, {});
});

test("loopback http urls parse", () => {
  const ipv4 = new PmuxMessages({ baseUrl: " http://127.0.0.1:8765/ " });
  assert.equal(ipv4.baseUrl, "http://127.0.0.1:8765");
  assert.equal(ipv4.apiKey, "pmux");
  const ipv6 = new PmuxMessages({ baseUrl: "http://[::1]:8765" });
  assert.equal(ipv6.baseUrl, "http://[::1]:8765");
  const local = new PmuxMessages({ baseUrl: "http://localhost:8765" });
  assert.equal(local.baseUrl, "http://localhost:8765");
  assert.equal(new PmuxMessages({ baseUrl: "http://127.0.0.1:8765", apiKey: "  " }).apiKey, "pmux");
});

test("api_key with CR, LF, or NUL is refused", () => {
  for (const key of ["a\nb", "a\rb", "a\0b", "a\r\nb"]) {
    assert.throws(
      () => new PmuxMessages({ baseUrl: "http://127.0.0.1:8765", apiKey: key }),
      /CR, LF, or NUL/,
    );
  }
  assert.equal(new PmuxMessages({ baseUrl: "http://127.0.0.1:8765", apiKey: " k " }).apiKey, "k");
});

test("https and non-loopback urls are refused", () => {
  assert.throws(() => new PmuxMessages({ baseUrl: "https://127.0.0.1:8765" }), /http:\/\/HOST:PORT/);
  assert.throws(() => new PmuxMessages({ baseUrl: "http://192.168.1.4:8765" }), /loopback-only/);
  assert.throws(() => new PmuxMessages({ baseUrl: "http://127.0.0.2:8765" }), /loopback-only/);
  assert.throws(() => new PmuxMessages({ baseUrl: "http://example.com:8765" }), /loopback-only/);
  assert.throws(() => new PmuxMessages({ baseUrl: "http://127.0.0.1" }), /http:\/\/HOST:PORT/);
  assert.throws(() => new PmuxMessages({ baseUrl: "   " }), /must not be empty/);
  assert.throws(
    () => new PmuxMessages({ baseUrl: "http://user:pass@127.0.0.1:8765" }),
    /http:\/\/HOST:PORT/,
  );
});

test("PmuxMessages models and release speak HTTP", async () => {
  const seen = [];
  const server = createServer((req, res) => {
    seen.push(`${req.method} ${req.url}`);
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ ok: true, path: req.url }));
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  const client = new PmuxMessages({ baseUrl: `http://127.0.0.1:${port}` });
  await client.models();
  await client.release("abc");
  await client.release("a!b");
  await client.release("100%");
  server.close();
  assert.deepEqual(seen, [
    "GET /v1/models",
    "POST /v1/conversations/abc/release",
    "POST /v1/conversations/a!b/release",
    "POST /v1/conversations/100%/release",
  ]);
});

test("exchange times out when the peer sends nothing", async () => {
  const sockets = [];
  const server = createNetServer((socket) => {
    sockets.push(socket);
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  const client = new PmuxMessages({
    baseUrl: `http://127.0.0.1:${port}`,
    timeoutMs: 50,
  });
  await assert.rejects(client.models(), /timed out/i);
  for (const socket of sockets) {
    socket.destroy();
  }
  await new Promise((resolve) => server.close(resolve));
});

test("exchange refuses an oversized response", async () => {
  const body = "x".repeat(2 * 1024 * 1024 + 1);
  const server = createServer((_req, res) => {
    res.on("error", () => {});
    res.writeHead(200, {
      "content-type": "application/json",
      "content-length": String(body.length),
    });
    res.end(body);
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  const client = new PmuxMessages({ baseUrl: `http://127.0.0.1:${port}` });
  await assert.rejects(client.models(), /exceeds/);
  await new Promise((resolve) => server.close(resolve));
});
