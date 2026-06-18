import assert from "node:assert/strict";
import { after, before, test } from "node:test";

import Redis from "ioredis";

import { discoverGameServerAdminEndpoints as discoverAuthGameServerAdminEndpoints } from "../apps/auth-http/src/registry-client.js";
import {
  createRegistryDiscoveryClient as createAdminApiRegistryDiscoveryClient,
  discoverGameProxyAdminEndpoints as discoverAdminApiGameProxyAdminEndpoints,
  discoverGameServerAdminEndpoints as discoverAdminApiGameServerAdminEndpoints
} from "../apps/admin-api/src/registry-client.js";
import { discoverGameServerAdminEndpoints as discoverMailGameServerAdminEndpoints } from "../apps/mail-service/src/registry-client.js";
import {
  createServiceInstancePayload,
  RegistryDiscoveryClient,
  registryHeartbeatKey,
  registryInstanceKey
} from "../packages/service-registry/node/registry-schema.js";
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
const registryKeyPrefix = `test:registry:${randomId("registry")}:`;
const gameServerInstanceId = randomId("game-server");
const gameProxyInstanceId = randomId("game-proxy");
const manualGameServerInstanceId = randomId("manual-game-server");
const manualGameProxyInstanceId = randomId("manual-game-proxy");
const manualMatchServiceInstanceId = randomId("manual-match-service");
const manualMailServiceInstanceId = randomId("manual-mail-service");
const manualAdminApiInstanceId = randomId("manual-admin-api");
const registryInstances = [
  { serviceName: "game-server", instanceId: gameServerInstanceId },
  { serviceName: "game-proxy", instanceId: gameProxyInstanceId }
];
const manualRegistryInstances = [
  { serviceName: "game-server", instanceId: manualGameServerInstanceId },
  { serviceName: "game-proxy", instanceId: manualGameProxyInstanceId },
  { serviceName: "match-service", instanceId: manualMatchServiceInstanceId },
  { serviceName: "mail-service", instanceId: manualMailServiceInstanceId },
  { serviceName: "admin-api", instanceId: manualAdminApiInstanceId }
];
const allRegistryInstances = [...registryInstances, ...manualRegistryInstances];

const manualEndpoints = {
  gameServer: {
    host: "127.0.0.31",
    clientPort: 18700,
    adminHost: "127.0.0.32",
    adminPort: 18750,
    internalSocket: `manual-game-server-internal-${manualGameServerInstanceId}.sock`,
    proxyLocalSocket: `manual-game-server-proxy-${manualGameServerInstanceId}.sock`
  },
  gameProxy: {
    host: "127.0.0.41",
    clientPort: 18400,
    tcpFallbackHost: "127.0.0.42",
    tcpFallbackPort: 28400,
    adminHost: "127.0.0.43",
    adminPort: 18101
  },
  matchService: {
    host: "127.0.0.51",
    grpcPort: 19002
  },
  mailService: {
    host: "127.0.0.61",
    httpPort: 19003
  },
  adminApi: {
    host: "127.0.0.71",
    httpPort: 13001
  }
};

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

async function readRegistryInstance(serviceName, instanceId, keyPrefix = "") {
  const redis = new Redis(redisUrl, {
    lazyConnect: true,
    maxRetriesPerRequest: 1,
    enableReadyCheck: true
  });

  await redis.connect();
  try {
    const [data, heartbeatExists] = await Promise.all([
      redis.hget(`${keyPrefix}service:${serviceName}:instances:${instanceId}`, "data"),
      redis.exists(`${keyPrefix}heartbeat:${serviceName}:${instanceId}`)
    ]);
    return {
      data: data ? JSON.parse(data) : null,
      heartbeatExists: heartbeatExists === 1
    };
  } finally {
    await redis.quit();
  }
}

async function registryKeyExists(serviceName, instanceId, keyPrefix = "") {
  const redis = new Redis(redisUrl, {
    lazyConnect: true,
    maxRetriesPerRequest: 1,
    enableReadyCheck: true
  });

  await redis.connect();
  try {
    return await redis.exists(`${keyPrefix}service:${serviceName}:instances:${instanceId}`) === 1;
  } finally {
    await redis.quit();
  }
}

function endpoint(name, protocol, host, port, visibility, metadata = {}) {
  return {
    name,
    protocol,
    host,
    port,
    socket: "",
    visibility,
    metadata,
    healthy: true
  };
}

function socketEndpoint(name, socket, metadata = {}) {
  return {
    name,
    protocol: "local_socket",
    host: "",
    port: 0,
    socket,
    visibility: "local",
    metadata,
    healthy: true
  };
}

function endpointMetadata(serviceName, instanceId, extra = {}) {
  return {
    service_name: serviceName,
    service_instance_id: instanceId,
    instance_id: instanceId,
    build_version: "registry-e2e",
    zone: "registry-e2e",
    ...extra
  };
}

function createManualRegistryPayloads() {
  const gameServerMetadata = endpointMetadata("game-server", manualGameServerInstanceId, {
    server_id: manualGameServerInstanceId
  });
  const gameProxyMetadata = endpointMetadata("game-proxy", manualGameProxyInstanceId);
  const matchMetadata = endpointMetadata("match-service", manualMatchServiceInstanceId, {
    protocol: "grpc",
    modes: ["1v1", "5v5"],
    runtime_store_backend: "redis"
  });
  const mailMetadata = endpointMetadata("mail-service", manualMailServiceInstanceId);
  const adminApiMetadata = endpointMetadata("admin-api", manualAdminApiInstanceId);

  return [
    createServiceInstancePayload({
      id: manualGameServerInstanceId,
      name: "game-server",
      host: manualEndpoints.gameServer.host,
      port: manualEndpoints.gameServer.clientPort,
      admin_port: manualEndpoints.gameServer.adminPort,
      local_socket: manualEndpoints.gameServer.proxyLocalSocket,
      endpoints: [
        endpoint(
          "client",
          "tcp",
          manualEndpoints.gameServer.host,
          manualEndpoints.gameServer.clientPort,
          "internal",
          gameServerMetadata
        ),
        endpoint(
          "admin",
          "tcp",
          manualEndpoints.gameServer.adminHost,
          manualEndpoints.gameServer.adminPort,
          "admin",
          gameServerMetadata
        ),
        socketEndpoint(
          "internal",
          manualEndpoints.gameServer.internalSocket,
          gameServerMetadata
        ),
        socketEndpoint(
          "proxy-local",
          manualEndpoints.gameServer.proxyLocalSocket,
          gameServerMetadata
        )
      ],
      tags: ["game", "tcp", "manual-e2e"],
      weight: 1_000_000_000,
      metadata: gameServerMetadata
    }),
    createServiceInstancePayload({
      id: manualGameProxyInstanceId,
      name: "game-proxy",
      host: manualEndpoints.gameProxy.host,
      port: manualEndpoints.gameProxy.clientPort,
      endpoints: [
        endpoint(
          "client",
          "kcp",
          manualEndpoints.gameProxy.host,
          manualEndpoints.gameProxy.clientPort,
          "public",
          gameProxyMetadata
        ),
        endpoint(
          "client-tcp-fallback",
          "tcp",
          manualEndpoints.gameProxy.tcpFallbackHost,
          manualEndpoints.gameProxy.tcpFallbackPort,
          "public",
          gameProxyMetadata
        ),
        endpoint(
          "admin",
          "http",
          manualEndpoints.gameProxy.adminHost,
          manualEndpoints.gameProxy.adminPort,
          "admin",
          gameProxyMetadata
        )
      ],
      tags: ["proxy", "kcp", "manual-e2e"],
      weight: 1_000_000_000,
      metadata: gameProxyMetadata
    }),
    createServiceInstancePayload({
      id: manualMatchServiceInstanceId,
      name: "match-service",
      host: manualEndpoints.matchService.host,
      port: manualEndpoints.matchService.grpcPort,
      endpoints: [
        endpoint(
          "grpc",
          "grpc",
          manualEndpoints.matchService.host,
          manualEndpoints.matchService.grpcPort,
          "internal",
          matchMetadata
        )
      ],
      tags: ["match", "grpc", "manual-e2e"],
      metadata: matchMetadata
    }),
    createServiceInstancePayload({
      id: manualMailServiceInstanceId,
      name: "mail-service",
      host: manualEndpoints.mailService.host,
      port: manualEndpoints.mailService.httpPort,
      endpoints: [
        endpoint(
          "http",
          "http",
          manualEndpoints.mailService.host,
          manualEndpoints.mailService.httpPort,
          "internal",
          mailMetadata
        )
      ],
      tags: ["mail", "http", "manual-e2e"],
      metadata: mailMetadata
    }),
    createServiceInstancePayload({
      id: manualAdminApiInstanceId,
      name: "admin-api",
      host: manualEndpoints.adminApi.host,
      port: manualEndpoints.adminApi.httpPort,
      endpoints: [
        endpoint(
          "http",
          "http",
          manualEndpoints.adminApi.host,
          manualEndpoints.adminApi.httpPort,
          "admin",
          adminApiMetadata
        )
      ],
      tags: ["admin", "http", "manual-e2e"],
      metadata: adminApiMetadata
    })
  ];
}

async function writeRegistryPayloads(payloads, keyPrefix = "") {
  const redis = new Redis(redisUrl, {
    lazyConnect: true,
    maxRetriesPerRequest: 1,
    enableReadyCheck: true
  });

  await redis.connect();
  try {
    for (const payload of payloads) {
      await redis.hset(
        registryInstanceKey(keyPrefix, payload.name, payload.id),
        "data",
        JSON.stringify(payload)
      );
      await redis.setex(registryHeartbeatKey(keyPrefix, payload.name, payload.id), 60, "1");
    }
  } finally {
    await redis.quit();
  }
}

function assertEndpointSelection(selection, expected) {
  assert.ok(selection, `expected ${expected.serviceName}.${expected.name} endpoint selection`);
  assert.equal(selection.instance.id, expected.instanceId);
  assert.equal(selection.instance.name, expected.serviceName);
  assert.equal(selection.endpoint.name, expected.name);
  assert.equal(selection.endpoint.protocol, expected.protocol);
  assert.equal(selection.endpoint.host, expected.host);
  assert.equal(selection.endpoint.port, expected.port);
  assert.equal(selection.endpoint.socket, expected.socket ?? "");
  assert.equal(selection.endpoint.visibility, expected.visibility);
}

function findEndpoint(endpoints, instanceId) {
  const endpoint = endpoints.find((candidate) => candidate.instanceId === instanceId);
  assert.ok(endpoint, `expected endpoint for instance ${instanceId}`);
  return endpoint;
}

function assertFlatEndpoint(endpoint, expected) {
  assert.equal(endpoint.service, expected.serviceName);
  assert.equal(endpoint.instanceId, expected.instanceId);
  assert.equal(endpoint.instance_id, expected.instanceId);
  assert.equal(endpoint.endpointName, expected.name);
  assert.equal(endpoint.endpoint_name, expected.name);
  assert.equal(endpoint.protocol, expected.protocol);
  assert.equal(endpoint.host, expected.host);
  assert.equal(endpoint.port, expected.port);
  assert.equal(endpoint.healthy, true);
}

before(async () => {
  await cleanupRegistryInstances(redisUrl, allRegistryInstances, registryKeyPrefix);

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
      REGISTRY_KEY_PREFIX: registryKeyPrefix,
      NATS_URL: natsServer.url,
      SERVICE_INSTANCE_ID: gameServerInstanceId,
      REGISTRY_HEARTBEAT_INTERVAL: "1"
    }
  });

  await waitFor(async () => {
    const instance = await readRegistryInstance("game-server", gameServerInstanceId, registryKeyPrefix);
    return instance.data && instance.heartbeatExists ? instance : false;
  }, "game-server registry instance and heartbeat");
  assert.equal(
    await registryKeyExists("game-server", gameServerInstanceId, registryKeyPrefix),
    true
  );
  assert.equal(
    await registryKeyExists("game-server", gameServerInstanceId),
    false
  );

  gameProxy = await startGameProxy({
    host: "127.0.0.1",
    port: proxyPort,
    adminPort: proxyAdminPort,
    tcpFallbackPort: proxyTcpFallbackPort,
    upstreamLocalSocketName: localSocketName,
    envOverrides: {
      REGISTRY_ENABLED: "true",
      REGISTRY_URL: redisUrl,
      REGISTRY_KEY_PREFIX: registryKeyPrefix,
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
    const instance = await readRegistryInstance("game-proxy", gameProxyInstanceId, registryKeyPrefix);
    return instance.data && instance.heartbeatExists ? instance : false;
  }, "game-proxy registry instance and heartbeat");
  assert.equal(
    await registryKeyExists("game-proxy", gameProxyInstanceId, registryKeyPrefix),
    true
  );
  assert.equal(
    await registryKeyExists("game-proxy", gameProxyInstanceId),
    false
  );

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
      REGISTRY_KEY_PREFIX: registryKeyPrefix,
      NATS_URL: natsServer.url,
      SERVICE_INSTANCE_ID: randomId("auth-http"),
      GAME_PROXY_HOST: "127.0.0.1",
      GAME_PROXY_PORT: String(gamePort),
      AUTH_EXPOSE_INTERNAL_SERVICE_ENDPOINTS: "true"
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

  await cleanupRegistryInstances(redisUrl, allRegistryInstances, registryKeyPrefix);
  await cleanupRedisPrefix(redisUrl, registryKeyPrefix);
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

test("registry consumers discover correct endpoints across multiple service instances", { timeout: 60000 }, async () => {
  await cleanupRegistryInstances(redisUrl, manualRegistryInstances, registryKeyPrefix);
  await writeRegistryPayloads(createManualRegistryPayloads(), registryKeyPrefix);

  const redis = new Redis(redisUrl, {
    lazyConnect: true,
    maxRetriesPerRequest: 1,
    enableReadyCheck: true
  });

  await redis.connect();
  try {
    const discovery = new RegistryDiscoveryClient(redis, {
      registryKeyPrefix,
      discoveryCacheTtlMs: 0
    });

    assertEndpointSelection(
      await discovery.discoverRequiredEndpoint("match-service", "grpc"),
      {
        serviceName: "match-service",
        instanceId: manualMatchServiceInstanceId,
        name: "grpc",
        protocol: "grpc",
        host: manualEndpoints.matchService.host,
        port: manualEndpoints.matchService.grpcPort,
        visibility: "internal"
      }
    );
    assertEndpointSelection(
      await discovery.discoverRequiredEndpoint("game-server", "internal"),
      {
        serviceName: "game-server",
        instanceId: manualGameServerInstanceId,
        name: "internal",
        protocol: "local_socket",
        host: "",
        port: 0,
        socket: manualEndpoints.gameServer.internalSocket,
        visibility: "local"
      }
    );
    assertEndpointSelection(
      await discovery.discoverRequiredEndpoint("game-server", "proxy-local"),
      {
        serviceName: "game-server",
        instanceId: manualGameServerInstanceId,
        name: "proxy-local",
        protocol: "local_socket",
        host: "",
        port: 0,
        socket: manualEndpoints.gameServer.proxyLocalSocket,
        visibility: "local"
      }
    );
    assertEndpointSelection(
      await discovery.discoverRequiredEndpoint("admin-api", "http"),
      {
        serviceName: "admin-api",
        instanceId: manualAdminApiInstanceId,
        name: "http",
        protocol: "http",
        host: manualEndpoints.adminApi.host,
        port: manualEndpoints.adminApi.httpPort,
        visibility: "admin"
      }
    );
    assertEndpointSelection(
      await discovery.discoverRequiredEndpoint("mail-service", "http"),
      {
        serviceName: "mail-service",
        instanceId: manualMailServiceInstanceId,
        name: "http",
        protocol: "http",
        host: manualEndpoints.mailService.host,
        port: manualEndpoints.mailService.httpPort,
        visibility: "internal"
      }
    );

    const proxyClientEndpoints = await discovery.discoverAllEndpoints("game-proxy", "client");
    assert.ok(
      proxyClientEndpoints.some(({ instance, endpoint: discoveredEndpoint }) =>
        instance.id === gameProxyInstanceId &&
        discoveredEndpoint.host === gameProxy.host &&
        discoveredEndpoint.port === gameProxy.port &&
        discoveredEndpoint.protocol === "kcp" &&
        discoveredEndpoint.visibility === "public"
      ),
      "expected real game-proxy.client endpoint to remain discoverable"
    );
    assertEndpointSelection(
      proxyClientEndpoints.find(({ instance }) => instance.id === manualGameProxyInstanceId),
      {
        serviceName: "game-proxy",
        instanceId: manualGameProxyInstanceId,
        name: "client",
        protocol: "kcp",
        host: manualEndpoints.gameProxy.host,
        port: manualEndpoints.gameProxy.clientPort,
        visibility: "public"
      }
    );

    const proxyAdminEndpoints = await discoverAdminApiGameProxyAdminEndpoints(redis, registryKeyPrefix);
    assertFlatEndpoint(findEndpoint(proxyAdminEndpoints, manualGameProxyInstanceId), {
      serviceName: "game-proxy",
      instanceId: manualGameProxyInstanceId,
      name: "admin",
      protocol: "http",
      host: manualEndpoints.gameProxy.adminHost,
      port: manualEndpoints.gameProxy.adminPort
    });
    assertFlatEndpoint(findEndpoint(proxyAdminEndpoints, gameProxyInstanceId), {
      serviceName: "game-proxy",
      instanceId: gameProxyInstanceId,
      name: "admin",
      protocol: "http",
      host: gameProxy.host,
      port: gameProxy.adminPort
    });

    for (const discoverGameServerAdminEndpoints of [
      discoverAuthGameServerAdminEndpoints,
      discoverAdminApiGameServerAdminEndpoints,
      discoverMailGameServerAdminEndpoints
    ]) {
      const endpoints = await discoverGameServerAdminEndpoints(redis, registryKeyPrefix);
      assertFlatEndpoint(findEndpoint(endpoints, manualGameServerInstanceId), {
        serviceName: "game-server",
        instanceId: manualGameServerInstanceId,
        name: "admin",
        protocol: "tcp",
        host: manualEndpoints.gameServer.adminHost,
        port: manualEndpoints.gameServer.adminPort
      });
      assertFlatEndpoint(findEndpoint(endpoints, gameServerInstanceId), {
        serviceName: "game-server",
        instanceId: gameServerInstanceId,
        name: "admin",
        protocol: "tcp",
        host: gameServer.host,
        port: gameServer.adminPort
      });
    }

    const adminApiDiscovery = createAdminApiRegistryDiscoveryClient(redis, {
      registryKeyPrefix,
      discoveryCacheTtlMs: 0
    });
    const adminApiSnapshot = await adminApiDiscovery.refreshSnapshot("game-proxy", {
      endpointName: "admin",
      kind: "all_endpoints",
      refreshIntervalMs: 0
    });
    assert.equal(adminApiSnapshot.ok, true);
    assert.ok(
      adminApiSnapshot.value.some(({ instance, endpoint: discoveredEndpoint }) =>
        instance.id === manualGameProxyInstanceId &&
        discoveredEndpoint.name === "admin" &&
        discoveredEndpoint.protocol === "http" &&
        discoveredEndpoint.visibility === "admin" &&
        discoveredEndpoint.host === manualEndpoints.gameProxy.adminHost &&
        discoveredEndpoint.port === manualEndpoints.gameProxy.adminPort
      ),
      "expected admin-api discovery refresh snapshot to include manual game-proxy.admin"
    );
    adminApiDiscovery.stop();

    const authLogin = await waitFor(async () => {
      const payload = await fetchGuestLogin(authServer.baseUrl, randomId("registry-manual-login"));
      return payload.services?.game?.port === manualEndpoints.gameProxy.clientPort ? payload : false;
    }, "auth-http to discover manual game-proxy.client among multiple instances");
    assert.deepEqual(authLogin.services.game, {
      host: manualEndpoints.gameProxy.host,
      port: manualEndpoints.gameProxy.clientPort,
      protocol: "kcp"
    });
    assert.deepEqual(authLogin.services.mail, {
      host: manualEndpoints.mailService.host,
      port: manualEndpoints.mailService.httpPort,
      protocol: "http"
    });
    assert.equal(authLogin.gameProxyHost, manualEndpoints.gameProxy.host);
    assert.equal(authLogin.gameProxyPort, manualEndpoints.gameProxy.clientPort);

    assert.equal(
      await registryKeyExists("match-service", manualMatchServiceInstanceId),
      false
    );
  } finally {
    await redis.quit();
  }
});
