import assert from "node:assert/strict";
import net from "node:net";
import test from "node:test";

import {
  MESSAGE_TYPE,
  buildAdminAuthBody,
  buildGrantMailAttachmentsPayload,
  GameAdminClient,
  getDefaultGameAdminActor,
  normalizeGameAdminActor,
  normalizeServiceActorCandidate,
  sendRequest
} from "./game-admin-client.js";

const config = {
  gameAdminToken: "secret-admin-token",
  serviceInstanceId: "mail-001",
  serviceName: "mail-service",
  gameServerAdminHost: "127.0.0.1",
  gameServerAdminPort: 7500
};

function createRedisWithGameServers(instances) {
  const instanceById = new Map(instances.map((instance) => [instance.id, instance]));

  return {
    async scan(cursor, _match, pattern) {
      assert.equal(cursor, "0");
      assert.equal(pattern, "service:game-server:instances:*");
      return ["0", instances.map((instance) => `service:game-server:instances:${instance.id}`)];
    },
    async exists(key) {
      const instanceId = key.split(":").at(-1);
      return instanceById.has(instanceId) ? 1 : 0;
    },
    async hget(key, field) {
      assert.equal(field, "data");
      const instanceId = key.split(":").at(-1);
      const instance = instanceById.get(instanceId);
      return instance ? JSON.stringify(instance) : null;
    }
  };
}

function createGameServerInstance(id, port) {
  return {
    schema_version: 2,
    id,
    name: "game-server",
    host: "127.0.0.1",
    port: 7000,
    admin_port: port,
    local_socket: "",
    endpoints: [
      {
        name: "admin",
        protocol: "tcp",
        host: "127.0.0.1",
        port,
        socket: "",
        visibility: "admin",
        metadata: {},
        healthy: true
      }
    ],
    tags: ["game"],
    weight: 100,
    metadata: {},
    registered_at: Date.now(),
    healthy: true
  };
}

function encodeTestPacket(messageType, seq, body = Buffer.from("{}")) {
  const header = Buffer.alloc(14);
  header.writeUInt16BE(0xcafe, 0);
  header.writeUInt8(1, 2);
  header.writeUInt8(0, 3);
  header.writeUInt16BE(messageType, 4);
  header.writeUInt32BE(seq, 6);
  header.writeUInt32BE(body.length, 10);
  return Buffer.concat([header, body]);
}

async function createGrantCaptureServer(label) {
  const requests = [];
  const server = net.createServer((socket) => {
    let buffer = Buffer.alloc(0);

    socket.on("data", (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);

      while (buffer.length >= 14) {
        const bodyLen = buffer.readUInt32BE(10);
        const packetLen = 14 + bodyLen;
        if (buffer.length < packetLen) {
          return;
        }

        const messageType = buffer.readUInt16BE(4);
        const seq = buffer.readUInt32BE(6);
        const body = buffer.subarray(14, packetLen);
        buffer = buffer.subarray(packetLen);

        if (messageType === MESSAGE_TYPE.GM_SEND_ITEM_REQ) {
          requests.push({ label, body: JSON.parse(body.toString("utf8")) });
          socket.write(encodeTestPacket(MESSAGE_TYPE.GM_SEND_ITEM_RES, seq));
        }
      }
    });
  });

  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));

  return {
    port: server.address().port,
    requests,
    close: () => new Promise((resolve) => server.close(resolve))
  };
}

test("admin auth body keeps legacy plain token when actor is missing", () => {
  const body = buildAdminAuthBody(config);

  assert.equal(body.toString("utf8"), "secret-admin-token");
});

test("admin auth body uses JSON envelope when actor is valid", () => {
  const body = buildAdminAuthBody(config, " mail-service ");

  assert.deepEqual(JSON.parse(body.toString("utf8")), {
    token: "secret-admin-token",
    actor: "mail-service"
  });
});

test("admin auth body falls back to plain token for invalid actor", () => {
  const body = buildAdminAuthBody(config, "mail/service");

  assert.equal(normalizeGameAdminActor("mail/service"), null);
  assert.equal(body.toString("utf8"), "secret-admin-token");
});

test("default service actor uses normalized service identity", () => {
  assert.equal(getDefaultGameAdminActor(config), "mail-001");
  assert.equal(
    getDefaultGameAdminActor({ ...config, serviceInstanceId: "mail/service 01" }),
    "mail-service-01"
  );
  assert.equal(
    getDefaultGameAdminActor({ ...config, serviceInstanceId: "mail/service", serviceName: "mail service" }),
    "mail-service"
  );
  assert.equal(normalizeServiceActorCandidate("mail/service 01"), "mail-service-01");
});

test("grant mail attachments payload keeps stable idempotency fields", () => {
  const body = buildGrantMailAttachmentsPayload(
    "chr_1",
    "mail_claim:mail-1",
    [{ itemId: 1001, count: 2, binded: true }],
    "claim mail mail-1"
  );

  assert.deepEqual(JSON.parse(body.toString("utf8")), {
    requestId: "mail_claim:mail-1",
    characterId: "chr_1",
    items: [{ itemId: 1001, count: 2, binded: true }],
    source: "mail-claim",
    reason: "claim mail mail-1"
  });
  assert.equal("playerId" in JSON.parse(body.toString("utf8")), false);
});

test("mail admin client rejects response larger than configured limit", async () => {
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

test("GameAdminClient requires targetInstanceId when multiple registry endpoints exist", async () => {
  const client = new GameAdminClient(
    { ...config, registryDiscoveryEnabled: true, registryDiscoveryRequired: true },
    createRedisWithGameServers([
      createGameServerInstance("game-server-a", 7501),
      createGameServerInstance("game-server-b", 7502)
    ])
  );

  await assert.rejects(
    () => client.resolveAdminEndpoint({ requireExplicitTarget: true }),
    { code: "GAME_SERVER_ADMIN_TARGET_REQUIRED" }
  );
});

test("GameAdminClient resolves explicit targetInstanceId to matching registry endpoint", async () => {
  const client = new GameAdminClient(
    { ...config, registryDiscoveryEnabled: true, registryDiscoveryRequired: true },
    createRedisWithGameServers([
      createGameServerInstance("game-server-a", 7501),
      createGameServerInstance("game-server-b", 7502)
    ])
  );

  const endpoint = await client.resolveAdminEndpoint({
    requireExplicitTarget: true,
    targetInstanceId: "game-server-b"
  });

  assert.equal(endpoint.instanceId, "game-server-b");
  assert.equal(endpoint.port, 7502);
});

test("GameAdminClient grantMailAttachments sends grant to explicit registry target endpoint", async () => {
  const serverA = await createGrantCaptureServer("game-server-a");
  const serverB = await createGrantCaptureServer("game-server-b");

  try {
    const client = new GameAdminClient(
      {
        ...config,
        registryDiscoveryEnabled: true,
        registryDiscoveryRequired: true,
        gameAdminConnectTimeoutMs: 1000,
        gameAdminWriteTimeoutMs: 1000,
        gameAdminReadTimeoutMs: 1000,
        gameAdminMaxResponseBytes: 1024
      },
      createRedisWithGameServers([
        createGameServerInstance("game-server-a", serverA.port),
        createGameServerInstance("game-server-b", serverB.port)
      ])
    );

    const result = await client.grantMailAttachments(
      "chr_1",
      "mail_claim:mail-1",
      [{ itemId: 1001, count: 2, binded: true }],
      "claim mail mail-1",
      { targetInstanceId: "game-server-b" }
    );

    assert.deepEqual(result, { ok: true, instanceId: "game-server-b" });
    assert.equal(serverA.requests.length, 0);
    assert.equal(serverB.requests.length, 1);
    assert.equal(serverB.requests[0].body.requestId, "mail_claim:mail-1");
    assert.equal(serverB.requests[0].body.characterId, "chr_1");
    assert.equal("playerId" in serverB.requests[0].body, false);
  } finally {
    await Promise.all([serverA.close(), serverB.close()]);
  }
});

test("GameAdminClient grantMailAttachments rejects ambiguous registry endpoints without explicit target", async () => {
  const serverA = await createGrantCaptureServer("game-server-a");
  const serverB = await createGrantCaptureServer("game-server-b");

  try {
    const client = new GameAdminClient(
      {
        ...config,
        registryDiscoveryEnabled: true,
        registryDiscoveryRequired: true
      },
      createRedisWithGameServers([
        createGameServerInstance("game-server-a", serverA.port),
        createGameServerInstance("game-server-b", serverB.port)
      ])
    );

    await assert.rejects(
      () => client.grantMailAttachments(
        "chr_1",
        "mail_claim:mail-1",
        [{ itemId: 1001, count: 2, binded: true }],
        "claim mail mail-1"
      ),
      { code: "GAME_SERVER_ADMIN_TARGET_REQUIRED" }
    );
    assert.equal(serverA.requests.length, 0);
    assert.equal(serverB.requests.length, 0);
  } finally {
    await Promise.all([serverA.close(), serverB.close()]);
  }
});

test("GameAdminClient forbids local fallback when discovery is required", async () => {
  const client = new GameAdminClient({
    ...config,
    registryDiscoveryEnabled: false,
    registryDiscoveryRequired: true
  });

  await assert.rejects(client.listAdminEndpoints(), { code: "SERVICE_DISCOVERY_REQUIRED" });
});

test("GameAdminClient forbids direct fallback when local fallback is disabled", async () => {
  const client = new GameAdminClient({
    ...config,
    registryDiscoveryEnabled: false,
    registryDiscoveryRequired: false,
    localDiscoveryFallbackEnabled: false,
    gameServerAdminHost: "203.0.113.20",
    gameServerAdminPort: 17500
  });

  await assert.rejects(client.listAdminEndpoints(), { code: "SERVICE_DISCOVERY_REQUIRED" });
});

test("GameAdminClient allows local fallback when discovery is disabled and optional", async () => {
  const client = new GameAdminClient({
    ...config,
    registryDiscoveryEnabled: false,
    registryDiscoveryRequired: false,
    localDiscoveryFallbackEnabled: true
  });

  const endpoints = await client.listAdminEndpoints();

  assert.deepEqual(endpoints, [
    {
      service: "game-server",
      instanceId: "local-fallback",
      instance_id: "local-fallback",
      endpointName: "admin",
      endpoint_name: "admin",
      protocol: "tcp",
      host: "127.0.0.1",
      port: 7500,
      healthy: true,
      fallback: true,
      source: "fallback",
      reason: "fallback_used"
    }
  ]);
});
