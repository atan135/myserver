import assert from "node:assert/strict";
import { after, before, test } from "node:test";

import {
  cleanupRedisPrefix,
  findFreePort,
  randomId,
  runMockClientScenario,
  startAuthHttpServer,
  startGameServer
} from "./helpers/runtime.mjs";

const redisUrl = process.env.TEST_REDIS_URL || "redis://127.0.0.1:6379";
const ticketSecret = "test-only-ticket-secret";
const redisKeyPrefix = `test:integration:${randomId("redis")}:`;

let authServer;
let gameServer;

before(async () => {
  const authPort = await findFreePort();
  const gamePort = await findFreePort();
  const adminPort = await findFreePort();

  gameServer = await startGameServer({
    host: "127.0.0.1",
    port: gamePort,
    adminPort,
    ticketSecret,
    redisUrl,
    redisKeyPrefix
  });

  authServer = await startAuthHttpServer({
    host: "127.0.0.1",
    port: authPort,
    ticketSecret,
    redisUrl,
    redisKeyPrefix,
    gameServerAdminHost: "127.0.0.1",
    gameServerAdminPort: adminPort
  });
});

after(async () => {
  if (gameServer) {
    await gameServer.close();
  }
  if (authServer) {
    await authServer.close();
  }
  await cleanupRedisPrefix(redisUrl, redisKeyPrefix);
});

test("auth-http proxies protobuf admin calls to game-server", async () => {
  const statusResponse = await fetch(`${authServer.baseUrl}/api/v1/internal/game-server/status`);
  assert.equal(statusResponse.status, 200);
  const statusPayload = await statusResponse.json();
  assert.equal(statusPayload.ok, true);
  assert.equal(statusPayload.status, "ok");
  assert.equal(statusPayload.maxBodyLen, 4096);
  assert.equal(statusPayload.heartbeatTimeoutSecs, 10);

  const updateResponse = await fetch(`${authServer.baseUrl}/api/v1/internal/game-server/config`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ key: "max_body_len", value: "8192" })
  });
  assert.equal(updateResponse.status, 200);
  assert.deepEqual(await updateResponse.json(), {
    ok: true,
    errorCode: ""
  });

  const updatedStatusResponse = await fetch(`${authServer.baseUrl}/api/v1/internal/game-server/status`);
  assert.equal(updatedStatusResponse.status, 200);
  const updatedStatusPayload = await updatedStatusResponse.json();
  assert.equal(updatedStatusPayload.ok, true);
  assert.equal(updatedStatusPayload.maxBodyLen, 8192);
});

test("mock-client scenarios cover core e2e flows", { timeout: 180000 }, async (t) => {
  await t.test("happy", async () => {
    await runMockClientScenario({
      scenario: "happy",
      httpBaseUrl: authServer.baseUrl,
      host: gameServer.host,
      port: gameServer.port,
      roomId: randomId("room-happy")
    });
  });

  await t.test("invalid-ticket", async () => {
    await runMockClientScenario({
      scenario: "invalid-ticket",
      httpBaseUrl: authServer.baseUrl,
      host: gameServer.host,
      port: gameServer.port,
      roomId: randomId("room-invalid")
    });
  });

  await t.test("unauth-room-join", async () => {
    await runMockClientScenario({
      scenario: "unauth-room-join",
      httpBaseUrl: authServer.baseUrl,
      host: gameServer.host,
      port: gameServer.port,
      roomId: randomId("room-unauth")
    });
  });

  await t.test("unknown-message", async () => {
    await runMockClientScenario({
      scenario: "unknown-message",
      httpBaseUrl: authServer.baseUrl,
      host: gameServer.host,
      port: gameServer.port
    });
  });

  await t.test("oversized-room-join", async () => {
    await runMockClientScenario({
      scenario: "oversized-room-join",
      httpBaseUrl: authServer.baseUrl,
      host: gameServer.host,
      port: gameServer.port,
      roomId: randomId("room-oversized"),
      maxBodyLen: 8192
    });
  });

  await t.test("two-client-room", async () => {
    await runMockClientScenario({
      scenario: "two-client-room",
      httpBaseUrl: authServer.baseUrl,
      host: gameServer.host,
      port: gameServer.port,
      roomId: randomId("room-multi")
    });
  });

  await t.test("start-game-single-client", async () => {
    await runMockClientScenario({
      scenario: "start-game-single-client",
      httpBaseUrl: authServer.baseUrl,
      host: gameServer.host,
      port: gameServer.port,
      roomId: randomId("room-start-single")
    });
  });

  await t.test("start-game-ready-room", async () => {
    await runMockClientScenario({
      scenario: "start-game-ready-room",
      httpBaseUrl: authServer.baseUrl,
      host: gameServer.host,
      port: gameServer.port,
      roomId: randomId("room-start-ready")
    });
  });

  await t.test("gameplay-roundtrip", async () => {
    await runMockClientScenario({
      scenario: "gameplay-roundtrip",
      httpBaseUrl: authServer.baseUrl,
      host: gameServer.host,
      port: gameServer.port,
      roomId: randomId("room-gameplay")
    });
  });
});
