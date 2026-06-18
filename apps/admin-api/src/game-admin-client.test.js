import assert from "node:assert/strict";
import net from "node:net";
import test from "node:test";

import {
  GameAdminClient,
  MESSAGE_TYPE,
  buildAdminAuthBody,
  normalizeGameAdminActor,
  sendRequest
} from "./game-admin-client.js";

const config = { gameAdminToken: "secret-admin-token" };

test("admin auth body keeps legacy plain token when actor is missing", () => {
  const body = buildAdminAuthBody(config);

  assert.equal(body.toString("utf8"), "secret-admin-token");
});

test("admin auth body uses JSON envelope when actor is valid", () => {
  const body = buildAdminAuthBody(config, " ops@example.com ");

  assert.deepEqual(JSON.parse(body.toString("utf8")), {
    token: "secret-admin-token",
    actor: "ops@example.com"
  });
});

test("admin auth body falls back to plain token for invalid actor", () => {
  const body = buildAdminAuthBody(config, "ops+admin@example.com");

  assert.equal(normalizeGameAdminActor("ops+admin@example.com"), null);
  assert.equal(body.toString("utf8"), "secret-admin-token");
});

test("admin actor rejects values longer than game-server limit", () => {
  assert.equal(normalizeGameAdminActor("a".repeat(129)), null);
});

test("admin client rejects response larger than configured limit", async () => {
  const server = net.createServer((socket) => {
    socket.once("data", () => {
      const header = Buffer.alloc(14);
      header.writeUInt16BE(0xcafe, 0);
      header.writeUInt8(1, 2);
      header.writeUInt8(0, 3);
      header.writeUInt16BE(MESSAGE_TYPE.GM_SEND_ITEM_RES, 4);
      header.writeUInt32BE(1, 6);
      header.writeUInt32BE(64, 10);
      socket.write(header);
    });
  });

  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = server.address().port;

  try {
    await assert.rejects(
      sendRequest(
        {
          gameServerAdminHost: "127.0.0.1",
          gameServerAdminPort: port,
          gameAdminToken: "secret-admin-token",
          gameAdminConnectTimeoutMs: 1000,
          gameAdminWriteTimeoutMs: 1000,
          gameAdminReadTimeoutMs: 1000,
          gameAdminMaxResponseBytes: 32
        },
        MESSAGE_TYPE.GM_SEND_ITEM_REQ,
        Buffer.from("{}"),
        MESSAGE_TYPE.GM_SEND_ITEM_RES
      ),
      { code: "GAME_ADMIN_RESPONSE_TOO_LARGE" }
    );
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});

function createDiscoveryRedis(instances) {
  const hashes = new Map();
  const keys = new Set();

  for (const instance of instances) {
    hashes.set(`service:game-server:instances:${instance.id}:data`, JSON.stringify(instance));
    keys.add(`heartbeat:game-server:${instance.id}`);
  }

  return {
    async scan(cursor, _match, pattern) {
      if (cursor !== "0") {
        return ["0", []];
      }
      const prefix = pattern.replace("*", "");
      return [
        "0",
        [...hashes.keys()]
          .map((key) => key.slice(0, -5))
          .filter((key) => key.startsWith(prefix))
      ];
    },
    async exists(key) {
      return keys.has(key) ? 1 : 0;
    },
    async hget(key, field) {
      return hashes.get(`${key}:${field}`) || null;
    }
  };
}

function gameServerInstance(id, host, port) {
  return {
    schema_version: 2,
    id,
    name: "game-server",
    host,
    port: 7000,
    admin_port: port,
    local_socket: "",
    endpoints: [
      {
        name: "admin",
        protocol: "tcp",
        host,
        port,
        socket: "",
        visibility: "admin",
        metadata: {},
        healthy: true
      }
    ],
    tags: [],
    weight: 100,
    metadata: {},
    registered_at: 1,
    healthy: true
  };
}

test("GameAdminClient lists discovered game-server admin endpoints", async () => {
  const client = new GameAdminClient(
    { registryDiscoveryEnabled: true, registryDiscoveryRequired: true },
    createDiscoveryRedis([
      gameServerInstance("game-server-a", "10.0.0.1", 7500),
      gameServerInstance("game-server-b", "10.0.0.2", 7501)
    ])
  );

  const endpoints = await client.listAdminEndpoints();

  assert.deepEqual(
    endpoints.map((endpoint) => [endpoint.instanceId, endpoint.host, endpoint.port]),
    [
      ["game-server-a", "10.0.0.1", 7500],
      ["game-server-b", "10.0.0.2", 7501]
    ]
  );
});

test("GameAdminClient requires explicit target for single-target GM commands with multiple instances", async () => {
  const client = new GameAdminClient(
    { registryDiscoveryEnabled: true, registryDiscoveryRequired: true },
    createDiscoveryRedis([
      gameServerInstance("game-server-a", "10.0.0.1", 7500),
      gameServerInstance("game-server-b", "10.0.0.2", 7501)
    ])
  );

  await assert.rejects(
    client.resolveAdminEndpoint({ requireExplicitTarget: true }),
    { code: "GAME_SERVER_ADMIN_TARGET_REQUIRED" }
  );
});

test("GameAdminClient resolves explicit target instance", async () => {
  const client = new GameAdminClient(
    { registryDiscoveryEnabled: true, registryDiscoveryRequired: true },
    createDiscoveryRedis([
      gameServerInstance("game-server-a", "10.0.0.1", 7500),
      gameServerInstance("game-server-b", "10.0.0.2", 7501)
    ])
  );

  const endpoint = await client.resolveAdminEndpoint({
    requireExplicitTarget: true,
    targetInstanceId: "game-server-b"
  });

  assert.equal(endpoint.host, "10.0.0.2");
  assert.equal(endpoint.port, 7501);
});

test("GameAdminClient rejects local fallback when discovery is required", async () => {
  const client = new GameAdminClient({
    registryDiscoveryEnabled: false,
    registryDiscoveryRequired: true,
    localDiscoveryFallbackEnabled: true,
    gameServerAdminHost: "127.0.0.1",
    gameServerAdminPort: 7500
  });

  await assert.rejects(client.listAdminEndpoints(), { code: "SERVICE_DISCOVERY_REQUIRED" });
});

test("GameAdminClient rejects direct fallback when local fallback is disabled", async () => {
  const client = new GameAdminClient({
    registryDiscoveryEnabled: false,
    registryDiscoveryRequired: false,
    localDiscoveryFallbackEnabled: false,
    gameServerAdminHost: "203.0.113.20",
    gameServerAdminPort: 17500
  });

  await assert.rejects(client.listAdminEndpoints(), { code: "SERVICE_DISCOVERY_REQUIRED" });
});

test("GameAdminClient marks optional local fallback endpoint source and reason", async () => {
  const client = new GameAdminClient({
    registryDiscoveryEnabled: false,
    registryDiscoveryRequired: false,
    localDiscoveryFallbackEnabled: true,
    gameServerAdminHost: "127.0.0.1",
    gameServerAdminPort: 7500
  });

  const endpoints = await client.listAdminEndpoints();

  assert.deepEqual(endpoints.map(({ source, reason, instance_id }) => ({ source, reason, instance_id })), [
    { source: "fallback", reason: "fallback_used", instance_id: "local-fallback" }
  ]);
});
