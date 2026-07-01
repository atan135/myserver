import assert from "node:assert/strict";
import crypto from "node:crypto";
import { after, before, test } from "node:test";

import Redis from "ioredis";

import { RegistryDiscoveryClient } from "../../packages/service-registry/node/registry-schema.js";
import {
  cleanupRedisPrefix,
  cleanupRegistryInstances,
  findFreePort,
  randomId,
  startAnnounceService,
  startNatsServer
} from "../helpers/runtime.mjs";
import {
  runAnnounceCreate,
  runAnnounceGet,
  runAnnounceList
} from "../../tools/mock-client/src/scenarios/announce.js";

const redisUrl = process.env.TEST_REDIS_URL || "redis://127.0.0.1:6379";
const ticketSecret = "test-only-announce-ticket-secret";
const announceAdminToken = "test-only-announce-admin-token";
const announceReadToken = "test-only-announce-read-token";
const redisKeyPrefix = `test:announce-drill:${randomId("redis")}:`;
const registryKeyPrefix = `test:announce-drill:${randomId("registry")}:`;
const playerId = randomId("player");
const announceServiceInstanceId = randomId("announce-service");

let redis;
let natsServer;
let announceService;
let createdAnnouncementId;

function createGameTicket({ playerId: ticketPlayerId, version = 1, ttlSeconds = 300 }) {
  const payload = {
    playerId: ticketPlayerId,
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

function mockAnnounceOptions(overrides = {}) {
  return {
    announceBaseUrl: announceService.baseUrl,
    announceActiveOnly: true,
    announceOffset: 0,
    announceAdminToken,
    announceContent: "strict registry discovery announce read drill",
    announceDurationSeconds: "3600",
    announceLocale: "default",
    announcePriority: "7",
    announceTargetGroup: "all",
    announceTitle: "M7 announce read drill",
    announceType: "banner",
    limit: 20,
    serviceToken: "",
    ticket: "",
    timeoutMs: 5000,
    ...overrides
  };
}

async function fetchJson(url, options = {}) {
  const response = await fetch(url, options);
  const text = await response.text();
  const payload = text ? JSON.parse(text) : null;
  return { response, payload };
}

async function captureConsole(fn) {
  const lines = [];
  const originalLog = console.log;
  console.log = (...args) => {
    lines.push(args.map((arg) => String(arg)).join(" "));
  };

  try {
    await fn();
  } finally {
    console.log = originalLog;
  }

  return lines.join("\n");
}

function announceIdFromMockOutput(output) {
  const match = output.match(/announce_id:\s*(ann_[^\s]+)/);
  assert.ok(match, `mock-client create output did not include announce_id:\n${output}`);
  return match[1];
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
    { serviceName: "announce-service", instanceId: announceServiceInstanceId }
  ], registryKeyPrefix);

  natsServer = await startNatsServer();

  const ticket = createGameTicket({ playerId });
  await redis.setex(ticketKey(ticket), 300, playerId);
  await redis.set(`${redisKeyPrefix}player-ticket-version:${playerId}`, "1", "EX", 300);

  announceService = await startAnnounceService({
    host: "127.0.0.1",
    port: await findFreePort(),
    redisUrl,
    redisKeyPrefix,
    registryKeyPrefix,
    natsUrl: natsServer.url,
    ticketSecret,
    announceAdminToken,
    announceReadToken,
    serviceInstanceId: announceServiceInstanceId
  });
  announceService.ticket = ticket;
});

after(async () => {
  if (announceService) {
    await announceService.close();
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

test("announce-service registers http endpoint and read token or game ticket can read list/detail", { timeout: 30000 }, async () => {
  const discovery = new RegistryDiscoveryClient(redis, {
    registryKeyPrefix,
    discoveryCacheTtlMs: 0
  });
  const announceEndpoint = await discovery.discoverRequiredEndpoint("announce-service", "http");
  assert.equal(announceEndpoint.instance.id, announceServiceInstanceId);
  assert.equal(announceEndpoint.endpoint.host, announceService.host);
  assert.equal(announceEndpoint.endpoint.port, announceService.port);
  assert.equal(announceEndpoint.endpoint.protocol, "http");
  assert.equal(announceEndpoint.endpoint.visibility, "internal");
  assert.equal(announceEndpoint.instance.metadata.read_auth_required, true);

  const createOutput = await captureConsole(() =>
    runAnnounceCreate(mockAnnounceOptions())
  );
  createdAnnouncementId = announceIdFromMockOutput(createOutput);

  const readTokenListOutput = await captureConsole(() =>
    runAnnounceList(mockAnnounceOptions({
      announceLocale: "default",
      serviceToken: announceReadToken
    }))
  );
  assert.match(readTokenListOutput, new RegExp(createdAnnouncementId));

  const readTokenGetOutput = await captureConsole(() =>
    runAnnounceGet(mockAnnounceOptions({
      announceId: createdAnnouncementId,
      serviceToken: announceReadToken
    }))
  );
  assert.match(readTokenGetOutput, new RegExp(createdAnnouncementId));

  const ticketListOutput = await captureConsole(() =>
    runAnnounceList(mockAnnounceOptions({
      announceLocale: "default",
      ticket: announceService.ticket
    }))
  );
  assert.match(ticketListOutput, new RegExp(createdAnnouncementId));

  const ticketGetOutput = await captureConsole(() =>
    runAnnounceGet(mockAnnounceOptions({
      announceId: createdAnnouncementId,
      ticket: announceService.ticket
    }))
  );
  assert.match(ticketGetOutput, new RegExp(createdAnnouncementId));

  const noAuthResult = await fetchJson(`${announceService.baseUrl}/api/v1/announcements`);
  assert.equal(noAuthResult.response.status, 401);
});
