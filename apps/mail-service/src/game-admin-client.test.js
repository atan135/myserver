import assert from "node:assert/strict";
import net from "node:net";
import test from "node:test";

import { DbMailStore } from "./db-store.js";
import {
  MESSAGE_TYPE,
  buildAdminAuthBody,
  buildGrantMailAttachmentsPayload,
  computeGrantRequestFingerprint,
  gameOnlineRouteKey,
  GameAdminClient,
  getDefaultGameAdminActor,
  normalizeGameAdminActor,
  normalizeServiceActorCandidate,
  sendRequest
} from "./game-admin-client.js";
import { configureLogger } from "./logger.js";
import { MailsService } from "./mails/mails.service.js";

configureLogger({
  appName: "mail-game-admin-test",
  logEnableConsole: false,
  logEnableFile: false,
  logLevel: "fatal"
});

const config = {
  gameAdminToken: "secret-admin-token",
  serviceInstanceId: "mail-001",
  serviceName: "mail-service",
  gameServerAdminHost: "127.0.0.1",
  gameServerAdminPort: 7500
};

function createRedisWithGameServers(instances, routes = new Map()) {
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
    },
    async get(key) {
      const value = routes.get(key);
      return typeof value === "function" ? value() : value ?? null;
    }
  };
}

function onlineRoute(
  characterId,
  instanceId,
  sessionId = "42",
  authorityGeneration = instanceId.endsWith("b") ? "2" : "1",
  authorityToken = (instanceId.endsWith("b") ? "b" : "a").repeat(64)
) {
  return JSON.stringify({
    version: 2,
    character_id: characterId,
    instance_id: instanceId,
    session_id: sessionId,
    authority_generation: authorityGeneration,
    authority_token: authorityToken
  });
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

function encodeTestPacket(messageType, seq, body = Buffer.from("{}"), flags = 0) {
  const header = Buffer.alloc(14);
  header.writeUInt16BE(0xcafe, 0);
  header.writeUInt8(1, 2);
  header.writeUInt8(flags, 3);
  header.writeUInt16BE(messageType, 4);
  header.writeUInt32BE(seq, 6);
  header.writeUInt32BE(body.length, 10);
  return Buffer.concat([header, body]);
}

function encodeVarint(value) {
  let remaining = BigInt(value);
  const bytes = [];
  do {
    let byte = Number(remaining & 0x7fn);
    remaining >>= 7n;
    if (remaining > 0n) byte |= 0x80;
    bytes.push(byte);
  } while (remaining > 0n);
  return Buffer.from(bytes);
}

function protobufField(fieldNumber, value, wireType = 2) {
  const key = encodeVarint((fieldNumber << 3) | wireType);
  if (wireType === 0) return Buffer.concat([key, encodeVarint(value)]);
  const body = Buffer.isBuffer(value) ? value : Buffer.from(String(value));
  return Buffer.concat([key, encodeVarint(body.length), body]);
}

function encodeGrantItem(item) {
  const fields = [
    protobufField(1, item.itemId, 0),
    protobufField(2, item.count, 0)
  ];
  if (item.binded) fields.push(protobufField(3, 1, 0));
  return Buffer.concat(fields);
}

function encodeGrantResponse(request, overrides = {}) {
  const ok = overrides.ok ?? true;
  const summary = Buffer.concat([
    protobufField(1, request.characterId),
    protobufField(2, "mail-claim"),
    ...request.items.map((item) => protobufField(3, encodeGrantItem(item)))
  ]);
  const fields = [];
  if (ok) fields.push(protobufField(1, 1, 0));
  if (overrides.errorCode) fields.push(protobufField(2, overrides.errorCode));
  if (overrides.applied ?? ok) fields.push(protobufField(3, 1, 0));
  fields.push(protobufField(4, request.requestId));
  fields.push(protobufField(5, request.requestFingerprint));
  if (overrides.errorCategory) fields.push(protobufField(6, overrides.errorCategory));
  fields.push(protobufField(7, overrides.resultState || (ok ? "applied" : "not_applied")));
  if (overrides.retryable) fields.push(protobufField(8, 1, 0));
  if (ok) fields.push(protobufField(9, summary));
  fields.push(protobufField(10, request.traceId));
  return Buffer.concat(fields);
}

async function createGrantCaptureServer(label, onGrant = null) {
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
          const request = JSON.parse(body.toString("utf8"));
          requests.push({ label, body: request });
          const response = onGrant ? onGrant(request) : encodeGrantResponse(request);
          if (response !== null && response !== undefined) {
            socket.write(encodeTestPacket(MESSAGE_TYPE.GM_SEND_ITEM_RES, seq, response));
          }
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

function createCrossLayerClaimService(serverPort, characterId, mailId) {
  const routeKey = gameOnlineRouteKey("", characterId);
  const gameAdminClient = new GameAdminClient({
    ...config,
    registryDiscoveryEnabled: true,
    registryDiscoveryRequired: true,
    localDiscoveryFallbackEnabled: false,
    gameAdminConnectTimeoutMs: 1000,
    gameAdminWriteTimeoutMs: 1000,
    gameAdminReadTimeoutMs: 1000,
    gameAdminMaxResponseBytes: 4096
  }, createRedisWithGameServers(
    [createGameServerInstance("game-server-a", serverPort)],
    new Map([[routeKey, onlineRoute(characterId, "game-server-a")]])
  ));
  const mailStore = new DbMailStore(null);
  mailStore.memory.set(mailId, {
    id: 1,
    mail_id: mailId,
    sender_type: "system",
    sender_id: "system",
    sender_name: "system",
    from_player_id: "system",
    to_player_id: "player-1",
    title: "Reward",
    content: "",
    attachments: [{ type: "item", id: 1001, count: 1 }],
    mail_type: "system",
    created_by_type: "system",
    created_by_id: "system",
    created_by_name: "system",
    status: "unread",
    created_at: new Date(),
    read_at: null,
    claimed_at: null,
    expires_at: null
  });
  let observedError = null;
  const observingGameAdminClient = {
    async grantMailAttachments(...args) {
      try {
        return await gameAdminClient.grantMailAttachments(...args);
      } catch (error) {
        observedError = error;
        throw error;
      }
    }
  };
  return {
    mailStore,
    getObservedError: () => observedError,
    service: new MailsService(
      mailStore,
      {},
      observingGameAdminClient,
      { claimLeaseMs: 30_000, serviceInstanceId: "mail-test" },
      null
    )
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
    "claim mail mail-1",
    { traceId: "0123456789abcdef0123456789abcdef" }
  );

  assert.deepEqual(JSON.parse(body.toString("utf8")), {
    requestId: "mail_claim:mail-1",
    mailId: "mail-1",
    characterId: "chr_1",
    items: [{ itemId: 1001, count: 2, binded: true }],
    requestFingerprint: "sha256:9f4049b6004cca687efea31f7a03b8f19fe65563c42c9a89657dec3f1f2bbab8",
    source: "mail-claim",
    reason: "claim mail mail-1",
    traceId: "0123456789abcdef0123456789abcdef",
    routeGeneration: "",
    routeToken: ""
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

test("mail admin client enforces bounded response read timeout", async () => {
  const server = net.createServer(() => {});
  const sockets = new Set();
  server.on("connection", (socket) => {
    sockets.add(socket);
    socket.once("close", () => sockets.delete(socket));
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
          gameAdminReadTimeoutMs: 30,
          gameAdminMaxResponseBytes: 1024
        },
        MESSAGE_TYPE.GM_SEND_ITEM_REQ,
        Buffer.from("{}"),
        MESSAGE_TYPE.GM_SEND_ITEM_RES
      ),
      (error) => {
        assert.equal(error.code, "GAME_ADMIN_READ_TIMEOUT");
        assert.equal(error.requestPhase, "response_read");
        assert.equal(error.requestWritten, true);
        assert.equal(error.errorCategory, "RESULT_UNKNOWN");
        assert.equal(error.resultState, "unknown");
        return true;
      }
    );
  } finally {
    for (const socket of sockets) socket.destroy();
    await new Promise((resolve) => server.close(resolve));
  }
});

test("mail admin client rejects mismatched response sequence", async () => {
  const server = net.createServer((socket) => {
    let buffer = Buffer.alloc(0);
    socket.on("data", (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);
      while (buffer.length >= 14) {
        const packetLen = 14 + buffer.readUInt32BE(10);
        if (buffer.length < packetLen) return;
        const messageType = buffer.readUInt16BE(4);
        const seq = buffer.readUInt32BE(6);
        buffer = buffer.subarray(packetLen);
        if (messageType === MESSAGE_TYPE.GM_SEND_ITEM_REQ) {
          socket.write(encodeTestPacket(MESSAGE_TYPE.GM_SEND_ITEM_RES, seq + 1, Buffer.alloc(0)));
        }
      }
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
          gameAdminMaxResponseBytes: 1024
        },
        MESSAGE_TYPE.GM_SEND_ITEM_REQ,
        Buffer.from("{}"),
        MESSAGE_TYPE.GM_SEND_ITEM_RES
      ),
      { code: "UNEXPECTED_RESPONSE_SEQUENCE" }
    );
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});

test("mail admin client rejects nonzero response flags", async () => {
  const server = net.createServer((socket) => {
    let buffer = Buffer.alloc(0);
    socket.on("data", (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);
      while (buffer.length >= 14) {
        const packetLen = 14 + buffer.readUInt32BE(10);
        if (buffer.length < packetLen) return;
        const messageType = buffer.readUInt16BE(4);
        const seq = buffer.readUInt32BE(6);
        buffer = buffer.subarray(packetLen);
        if (messageType === MESSAGE_TYPE.GM_SEND_ITEM_REQ) {
          socket.write(encodeTestPacket(
            MESSAGE_TYPE.GM_SEND_ITEM_RES,
            seq,
            Buffer.alloc(0),
            1
          ));
        }
      }
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));

  try {
    await assert.rejects(
      sendRequest(
        {
          gameServerAdminHost: "127.0.0.1",
          gameServerAdminPort: server.address().port,
          gameAdminToken: "secret-admin-token",
          gameAdminConnectTimeoutMs: 1000,
          gameAdminWriteTimeoutMs: 1000,
          gameAdminReadTimeoutMs: 1000,
          gameAdminMaxResponseBytes: 1024
        },
        MESSAGE_TYPE.GM_SEND_ITEM_REQ,
        Buffer.from("{}"),
        MESSAGE_TYPE.GM_SEND_ITEM_RES
      ),
      { code: "INVALID_FLAGS" }
    );
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});

test("mail admin client classifies connect refusal as route unavailable before request write", async () => {
  const reserved = net.createServer();
  await new Promise((resolve) => reserved.listen(0, "127.0.0.1", resolve));
  const port = reserved.address().port;
  await new Promise((resolve) => reserved.close(resolve));

  await assert.rejects(
    sendRequest(
      {
        gameServerAdminHost: "127.0.0.1",
        gameServerAdminPort: port,
        gameAdminToken: "secret-admin-token",
        gameAdminConnectTimeoutMs: 1000,
        gameAdminWriteTimeoutMs: 1000,
        gameAdminReadTimeoutMs: 1000,
        gameAdminMaxResponseBytes: 1024
      },
      MESSAGE_TYPE.GM_SEND_ITEM_REQ,
      Buffer.from("{}"),
      MESSAGE_TYPE.GM_SEND_ITEM_RES
    ),
    (error) => {
      assert.equal(error.code, "ECONNREFUSED");
      assert.equal(error.errorCategory, "ROUTE_UNAVAILABLE");
      assert.equal(error.resultState, "not_applied");
      assert.equal(error.retryable, true);
      assert.equal(error.requestPhase, "connect");
      assert.equal(error.requestWritten, false);
      return true;
    }
  );
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

test("GameAdminClient grantMailAttachments allows explicit target only for local development", async () => {
  const serverA = await createGrantCaptureServer("game-server-a");
  const serverB = await createGrantCaptureServer("game-server-b");

  try {
    const client = new GameAdminClient(
      {
        ...config,
        registryDiscoveryEnabled: true,
        registryDiscoveryRequired: false,
        localDiscoveryFallbackEnabled: true,
        gameAdminConnectTimeoutMs: 1000,
        gameAdminWriteTimeoutMs: 1000,
        gameAdminReadTimeoutMs: 1000,
        gameAdminMaxResponseBytes: 1024
      },
      createRedisWithGameServers(
        [
          createGameServerInstance("game-server-a", serverA.port),
          createGameServerInstance("game-server-b", serverB.port)
        ],
        new Map([[
          gameOnlineRouteKey("", "chr_1"),
          onlineRoute("chr_1", "game-server-b")
        ]])
      )
    );

    const result = await client.grantMailAttachments(
      "chr_1",
      "mail_claim:mail-1",
      [{ itemId: 1001, count: 2, binded: true }],
      "claim mail mail-1",
      { targetInstanceId: "game-server-b" }
    );

    assert.equal(result.ok, true);
    assert.equal(result.applied, true);
    assert.equal(result.instanceId, "game-server-b");
    assert.equal(result.resultState, "applied");
    assert.equal(serverA.requests.length, 0);
    assert.equal(serverB.requests.length, 1);
    assert.equal(serverB.requests[0].body.requestId, "mail_claim:mail-1");
    assert.equal(serverB.requests[0].body.mailId, "mail-1");
    assert.match(serverB.requests[0].body.requestFingerprint, /^sha256:[0-9a-f]{64}$/);
    assert.match(serverB.requests[0].body.traceId, /^[0-9a-f]{32}$/);
    assert.equal(serverB.requests[0].body.characterId, "chr_1");
    assert.equal(serverB.requests[0].body.routeGeneration, "2");
    assert.equal(serverB.requests[0].body.routeToken, "b".repeat(64));
    assert.equal("playerId" in serverB.requests[0].body, false);
  } finally {
    await Promise.all([serverA.close(), serverB.close()]);
  }
});

test("GameAdminClient grantMailAttachments rejects missing authoritative online route", async () => {
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
      { code: "MAIL_CLAIM_ROUTE_UNAVAILABLE" }
    );
    assert.equal(serverA.requests.length, 0);
    assert.equal(serverB.requests.length, 0);
  } finally {
    await Promise.all([serverA.close(), serverB.close()]);
  }
});

test("GameAdminClient classifies registry scan failure as route unavailable", async () => {
  let scanCalls = 0;
  const redis = {
    async scan() {
      scanCalls += 1;
      const error = new Error("registry scan failed");
      error.code = "REDIS_SCAN_FAILED";
      throw error;
    }
  };
  const client = new GameAdminClient({
    ...config,
    registryDiscoveryEnabled: true,
    registryDiscoveryRequired: true,
    localDiscoveryFallbackEnabled: false
  }, redis);

  await assert.rejects(
    () => client.grantMailAttachments(
      "chr_1",
      "mail_claim:mail-discovery",
      [{ itemId: 1001, count: 1, binded: false }]
    ),
    (error) => {
      assert.equal(error.code, "REDIS_SCAN_FAILED");
      assert.equal(error.errorCategory, "ROUTE_UNAVAILABLE");
      assert.equal(error.resultState, "not_applied");
      assert.equal(error.retryable, true);
      assert.equal(error.requestPhase, "discovery");
      assert.equal(error.requestWritten, false);
      return true;
    }
  );
  assert.equal(scanCalls, 2);
});

test("GameAdminClient routes a strict claim to the server-owned online instance", async () => {
  const serverA = await createGrantCaptureServer("game-server-a");
  const serverB = await createGrantCaptureServer("game-server-b");
  const characterId = "chr_1";
  const routeKey = gameOnlineRouteKey("", characterId);

  try {
    const client = new GameAdminClient(
      {
        ...config,
        registryDiscoveryEnabled: true,
        registryDiscoveryRequired: true,
        localDiscoveryFallbackEnabled: false,
        gameAdminConnectTimeoutMs: 1000,
        gameAdminWriteTimeoutMs: 1000,
        gameAdminReadTimeoutMs: 1000,
        gameAdminMaxResponseBytes: 4096
      },
      createRedisWithGameServers(
        [
          createGameServerInstance("game-server-a", serverA.port),
          createGameServerInstance("game-server-b", serverB.port)
        ],
        new Map([[routeKey, onlineRoute(characterId, "game-server-b")]])
      )
    );

    const result = await client.grantMailAttachments(
      characterId,
      "mail_claim:mail-1",
      [{ itemId: 1001, count: 2, binded: false }]
    );

    assert.equal(result.ok, true);
    assert.equal(result.instanceId, "game-server-b");
    assert.equal(serverA.requests.length, 0);
    assert.equal(serverB.requests.length, 1);
  } finally {
    await Promise.all([serverA.close(), serverB.close()]);
  }
});

test("GameAdminClient rediscovers changed authority with the same request identity", async () => {
  const characterId = "chr_1";
  const routeKey = gameOnlineRouteKey("", characterId);
  let currentInstance = "game-server-a";
  let serverB;
  const serverA = await createGrantCaptureServer("game-server-a", (request) => {
    currentInstance = "game-server-b";
    return encodeGrantResponse(request, {
      ok: false,
      errorCode: "MAIL_CLAIM_ROUTE_MISMATCH",
      errorCategory: "ROUTE_UNAVAILABLE",
      resultState: "not_applied",
      retryable: true
    });
  });
  serverB = await createGrantCaptureServer("game-server-b");

  try {
    const redis = createRedisWithGameServers(
      [
        createGameServerInstance("game-server-a", serverA.port),
        createGameServerInstance("game-server-b", serverB.port)
      ],
      new Map([[routeKey, () => onlineRoute(characterId, currentInstance)]])
    );
    const client = new GameAdminClient({
      ...config,
      registryDiscoveryEnabled: true,
      registryDiscoveryRequired: true,
      localDiscoveryFallbackEnabled: false,
      gameAdminConnectTimeoutMs: 1000,
      gameAdminWriteTimeoutMs: 1000,
      gameAdminReadTimeoutMs: 1000,
      gameAdminMaxResponseBytes: 4096
    }, redis);

    const result = await client.grantMailAttachments(
      characterId,
      "mail_claim:mail-switch",
      [{ itemId: 1001, count: 1, binded: false }]
    );

    assert.equal(result.instanceId, "game-server-b");
    assert.equal(serverA.requests.length, 1);
    assert.equal(serverB.requests.length, 1);
    assert.equal(serverA.requests[0].body.requestId, serverB.requests[0].body.requestId);
    assert.equal(serverA.requests[0].body.requestFingerprint, serverB.requests[0].body.requestFingerprint);
    assert.equal(serverA.requests[0].body.traceId, serverB.requests[0].body.traceId);
    assert.notEqual(serverA.requests[0].body.routeGeneration, serverB.requests[0].body.routeGeneration);
    assert.notEqual(serverA.requests[0].body.routeToken, serverB.requests[0].body.routeToken);
  } finally {
    await Promise.all([serverA.close(), serverB.close()]);
  }
});

test("GameAdminClient does not retry after response timeout with request written", async () => {
  const characterId = "chr_1";
  const routeKey = gameOnlineRouteKey("", characterId);
  const server = await createGrantCaptureServer("game-server-a", () => null);

  try {
    const client = new GameAdminClient({
      ...config,
      registryDiscoveryEnabled: true,
      registryDiscoveryRequired: true,
      localDiscoveryFallbackEnabled: false,
      gameAdminConnectTimeoutMs: 1000,
      gameAdminWriteTimeoutMs: 1000,
      gameAdminReadTimeoutMs: 30,
      gameAdminMaxResponseBytes: 4096
    }, createRedisWithGameServers(
      [createGameServerInstance("game-server-a", server.port)],
      new Map([[routeKey, onlineRoute(characterId, "game-server-a")]])
    ));

    await assert.rejects(
      () => client.grantMailAttachments(
        characterId,
        "mail_claim:mail-timeout",
        [{ itemId: 1001, count: 1, binded: false }]
      ),
      (error) => {
        assert.equal(error.code, "GAME_ADMIN_READ_TIMEOUT");
        assert.equal(error.errorCategory, "RESULT_UNKNOWN");
        assert.equal(error.resultState, "unknown");
        assert.equal(error.requestWritten, true);
        assert.equal(error.requestPhase, "response_read");
        return true;
      }
    );
    assert.equal(server.requests.length, 1);
  } finally {
    await server.close();
  }
});

test("corrupt applied response identity becomes reconciliation_pending without player regrant", async () => {
  const characterId = "chr_1";
  const mailId = "mail-corrupt-identity";
  const server = await createGrantCaptureServer("game-server-a", (request) =>
    encodeGrantResponse({ ...request, requestId: `${request.requestId}:corrupt` })
  );

  try {
    const { service, mailStore, getObservedError } = createCrossLayerClaimService(
      server.port,
      characterId,
      mailId
    );

    const first = await service.claim(mailId, "player-1", characterId);
    const second = await service.claim(mailId, "player-1", characterId);

    assert.equal(first.claim_status, "reconciliation_pending");
    assert.equal(first._http_status, 202);
    assert.equal(first.error, "MAIL_CLAIM_RECONCILIATION_PENDING");
    assert.equal(second.claim_status, "reconciliation_pending");
    assert.equal(server.requests.length, 1);
    const workflow = await mailStore.getMailClaimWorkflow(mailId);
    assert.equal(workflow.status, "reconciliation_pending");
    assert.equal(workflow.last_error_category, "RESULT_UNKNOWN");
    assert.equal(workflow.last_result_state, "unknown");
    assert.equal(getObservedError().requestWritten, true);
    assert.equal(getObservedError().requestPhase, "response_validation");
    assert.equal(getObservedError().retryable, true);
  } finally {
    await server.close();
  }
});

test("malformed grant protobuf becomes reconciliation_pending without player regrant", async () => {
  const characterId = "chr_1";
  const mailId = "mail-malformed-protobuf";
  const server = await createGrantCaptureServer(
    "game-server-a",
    () => Buffer.from([0x0f])
  );

  try {
    const { service, mailStore, getObservedError } = createCrossLayerClaimService(
      server.port,
      characterId,
      mailId
    );

    const first = await service.claim(mailId, "player-1", characterId);
    const second = await service.claim(mailId, "player-1", characterId);

    assert.equal(first.claim_status, "reconciliation_pending");
    assert.equal(first._http_status, 202);
    assert.equal(first.error, "MAIL_CLAIM_RECONCILIATION_PENDING");
    assert.equal(second.claim_status, "reconciliation_pending");
    assert.equal(server.requests.length, 1);
    const workflow = await mailStore.getMailClaimWorkflow(mailId);
    assert.equal(workflow.status, "reconciliation_pending");
    assert.equal(workflow.last_error_code, "INVALID_PROTOBUF_RESPONSE");
    assert.equal(workflow.last_error_category, "RESULT_UNKNOWN");
    assert.equal(workflow.last_result_state, "unknown");
    assert.equal(getObservedError().requestWritten, true);
    assert.equal(getObservedError().requestPhase, "response_validation");
  } finally {
    await server.close();
  }
});

test("GameAdminClient preserves validated business error code without route retry", async () => {
  const characterId = "chr_1";
  const routeKey = gameOnlineRouteKey("", characterId);
  const server = await createGrantCaptureServer("game-server-a", (request) =>
    encodeGrantResponse(request, {
      ok: false,
      errorCode: "ITEM_NOT_FOUND",
      errorCategory: "PERMANENT_FAILURE",
      resultState: "not_applied",
      retryable: false
    })
  );

  try {
    const client = new GameAdminClient({
      ...config,
      registryDiscoveryEnabled: true,
      registryDiscoveryRequired: true,
      localDiscoveryFallbackEnabled: false,
      gameAdminConnectTimeoutMs: 1000,
      gameAdminWriteTimeoutMs: 1000,
      gameAdminReadTimeoutMs: 1000,
      gameAdminMaxResponseBytes: 4096
    }, createRedisWithGameServers(
      [createGameServerInstance("game-server-a", server.port)],
      new Map([[routeKey, onlineRoute(characterId, "game-server-a")]])
    ));

    await assert.rejects(
      () => client.grantMailAttachments(
        characterId,
        "mail_claim:mail-permanent",
        [{ itemId: 9999, count: 1, binded: false }]
      ),
      (error) => {
        assert.equal(error.code, "ITEM_NOT_FOUND");
        assert.equal(error.errorCategory, "PERMANENT_FAILURE");
        assert.equal(error.resultState, "not_applied");
        assert.equal(error.retryable, false);
        assert.equal(error.requestWritten, true);
        assert.equal(error.requestPhase, "response_validation");
        assert.equal(error.structuredGrantFailure, true);
        return true;
      }
    );
    assert.equal(server.requests.length, 1);
  } finally {
    await server.close();
  }
});

test("canonical grant fingerprint sorts and merges persisted attachment items", () => {
  assert.equal(
    computeGrantRequestFingerprint("mail_01ABC", "chr_01ABC", [
      { itemId: 1001, count: 2, binded: false }
    ]),
    "sha256:4951d5c6cbf4612e0cd91e8a7acd106b570441a6a014e96d7c4d6c423bb94dce"
  );
  assert.equal(
    computeGrantRequestFingerprint("mail-1", "chr_1", [
      { itemId: 1002, count: 1, binded: false },
      { itemId: 1001, count: 2, binded: true },
      { itemId: 1001, count: 3, binded: true }
    ]),
    computeGrantRequestFingerprint("mail-1", "chr_1", [
      { itemId: 1001, count: 5, binded: true },
      { itemId: 1002, count: 1, binded: false }
    ])
  );
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

test("GameAdminClient grants through a fixed local endpoint using the authoritative route identity", async () => {
  const server = await createGrantCaptureServer("fixed-local-endpoint");
  const characterId = "chr_1";
  const route = onlineRoute(
    characterId,
    "game-server-001",
    "42",
    "7",
    "c".repeat(64)
  );

  try {
    const client = new GameAdminClient(
      {
        ...config,
        registryDiscoveryEnabled: false,
        registryDiscoveryRequired: false,
        localDiscoveryFallbackEnabled: true,
        gameServerAdminPort: server.port,
        gameAdminConnectTimeoutMs: 1000,
        gameAdminWriteTimeoutMs: 1000,
        gameAdminReadTimeoutMs: 1000,
        gameAdminMaxResponseBytes: 4096
      },
      createRedisWithGameServers(
        [],
        new Map([[gameOnlineRouteKey("", characterId), route]])
      )
    );

    const automatic = await client.grantMailAttachments(
      characterId,
      "mail_claim:mail-local-automatic",
      [{ itemId: 1001, count: 2, binded: true }]
    );
    const explicit = await client.grantMailAttachments(
      characterId,
      "mail_claim:mail-local-explicit",
      [{ itemId: 1002, count: 1, binded: false }],
      "",
      { targetInstanceId: "game-server-001" }
    );

    assert.equal(automatic.instanceId, "game-server-001");
    assert.equal(explicit.instanceId, "game-server-001");
    assert.equal(server.requests.length, 2);
    for (const request of server.requests) {
      assert.equal(request.body.routeGeneration, "7");
      assert.equal(request.body.routeToken, "c".repeat(64));
    }
  } finally {
    await server.close();
  }
});

test("GameAdminClient rejects a local fallback target that is not the authoritative route owner", async () => {
  const server = await createGrantCaptureServer("fixed-local-endpoint");
  const characterId = "chr_1";

  try {
    const client = new GameAdminClient(
      {
        ...config,
        registryDiscoveryEnabled: false,
        registryDiscoveryRequired: false,
        localDiscoveryFallbackEnabled: true,
        gameServerAdminPort: server.port
      },
      createRedisWithGameServers(
        [],
        new Map([[
          gameOnlineRouteKey("", characterId),
          onlineRoute(characterId, "game-server-001")
        ]])
      )
    );

    await assert.rejects(
      () => client.grantMailAttachments(
        characterId,
        "mail_claim:mail-local-wrong-target",
        [{ itemId: 1001, count: 1, binded: false }],
        "",
        { targetInstanceId: "game-server-002" }
      ),
      { code: "MAIL_CLAIM_ROUTE_TARGET_NOT_FOUND" }
    );
    assert.equal(server.requests.length, 0);
  } finally {
    await server.close();
  }
});
