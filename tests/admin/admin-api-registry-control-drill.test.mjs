import assert from "node:assert/strict";
import http from "node:http";
import net from "node:net";
import { register } from "node:module";
import path from "node:path";
import { afterEach, test } from "node:test";
import { pathToFileURL } from "node:url";

import "reflect-metadata";
import { Module } from "@nestjs/common";
import { NestFactory } from "@nestjs/core";
import { JwtModule, JwtService } from "@nestjs/jwt";
import { FastifyAdapter } from "@nestjs/platform-fastify";

import { createServiceInstancePayload } from "../../packages/service-registry/node/registry-schema.js";
import {
  GameAdminClient,
  MESSAGE_TYPE
} from "../../apps/admin-api/src/game-admin-client.js";

process.env.TS_NODE_PROJECT = path.resolve("apps/admin-api/tsconfig.json");
process.env.TS_NODE_TRANSPILE_ONLY = "true";
register("ts-node/esm", pathToFileURL("./"));

const { GmController } = await import("../../apps/admin-api/src/gm/gm.controller.js");
const { JwtAuthGuard } = await import("../../apps/admin-api/src/auth/jwt-auth.guard.js");
const { RolesGuard } = await import("../../apps/admin-api/src/auth/roles.guard.js");
const { HttpExceptionFilter } = await import("../../apps/admin-api/src/common/http-exception.filter.js");
const { MonitoringController } = await import("../../apps/admin-api/src/monitoring/monitoring.controller.js");
const { MonitoringService } = await import("../../apps/admin-api/src/monitoring/monitoring.service.js");
const {
  ADMIN_CONFIG,
  ADMIN_DB_POOL,
  ADMIN_GAME_ADMIN_CLIENT,
  ADMIN_NATS,
  ADMIN_REDIS,
  ADMIN_SESSION_STORE,
  ADMIN_STORE
} = await import("../../apps/admin-api/src/tokens.js");

const MAGIC = 0xcafe;
const VERSION = 1;
const HEADER_LEN = 14;
const gameAdminToken = "strict-registry-game-admin-token";
const proxyReadToken = "strict-registry-proxy-read-token";
const serversToClose = [];
const appsToClose = [];

afterEach(async () => {
  const apps = appsToClose.splice(0);
  await Promise.all(apps.map((app) => app.close()));

  const closing = serversToClose.splice(0);
  await Promise.all(closing.map((server) => closeServer(server)));
});

function randomId(prefix) {
  return `${prefix}-${Date.now()}-${Math.random().toString(16).slice(2, 10)}`;
}

function strictConfig(registryKeyPrefix, overrides = {}) {
  return {
    registryDiscoveryEnabled: true,
    registryDiscoveryRequired: true,
    registryDiscoveryCacheTtlMs: 0,
    registryKeyPrefix,
    jwtSecret: "strict-registry-admin-jwt-secret",
    localDiscoveryFallbackEnabled: false,
    disallowLegacyDirectConfig: true,
    gameServerAdminHost: "127.0.0.1",
    gameServerAdminPort: 1,
    gameAdminToken,
    gameAdminConnectTimeoutMs: 200,
    gameAdminWriteTimeoutMs: 200,
    gameAdminReadTimeoutMs: 500,
    gameAdminMaxResponseBytes: 4096,
    gameProxyAdminHost: "127.0.0.1",
    gameProxyAdminPort: 1,
    gameProxyAdminToken: "strict-registry-proxy-write-token",
    gameProxyAdminReadToken: proxyReadToken,
    gameProxyAdminRequestTimeoutMs: 500,
    gameProxyAdminMaxResponseBytes: 4096,
    trustProxy: false,
    trustedProxies: [],
    ...overrides
  };
}

class MemoryRegistryRedis {
  constructor({ registryKeyPrefix, instances, serviceHeartbeats = [] }) {
    this.hashes = new Map();
    this.values = new Map();

    for (const instance of instances) {
      this.hashes.set(
        `${registryKeyPrefix}service:${instance.name}:instances:${instance.id}`,
        { data: JSON.stringify(instance) }
      );
      this.values.set(`${registryKeyPrefix}heartbeat:${instance.name}:${instance.id}`, "1");
    }

    const nowSeconds = Math.floor(Date.now() / 1000);
    for (const serviceName of serviceHeartbeats) {
      this.values.set(`metrics:heartbeat:${serviceName}`, String(nowSeconds));
    }
  }

  async get(key) {
    return this.values.get(key) ?? null;
  }

  async hget(key, field) {
    return this.hashes.get(key)?.[field] ?? null;
  }

  async hgetall() {
    return {};
  }

  async exists(key) {
    return this.values.has(key) || this.hashes.has(key) ? 1 : 0;
  }

  async scan(cursor, ...args) {
    if (cursor !== "0") {
      return ["0", []];
    }

    const matchIndex = args.findIndex((arg) => String(arg).toUpperCase() === "MATCH");
    const pattern = matchIndex >= 0 ? String(args[matchIndex + 1]) : "*";
    const keys = [...new Set([...this.hashes.keys(), ...this.values.keys()])]
      .filter((key) => matchesGlob(key, pattern))
      .sort();
    return ["0", keys];
  }
}

function matchesGlob(value, pattern) {
  const escaped = pattern
    .replace(/[.+?^${}()|[\]\\]/g, "\\$&")
    .replace(/\*/g, ".*");
  return new RegExp(`^${escaped}$`).test(value);
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

function adminEndpointFields(endpoint) {
  return {
    instance_id: endpoint.instance_id,
    host: endpoint.host,
    port: endpoint.port,
    protocol: endpoint.protocol,
    source: endpoint.source,
    fallback: endpoint.fallback,
    reason: endpoint.reason
  };
}

function adminEndpointFieldsFromInstance(instance) {
  return {
    instance_id: instance.instance_id,
    status: instance.status,
    endpoint: adminEndpointFields(instance.endpoint)
  };
}

function createGameServerRegistryInstance(instanceId, adminPort) {
  const metadata = {
    service_name: "game-server",
    service_instance_id: instanceId,
    server_id: instanceId,
    zone: "test",
    build_version: "registry-control-drill"
  };
  return createServiceInstancePayload({
    id: instanceId,
    name: "game-server",
    host: "203.0.113.10",
    port: 17000,
    admin_port: 17500,
    endpoints: [
      endpoint("client", "tcp", "203.0.113.10", 17000, "internal", metadata),
      endpoint("admin", "tcp", "127.0.0.1", adminPort, "admin", metadata)
    ],
    tags: ["game", "admin", "strict-registry-test"],
    metadata
  });
}

function createGameProxyRegistryInstance(instanceId, adminPort) {
  const metadata = {
    service_name: "game-proxy",
    service_instance_id: instanceId,
    zone: "test",
    build_version: "registry-control-drill"
  };
  return createServiceInstancePayload({
    id: instanceId,
    name: "game-proxy",
    host: "203.0.113.20",
    port: 14000,
    endpoints: [
      endpoint("client", "kcp", "203.0.113.20", 14000, "public", metadata),
      endpoint("admin", "http", "127.0.0.1", adminPort, "admin", metadata)
    ],
    tags: ["proxy", "admin", "strict-registry-test"],
    metadata
  });
}

async function startGameAdminMock(instanceId) {
  const requests = [];
  const authPackets = [];
  const sockets = new Set();

  const server = net.createServer((socket) => {
    sockets.add(socket);
    socket.once("close", () => sockets.delete(socket));
    let buffer = Buffer.alloc(0);

    socket.on("data", (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);

      while (buffer.length >= HEADER_LEN) {
        const bodyLen = buffer.readUInt32BE(10);
        const packetLen = HEADER_LEN + bodyLen;
        if (buffer.length < packetLen) {
          return;
        }

        const packet = decodePacket(buffer.subarray(0, packetLen));
        buffer = buffer.subarray(packetLen);

        if (packet.messageType === MESSAGE_TYPE.ADMIN_AUTH_REQ) {
          authPackets.push(parseAuthBody(packet.body));
          continue;
        }

        const bodyText = packet.body.toString("utf8");
        requests.push({
          instanceId,
          messageType: packet.messageType,
          seq: packet.seq,
          bodyText,
          bodyJson: bodyText ? JSON.parse(bodyText) : null
        });

        const responseType = packet.messageType === MESSAGE_TYPE.ADMIN_SERVER_STATUS_REQ
          ? MESSAGE_TYPE.ADMIN_SERVER_STATUS_RES
          : packet.messageType === MESSAGE_TYPE.GM_SEND_ITEM_REQ
            ? MESSAGE_TYPE.GM_SEND_ITEM_RES
            : MESSAGE_TYPE.ERROR_RES;
        socket.write(encodePacket(responseType, packet.seq, Buffer.alloc(0)));
      }
    });
  });

  const port = await listen(server);
  serversToClose.push({ server, sockets });
  return { port, requests, authPackets };
}

async function startProxyAdminMock(body) {
  const requests = [];
  const server = http.createServer((req, res) => {
    requests.push({
      method: req.method,
      url: req.url,
      authorization: req.headers.authorization
    });
    assert.equal(req.method, "GET");
    assert.equal(req.url, "/rollout");
    assert.equal(req.headers.authorization, `Bearer ${proxyReadToken}`);
    res.setHeader("content-type", "application/json");
    res.end(JSON.stringify(body));
  });

  const port = await listen(server);
  serversToClose.push({ server });
  return { port, requests };
}

function encodePacket(messageType, seq, body) {
  const header = Buffer.alloc(HEADER_LEN);
  header.writeUInt16BE(MAGIC, 0);
  header.writeUInt8(VERSION, 2);
  header.writeUInt8(0, 3);
  header.writeUInt16BE(messageType, 4);
  header.writeUInt32BE(seq, 6);
  header.writeUInt32BE(body.length, 10);
  return Buffer.concat([header, body]);
}

function decodePacket(buffer) {
  assert.equal(buffer.readUInt16BE(0), MAGIC);
  assert.equal(buffer.readUInt8(2), VERSION);
  const bodyLen = buffer.readUInt32BE(10);
  assert.equal(buffer.length, HEADER_LEN + bodyLen);
  return {
    messageType: buffer.readUInt16BE(4),
    seq: buffer.readUInt32BE(6),
    body: buffer.subarray(HEADER_LEN)
  };
}

function parseAuthBody(body) {
  const text = body.toString("utf8");
  try {
    return JSON.parse(text);
  } catch {
    return { token: text, actor: null };
  }
}

function listen(server) {
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      server.off("error", reject);
      resolve(server.address().port);
    });
  });
}

async function closeServer(entry) {
  for (const socket of entry.sockets || []) {
    socket.destroy();
  }
  await new Promise((resolve) => entry.server.close(resolve));
}

function makeAdminStore() {
  const audits = [];
  return {
    audits,
    async appendAuditLog(entry) {
      audits.push(entry);
    }
  };
}

async function createAdminHttpTestApp({ config, redis, adminStore = makeAdminStore() }) {
  const gameAdminClient = new GameAdminClient(config, redis);
  const nats = {
    publishes: [],
    async publishJson(subject, payload) {
      this.publishes.push({ subject, payload });
    }
  };
  const sessionStore = {
    async getSession(jti) {
      return jti === "strict-registry-session"
        ? { adminId: "1", tokenVersion: 0 }
        : null;
    },
    async getTokenVersion(adminId) {
      assert.equal(String(adminId), "1");
      return 0;
    }
  };
  adminStore.findAdminByUsername = async (username) => {
    return username === "ops"
      ? {
          id: "1",
          username: "ops",
          displayName: "Ops",
          role: "admin",
          status: "active"
        }
      : null;
  };
  adminStore.appendSecurityAuditLog ??= async () => {};

  class AdminRegistryControlDrillModule {}
  Module({
    imports: [JwtModule.register({})],
    controllers: [MonitoringController, GmController],
    providers: [
      MonitoringService,
      { provide: ADMIN_CONFIG, useValue: config },
      { provide: ADMIN_REDIS, useValue: redis },
      { provide: ADMIN_DB_POOL, useValue: {} },
      { provide: ADMIN_NATS, useValue: nats },
      { provide: ADMIN_STORE, useValue: adminStore },
      { provide: ADMIN_SESSION_STORE, useValue: sessionStore },
      { provide: ADMIN_GAME_ADMIN_CLIENT, useValue: gameAdminClient },
      JwtAuthGuard,
      RolesGuard
    ]
  })(AdminRegistryControlDrillModule);

  const app = await NestFactory.create(
    AdminRegistryControlDrillModule,
    new FastifyAdapter({ bodyLimit: 64 * 1024 }),
    { logger: false, abortOnError: false }
  );
  app.useGlobalFilters(new HttpExceptionFilter());
  await app.init();
  const fastify = app.getHttpAdapter().getInstance();
  await fastify.ready();
  const jwtService = app.get(JwtService);
  const token = await jwtService.signAsync(
    {
      sub: "1",
      username: "ops",
      role: "admin",
      jti: "strict-registry-session",
      tokenVersion: 0
    },
    { secret: config.jwtSecret }
  );
  appsToClose.push(app);

  return { app, fastify, adminStore, nats, gameAdminClient, authHeader: `Bearer ${token}` };
}

function parseJsonResponse(response) {
  return JSON.parse(response.body);
}

test("admin-api HTTP status and GM routes use registry game-server.admin under strict discovery", async () => {
  const registryKeyPrefix = `test:admin-api-control:${randomId("registry")}:`;
  const statusAdmin = await startGameAdminMock("game-server-status");
  const gmAdmin = await startGameAdminMock("game-server-gm");
  const legacyGameAdmin = await startGameAdminMock("legacy-direct-game-server-admin");
  const redis = new MemoryRegistryRedis({
    registryKeyPrefix,
    serviceHeartbeats: ["game-server"],
    instances: [
      createGameServerRegistryInstance("game-server-status", statusAdmin.port),
      createGameServerRegistryInstance("game-server-gm", gmAdmin.port)
    ]
  });
  const config = strictConfig(registryKeyPrefix, {
    gameServerAdminHost: "127.0.0.1",
    gameServerAdminPort: legacyGameAdmin.port
  });
  const adminStore = makeAdminStore();
  const { fastify, authHeader } = await createAdminHttpTestApp({ config, redis, adminStore });

  const servicesResponse = await fastify.inject({
    method: "GET",
    url: "/api/admin/monitoring/services",
    headers: { authorization: authHeader }
  });
  assert.equal(servicesResponse.statusCode, 200);
  const services = parseJsonResponse(servicesResponse);
  const gameServer = services.services.find((service) => service.name === "game-server");
  assert.equal(gameServer.status, "online");
  assert.deepEqual(
    gameServer.endpoints.map(adminEndpointFields),
    [
      {
        instance_id: "game-server-gm",
        host: "127.0.0.1",
        port: gmAdmin.port,
        protocol: "tcp",
        source: "registry",
        fallback: false,
        reason: "discovered"
      },
      {
        instance_id: "game-server-status",
        host: "127.0.0.1",
        port: statusAdmin.port,
        protocol: "tcp",
        source: "registry",
        fallback: false,
        reason: "discovered"
      }
    ]
  );

  const gmResponse = await fastify.inject({
    method: "POST",
    url: "/api/v1/gm/send-item",
    headers: {
      authorization: authHeader,
      "content-type": "application/json"
    },
    payload: {
      characterId: "character-001",
      itemId: "item-001",
      itemCount: 3,
      reason: "strict registry drill",
      targetInstanceId: "game-server-gm"
    }
  });
  assert.equal(gmResponse.statusCode, 200);
  const gmResult = parseJsonResponse(gmResponse);

  assert.equal(gmResult.ok, true);
  assert.deepEqual(gmAdmin.requests.map((request) => ({
    messageType: request.messageType,
    bodyJson: request.bodyJson
  })), [
    {
      messageType: MESSAGE_TYPE.GM_SEND_ITEM_REQ,
      bodyJson: {
        characterId: "character-001",
        itemId: "item-001",
        itemCount: 3,
        reason: "strict registry drill"
      }
    }
  ]);
  assert.deepEqual(gmAdmin.authPackets, [
    { token: gameAdminToken, actor: "ops" }
  ]);
  assert.equal(
    statusAdmin.requests.filter((request) => request.messageType === MESSAGE_TYPE.GM_SEND_ITEM_REQ).length,
    0
  );
  assert.deepEqual(legacyGameAdmin.authPackets, []);
  assert.deepEqual(legacyGameAdmin.requests, []);
  assert.equal(adminStore.audits[0].details.requestedTargetInstanceId, "game-server-gm");
});

test("admin-api HTTP rollout drain reads registry game-proxy.admin under strict discovery", async () => {
  const registryKeyPrefix = `test:admin-api-control:${randomId("registry")}:`;
  const legacyProxyAdmin = await startProxyAdminMock({
    ok: true,
    rollout_session: null,
    drain_evaluation: { status: "NoActiveRollout" }
  });
  const proxyAdmin = await startProxyAdminMock({
    ok: true,
    rollout_session: {
      rollout_epoch: "rollout-registry-drill",
      old_server_id: "game-server-old",
      new_server_id: "game-server-new",
      state: "Active",
      started_at_ms: 1713000000000
    },
    drain_evaluation: {
      status: "Drained",
      blocked_room_count: 0,
      blocked_player_count: 0,
      stale_room_route_count: 0,
      stale_player_route_count: 0,
      blocked_room_samples: [],
      blocked_player_samples: []
    }
  });
  const redis = new MemoryRegistryRedis({
    registryKeyPrefix,
    serviceHeartbeats: ["game-proxy"],
    instances: [
      createGameProxyRegistryInstance("game-proxy-registry", proxyAdmin.port)
    ]
  });
  const { fastify, authHeader } = await createAdminHttpTestApp({
    config: strictConfig(registryKeyPrefix, {
      gameProxyAdminHost: "127.0.0.1",
      gameProxyAdminPort: legacyProxyAdmin.port
    }),
    redis
  });

  const servicesResponse = await fastify.inject({
    method: "GET",
    url: "/api/admin/monitoring/services",
    headers: { authorization: authHeader }
  });
  assert.equal(servicesResponse.statusCode, 200);
  const services = parseJsonResponse(servicesResponse);
  const gameProxy = services.services.find((service) => service.name === "game-proxy");
  assert.equal(gameProxy.status, "online");
  assert.deepEqual(gameProxy.endpoints.map(adminEndpointFields), [
    {
      instance_id: "game-proxy-registry",
      host: "127.0.0.1",
      port: proxyAdmin.port,
      protocol: "http",
      source: "registry",
      fallback: false,
      reason: "discovered"
    }
  ]);

  const rolloutResponse = await fastify.inject({
    method: "GET",
    url: "/api/admin/monitoring/rollout-drain",
    headers: { authorization: authHeader }
  });
  assert.equal(rolloutResponse.statusCode, 200);
  const rollout = parseJsonResponse(rolloutResponse);
  assert.equal(rollout.ok, true);
  assert.equal(rollout.source, "game-proxy");
  assert.equal(rollout.status, "drained");
  assert.equal(rollout.rollout.epoch, "rollout-registry-drill");
  assert.deepEqual(rollout.instances.map(adminEndpointFieldsFromInstance), [
    {
      instance_id: "game-proxy-registry",
      status: "drained",
      endpoint: {
        instance_id: "game-proxy-registry",
        host: "127.0.0.1",
        port: proxyAdmin.port,
        protocol: "http",
        source: "registry",
        fallback: false,
        reason: "discovered"
      }
    }
  ]);
  assert.deepEqual(proxyAdmin.requests, [
    {
      method: "GET",
      url: "/rollout",
      authorization: `Bearer ${proxyReadToken}`
    }
  ]);
  assert.deepEqual(legacyProxyAdmin.requests, []);
});
