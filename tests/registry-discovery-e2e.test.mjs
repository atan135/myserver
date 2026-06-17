import assert from "node:assert/strict";
import { after, before, test } from "node:test";

import Redis from "ioredis";

import {
  cleanupRedisPrefix,
  cleanupRegistryInstances,
  findFreePort,
  randomId,
  runMockClientScenario,
  startAuthHttpServer,
  startGameProxy,
  startGameServer,
  startNatsServer
} from "./helpers/runtime.mjs";

const redisUrl = process.env.TEST_REDIS_URL || "redis://127.0.0.1:6379";
const ticketSecret = "test-only-ticket-secret";
const proxyAdminToken = "test-only-proxy-admin-token";
const redisKeyPrefix = `test:registry:${randomId("redis")}:`;
const gameServerInstanceId = randomId("game-server");
const gameProxyInstanceId = randomId("game-proxy");
const registryInstances = [
  { serviceName: "game-server", instanceId: gameServerInstanceId },
  { serviceName: "game-proxy", instanceId: gameProxyInstanceId }
];

let authServer;
let gameServer;
let gameProxy;
let natsServer;

async function waitFor(condition, description, timeoutMs = 30000) {
  const startedAt = Date.now();
  let lastError;

  while (Date.now() - startedAt < timeoutMs) {
    try {
      const value = await condition();
      if (value) {
        return value;
      }
    } catch (error) {
      lastError = error;
    }

    await new Promise((resolve) => setTimeout(resolve, 250));
  }

  throw new Error(`timed out waiting for ${description}${lastError ? `: ${lastError.message}` : ""}`);
}

async function fetchGuestLogin(baseUrl, guestId) {
  const response = await fetch(`${baseUrl}/api/v1/auth/guest-login`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ guestId })
  });
  const payload = await response.json();

  assert.equal(response.status, 201);
  assert.equal(payload.ok, true);
  return payload;
}

async function readRegistryInstance(serviceName, instanceId) {
  const redis = new Redis(redisUrl, {
    lazyConnect: true,
    maxRetriesPerRequest: 1,
    enableReadyCheck: true
  });

  await redis.connect();
  try {
    const [data, heartbeatExists] = await Promise.all([
      redis.hget(`service:${serviceName}:instances:${instanceId}`, "data"),
      redis.exists(`heartbeat:${serviceName}:${instanceId}`)
    ]);
    return {
      data: data ? JSON.parse(data) : null,
      heartbeatExists: heartbeatExists === 1
    };
  } finally {
    await redis.quit();
  }
}

before(async () => {
  await cleanupRegistryInstances(redisUrl, registryInstances);

  const authPort = await findFreePort();
  const gamePort = await findFreePort();
  const adminPort = await findFreePort();
  const proxyPort = await findFreePort();
  const proxyAdminPort = await findFreePort();
  const proxyTcpFallbackPort = await findFreePort();
  const localSocketName = process.platform === "win32"
    ? randomId("registry-game-server")
    : randomId("registry-game-server") + ".sock";

  natsServer = await startNatsServer();

  gameServer = await startGameServer({
    host: "127.0.0.1",
    port: gamePort,
    adminPort,
    localSocketName,
    ticketSecret,
    redisUrl,
    redisKeyPrefix,
    envOverrides: {
      REGISTRY_ENABLED: "true",
      REGISTRY_URL: redisUrl,
      NATS_URL: natsServer.url,
      SERVICE_INSTANCE_ID: gameServerInstanceId,
      REGISTRY_HEARTBEAT_INTERVAL: "1"
    }
  });

  await waitFor(async () => {
    const instance = await readRegistryInstance("game-server", gameServerInstanceId);
    return instance.data && instance.heartbeatExists ? instance : false;
  }, "game-server registry instance and heartbeat");

  gameProxy = await startGameProxy({
    host: "127.0.0.1",
    port: proxyPort,
    adminPort: proxyAdminPort,
    tcpFallbackPort: proxyTcpFallbackPort,
    upstreamLocalSocketName: localSocketName,
    envOverrides: {
      REGISTRY_ENABLED: "true",
      REGISTRY_URL: redisUrl,
      REDIS_URL: redisUrl,
      REDIS_KEY_PREFIX: redisKeyPrefix,
      NATS_URL: natsServer.url,
      TICKET_SECRET: ticketSecret,
      DISCOVERY_REQUIRED: "true",
      PROXY_ADMIN_TOKEN: proxyAdminToken,
      SERVICE_INSTANCE_ID: gameProxyInstanceId,
      UPSTREAM_SERVICE_NAME: "game-server",
      REGISTRY_DISCOVER_INTERVAL_SECS: "1"
    }
  });

  await waitFor(async () => {
    const instance = await readRegistryInstance("game-proxy", gameProxyInstanceId);
    return instance.data && instance.heartbeatExists ? instance : false;
  }, "game-proxy registry instance and heartbeat");

  await waitFor(async () => {
    const response = await fetch(`http://127.0.0.1:${gameProxy.adminPort}/instances`, {
      headers: {
        authorization: `Bearer ${proxyAdminToken}`
      }
    });
    if (!response.ok) {
      return false;
    }
    const payload = await response.json();
    return payload.instances?.some((instance) =>
      instance.server_id === gameServerInstanceId &&
      instance.local_socket_name === localSocketName &&
      instance.operation_state === "Active" &&
      instance.health_state === "Healthy"
    );
  }, "game-proxy to discover registry game-server upstream");

  authServer = await startAuthHttpServer({
    host: "127.0.0.1",
    port: authPort,
    ticketSecret,
    redisUrl,
    redisKeyPrefix,
    gameServerAdminHost: "127.0.0.1",
    gameServerAdminPort: adminPort,
    envOverrides: {
      REGISTRY_ENABLED: "true",
      DISCOVERY_REQUIRED: "true",
      REGISTRY_URL: redisUrl,
      NATS_URL: natsServer.url,
      SERVICE_INSTANCE_ID: randomId("auth-http"),
      GAME_PROXY_HOST: "127.0.0.1",
      GAME_PROXY_PORT: String(gamePort)
    }
  });
});

after(async () => {
  if (authServer) {
    await authServer.close();
  }
  if (gameProxy) {
    await gameProxy.close();
  }
  if (gameServer) {
    await gameServer.close();
  }
  if (natsServer) {
    await natsServer.close();
  }

  await cleanupRegistryInstances(redisUrl, registryInstances);
  await cleanupRedisPrefix(redisUrl, redisKeyPrefix);
});

test("auth-http discovers game-proxy.client and mock-client connects through proxy fallback", { timeout: 180000 }, async () => {
  const login = await waitFor(async () => {
    const payload = await fetchGuestLogin(authServer.baseUrl, randomId("registry-login"));
    return payload.services?.game?.port === gameProxy.port ? payload : false;
  }, "auth-http login services.game to resolve game-proxy.client");

  assert.deepEqual(login.services.game, {
    host: gameProxy.host,
    port: gameProxy.port,
    protocol: "kcp"
  });
  assert.equal(login.gameProxyHost, gameProxy.host);
  assert.equal(login.gameProxyPort, gameProxy.port);
  assert.notEqual(login.services.game.port, gameServer.port);
  assert.notEqual(login.gameProxyPort, gameServer.port);

  await runMockClientScenario({
    scenario: "happy",
    httpBaseUrl: authServer.baseUrl,
    host: gameProxy.host,
    gameHost: gameProxy.host,
    port: gameProxy.tcpFallbackPort,
    roomId: randomId("registry-room"),
    noServiceDiscovery: true,
    timeoutMs: 10000,
    processTimeoutMs: 45000
  });
});
