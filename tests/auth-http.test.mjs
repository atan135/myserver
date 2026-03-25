import assert from "node:assert/strict";
import { after, before, test } from "node:test";

import { cleanupRedisPrefix, findFreePort, randomId, startAuthHttpServer } from "./helpers/runtime.mjs";

const redisUrl = process.env.TEST_REDIS_URL || "redis://127.0.0.1:6379";
const ticketSecret = "test-only-ticket-secret";
const redisKeyPrefix = `test:auth-http:${randomId("redis")}:`;

let authServer;

before(async () => {
  authServer = await startAuthHttpServer({
    host: "127.0.0.1",
    port: await findFreePort(),
    ticketSecret,
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

test("GET /healthz returns service health", async () => {
  const response = await fetch(`${authServer.baseUrl}/healthz`);
  assert.equal(response.status, 200);

  const payload = await response.json();
  assert.equal(payload.ok, true);
  assert.equal(payload.service, "auth-http");
  assert.equal(payload.storage, "redis");
});

test("guest login creates session and game ticket", async () => {
  const guestId = randomId("guest");
  const loginResponse = await fetch(`${authServer.baseUrl}/api/v1/auth/guest-login`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ guestId })
  });

  assert.equal(loginResponse.status, 201);
  const loginPayload = await loginResponse.json();
  assert.equal(loginPayload.ok, true);
  assert.equal(loginPayload.guestId, guestId);
  assert.match(loginPayload.playerId, /^player-/);
  assert.ok(loginPayload.accessToken);
  assert.ok(loginPayload.ticket);

  const meResponse = await fetch(`${authServer.baseUrl}/api/v1/auth/me`, {
    headers: {
      authorization: `Bearer ${loginPayload.accessToken}`
    }
  });

  assert.equal(meResponse.status, 200);
  const mePayload = await meResponse.json();
  assert.equal(mePayload.ok, true);
  assert.equal(mePayload.playerId, loginPayload.playerId);
  assert.equal(mePayload.guestId, guestId);

  const ticketResponse = await fetch(`${authServer.baseUrl}/api/v1/game-ticket/issue`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${loginPayload.accessToken}`
    }
  });

  assert.equal(ticketResponse.status, 201);
  const ticketPayload = await ticketResponse.json();
  assert.equal(ticketPayload.ok, true);
  assert.equal(ticketPayload.playerId, loginPayload.playerId);
  assert.ok(ticketPayload.ticket);
});

test("auth endpoints reject missing or invalid bearer token", async () => {
  const missingTokenResponse = await fetch(`${authServer.baseUrl}/api/v1/auth/me`);
  assert.equal(missingTokenResponse.status, 401);
  assert.deepEqual(await missingTokenResponse.json(), {
    ok: false,
    error: "MISSING_BEARER_TOKEN"
  });

  const invalidTokenResponse = await fetch(`${authServer.baseUrl}/api/v1/game-ticket/issue`, {
    method: "POST",
    headers: {
      authorization: "Bearer invalid-token"
    }
  });

  assert.equal(invalidTokenResponse.status, 401);
  assert.deepEqual(await invalidTokenResponse.json(), {
    ok: false,
    error: "INVALID_ACCESS_TOKEN"
  });
});
