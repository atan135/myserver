import assert from "node:assert/strict";
import crypto from "node:crypto";
import { after, before, test } from "node:test";

import Redis from "ioredis";

import { discoverGameServerAdminEndpoints as discoverAuthGameServerAdminEndpoints } from "../../apps/auth-http/src/registry-client.js";
import {
  createRegistryDiscoveryClient as createAdminApiRegistryDiscoveryClient,
  discoverGameProxyAdminEndpoints as discoverAdminApiGameProxyAdminEndpoints,
  discoverGameServerAdminEndpoints as discoverAdminApiGameServerAdminEndpoints
} from "../../apps/admin-api/src/registry-client.js";
import { discoverGameServerAdminEndpoints as discoverMailGameServerAdminEndpoints } from "../../apps/mail-service/src/registry-client.js";
import {
  createServiceInstancePayload,
  RegistryDiscoveryClient,
  registryHeartbeatKey,
  registryInstanceKey
} from "../../packages/service-registry/node/registry-schema.js";
import {
  cleanupRedisPrefix,
  cleanupRegistryInstances,
  findFreePort,
  randomId,
  runMatchFlowProbe,
  runMockClientScenario,
  startAuthHttpServer,
  startGameProxy,
  startGameServer,
  startMatchService,
  startNatsServer
} from "../helpers/runtime.mjs";
import { TcpProtocolClient } from "../../tools/mock-client/src/client.js";
import { MESSAGE_TYPE } from "../../tools/mock-client/src/constants.js";
import { decodeByMessageType, encodePingReq, encodeRoomJoinReq, encodeRoomReconnectReq } from "../../tools/mock-client/src/messages.js";
import { authenticateClient } from "../../tools/mock-client/src/scenarios/room.js";

const redisUrl = process.env.TEST_REDIS_URL || "redis://127.0.0.1:6379";
const ticketSecret = "test-only-ticket-secret";
const proxyAdminToken = "test-only-proxy-admin-token";
const redisKeyPrefix = `test:registry:${randomId("redis")}:`;
const registryKeyPrefix = `test:registry:${randomId("registry")}:`;
const gameServerInstanceId = randomId("game-server");
const gameProxyInstanceId = randomId("game-proxy");
const matchServiceInstanceId = randomId("match-service");
const manualGameServerInstanceId = randomId("manual-game-server");
const manualGameProxyInstanceId = randomId("manual-game-proxy");
const manualMatchServiceInstanceId = randomId("manual-match-service");
const manualMailServiceInstanceId = randomId("manual-mail-service");
const manualAdminApiInstanceId = randomId("manual-admin-api");
const heartbeatExpiredGameServerInstanceId = randomId("heartbeat-expired-game-server");
const heartbeatHealthyGameServerInstanceId = randomId("heartbeat-healthy-game-server");
const mixedLegacyGameServerInstanceId = randomId("mixed-legacy-game-server");
const mixedEndpointGameServerInstanceId = randomId("mixed-endpoint-game-server");
const registryInstances = [
  { serviceName: "game-server", instanceId: gameServerInstanceId },
  { serviceName: "game-proxy", instanceId: gameProxyInstanceId },
  { serviceName: "match-service", instanceId: matchServiceInstanceId }
];
const manualRegistryInstances = [
  { serviceName: "game-server", instanceId: manualGameServerInstanceId },
  { serviceName: "game-proxy", instanceId: manualGameProxyInstanceId },
  { serviceName: "match-service", instanceId: manualMatchServiceInstanceId },
  { serviceName: "mail-service", instanceId: manualMailServiceInstanceId },
  { serviceName: "admin-api", instanceId: manualAdminApiInstanceId },
  { serviceName: "game-server", instanceId: heartbeatExpiredGameServerInstanceId },
  { serviceName: "game-server", instanceId: heartbeatHealthyGameServerInstanceId },
  { serviceName: "game-server", instanceId: mixedLegacyGameServerInstanceId },
  { serviceName: "game-server", instanceId: mixedEndpointGameServerInstanceId }
];
const allRegistryInstances = [...registryInstances, ...manualRegistryInstances];

function base64UrlEncode(value) {
  return Buffer.from(value).toString("base64url");
}

function hashTicket(ticket) {
  return crypto.createHash("sha256").update(ticket).digest("hex");
}

function signTicketPayload(payloadB64, secret) {
  return crypto
    .createHmac("sha256", secret)
    .update(payloadB64)
    .digest("base64url");
}

function createTestLogin({ accountPlayerId, characterId, worldId = 0 }) {
  const expiresAt = new Date(Date.now() + 300_000).toISOString();
  const payload = {
    playerId: accountPlayerId,
    characterId,
    nonce: crypto.randomBytes(12).toString("hex"),
    ver: 1,
    exp: expiresAt,
    worldId
  };
  const payloadB64 = base64UrlEncode(JSON.stringify(payload));
  const ticket = `${payloadB64}.${signTicketPayload(payloadB64, ticketSecret)}`;
  return {
    playerId: accountPlayerId,
    accountPlayerId,
    characterId,
    worldId,
    accessToken: "",
    ticket,
    ticketExpiresAt: expiresAt,
    ticketPayload: {
      accountPlayerId,
      characterId,
      worldId,
      exp: expiresAt
    }
  };
}

async function writeTestTicket(login) {
  const redis = new Redis(redisUrl, {
    lazyConnect: true,
    maxRetriesPerRequest: 1,
    enableReadyCheck: true
  });

  await redis.connect();
  try {
    await redis.set(
      `${redisKeyPrefix}ticket:${hashTicket(login.ticket)}`,
      login.accountPlayerId,
      "EX",
      300
    );
    await redis.set(`${redisKeyPrefix}player-ticket-version:${login.accountPlayerId}`, "1", "EX", 300);
  } finally {
    await redis.quit();
  }
}

async function createWritableTestLogin(prefix) {
  const login = createTestLogin({
    accountPlayerId: randomId(`${prefix}-account`),
    characterId: `chr_${randomId(prefix).replace(/[^0-9a-hjkmnp-tv-z]/g, "").slice(-16) || "0000000000000001"}`
  });
  await writeTestTicket(login);
  return login;
}

async function readUntilWithPing(client, {
  timeoutMs,
  readSliceMs = 1000,
  pingIntervalMs = 2000,
  predicate,
  label
}) {
  const startedAt = Date.now();
  let lastPingAt = 0;
  let pingSeq = 800000;

  while (Date.now() - startedAt < timeoutMs) {
    const now = Date.now();
    if (now - lastPingAt >= pingIntervalMs) {
      await client.send(MESSAGE_TYPE.PING_REQ, pingSeq, encodePingReq(now));
      pingSeq += 1;
      lastPingAt = now;
    }

    try {
      const packet = await client.readNextPacket(Math.min(readSliceMs, Math.max(1, timeoutMs - (Date.now() - startedAt))));
      const decoded = decodeByMessageType(packet.messageType, packet.body);
      console.log(`${client.label}.${label}:`, JSON.stringify({ messageType: packet.messageType, seq: packet.seq, decoded }, null, 2));
      if (predicate(packet, decoded)) {
        return decoded;
      }
    } catch (error) {
      if (!/Timed out waiting/.test(error.message)) {
        throw error;
      }
    }
  }

  throw new Error(`Timed out waiting for ${client.label}.${label} after ${timeoutMs}ms`);
}

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
  },
  heartbeatExpiredGameServer: {
    host: "127.0.0.81",
    clientPort: 19700,
    adminHost: "127.0.0.82",
    adminPort: 19750
  },
  heartbeatHealthyGameServer: {
    host: "127.0.0.83",
    clientPort: 19701,
    adminHost: "127.0.0.84",
    adminPort: 19751
  },
  mixedLegacyGameServer: {
    host: "127.0.0.91",
    clientPort: 20700,
    adminPort: 20750,
    localSocket: `mixed-legacy-game-server-${mixedLegacyGameServerInstanceId}.sock`
  },
  mixedEndpointGameServer: {
    legacyHost: "127.0.0.92",
    legacyClientPort: 20701,
    legacyAdminPort: 20751,
    clientHost: "127.0.0.93",
    clientPort: 20702,
    adminHost: "127.0.0.94",
    adminPort: 20752,
    internalSocket: `mixed-endpoint-game-server-${mixedEndpointGameServerInstanceId}.sock`
  }
};

let authServer;
let gameServer;
let gameProxy;
let matchService;
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

function createHeartbeatExpiryGameServerPayloads() {
  const expiredMetadata = endpointMetadata("game-server", heartbeatExpiredGameServerInstanceId, {
    server_id: heartbeatExpiredGameServerInstanceId
  });
  const healthyMetadata = endpointMetadata("game-server", heartbeatHealthyGameServerInstanceId, {
    server_id: heartbeatHealthyGameServerInstanceId
  });

  return [
    createServiceInstancePayload({
      id: heartbeatExpiredGameServerInstanceId,
      name: "game-server",
      host: manualEndpoints.heartbeatExpiredGameServer.host,
      port: manualEndpoints.heartbeatExpiredGameServer.clientPort,
      admin_port: manualEndpoints.heartbeatExpiredGameServer.adminPort,
      endpoints: [
        endpoint(
          "admin",
          "tcp",
          manualEndpoints.heartbeatExpiredGameServer.adminHost,
          manualEndpoints.heartbeatExpiredGameServer.adminPort,
          "admin",
          expiredMetadata
        )
      ],
      tags: ["game", "admin", "manual-e2e"],
      weight: 1_000_000_000,
      metadata: expiredMetadata
    }),
    createServiceInstancePayload({
      id: heartbeatHealthyGameServerInstanceId,
      name: "game-server",
      host: manualEndpoints.heartbeatHealthyGameServer.host,
      port: manualEndpoints.heartbeatHealthyGameServer.clientPort,
      admin_port: manualEndpoints.heartbeatHealthyGameServer.adminPort,
      endpoints: [
        endpoint(
          "admin",
          "tcp",
          manualEndpoints.heartbeatHealthyGameServer.adminHost,
          manualEndpoints.heartbeatHealthyGameServer.adminPort,
          "admin",
          healthyMetadata
        )
      ],
      tags: ["game", "admin", "manual-e2e"],
      weight: 1_000_000_000,
      metadata: healthyMetadata
    })
  ];
}

function createMixedVersionGameServerPayloads() {
  const legacyMetadata = endpointMetadata("game-server", mixedLegacyGameServerInstanceId, {
    server_id: mixedLegacyGameServerInstanceId,
    registry_shape: "legacy-v1"
  });
  const endpointMetadataV2 = endpointMetadata("game-server", mixedEndpointGameServerInstanceId, {
    server_id: mixedEndpointGameServerInstanceId,
    registry_shape: "endpoint-v2"
  });

  return [
    {
      id: mixedLegacyGameServerInstanceId,
      name: "game-server",
      host: manualEndpoints.mixedLegacyGameServer.host,
      port: manualEndpoints.mixedLegacyGameServer.clientPort,
      admin_port: manualEndpoints.mixedLegacyGameServer.adminPort,
      local_socket: manualEndpoints.mixedLegacyGameServer.localSocket,
      tags: ["game", "legacy-v1", "manual-e2e"],
      weight: 1_000_000_000,
      metadata: legacyMetadata,
      registered_at: Date.now(),
      healthy: true
    },
    createServiceInstancePayload({
      id: mixedEndpointGameServerInstanceId,
      name: "game-server",
      host: manualEndpoints.mixedEndpointGameServer.legacyHost,
      port: manualEndpoints.mixedEndpointGameServer.legacyClientPort,
      admin_port: manualEndpoints.mixedEndpointGameServer.legacyAdminPort,
      endpoints: [
        endpoint(
          "client",
          "tcp",
          manualEndpoints.mixedEndpointGameServer.clientHost,
          manualEndpoints.mixedEndpointGameServer.clientPort,
          "internal",
          endpointMetadataV2
        ),
        endpoint(
          "admin",
          "tcp",
          manualEndpoints.mixedEndpointGameServer.adminHost,
          manualEndpoints.mixedEndpointGameServer.adminPort,
          "admin",
          endpointMetadataV2
        ),
        socketEndpoint(
          "internal",
          manualEndpoints.mixedEndpointGameServer.internalSocket,
          endpointMetadataV2
        )
      ],
      tags: ["game", "endpoint-v2", "manual-e2e"],
      weight: 1_000_000_000,
      metadata: endpointMetadataV2
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

function parseProbeJson(stdout) {
  const line = stdout
    .split(/\r?\n/)
    .find((candidate) => candidate.startsWith("MATCH_FLOW_PROBE_JSON:"));
  assert.ok(line, `expected match_flow_probe JSON output in stdout:\n${stdout}`);
  return JSON.parse(line.slice("MATCH_FLOW_PROBE_JSON:".length));
}

function proxyAdminUrl(pathname, query = {}) {
  const url = new URL(`http://127.0.0.1:${gameProxy.adminPort}${pathname}`);
  for (const [key, value] of Object.entries(query)) {
    if (value !== undefined && value !== null) {
      url.searchParams.set(key, String(value));
    }
  }
  return url;
}

async function fetchProxyAdmin(pathname, { method = "GET", query } = {}) {
  const response = await fetch(proxyAdminUrl(pathname, query), {
    method,
    headers: {
      authorization: `Bearer ${proxyAdminToken}`,
      "X-Admin-Actor": "registry-e2e@example.com"
    }
  });
  const contentType = response.headers.get("content-type") || "";
  const body = contentType.includes("application/json")
    ? await response.json()
    : await response.text();

  assert.equal(
    response.ok,
    true,
    `proxy admin ${method} ${pathname} failed with ${response.status}: ${typeof body === "string" ? body : JSON.stringify(body)}`
  );
  return body;
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

  const matchPort = await findFreePort();
  matchService = await startMatchService({
    host: "127.0.0.1",
    port: matchPort,
    redisUrl,
    redisKeyPrefix,
    natsUrl: natsServer.url,
    envOverrides: {
      APP_ENV: "test",
      REGISTRY_ENABLED: "true",
      DISCOVERY_REQUIRED: "true",
      REGISTRY_URL: redisUrl,
      REGISTRY_KEY_PREFIX: registryKeyPrefix,
      SERVICE_INSTANCE_ID: matchServiceInstanceId,
      REGISTRY_HEARTBEAT_INTERVAL: "1",
      GAME_INTERNAL_TOKEN: "dev-only-change-this-game-internal-token"
    }
  });

  await waitFor(async () => {
    const instance = await readRegistryInstance("match-service", matchServiceInstanceId, registryKeyPrefix);
    return instance.data && instance.heartbeatExists ? instance : false;
  }, "match-service registry instance and heartbeat");
  assert.equal(
    await registryKeyExists("match-service", matchServiceInstanceId, registryKeyPrefix),
    true
  );
  assert.equal(
    await registryKeyExists("match-service", matchServiceInstanceId),
    false
  );

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
      APP_ENV: "test",
      DISCOVERY_REQUIRED: "true",
      SERVICE_INSTANCE_ID: gameServerInstanceId,
      REGISTRY_HEARTBEAT_INTERVAL: "1",
      MATCH_SERVICE_NAME: "match-service",
      MATCH_SERVICE_REDISCOVERY_INTERVAL_SECS: "1",
      GAME_INTERNAL_TOKEN: "dev-only-change-this-game-internal-token"
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
  if (matchService) {
    await matchService.close();
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

  const testLogin = await createWritableTestLogin("registry-happy");
  await runMockClientScenario({
    scenario: "happy",
    httpBaseUrl: authServer.baseUrl,
    host: gameProxy.host,
    gameHost: gameProxy.host,
    port: gameProxy.tcpFallbackPort,
    roomId: randomId("registry-room"),
    ticket: testLogin.ticket,
    characterId: testLogin.characterId,
    noServiceDiscovery: true,
    timeoutMs: 10000,
    processTimeoutMs: 45000
  });
});

test("M7 drills match-service create-room then players join through registry-discovered proxy", { timeout: 240000 }, async () => {
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
        instanceId: matchServiceInstanceId,
        name: "grpc",
        protocol: "grpc",
        host: matchService.host,
        port: matchService.port,
        visibility: "internal"
      }
    );
    assertEndpointSelection(
      await discovery.discoverRequiredEndpoint("game-server", "internal"),
      {
        serviceName: "game-server",
        instanceId: gameServerInstanceId,
        name: "internal",
        protocol: "local_socket",
        host: "",
        port: 0,
        socket: gameServer.internalSocketName,
        visibility: "local"
      }
    );
  } finally {
    await redis.quit();
  }

  const baseClientOptions = {
    httpBaseUrl: authServer.baseUrl,
    host: gameProxy.host,
    gameHost: gameProxy.host,
    port: gameProxy.tcpFallbackPort,
    timeoutMs: 10000,
    maxBodyLen: 4096,
    useServiceDiscovery: false
  };
  const loginA = await createWritableTestLogin("m7-match-a");
  const loginB = await createWritableTestLogin("m7-match-b");

  const probe = await runMatchFlowProbe({
    addr: matchService.addr,
    scenario: "matched",
    mode: "1v1",
    characterIds: [loginA.characterId, loginB.characterId],
    timeoutSecs: 20,
    jsonOutput: true,
    processTimeoutMs: 90000
  });
  const matchResult = parseProbeJson(probe.stdout);
  assert.equal(matchResult.ok, true);
  assert.equal(matchResult.scenario, "matched");
  assert.equal(matchResult.mode, "1v1");
  assert.deepEqual(matchResult.characterIds, [loginA.characterId, loginB.characterId]);
  assert.ok(matchResult.matchId);
  assert.ok(matchResult.roomId);
  assert.deepEqual(matchResult.statuses, ["matched", "matched"]);

  const clientA = new TcpProtocolClient(baseClientOptions, "m7ClientA");
  const clientB = new TcpProtocolClient(baseClientOptions, "m7ClientB");
  let reconnectClientA = null;

  await clientA.connect();
  await clientB.connect();
  try {
    await authenticateClient(clientA, baseClientOptions, loginA, 1);
    await authenticateClient(clientB, baseClientOptions, loginB, 1);

    await clientA.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(matchResult.roomId));
    const joinA = await clientA.readUntil(
      baseClientOptions.timeoutMs,
      (packet) => packet.messageType === MESSAGE_TYPE.ROOM_JOIN_RES && packet.seq === 2,
      "m7RoomJoinA"
    );
    assert.equal(joinA.ok, true);
    assert.equal(joinA.roomId, matchResult.roomId);

    await clientB.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(matchResult.roomId));
    const joinB = await clientB.readUntil(
      baseClientOptions.timeoutMs,
      (packet) => packet.messageType === MESSAGE_TYPE.ROOM_JOIN_RES && packet.seq === 2,
      "m7RoomJoinB"
    );
    assert.equal(joinB.ok, true);
    assert.equal(joinB.roomId, matchResult.roomId);

    const stateB = await clientB.readUntil(
      baseClientOptions.timeoutMs,
      (packet, decoded) =>
        packet.messageType === MESSAGE_TYPE.ROOM_STATE_PUSH &&
        decoded.snapshot?.roomId === matchResult.roomId &&
        decoded.snapshot.members?.some((member) => member.characterId === loginB.characterId),
      "m7RoomStateB"
    );
    assert.equal(stateB.snapshot.members.length, 2);

    clientA.close();
    await readUntilWithPing(clientB, {
      timeoutMs: baseClientOptions.timeoutMs * 2,
      predicate: (packet, decoded) =>
        packet.messageType === MESSAGE_TYPE.ROOM_STATE_PUSH &&
        decoded.snapshot?.members?.some((member) => member.characterId === loginA.characterId && member.offline),
      label: "m7MemberOfflineA"
    });

    reconnectClientA = new TcpProtocolClient(baseClientOptions, "m7ClientAReconnect");
    await reconnectClientA.connect();
    await authenticateClient(reconnectClientA, baseClientOptions, loginA, 3);
    await reconnectClientA.send(MESSAGE_TYPE.ROOM_RECONNECT_REQ, 4, encodeRoomReconnectReq());
    const reconnectA = await reconnectClientA.readUntil(
      baseClientOptions.timeoutMs,
      (packet) => packet.messageType === MESSAGE_TYPE.ROOM_RECONNECT_RES && packet.seq === 4,
      "m7RoomReconnectA"
    );
    assert.equal(reconnectA.ok, true);
    assert.equal(reconnectA.roomId, matchResult.roomId);
    assert.equal(reconnectA.snapshot?.roomId, matchResult.roomId);
    assert.equal(
      reconnectA.snapshot?.members?.some((member) => member.characterId === loginA.characterId && !member.offline),
      true
    );
  } finally {
    clientA.close();
    clientB.close();
    reconnectClientA?.close();
  }
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

    const matchGrpcEndpoints = await discovery.discoverAllEndpoints("match-service", "grpc");
    assertEndpointSelection(
      matchGrpcEndpoints.find(({ instance }) => instance.id === manualMatchServiceInstanceId),
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

test("game-proxy keeps route-store bindings while discovering multiple upstreams and proxies", { timeout: 90000 }, async () => {
  const roomId = randomId("multi-upstream-room");
  const characterId = randomId("multi-upstream-character");

  await cleanupRegistryInstances(redisUrl, manualRegistryInstances, registryKeyPrefix);
  await writeRegistryPayloads(createManualRegistryPayloads(), registryKeyPrefix);

  const instances = await waitFor(async () => {
    const payload = await fetchProxyAdmin("/instances");
    const discovered = payload.instances || [];
    const real = discovered.find((instance) => instance.server_id === gameServerInstanceId);
    const manual = discovered.find((instance) => instance.server_id === manualGameServerInstanceId);

    if (
      real?.health_state === "Healthy" &&
      real?.operation_state === "Active" &&
      manual?.health_state === "Healthy" &&
      manual?.operation_state === "Active" &&
      manual?.local_socket_name === manualEndpoints.gameServer.proxyLocalSocket
    ) {
      return discovered;
    }
    return false;
  }, "game-proxy to discover multiple healthy game-server upstreams");

  assert.ok(instances.length >= 2);
  assert.equal(
    instances.filter((instance) => instance.health_state === "Healthy").length >= 2,
    true
  );

  const status = await fetchProxyAdmin("/status");
  assert.equal(status.ok, true);
  assert.equal(status.active_upstream, gameServerInstanceId);

  await fetchProxyAdmin("/room-route/upsert", {
    method: "POST",
    query: {
      room_id: roomId,
      owner_server_id: manualGameServerInstanceId,
      migration_state: "OwnedByNew",
      member_count: 2,
      online_member_count: 1,
      room_version: 1,
      last_transfer_checksum: "checksum-1"
    }
  });
  await fetchProxyAdmin("/character-route/upsert", {
    method: "POST",
    query: {
      character_id: characterId,
      current_room_id: roomId,
      preferred_server_id: manualGameServerInstanceId
    }
  });

  const roomRoutes = await fetchProxyAdmin("/room-routes");
  assert.deepEqual(
    roomRoutes.routes.find((route) => route.room_id === roomId),
    {
      room_id: roomId,
      owner_server_id: manualGameServerInstanceId,
      migration_state: "OwnedByNew",
      member_count: 2,
      online_member_count: 1,
      empty_since_ms: null,
      room_version: 1,
      rollout_epoch: "",
      last_transfer_checksum: "checksum-1",
      updated_at_ms: roomRoutes.routes.find((route) => route.room_id === roomId).updated_at_ms
    }
  );

  const characterRoutes = await fetchProxyAdmin("/character-routes");
  assert.deepEqual(
    characterRoutes.routes.find((route) => route.character_id === characterId),
    {
      character_id: characterId,
      current_room_id: roomId,
      preferred_server_id: manualGameServerInstanceId,
      rollout_epoch: "",
      updated_at_ms: characterRoutes.routes.find((route) => route.character_id === characterId).updated_at_ms
    }
  );

  await waitFor(async () => {
    const payload = await fetchProxyAdmin("/instances");
    const manual = payload.instances?.find((instance) => instance.server_id === manualGameServerInstanceId);
    return manual?.health_state === "Healthy" ? payload : false;
  }, "game-proxy discovery refresh after route binding");

  const roomRoutesAfterRefresh = await fetchProxyAdmin("/room-routes");
  assert.equal(
    roomRoutesAfterRefresh.routes.find((route) => route.room_id === roomId)?.owner_server_id,
    manualGameServerInstanceId
  );
  const characterRoutesAfterRefresh = await fetchProxyAdmin("/character-routes");
  const characterRouteAfterRefresh = characterRoutesAfterRefresh.routes.find((route) => route.character_id === characterId);
  assert.equal(characterRouteAfterRefresh?.current_room_id, roomId);
  assert.equal(characterRouteAfterRefresh?.preferred_server_id, manualGameServerInstanceId);

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

    const proxyClientEndpoints = await discovery.discoverAllEndpoints("game-proxy", "client");
    assertEndpointSelection(
      proxyClientEndpoints.find(({ instance }) => instance.id === gameProxyInstanceId),
      {
        serviceName: "game-proxy",
        instanceId: gameProxyInstanceId,
        name: "client",
        protocol: "kcp",
        host: gameProxy.host,
        port: gameProxy.port,
        visibility: "public"
      }
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

    const proxyAdminEndpoints = await discoverAdminApiGameProxyAdminEndpoints(redis, {
      registryKeyPrefix,
      discoveryCacheTtlMs: 0
    });
    assertFlatEndpoint(findEndpoint(proxyAdminEndpoints, gameProxyInstanceId), {
      serviceName: "game-proxy",
      instanceId: gameProxyInstanceId,
      name: "admin",
      protocol: "http",
      host: gameProxy.host,
      port: gameProxy.adminPort
    });
    assertFlatEndpoint(findEndpoint(proxyAdminEndpoints, manualGameProxyInstanceId), {
      serviceName: "game-proxy",
      instanceId: manualGameProxyInstanceId,
      name: "admin",
      protocol: "http",
      host: manualEndpoints.gameProxy.adminHost,
      port: manualEndpoints.gameProxy.adminPort
    });

    const adminApiDiscovery = createAdminApiRegistryDiscoveryClient(redis, {
      registryKeyPrefix,
      discoveryCacheTtlMs: 0
    });
    try {
      const snapshot = await adminApiDiscovery.refreshSnapshot("game-proxy", {
        endpointName: "admin",
        kind: "all_endpoints",
        refreshIntervalMs: 0
      });
      assert.equal(snapshot.ok, true);
      assert.equal(
        snapshot.value.some(({ instance }) => instance.id === gameProxyInstanceId),
        true
      );
      assert.equal(
        snapshot.value.some(({ instance }) => instance.id === manualGameProxyInstanceId),
        true
      );
    } finally {
      adminApiDiscovery.stop();
    }
  } finally {
    await redis.quit();
  }
});

test("registry discovery stays compatible while v1 legacy and v2 endpoint instances are mixed", { timeout: 60000 }, async () => {
  const mixedRegistryInstances = [
    { serviceName: "game-server", instanceId: mixedLegacyGameServerInstanceId },
    { serviceName: "game-server", instanceId: mixedEndpointGameServerInstanceId }
  ];

  await cleanupRegistryInstances(redisUrl, mixedRegistryInstances, registryKeyPrefix);
  await writeRegistryPayloads(createMixedVersionGameServerPayloads(), registryKeyPrefix);

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

    const adminEndpoints = await discovery.discoverAllEndpoints("game-server", "admin");
    assertEndpointSelection(
      adminEndpoints.find(({ instance }) => instance.id === mixedLegacyGameServerInstanceId),
      {
        serviceName: "game-server",
        instanceId: mixedLegacyGameServerInstanceId,
        name: "admin",
        protocol: "tcp",
        host: manualEndpoints.mixedLegacyGameServer.host,
        port: manualEndpoints.mixedLegacyGameServer.adminPort,
        visibility: "admin"
      }
    );
    assertEndpointSelection(
      adminEndpoints.find(({ instance }) => instance.id === mixedEndpointGameServerInstanceId),
      {
        serviceName: "game-server",
        instanceId: mixedEndpointGameServerInstanceId,
        name: "admin",
        protocol: "tcp",
        host: manualEndpoints.mixedEndpointGameServer.adminHost,
        port: manualEndpoints.mixedEndpointGameServer.adminPort,
        visibility: "admin"
      }
    );
    assert.equal(
      adminEndpoints.some(({ instance, endpoint: discoveredEndpoint }) =>
        instance.id === mixedEndpointGameServerInstanceId &&
        (
          discoveredEndpoint.host === manualEndpoints.mixedEndpointGameServer.legacyHost ||
          discoveredEndpoint.port === manualEndpoints.mixedEndpointGameServer.legacyAdminPort
        )
      ),
      false,
      "expected v2 admin discovery to prefer explicit endpoint over legacy top-level fields"
    );

    const clientEndpoints = await discovery.discoverAllEndpoints("game-server", "client");
    assertEndpointSelection(
      clientEndpoints.find(({ instance }) => instance.id === mixedLegacyGameServerInstanceId),
      {
        serviceName: "game-server",
        instanceId: mixedLegacyGameServerInstanceId,
        name: "client",
        protocol: "tcp",
        host: manualEndpoints.mixedLegacyGameServer.host,
        port: manualEndpoints.mixedLegacyGameServer.clientPort,
        visibility: "public"
      }
    );
    assertEndpointSelection(
      clientEndpoints.find(({ instance }) => instance.id === mixedEndpointGameServerInstanceId),
      {
        serviceName: "game-server",
        instanceId: mixedEndpointGameServerInstanceId,
        name: "client",
        protocol: "tcp",
        host: manualEndpoints.mixedEndpointGameServer.clientHost,
        port: manualEndpoints.mixedEndpointGameServer.clientPort,
        visibility: "internal"
      }
    );

    const internalEndpoints = await discovery.discoverAllEndpoints("game-server", "internal");
    assertEndpointSelection(
      internalEndpoints.find(({ instance }) => instance.id === mixedEndpointGameServerInstanceId),
      {
        serviceName: "game-server",
        instanceId: mixedEndpointGameServerInstanceId,
        name: "internal",
        protocol: "local_socket",
        host: "",
        port: 0,
        socket: manualEndpoints.mixedEndpointGameServer.internalSocket,
        visibility: "local"
      }
    );

    const localSocketEndpoints = await discovery.discoverAllEndpoints("game-server", "local_socket");
    assertEndpointSelection(
      localSocketEndpoints.find(({ instance }) => instance.id === mixedLegacyGameServerInstanceId),
      {
        serviceName: "game-server",
        instanceId: mixedLegacyGameServerInstanceId,
        name: "local_socket",
        protocol: "local_socket",
        host: "",
        port: 0,
        socket: manualEndpoints.mixedLegacyGameServer.localSocket,
        visibility: "local"
      }
    );

    for (const discoverGameServerAdminEndpoints of [
      discoverAuthGameServerAdminEndpoints,
      discoverAdminApiGameServerAdminEndpoints,
      discoverMailGameServerAdminEndpoints
    ]) {
      const endpoints = await discoverGameServerAdminEndpoints(redis, {
        registryKeyPrefix,
        discoveryCacheTtlMs: 0
      });
      assertFlatEndpoint(findEndpoint(endpoints, mixedLegacyGameServerInstanceId), {
        serviceName: "game-server",
        instanceId: mixedLegacyGameServerInstanceId,
        name: "admin",
        protocol: "tcp",
        host: manualEndpoints.mixedLegacyGameServer.host,
        port: manualEndpoints.mixedLegacyGameServer.adminPort
      });
      assertFlatEndpoint(findEndpoint(endpoints, mixedEndpointGameServerInstanceId), {
        serviceName: "game-server",
        instanceId: mixedEndpointGameServerInstanceId,
        name: "admin",
        protocol: "tcp",
        host: manualEndpoints.mixedEndpointGameServer.adminHost,
        port: manualEndpoints.mixedEndpointGameServer.adminPort
      });
    }

    const adminApiDiscovery = createAdminApiRegistryDiscoveryClient(redis, {
      registryKeyPrefix,
      discoveryCacheTtlMs: 0
    });
    const adminApiSnapshot = await adminApiDiscovery.refreshSnapshot("game-server", {
      endpointName: "admin",
      kind: "all_endpoints",
      refreshIntervalMs: 0
    });
    assert.equal(adminApiSnapshot.ok, true);
    assertEndpointSelection(
      adminApiSnapshot.value.find(({ instance }) => instance.id === mixedLegacyGameServerInstanceId),
      {
        serviceName: "game-server",
        instanceId: mixedLegacyGameServerInstanceId,
        name: "admin",
        protocol: "tcp",
        host: manualEndpoints.mixedLegacyGameServer.host,
        port: manualEndpoints.mixedLegacyGameServer.adminPort,
        visibility: "admin"
      }
    );
    assertEndpointSelection(
      adminApiSnapshot.value.find(({ instance }) => instance.id === mixedEndpointGameServerInstanceId),
      {
        serviceName: "game-server",
        instanceId: mixedEndpointGameServerInstanceId,
        name: "admin",
        protocol: "tcp",
        host: manualEndpoints.mixedEndpointGameServer.adminHost,
        port: manualEndpoints.mixedEndpointGameServer.adminPort,
        visibility: "admin"
      }
    );
    adminApiDiscovery.stop();
  } finally {
    await redis.quit();
    await cleanupRegistryInstances(redisUrl, mixedRegistryInstances, registryKeyPrefix);
  }
});

test("registry discovery ignores instances whose heartbeat expired but keeps healthy peers", { timeout: 60000 }, async () => {
  const heartbeatRegistryInstances = [
    { serviceName: "game-server", instanceId: heartbeatExpiredGameServerInstanceId },
    { serviceName: "game-server", instanceId: heartbeatHealthyGameServerInstanceId }
  ];

  await cleanupRegistryInstances(redisUrl, heartbeatRegistryInstances, registryKeyPrefix);
  await writeRegistryPayloads(createHeartbeatExpiryGameServerPayloads(), registryKeyPrefix);

  const redis = new Redis(redisUrl, {
    lazyConnect: true,
    maxRetriesPerRequest: 1,
    enableReadyCheck: true
  });

  await redis.connect();
  try {
    const expiredInstanceKey = registryInstanceKey(
      registryKeyPrefix,
      "game-server",
      heartbeatExpiredGameServerInstanceId
    );
    const expiredHeartbeatKey = registryHeartbeatKey(
      registryKeyPrefix,
      "game-server",
      heartbeatExpiredGameServerInstanceId
    );
    const healthyHeartbeatKey = registryHeartbeatKey(
      registryKeyPrefix,
      "game-server",
      heartbeatHealthyGameServerInstanceId
    );

    assert.equal(await redis.del(expiredHeartbeatKey), 1);
    assert.equal(await redis.exists(expiredInstanceKey), 1);
    assert.equal(await redis.exists(expiredHeartbeatKey), 0);
    assert.equal(await redis.exists(healthyHeartbeatKey), 1);

    const discovery = new RegistryDiscoveryClient(redis, {
      registryKeyPrefix,
      discoveryCacheTtlMs: 0
    });
    const adminEndpoints = await discovery.discoverAllEndpoints("game-server", "admin");

    assert.equal(
      adminEndpoints.some(({ instance, endpoint: discoveredEndpoint }) =>
        instance.id === heartbeatExpiredGameServerInstanceId ||
        (
          discoveredEndpoint.host === manualEndpoints.heartbeatExpiredGameServer.adminHost &&
          discoveredEndpoint.port === manualEndpoints.heartbeatExpiredGameServer.adminPort
        )
      ),
      false,
      "expected expired-heartbeat game-server.admin endpoint to be filtered"
    );
    assert.ok(
      adminEndpoints.some(({ instance, endpoint: discoveredEndpoint }) =>
        instance.id === heartbeatHealthyGameServerInstanceId &&
        discoveredEndpoint.host === manualEndpoints.heartbeatHealthyGameServer.adminHost &&
        discoveredEndpoint.port === manualEndpoints.heartbeatHealthyGameServer.adminPort
      ),
      "expected healthy game-server.admin peer to remain discoverable"
    );

    const requiredAdminEndpoint = await discovery.discoverRequiredEndpoint("game-server", "admin");
    assert.notEqual(requiredAdminEndpoint.instance.id, heartbeatExpiredGameServerInstanceId);
    assert.notEqual(requiredAdminEndpoint.endpoint.host, manualEndpoints.heartbeatExpiredGameServer.adminHost);
    assert.notEqual(requiredAdminEndpoint.endpoint.port, manualEndpoints.heartbeatExpiredGameServer.adminPort);

    const consumerAdminEndpoints = await discoverAdminApiGameServerAdminEndpoints(redis, {
      registryKeyPrefix,
      discoveryCacheTtlMs: 0
    });
    assert.equal(
      consumerAdminEndpoints.some((discoveredEndpoint) =>
        discoveredEndpoint.instanceId === heartbeatExpiredGameServerInstanceId ||
        (
          discoveredEndpoint.host === manualEndpoints.heartbeatExpiredGameServer.adminHost &&
          discoveredEndpoint.port === manualEndpoints.heartbeatExpiredGameServer.adminPort
        )
      ),
      false,
      "expected admin-api game-server admin discovery to filter expired-heartbeat instance"
    );
    assertFlatEndpoint(findEndpoint(consumerAdminEndpoints, heartbeatHealthyGameServerInstanceId), {
      serviceName: "game-server",
      instanceId: heartbeatHealthyGameServerInstanceId,
      name: "admin",
      protocol: "tcp",
      host: manualEndpoints.heartbeatHealthyGameServer.adminHost,
      port: manualEndpoints.heartbeatHealthyGameServer.adminPort
    });
  } finally {
    await redis.quit();
    await cleanupRegistryInstances(redisUrl, heartbeatRegistryInstances, registryKeyPrefix);
  }
});
