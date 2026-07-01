import assert from "node:assert/strict";
import { after, before, test } from "node:test";

import { cleanupRedisPrefix, findFreePort, randomId, startAuthHttpServer } from "../helpers/runtime.mjs";

const redisUrl = process.env.TEST_REDIS_URL || "redis://127.0.0.1:6379";
const redisKeyPrefix = `test:reqid:${randomId("r")}:`;

let authServer;

before(async () => {
  authServer = await startAuthHttpServer({
    host: "127.0.0.1",
    port: await findFreePort(),
    ticketSecret: "test-secret",
    redisUrl,
    redisKeyPrefix
  });
});

after(async () => {
  if (authServer) {
    await authServer.close();
  }
  await cleanupRedisPrefix(redisUrl, redisKeyPrefix);
});

test("response includes auto-generated X-Request-Id header", async () => {
  const res = await fetch(`${authServer.baseUrl}/healthz`);
  const id = res.headers.get("x-request-id");
  assert.ok(id, "X-Request-Id header should be present");
  assert.equal(id.length, 16, "should be 16 hex chars (8 random bytes)");
  assert.match(id, /^[0-9a-f]{16}$/);
});

test("client-provided X-Request-Id is echoed back", async () => {
  const res = await fetch(`${authServer.baseUrl}/healthz`, {
    headers: { "x-request-id": "client-trace-abc123" }
  });
  assert.equal(res.headers.get("x-request-id"), "client-trace-abc123");
});

test("different requests get unique IDs", async () => {
  const r1 = await fetch(`${authServer.baseUrl}/healthz`);
  const r2 = await fetch(`${authServer.baseUrl}/healthz`);
  const id1 = r1.headers.get("x-request-id");
  const id2 = r2.headers.get("x-request-id");
  assert.notEqual(id1, id2);
});

test("X-Request-Id is present on error responses", async () => {
  const res = await fetch(`${authServer.baseUrl}/api/v1/nonexistent`);
  assert.equal(res.status, 404);
  const id = res.headers.get("x-request-id");
  assert.ok(id);
});

test("X-Request-Id is present on authenticated endpoints", async () => {
  const res = await fetch(`${authServer.baseUrl}/api/v1/auth/me`);
  assert.equal(res.status, 401);
  const id = res.headers.get("x-request-id");
  assert.ok(id);
  assert.match(id, /^[0-9a-f]{16}$/);
});
