import assert from "node:assert/strict";
import crypto from "node:crypto";
import { after, before, test } from "node:test";
import Redis from "ioredis";

import {
  cleanupRedisPrefix,
  findFreePort,
  randomId,
  runMockClientScenario,
  startAuthHttpServer,
  startGameProxy,
  startGameServer
} from "../helpers/runtime.mjs";

const redisUrl = process.env.TEST_REDIS_URL || "redis://127.0.0.1:6379";
const ticketSecret = "test-only-ticket-secret";
const redisKeyPrefix = `test:integration:${randomId("redis")}:`;
const proxyAdminToken = "dev-only-change-this-proxy-admin-token";

let authServer;
let gameServer;
let gameProxy;
let ticketCounter = 1;

function hashTicket(ticket) {
  return crypto.createHash("sha256").update(ticket).digest("hex");
}

function signTicketPayload(payloadB64) {
  return crypto.createHmac("sha256", ticketSecret).update(payloadB64).digest("base64url");
}

async function createTestTicket({ suffix = String(ticketCounter++), ttlSeconds = 300 } = {}) {
  const playerId = `player-${suffix}`;
  const characterId = `chr_${String(suffix).padStart(13, "0")}`;
  const payload = {
    playerId,
    characterId,
    nonce: crypto.randomBytes(12).toString("hex"),
    ver: 1,
    exp: new Date(Date.now() + ttlSeconds * 1000).toISOString()
  };
  const payloadB64 = Buffer.from(JSON.stringify(payload)).toString("base64url");
  const ticket = `${payloadB64}.${signTicketPayload(payloadB64)}`;
  const redis = new Redis(redisUrl, {
    lazyConnect: true,
    maxRetriesPerRequest: 1,
    enableReadyCheck: true
  });

  await redis.connect();
  try {
    await redis.set(`${redisKeyPrefix}ticket:${hashTicket(ticket)}`, playerId, "EX", ttlSeconds);
    await redis.set(`${redisKeyPrefix}player-ticket-version:${playerId}`, "1", "EX", ttlSeconds);
  } finally {
    await redis.quit();
  }

  return { playerId, characterId, ticket };
}

async function runIntegrationMockClientScenario(options) {
  const ticketA = await createTestTicket();
  const ticketB = await createTestTicket();
  const ticketC = await createTestTicket();

  return runMockClientScenario({
    httpBaseUrl: authServer.baseUrl,
    host: gameServer.host,
    port: gameServer.port,
    ticket: ticketA.ticket,
    ticketA: ticketA.ticket,
    ticketB: ticketB.ticket,
    ticketC: ticketC.ticket,
    ...options
  });
}

before(async () => {
  const authPort = await findFreePort();
  const gamePort = await findFreePort();
  const adminPort = await findFreePort();
  const proxyPort = await findFreePort();
  const proxyAdminPort = await findFreePort();
  const proxyTcpFallbackPort = await findFreePort();
  const localSocketName = process.platform === "win32"
    ? randomId("game-server")
    : randomId("game-server") + ".sock";

  gameServer = await startGameServer({
    host: "127.0.0.1",
    port: gamePort,
    adminPort,
    localSocketName,
    ticketSecret,
    redisUrl,
    redisKeyPrefix
  });

  gameProxy = await startGameProxy({
    host: "127.0.0.1",
    port: proxyPort,
    adminPort: proxyAdminPort,
    tcpFallbackPort: proxyTcpFallbackPort,
    upstreamLocalSocketName: localSocketName
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
  if (gameProxy) {
    await gameProxy.close();
  }
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

test("game-proxy exposes active upstream status", async () => {
  const response = await fetch(`http://127.0.0.1:${gameProxy.adminPort}/status`, {
    headers: { authorization: `Bearer ${proxyAdminToken}` }
  });
  assert.equal(response.status, 200);
  const payload = await response.json();
  assert.equal(payload.ok, true);
  assert.equal(payload.active_upstream, "game-server-1");
});

test("mock-client scenarios cover core e2e flows", { timeout: 180000 }, async (t) => {
  await t.test("happy", async () => {
    await runIntegrationMockClientScenario({
      scenario: "happy",
      roomId: randomId("room-happy")
    });
  });

  await t.test("invalid-ticket", async () => {
    await runIntegrationMockClientScenario({
      scenario: "invalid-ticket",
      roomId: randomId("room-invalid")
    });
  });

  await t.test("unauth-room-join", async () => {
    await runIntegrationMockClientScenario({
      scenario: "unauth-room-join",
      roomId: randomId("room-unauth")
    });
  });

  await t.test("unknown-message", async () => {
    await runIntegrationMockClientScenario({
      scenario: "unknown-message",
    });
  });

  await t.test("oversized-room-join", async () => {
    await runIntegrationMockClientScenario({
      scenario: "oversized-room-join",
      roomId: randomId("room-oversized"),
      maxBodyLen: 8192
    });
  });

  await t.test("two-client-room", async () => {
    await runIntegrationMockClientScenario({
      scenario: "two-client-room",
      roomId: randomId("room-multi")
    });
  });

  await t.test("start-game-single-client", async () => {
    await runIntegrationMockClientScenario({
      scenario: "start-game-single-client",
      roomId: randomId("room-start-single")
    });
  });

  await t.test("start-game-ready-room", async () => {
    await runIntegrationMockClientScenario({
      scenario: "start-game-ready-room",
      roomId: randomId("room-start-ready")
    });
  });

  await t.test("gameplay-roundtrip", async () => {
    await runIntegrationMockClientScenario({
      scenario: "gameplay-roundtrip",
      roomId: randomId("room-gameplay")
    });
  });
  await t.test("get-room-data-in-room", async () => {
    await runIntegrationMockClientScenario({
      scenario: "get-room-data-in-room",
      roomId: randomId("room-data")
    });
  });
});

test("mock-client scenarios cover drain guard flows", { timeout: 180000 }, async (t) => {
  await t.test("drain-new-room-rejected", async () => {
    await runIntegrationMockClientScenario({
      scenario: "drain-new-room-rejected",
      roomId: randomId("room-drain-new")
    });
  });

  await t.test("drain-existing-room-join", async () => {
    await runIntegrationMockClientScenario({
      scenario: "drain-existing-room-join",
      roomId: randomId("room-drain-join")
    });
  });

  await t.test("drain-existing-room-reconnect", async () => {
    await runIntegrationMockClientScenario({
      scenario: "drain-existing-room-reconnect",
      roomId: randomId("room-drain-reconnect"),
      timeoutMs: 10000
    });
  });

  await t.test("drain-existing-room-observer", async () => {
    await runIntegrationMockClientScenario({
      scenario: "drain-existing-room-observer",
      roomId: randomId("room-drain-observer")
    });
  });

  await t.test("drain-create-matched-room-rejected", async () => {
    await runIntegrationMockClientScenario({
      scenario: "drain-create-matched-room-rejected",
      roomId: randomId("room-drain-match")
    });
  });
});
