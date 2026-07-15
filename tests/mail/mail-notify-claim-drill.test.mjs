import assert from "node:assert/strict";
import crypto from "node:crypto";
import net from "node:net";
import { after, before, test } from "node:test";

import Redis from "ioredis";
import { connect, StringCodec } from "nats";

import {
  createServiceInstancePayload,
  RegistryDiscoveryClient,
  registryHeartbeatKey,
  registryInstanceKey
} from "../../packages/service-registry/node/registry-schema.js";
import { discoverGameServerAdminEndpoints } from "../../apps/mail-service/src/registry-client.js";
import {
  buildChatOnlineRouteKey,
  buildInstanceMailSubject
} from "../../apps/mail-service/src/pubsub-client.js";
import {
  cleanupRedisPrefix,
  cleanupRegistryInstances,
  findFreePort,
  randomId,
  startMailService,
  startNatsServer
} from "../helpers/runtime.mjs";

const redisUrl = process.env.TEST_REDIS_URL || "redis://127.0.0.1:6379";
const ticketSecret = "test-only-ticket-secret";
const mailServiceToken = "test-only-mail-service-token";
const redisKeyPrefix = `test:mail-drill:${randomId("redis")}:`;
const registryKeyPrefix = `test:mail-drill:${randomId("registry")}:`;
const playerId = randomId("player");
const characterId = "chr_0000000000001";
const chatInstanceId = randomId("chat-server");
const gameServerInstanceId = randomId("game-server");
const mailServiceInstanceId = randomId("mail-service");
const codec = StringCodec();

const MESSAGE_TYPE = {
  ADMIN_AUTH_REQ: 2099,
  GM_SEND_ITEM_REQ: 3003,
  GM_SEND_ITEM_RES: 3004
};

let redis;
let natsServer;
let mailService;
let grantServer;
let subscriber;

function encodePacket(messageType, seq, body = Buffer.from("{}")) {
  const header = Buffer.alloc(14);
  header.writeUInt16BE(0xcafe, 0);
  header.writeUInt8(1, 2);
  header.writeUInt8(0, 3);
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

function encodeGrantResponse(request) {
  const summary = Buffer.concat([
    protobufField(1, request.characterId),
    protobufField(2, "mail-claim"),
    ...request.items.map((item) => protobufField(3, encodeGrantItem(item)))
  ]);
  return Buffer.concat([
    protobufField(1, 1, 0),
    protobufField(3, 1, 0),
    protobufField(4, request.requestId),
    protobufField(5, request.requestFingerprint),
    protobufField(7, "applied"),
    protobufField(9, summary),
    protobufField(10, request.traceId)
  ]);
}

async function createGrantCaptureServer() {
  const requests = [];
  const authRequests = [];
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

        if (messageType === MESSAGE_TYPE.ADMIN_AUTH_REQ) {
          authRequests.push(JSON.parse(body.toString("utf8")));
        }

        if (messageType === MESSAGE_TYPE.GM_SEND_ITEM_REQ) {
          const request = JSON.parse(body.toString("utf8"));
          requests.push(request);
          socket.write(encodePacket(MESSAGE_TYPE.GM_SEND_ITEM_RES, seq, encodeGrantResponse(request)));
        }
      }
    });
  });

  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  return {
    port: server.address().port,
    requests,
    authRequests,
    close: () => new Promise((resolve) => server.close(resolve))
  };
}

function createGameTicket({ playerId: ticketPlayerId, characterId: ticketCharacterId, version = 1, ttlSeconds = 300 }) {
  const payload = {
    playerId: ticketPlayerId,
    characterId: ticketCharacterId,
    ver: version,
    exp: new Date(Date.now() + ttlSeconds * 1000).toISOString()
  };
  const payloadB64 = Buffer.from(JSON.stringify(payload), "utf8").toString("base64url");
  const signatureB64 = crypto
    .createHmac("sha256", ticketSecret)
    .update(payloadB64)
    .digest("base64url");
  return `${payloadB64}.${signatureB64}`;
}

function ticketKey(ticket) {
  const hash = crypto.createHash("sha256").update(ticket).digest("hex");
  return `${redisKeyPrefix}ticket:${hash}`;
}

function gameOnlineRouteKey(characterId) {
  const hash = crypto.createHash("sha256").update(characterId).digest("hex");
  return `${redisKeyPrefix}game:online-route:${hash}`;
}

function endpointMetadata() {
  return {
    service_name: "game-server",
    service_instance_id: gameServerInstanceId,
    instance_id: gameServerInstanceId,
    build_version: "mail-drill",
    zone: "test"
  };
}

async function writeGameServerRegistryEndpoint() {
  const metadata = endpointMetadata();
  const payload = createServiceInstancePayload({
    id: gameServerInstanceId,
    name: "game-server",
    host: "127.0.0.1",
    port: 7000,
    admin_port: grantServer.port,
    endpoints: [
      {
        name: "admin",
        protocol: "tcp",
        host: "127.0.0.1",
        port: grantServer.port,
        socket: "",
        visibility: "admin",
        metadata,
        healthy: true
      }
    ],
    tags: ["game", "admin", "mail-drill"],
    metadata,
    weight: 100
  });

  await redis.hset(
    registryInstanceKey(registryKeyPrefix, "game-server", gameServerInstanceId),
    "data",
    JSON.stringify(payload)
  );
  await redis.setex(registryHeartbeatKey(registryKeyPrefix, "game-server", gameServerInstanceId), 60, "1");
}

function nextNatsJson(sub, timeoutMs = 5000) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("timed out waiting for mail notification")), timeoutMs);

    (async () => {
      try {
        for await (const message of sub) {
          clearTimeout(timer);
          resolve({
            subject: message.subject,
            payload: JSON.parse(codec.decode(message.data))
          });
          return;
        }
      } catch (error) {
        clearTimeout(timer);
        reject(error);
      }
    })();
  });
}

async function fetchJson(url, options = {}) {
  const response = await fetch(url, options);
  const text = await response.text();
  const payload = text ? JSON.parse(text) : null;
  return { response, payload };
}

before(async () => {
  redis = new Redis(redisUrl, {
    lazyConnect: true,
    maxRetriesPerRequest: 1,
    enableReadyCheck: true
  });
  await redis.connect();
  await cleanupRedisPrefix(redisUrl, redisKeyPrefix);
  await cleanupRedisPrefix(redisUrl, registryKeyPrefix);
  await cleanupRegistryInstances(redisUrl, [
    { serviceName: "game-server", instanceId: gameServerInstanceId },
    { serviceName: "mail-service", instanceId: mailServiceInstanceId }
  ], registryKeyPrefix);

  natsServer = await startNatsServer();
  grantServer = await createGrantCaptureServer();
  await writeGameServerRegistryEndpoint();

  const ticket = createGameTicket({ playerId, characterId });
  await redis.setex(ticketKey(ticket), 300, playerId);
  await redis.set(`${redisKeyPrefix}player-ticket-version:${playerId}`, "1", "EX", 300);
  await redis.set(buildChatOnlineRouteKey(playerId, redisKeyPrefix), chatInstanceId, "EX", 300);
  await redis.set(gameOnlineRouteKey(characterId), JSON.stringify({
    version: 2,
    character_id: characterId,
    instance_id: gameServerInstanceId,
    session_id: "1",
    authority_generation: "1",
    authority_token: "a".repeat(64)
  }), "EX", 300);

  mailService = await startMailService({
    host: "127.0.0.1",
    port: await findFreePort(),
    redisUrl,
    redisKeyPrefix,
    registryKeyPrefix,
    natsUrl: natsServer.url,
    ticketSecret,
    mailServiceToken,
    serviceInstanceId: mailServiceInstanceId
  });

  subscriber = await connect({ servers: natsServer.url, name: "mail-drill-subscriber" });
  subscriber.ticket = ticket;
});

after(async () => {
  if (subscriber) {
    await subscriber.drain().catch(() => subscriber.close());
  }
  if (mailService) {
    await mailService.close();
  }
  if (grantServer) {
    await grantServer.close();
  }
  if (natsServer) {
    await natsServer.close();
  }
  if (redis) {
    await redis.quit();
  }
  await cleanupRedisPrefix(redisUrl, redisKeyPrefix);
  await cleanupRedisPrefix(redisUrl, registryKeyPrefix);
});

test("mail create publishes online notification and claim grants attachments through strict registry discovery", { timeout: 30000 }, async () => {
  const subject = buildInstanceMailSubject(chatInstanceId);
  const sub = subscriber.subscribe(subject, { max: 1 });
  const notificationPromise = nextNatsJson(sub);

  const createResult = await fetchJson(`${mailService.baseUrl}/api/v1/mails`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${mailServiceToken}`,
      "content-type": "application/json"
    },
    body: JSON.stringify({
      to_player_id: playerId,
      title: "M7 mail notify claim drill",
      content: "strict registry discovery drill",
      attachments: [{ type: "item", item_id: 1001, count: 2, binded: true }],
      mail_type: "system"
    })
  });

  assert.equal(createResult.response.status, 201);
  assert.equal(createResult.payload.ok, true);
  assert.match(createResult.payload.mail_id, /^mail_/);

  const notification = await notificationPromise;
  assert.equal(notification.subject, subject);
  assert.deepEqual(notification.payload, {
    event_id: `mail.notify:${createResult.payload.mail_id}`,
    event_type: "mail.created",
    version: 1,
    occurred_at: notification.payload.occurred_at,
    player_id: playerId,
    mail: {
      mail_id: createResult.payload.mail_id,
      title: "M7 mail notify claim drill",
      from_player_id: "system",
      from_name: "系统",
      mail_type: "system",
      created_at: notification.payload.occurred_at
    },
    trace_id: notification.payload.trace_id
  });
  assert.ok(notification.payload.occurred_at);
  assert.match(notification.payload.trace_id, /^[0-9a-f]{32}$/);

  const discovery = new RegistryDiscoveryClient(redis, {
    registryKeyPrefix,
    discoveryCacheTtlMs: 0
  });
  const mailEndpoint = await discovery.discoverRequiredEndpoint("mail-service", "http");
  assert.equal(mailEndpoint.instance.id, mailServiceInstanceId);
  assert.equal(mailEndpoint.endpoint.host, mailService.host);
  assert.equal(mailEndpoint.endpoint.port, mailService.port);
  assert.equal(mailEndpoint.endpoint.visibility, "internal");

  const discoveredEndpoints = await discoverGameServerAdminEndpoints(redis, {
    registryKeyPrefix,
    discoveryCacheTtlMs: 0
  });
  assert.deepEqual(
    discoveredEndpoints.map(({ instanceId, endpointName, host, port, source, fallback, reason }) => ({
      instanceId,
      endpointName,
      host,
      port,
      source,
      fallback,
      reason
    })),
    [
      {
        instanceId: gameServerInstanceId,
        endpointName: "admin",
        host: "127.0.0.1",
        port: grantServer.port,
        source: "registry",
        fallback: false,
        reason: "discovered"
      }
    ]
  );

  const claimResult = await fetchJson(`${mailService.baseUrl}/api/v1/mails/${createResult.payload.mail_id}/claim`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${subscriber.ticket}`,
      "content-type": "application/json"
    },
    body: JSON.stringify({
      player_id: playerId
    })
  });

  assert.equal(claimResult.response.status, 200);
  assert.equal(claimResult.payload.ok, true);
  assert.equal(claimResult.payload.claimed, true);
  assert.equal(claimResult.payload.status, "claimed");
  assert.equal(grantServer.requests.length, 1);
  assert.equal(grantServer.authRequests.length, 1);
  assert.deepEqual(grantServer.authRequests[0], {
    token: "test-only-game-admin-token",
    actor: mailServiceInstanceId
  });
  assert.deepEqual(grantServer.requests[0], {
    requestId: `mail_claim:${createResult.payload.mail_id}`,
    mailId: createResult.payload.mail_id,
    characterId,
    items: [{ itemId: 1001, count: 2, binded: true }],
    requestFingerprint: grantServer.requests[0].requestFingerprint,
    source: "mail-claim",
    reason: `claim mail ${createResult.payload.mail_id}`,
    traceId: grantServer.requests[0].traceId,
    routeGeneration: "1",
    routeToken: "a".repeat(64)
  });
  assert.match(grantServer.requests[0].requestFingerprint, /^sha256:[0-9a-f]{64}$/);
  assert.match(grantServer.requests[0].traceId, /^[0-9a-f]{32}$/);
});
