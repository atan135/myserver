import assert from "node:assert/strict";
import crypto from "node:crypto";
import http from "node:http";
import { register } from "node:module";
import path from "node:path";
import { after, before, test } from "node:test";
import { pathToFileURL } from "node:url";
import Redis from "ioredis";

import { createServiceInstancePayload } from "../../packages/service-registry/node/registry-schema.js";
import { GameAdminClient } from "../../apps/admin-api/src/game-admin-client.js";

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
const gameServerInstanceId = "game-server-integration";
const adminApiToken = "integration-admin-api-token";
const gameAdminToken = "dev-only-change-this-game-admin-token";

process.env.TS_NODE_PROJECT = path.resolve("apps/admin-api/tsconfig.json");
process.env.TS_NODE_TRANSPILE_ONLY = "true";
register("ts-node/esm", pathToFileURL("./"));
const { AdminOperationAssertionService } = await import(
  "../../apps/admin-api/src/auth/admin-operation-assertion.service.ts"
);

let authServer;
let gameServer;
let gameProxy;
let adminControl;
let ticketCounter = 1;

function integrationGameServerRegistryInstance(adminPort) {
  return createServiceInstancePayload({
    id: gameServerInstanceId,
    name: "game-server",
    host: "127.0.0.1",
    port: 0,
    admin_port: adminPort,
    healthy: true,
    metadata: {
      service_name: "game-server",
      service_instance_id: gameServerInstanceId,
      server_id: gameServerInstanceId,
      zone: "integration",
      build_version: "integration"
    },
    endpoints: [
      {
        name: "admin",
        protocol: "tcp",
        host: "127.0.0.1",
        port: adminPort,
        visibility: "admin",
        healthy: true,
        metadata: {}
      }
    ]
  });
}

async function registerIntegrationGameServer(adminPort) {
  const redis = new Redis(redisUrl, { lazyConnect: true, maxRetriesPerRequest: 1 });
  await redis.connect();
  const instance = integrationGameServerRegistryInstance(adminPort);
  await redis.hset(
    `${redisKeyPrefix}service:game-server:instances:${gameServerInstanceId}`,
    "data",
    JSON.stringify(instance)
  );
  await redis.set(`${redisKeyPrefix}heartbeat:game-server:${gameServerInstanceId}`, "1", "EX", 300);
  await redis.zadd(
    `${redisKeyPrefix}service:game-server:instance-index`,
    Math.floor(Date.now() / 1000),
    gameServerInstanceId
  );
  return redis;
}

async function startIntegrationAdminControl({ adminPort, privateKeyPem }) {
  const redis = await registerIntegrationGameServer(adminPort);
  const config = {
    registryDiscoveryEnabled: true,
    registryDiscoveryRequired: true,
    localDiscoveryFallbackEnabled: false,
    registryKeyPrefix: redisKeyPrefix,
    gameAdminToken,
    gameAdminConnectTimeoutMs: 1000,
    gameAdminWriteTimeoutMs: 1000,
    gameAdminReadTimeoutMs: 3000,
    gameAdminMaxResponseBytes: 64 * 1024,
    adminAssertionIssuer: "admin-api",
    adminAssertionKeyId: "integration-v1",
    adminAssertionPrivateKey: privateKeyPem,
    adminAssertionTtlMs: 60_000
  };
  const policy = { async authorize() { return { allowed: true }; } };
  const assertions = new AdminOperationAssertionService(config, policy);
  const client = new GameAdminClient(config, redis, assertions);
  const server = http.createServer(async (req, res) => {
    res.setHeader("content-type", "application/json");
    if (req.headers.authorization !== `Bearer ${adminApiToken}`) {
      res.statusCode = 401;
      res.end(JSON.stringify({ ok: false, error: "UNAUTHORIZED" }));
      return;
    }
    const match = req.url?.match(/^\/api\/v1\/rollouts\/game-server\/([^/]+)\/(drain|shutdown)$/);
    if (req.method !== "POST" || !match || decodeURIComponent(match[1]) !== gameServerInstanceId) {
      res.statusCode = 404;
      res.end(JSON.stringify({ ok: false, error: "GAME_SERVER_ADMIN_TARGET_NOT_FOUND" }));
      return;
    }
    const chunks = [];
    for await (const chunk of req) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks).toString("utf8") || "{}");
    if (!body.preflightNonce || !body.preflightSummarySha256) {
      res.end(JSON.stringify({
        ok: true,
        preflight: { nonce: `nonce-${body.requestId}`, summarySha256: `summary-${body.requestId}` }
      }));
      return;
    }
    await redis.set(`${redisKeyPrefix}heartbeat:game-server:${gameServerInstanceId}`, "1", "EX", 300);
    await redis.zadd(
      `${redisKeyPrefix}service:game-server:instance-index`,
      Math.floor(Date.now() / 1000),
      gameServerInstanceId
    );
    const targetType = match[2] === "drain" ? "config" : "service";
    const targetIds = match[2] === "drain" ? ["drain_mode"] : [gameServerInstanceId];
    const assertionContext = {
      actorId: "integration-admin",
      permission: "game.config.write",
      scope: {
        serviceName: "game-server",
        instanceId: gameServerInstanceId,
        targetType,
        targetIds,
        targetCount: 1
      },
      target: { targetType, targetIds },
      requestId: body.requestId,
      traceId: `trace-${body.requestId}`
    };
    try {
      const result = match[2] === "drain"
        ? await client.updateConfig("drain_mode", body.enabled ? "on" : "off", {
            targetInstanceId: gameServerInstanceId,
            requireRegistryTarget: true,
            assertionContext
          })
        : await client.requestServerShutdown(body.reason || "integration", {
            targetInstanceId: gameServerInstanceId,
            requireRegistryTarget: true,
            assertionContext
          });
      res.end(JSON.stringify(result));
    } catch (error) {
      res.statusCode = 502;
      res.end(JSON.stringify({ ok: false, error: error.code || "GAME_ADMIN_ERROR" }));
    }
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      server.off("error", reject);
      resolve();
    });
  });
  return {
    baseUrl: `http://127.0.0.1:${server.address().port}`,
    async close() {
      await new Promise((resolve) => server.close(resolve));
      await redis.quit();
    }
  };
}

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
    adminBaseUrl: adminControl.baseUrl,
    adminToken: adminApiToken,
    gameServerInstanceId,
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
  const { privateKey, publicKey } = crypto.generateKeyPairSync("ed25519");
  const privateKeyPem = privateKey.export({ format: "pem", type: "pkcs8" }).toString();
  const publicKeyRaw = publicKey.export({ format: "der", type: "spki" }).subarray(-32).toString("base64url");

  gameServer = await startGameServer({
    host: "127.0.0.1",
    port: gamePort,
    adminPort,
    localSocketName,
    ticketSecret,
    redisUrl,
    redisKeyPrefix,
    envOverrides: {
      SERVICE_INSTANCE_ID: gameServerInstanceId,
      GAME_ADMIN_TOKEN: gameAdminToken,
      ADMIN_ASSERTION_ISSUER: "admin-api",
      ADMIN_ASSERTION_PUBLIC_KEYS_JSON: JSON.stringify({ "integration-v1": publicKeyRaw })
    }
  });

  adminControl = await startIntegrationAdminControl({
    adminPort,
    privateKeyPem
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
  if (adminControl) {
    await adminControl.close();
  }
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

test("auth-http keeps game-server writes retired while admin control uses signed protobuf", async () => {
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
  assert.equal(updateResponse.status, 410);
  assert.equal((await updateResponse.json()).error, "CONTROL_PLANE_ONLY");

  const drainResult = await runIntegrationMockClientScenario({
    scenario: "drain-new-room-rejected",
    roomId: randomId("room-admin-control")
  });
  assert.match(drainResult.stdout, /scenario completed: drain-new-room-rejected/);
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
