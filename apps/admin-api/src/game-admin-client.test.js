import assert from "node:assert/strict";
import net from "node:net";
import test from "node:test";

import {
  GameAdminClient,
  MESSAGE_TYPE,
  buildAdminAuthBody,
  buildAssertionAuthBody,
  describeAdminEndpoint,
  normalizeGameAdminActor,
  sendRequest
} from "./game-admin-client.js";

const config = { gameAdminToken: "secret-admin-token" };
const HEADER_LEN = 14;

function encodeTestPacket(messageType, seq, body = Buffer.alloc(0)) {
  const header = Buffer.alloc(HEADER_LEN);
  header.writeUInt16BE(0xcafe, 0);
  header.writeUInt8(1, 2);
  header.writeUInt8(0, 3);
  header.writeUInt16BE(messageType, 4);
  header.writeUInt32BE(seq, 6);
  header.writeUInt32BE(body.length, 10);
  return Buffer.concat([header, body]);
}

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

test("assertion auth mode does not carry a reusable admin token", () => {
  assert.deepEqual(JSON.parse(buildAssertionAuthBody().toString("utf8")), { mode: "assertion" });
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

test("GameAdminClient sendItem sends a signed assertion before the characterId payload", async () => {
  let grantPayload = null;
  let assertionPayload = null;
  let authPayload = null;
  const server = net.createServer((socket) => {
    let buffer = Buffer.alloc(0);

    socket.on("data", (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);
      while (buffer.length >= HEADER_LEN) {
        const bodyLen = buffer.readUInt32BE(10);
        const packetLen = HEADER_LEN + bodyLen;
        if (buffer.length < packetLen) {
          return;
        }

        const messageType = buffer.readUInt16BE(4);
        const seq = buffer.readUInt32BE(6);
        const body = buffer.subarray(HEADER_LEN, packetLen);
        buffer = buffer.subarray(packetLen);

        if (messageType === MESSAGE_TYPE.ADMIN_AUTH_REQ) {
          authPayload = JSON.parse(body.toString("utf8"));
        }
        if (messageType === MESSAGE_TYPE.ADMIN_OPERATION_ASSERTION_REQ) {
          assertionPayload = JSON.parse(body.toString("utf8"));
        }
        if (messageType === MESSAGE_TYPE.GM_SEND_ITEM_REQ) {
          grantPayload = JSON.parse(body.toString("utf8"));
          socket.write(encodeTestPacket(MESSAGE_TYPE.GM_SEND_ITEM_RES, seq));
        }
      }
    });
  });

  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = server.address().port;
  const assertions = [];
  const client = new GameAdminClient({
    registryDiscoveryEnabled: false,
    registryDiscoveryRequired: false,
    localDiscoveryFallbackEnabled: true,
    gameServerAdminHost: "127.0.0.1",
    gameServerAdminPort: port,
    gameAdminToken: "secret-admin-token",
    gameAdminConnectTimeoutMs: 1000,
    gameAdminWriteTimeoutMs: 1000,
    gameAdminReadTimeoutMs: 1000,
    gameAdminMaxResponseBytes: 1024
  }, null, {
    async issue(context, service, instanceId, payload) {
      assertions.push({ context, service, instanceId, payload: payload.toString("utf8") });
      return {
        version: 1,
        operationId: "op-test-1",
        requestId: "req-test-1",
        traceId: "trace-test-1",
        issuer: "admin-api",
        keyId: "test-v1",
        actorId: "admin-7",
        permission: "gm.send_item",
        scope: {},
        target: {},
        service,
        instanceId,
        issuedAtMs: 1,
        expiresAtMs: 2,
        payloadSha256: "fixture",
        signature: "fixture"
      };
    }
  });

  try {
    await client.sendItem("chr_1", "1001", 2, "gift", {
      assertionContext: { actorId: "admin-7", permission: "gm.send_item", scope: {}, target: { targetType: "character", targetIds: ["chr_1"] } }
    });

    assert.deepEqual(grantPayload, {
      characterId: "chr_1",
      itemId: "1001",
      itemCount: 2,
      reason: "gift"
    });
    assert.equal("playerId" in grantPayload, false);
    assert.deepEqual(authPayload, { mode: "assertion" });
    assert.equal(assertionPayload.permission, "gm.send_item");
    assert.deepEqual(assertions.map(({ service, instanceId }) => [service, instanceId]), [["game-server", "local-fallback"]]);
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});

test("GameAdminClient rejects a token-only game-server write", async () => {
  const client = new GameAdminClient({
    registryDiscoveryEnabled: false,
    registryDiscoveryRequired: false,
    localDiscoveryFallbackEnabled: true,
    gameServerAdminHost: "127.0.0.1",
    gameServerAdminPort: 7500
  });

  await assert.rejects(
    client.sendItem("chr_1", "1001", 1, "gift"),
    { code: "ADMIN_ASSERTION_REQUIRED" }
  );
});

function createDiscoveryRedis(instances) {
  const hashes = new Map();
  const keys = new Set();

  for (const instance of instances) {
    hashes.set(`service:game-server:instances:${instance.id}:data`, JSON.stringify(instance));
    keys.add(`heartbeat:game-server:${instance.id}`);
  }

  return {
    expireHeartbeat(instanceId) {
      keys.delete(`heartbeat:game-server:${instanceId}`);
    },
    async zrangebyscore() {
      return [...hashes.keys()]
        .map((key) => key.match(/^service:game-server:instances:([^:]+):data$/)?.[1])
        .filter(Boolean)
        .sort();
    },
    pipeline() {
      const commands = [];
      const pipeline = {
        hget(key, field) {
          commands.push(["hget", key, field]);
          return pipeline;
        },
        exists(key) {
          commands.push(["exists", key]);
          return pipeline;
        },
        async exec() {
          return commands.map(([command, key, field]) => [
            null,
            command === "hget" ? hashes.get(`${key}:${field}`) || null : keys.has(key) ? 1 : 0
          ]);
        }
      };
      return pipeline;
    },
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

function protoVarint(value) {
  let remaining = BigInt(value);
  const bytes = [];
  do {
    let byte = Number(remaining & 0x7fn);
    remaining >>= 7n;
    if (remaining) byte |= 0x80;
    bytes.push(byte);
  } while (remaining);
  return Buffer.from(bytes);
}

function protoString(field, value) {
  const body = Buffer.from(value, "utf8");
  return Buffer.concat([protoVarint((field << 3) | 2), protoVarint(body.length), body]);
}

function protoBool(field, value) {
  return value ? Buffer.from([(field << 3), 1]) : Buffer.alloc(0);
}

function protoUint(field, value) {
  return value ? Buffer.concat([Buffer.from([(field << 3)]), protoVarint(value)]) : Buffer.alloc(0);
}

function protoFixed(field, wireType, byteLength) {
  return Buffer.concat([protoVarint((field << 3) | wireType), Buffer.alloc(byteLength, 0xa5)]);
}

function decodeTestStringFields(body) {
  const result = {};
  let offset = 0;
  while (offset < body.length) {
    const tag = body[offset++];
    assert.equal(tag & 7, 2);
    const length = body[offset++];
    result[tag >> 3] = body.subarray(offset, offset + length).toString("utf8");
    offset += length;
  }
  return result;
}

test("GameAdminClient encodes drain protobuf and preserves shutdown blocker state", async () => {
  const requests = [];
  const server = net.createServer((socket) => {
    let buffer = Buffer.alloc(0);
    socket.on("data", (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);
      while (buffer.length >= HEADER_LEN) {
        const bodyLength = buffer.readUInt32BE(10);
        const packetLength = HEADER_LEN + bodyLength;
        if (buffer.length < packetLength) return;
        const messageType = buffer.readUInt16BE(4);
        const seq = buffer.readUInt32BE(6);
        const body = buffer.subarray(HEADER_LEN, packetLength);
        buffer = buffer.subarray(packetLength);
        if ([MESSAGE_TYPE.ADMIN_UPDATE_CONFIG_REQ, MESSAGE_TYPE.REQUEST_SERVER_SHUTDOWN_REQ].includes(messageType)) {
          requests.push({ messageType, body });
        }
        if (messageType === MESSAGE_TYPE.ADMIN_UPDATE_CONFIG_REQ) {
          socket.write(encodeTestPacket(
            MESSAGE_TYPE.ADMIN_UPDATE_CONFIG_RES,
            seq,
            Buffer.concat([
              protoUint(1, 2),
              protoFixed(90, 5, 4)
            ])
          ));
        }
        if (messageType === MESSAGE_TYPE.REQUEST_SERVER_SHUTDOWN_REQ) {
          socket.write(encodeTestPacket(
            MESSAGE_TYPE.REQUEST_SERVER_SHUTDOWN_RES,
            seq,
            Buffer.concat([
              protoString(2, "SHUTDOWN_CONNECTIONS_REMAIN"),
              protoUint(3, 2),
              protoBool(6, true),
              protoBool(8, true),
              protoFixed(91, 1, 8)
            ])
          ));
        }
      }
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = server.address().port;
  const assertionService = {
    async issue(context, service, instanceId) {
      return {
        version: 1,
        operationId: `op-${context.requestId}`,
        requestId: context.requestId,
        traceId: context.traceId,
        issuer: "admin-api",
        keyId: "test-v1",
        actorId: String(context.actorId),
        permission: context.permission,
        scope: {},
        target: {},
        service,
        instanceId,
        issuedAtMs: 1,
        expiresAtMs: 2,
        payloadSha256: "fixture",
        signature: "fixture"
      };
    }
  };
  const client = new GameAdminClient({
    registryDiscoveryEnabled: true,
    registryDiscoveryRequired: true,
    gameAdminConnectTimeoutMs: 1000,
    gameAdminWriteTimeoutMs: 1000,
    gameAdminReadTimeoutMs: 1000,
    gameAdminMaxResponseBytes: 4096
  }, createDiscoveryRedis([gameServerInstance("game-server-a", "127.0.0.1", port)]), assertionService);
  const assertionContext = {
    actorId: "admin-7",
    permission: "game.config.write",
    scope: {},
    target: { targetType: "service", targetIds: ["game-server-a"] },
    requestId: "request-control-1",
    traceId: "trace-control-1"
  };

  try {
    const update = await client.updateConfig("drain_mode", "on", {
      targetInstanceId: "game-server-a",
      requireRegistryTarget: true,
      assertionContext
    });
    assert.deepEqual({ ok: update.ok, errorCode: update.errorCode }, { ok: true, errorCode: "" });
    const shutdown = await client.requestServerShutdown("rolling replacement", {
      targetInstanceId: "game-server-a",
      requireRegistryTarget: true,
      assertionContext: { ...assertionContext, requestId: "request-control-2" }
    });
    assert.deepEqual({
      ok: shutdown.ok,
      error_code: shutdown.error_code,
      shutdown_armed: shutdown.shutdown_armed,
      connection_count: shutdown.connection_count,
      drain_mode_enabled: shutdown.drain_mode_enabled
    }, {
      ok: false,
      error_code: "SHUTDOWN_CONNECTIONS_REMAIN",
      shutdown_armed: true,
      connection_count: 2,
      drain_mode_enabled: true
    });
    assert.deepEqual(decodeTestStringFields(requests[0].body), { 1: "drain_mode", 2: "on" });
    assert.deepEqual(decodeTestStringFields(requests[1].body), { 1: "rolling replacement" });
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});

test("GameAdminClient registry-only writes reject direct endpoint overrides", async () => {
  const client = new GameAdminClient({
    registryDiscoveryEnabled: true,
    registryDiscoveryRequired: true
  });
  const options = {
    endpoint: { host: "127.0.0.1", port: 7500, instanceId: "bypass" },
    targetInstanceId: "game-server-a",
    requireRegistryTarget: true
  };

  await assert.rejects(
    client.updateConfig("drain_mode", "on", options),
    { code: "GAME_SERVER_ADMIN_DIRECT_ENDPOINT_FORBIDDEN" }
  );
  await assert.rejects(
    client.requestServerShutdown("rolling replacement", options),
    { code: "GAME_SERVER_ADMIN_DIRECT_ENDPOINT_FORBIDDEN" }
  );
});

test("GameAdminClient shutdown reaches an exact live drained registry target", async () => {
  const server = net.createServer((socket) => {
    let buffer = Buffer.alloc(0);
    socket.on("data", (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);
      while (buffer.length >= HEADER_LEN) {
        const bodyLen = buffer.readUInt32BE(10);
        const packetLen = HEADER_LEN + bodyLen;
        if (buffer.length < packetLen) return;
        const messageType = buffer.readUInt16BE(4);
        const seq = buffer.readUInt32BE(6);
        buffer = buffer.subarray(packetLen);
        if (messageType === MESSAGE_TYPE.REQUEST_SERVER_SHUTDOWN_REQ) {
          socket.write(encodeTestPacket(
            MESSAGE_TYPE.REQUEST_SERVER_SHUTDOWN_RES,
            seq,
            Buffer.concat([protoBool(1, true), protoBool(6, true), protoBool(8, true)])
          ));
        }
      }
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = server.address().port;
  const drained = gameServerInstance("game-server-drained", "127.0.0.1", port);
  drained.healthy = false;
  const client = new GameAdminClient({
    registryDiscoveryEnabled: true,
    registryDiscoveryRequired: true,
    gameAdminConnectTimeoutMs: 1000,
    gameAdminWriteTimeoutMs: 1000,
    gameAdminReadTimeoutMs: 1000,
    gameAdminMaxResponseBytes: 4096
  }, createDiscoveryRedis([drained]), {
    async issue() { return { signature: "fixture" }; }
  });

  try {
    const result = await client.requestServerShutdown("rolling replacement", {
      targetInstanceId: drained.id,
      requireRegistryTarget: true,
      allowLiveUnhealthyAdminTarget: true,
      assertionContext: { requestId: "shutdown-drained" }
    });
    assert.equal(result.ok, true);
    assert.equal(result.shutdown_armed, true);
    assert.equal(result.endpoint.instanceId, drained.id);
    assert.equal(result.endpoint.healthy, false);
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});

test("GameAdminClient live-unhealthy discovery remains shutdown-only", async () => {
  const drained = gameServerInstance("game-server-drained", "127.0.0.1", 7500);
  drained.healthy = false;
  const client = new GameAdminClient(
    { registryDiscoveryEnabled: true, registryDiscoveryRequired: true },
    createDiscoveryRedis([drained])
  );
  const options = {
    targetInstanceId: drained.id,
    requireRegistryTarget: true,
    allowLiveUnhealthyAdminTarget: true
  };

  for (const operation of [
    () => client.updateConfig("drain_mode", "off", options),
    () => client.broadcast("title", "content", "System", options),
    () => client.sendItem("character-1", "item-1", 1, "test", options),
    () => client.kickPlayer("player-1", "test", options),
    () => client.banPlayer("player-1", 60, "test", options)
  ]) {
    await assert.rejects(operation, { code: "GAME_SERVER_ADMIN_LIVE_DISCOVERY_FORBIDDEN" });
  }
  await assert.rejects(
    client.updateConfig("drain_mode", "off", {
      targetInstanceId: drained.id,
      requireRegistryTarget: true
    }),
    { code: "GAME_SERVER_ADMIN_ENDPOINT_NOT_FOUND" }
  );
  await assert.rejects(
    client.requestServerShutdown("test", {
      requireRegistryTarget: true,
      allowLiveUnhealthyAdminTarget: true
    }),
    { code: "GAME_SERVER_ADMIN_TARGET_REQUIRED" }
  );
});

test("GameAdminClient live shutdown rejects expired heartbeat and unhealthy admin endpoint", async () => {
  const expired = gameServerInstance("game-server-expired", "127.0.0.1", 7500);
  expired.healthy = false;
  const expiredRedis = createDiscoveryRedis([expired]);
  expiredRedis.expireHeartbeat(expired.id);
  const expiredClient = new GameAdminClient(
    { registryDiscoveryEnabled: true, registryDiscoveryRequired: true, registryDiscoveryCacheTtlMs: 0 },
    expiredRedis
  );
  const options = {
    targetInstanceId: expired.id,
    requireRegistryTarget: true,
    allowLiveUnhealthyAdminTarget: true,
    assertionContext: {}
  };
  await assert.rejects(
    expiredClient.requestServerShutdown("test", options),
    { code: "GAME_SERVER_ADMIN_ENDPOINT_NOT_FOUND" }
  );

  const badEndpoint = gameServerInstance("game-server-bad-admin", "127.0.0.1", 7501);
  badEndpoint.healthy = false;
  badEndpoint.endpoints[0].healthy = false;
  const badClient = new GameAdminClient(
    { registryDiscoveryEnabled: true, registryDiscoveryRequired: true, registryDiscoveryCacheTtlMs: 0 },
    createDiscoveryRedis([badEndpoint])
  );
  await assert.rejects(
    badClient.requestServerShutdown("test", { ...options, targetInstanceId: badEndpoint.id }),
    { code: "GAME_SERVER_ADMIN_ENDPOINT_NOT_FOUND" }
  );
});

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

test("describeAdminEndpoint returns only safe endpoint summary fields", () => {
  const summary = describeAdminEndpoint({
    service: "game-server",
    instanceId: "game-server-a",
    endpointName: "admin",
    protocol: "tcp",
    host: "10.0.0.1",
    port: 7500,
    healthy: true,
    fallback: false,
    source: "registry",
    reason: "discovered",
    metadata: {
      token: "secret",
      Authorization: "Bearer secret"
    }
  });

  assert.deepEqual(summary, {
    service: "game-server",
    instanceId: "game-server-a",
    instance_id: "game-server-a",
    endpointName: "admin",
    endpoint_name: "admin",
    protocol: "tcp",
    host: "10.0.0.1",
    port: 7500,
    healthy: true,
    fallback: false,
    source: "registry",
    reason: "discovered"
  });
  assert.equal("metadata" in summary, false);
});

test("GameAdminClient broadcast returns actual endpoint summaries for every called instance", async () => {
  const endpoints = [
    {
      service: "game-server",
      instanceId: "game-server-a",
      instance_id: "game-server-a",
      endpointName: "admin",
      endpoint_name: "admin",
      protocol: "tcp",
      host: "10.0.0.1",
      port: 7500,
      healthy: true,
      fallback: false,
      source: "registry",
      reason: "discovered"
    },
    {
      service: "game-server",
      instanceId: "game-server-b",
      instance_id: "game-server-b",
      endpointName: "admin",
      endpoint_name: "admin",
      protocol: "tcp",
      host: "10.0.0.2",
      port: 7501,
      healthy: true,
      fallback: false,
      source: "registry",
      reason: "discovered"
    }
  ];
  class TestGameAdminClient extends GameAdminClient {
    async listAdminEndpoints() {
      return endpoints;
    }

    async sendToEndpoint() {
      return Buffer.alloc(0);
    }
  }
  const client = new TestGameAdminClient({});

  const result = await client.broadcast("Notice", "Server restart", "Ops");

  assert.equal(result.ok, true);
  assert.deepEqual(
    result.instances.map((instance) => instance.endpoint),
    endpoints.map(describeAdminEndpoint)
  );
  assert.deepEqual(
    result.instances.map((instance) => instance.instanceId),
    ["game-server-a", "game-server-b"]
  );
});

test("GameAdminClient broadcast failure exposes attempted endpoint summaries", async () => {
  const endpoints = [
    {
      service: "game-server",
      instanceId: "game-server-a",
      instance_id: "game-server-a",
      endpointName: "admin",
      endpoint_name: "admin",
      protocol: "tcp",
      host: "10.0.0.1",
      port: 7500,
      healthy: true,
      fallback: false,
      source: "registry",
      reason: "discovered"
    },
    {
      service: "game-server",
      instanceId: "game-server-b",
      instance_id: "game-server-b",
      endpointName: "admin",
      endpoint_name: "admin",
      protocol: "tcp",
      host: "10.0.0.2",
      port: 7501,
      healthy: true,
      fallback: false,
      source: "registry",
      reason: "discovered"
    }
  ];
  class TestGameAdminClient extends GameAdminClient {
    async listAdminEndpoints() {
      return endpoints;
    }

    async sendToEndpoint(endpoint) {
      if (endpoint.instanceId === "game-server-b") {
        const error = new Error("connection refused");
        error.code = "ECONNREFUSED";
        throw error;
      }
      return Buffer.alloc(0);
    }
  }
  const client = new TestGameAdminClient({});

  await assert.rejects(
    client.broadcast("Notice", "Server restart", "Ops"),
    (error) => {
      assert.equal(error.code, "ECONNREFUSED");
      assert.deepEqual(error.gameAdminEndpoint, describeAdminEndpoint(endpoints[1]));
      assert.deepEqual(
        error.gameAdminInstances.map((instance) => instance.endpoint),
        endpoints.map(describeAdminEndpoint)
      );
      assert.deepEqual(
        error.gameAdminInstances.map((instance) => instance.ok),
        [true, false]
      );
      return true;
    }
  );
});
