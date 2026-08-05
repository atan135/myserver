import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";

import dotenv from "dotenv";
import Redis from "ioredis";
import pg from "pg";

import {
  createServiceInstancePayload,
  registerRegistryInstance
} from "../../packages/service-registry/node/registry-schema.js";
import { TcpProtocolClient } from "../../tools/mock-client/src/client.js";
import { authenticateClient } from "../../tools/mock-client/src/scenarios/room.js";
import {
  cleanupRedisPrefix,
  createMailAcceptanceDatabase,
  findFreePort,
  runWithCleanup,
  startAuthHttpServer,
  startGameServer,
  startMailService,
  startNatsServer,
  startRedisServer
} from "../helpers/runtime.mjs";

const { Pool } = pg;
const projectRoot = path.resolve(import.meta.dirname, "..", "..");
const ticketSecret = "mail-two-account-ticket-secret-2026";
const mailServiceToken = "mail-two-account-service-token-2026";
const gameAdminToken = "mail-two-account-game-admin-token-2026";
const grantKeyId = "mail-two-account-v1";

function resolvePostgresAdminUrl() {
  for (const value of [process.env.TEST_POSTGRES_ADMIN_URL, process.env.TEST_DATABASE_URL]) {
    if (value) return value;
  }
  for (const envPath of [
    path.join(projectRoot, "apps", "mail-service", ".env"),
    path.join(projectRoot, "apps", "auth-http", ".env")
  ]) {
    if (!fs.existsSync(envPath)) continue;
    const parsed = dotenv.parse(fs.readFileSync(envPath));
    if (parsed.DATABASE_URL) return parsed.DATABASE_URL;
  }
  throw new Error(
    "PostgreSQL admin credentials are unavailable; set TEST_POSTGRES_ADMIN_URL or configure a local service .env"
  );
}

async function requestJson(baseUrl, pathname, { method = "GET", headers = {}, body } = {}) {
  const response = await fetch(new URL(pathname, baseUrl), {
    method,
    headers: {
      ...(body === undefined ? {} : { "content-type": "application/json" }),
      ...headers
    },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(10_000)
  });
  const text = await response.text();
  return {
    status: response.status,
    ok: response.ok,
    payload: text ? JSON.parse(text) : null
  };
}

function assertResponse(result, expectedStatus) {
  assert.equal(
    result.status,
    expectedStatus,
    `unexpected HTTP response: ${result.status} ${result.payload?.error || result.payload?.message || ""}`.trim()
  );
  return result.payload;
}

async function waitFor(check, label, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const value = await check();
      if (value) return value;
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error(`${label} timed out${lastError ? `: ${lastError.message}` : ""}`);
}

async function createIdentity(authBaseUrl, suffix) {
  const loginName = `mail_${suffix}`;
  const password = `Mail-${suffix}-Pass`;
  const registration = assertResponse(await requestJson(authBaseUrl, "/api/v1/auth/register", {
    method: "POST",
    body: { loginName, password, displayName: `Mail ${suffix}` }
  }), 201);
  const login = assertResponse(await requestJson(authBaseUrl, "/api/v1/auth/login", {
    method: "POST",
    body: { loginName, password }
  }), 201);
  assert.equal(login.playerId, registration.playerId);

  const sessionHeaders = { authorization: `Bearer ${login.accessToken}` };
  const created = assertResponse(await requestJson(authBaseUrl, "/api/v1/characters", {
    method: "POST",
    headers: sessionHeaders,
    body: { name: `Mail${suffix}`, appearance: { body: "default" } }
  }), 201);
  const characterId = created.character.character_id;
  const selected = assertResponse(await requestJson(authBaseUrl, "/api/v1/characters/select", {
    method: "POST",
    headers: sessionHeaders,
    body: { character_id: characterId }
  }), 200);
  assert.equal(selected.playerId, login.playerId);
  assert.equal(selected.character.character_id, characterId);

  const ticketPayload = JSON.parse(
    Buffer.from(selected.ticket.split(".")[0], "base64url").toString("utf8")
  );
  assert.equal(ticketPayload.playerId, login.playerId);
  assert.equal(ticketPayload.characterId, characterId);
  return { playerId: login.playerId, characterId, ticket: selected.ticket };
}

function gameRouteKey(redisPrefix, characterId) {
  const digest = crypto.createHash("sha256").update(characterId).digest("hex");
  return `${redisPrefix}game:online-route:${digest}`;
}

async function connectGameClient(gameServer, identity, redis, redisPrefix, instanceId, label) {
  const client = new TcpProtocolClient(
    { host: gameServer.host, port: gameServer.port, timeoutMs: 5_000 },
    label
  );
  await client.connect();
  await authenticateClient(client, { timeoutMs: 5_000 }, { ticket: identity.ticket });
  await waitFor(async () => {
    const raw = await redis.get(gameRouteKey(redisPrefix, identity.characterId));
    if (!raw) return false;
    const route = JSON.parse(raw);
    return route.character_id === identity.characterId && route.instance_id === instanceId;
  }, `${label} authoritative route`);
  return client;
}

async function registerGameEndpoint(redis, registryPrefix, gameServer, instanceId) {
  const metadata = {
    service_name: "game-server",
    service_instance_id: instanceId,
    instance_id: instanceId,
    build_version: "mail-two-account-acceptance",
    zone: "test"
  };
  const payload = createServiceInstancePayload({
    id: instanceId,
    name: "game-server",
    host: gameServer.host,
    port: gameServer.port,
    admin_port: gameServer.adminPort,
    endpoints: [{
      name: "admin",
      protocol: "tcp",
      host: gameServer.host,
      port: gameServer.adminPort,
      socket: "",
      visibility: "admin",
      metadata,
      healthy: true
    }],
    tags: ["game", "admin", "mail-two-account-acceptance"],
    metadata,
    weight: 100
  });
  await registerRegistryInstance(redis, {
    registryKeyPrefix: registryPrefix,
    serviceName: "game-server",
    instanceId,
    data: payload
  });
}

async function inventoryItemCount(db, characterId, itemId) {
  const { rows } = await db.query(
    "SELECT inventory_data FROM character_inventory WHERE character_id = $1",
    [characterId]
  );
  const slots = rows[0]?.inventory_data?.slots || [];
  return slots
    .filter(Boolean)
    .filter((item) => item.item_id === itemId)
    .reduce((sum, item) => sum + item.count, 0);
}

test("two real accounts isolate mail ownership and grant only the ticket-bound character", { timeout: 180_000 }, async () => {
  const runId = crypto.randomBytes(8).toString("hex");
  const databaseName = `myserver_mail_acceptance_${runId}`;
  const redisPrefix = `acceptance:mail-two-account:${runId}:`;
  const registryPrefix = `${redisPrefix}registry:`;
  const gameInstanceId = `game-mail-two-account-${runId}`;
  const grantKeyPair = crypto.generateKeyPairSync("ed25519");
  const grantPrivateKey = grantKeyPair.privateKey.export({ type: "pkcs8", format: "pem" });
  const grantPublicKeys = JSON.stringify({
    [grantKeyId]: grantKeyPair.publicKey.export({ format: "jwk" }).x
  });
  let database;
  let db;
  let redisServer;
  let redis;
  let natsServer;
  let authServer;
  let gameServer;
  let mailServer;
  let clientA;
  let clientB;

  await runWithCleanup(async () => {
    database = await createMailAcceptanceDatabase({
      adminUrl: resolvePostgresAdminUrl(),
      databaseName,
      migrationPaths: [
        path.join(projectRoot, "db", "migrations", "auth", "20260718161350_initial_schema.sql"),
        path.join(projectRoot, "db", "migrations", "game", "20260718161350_initial_schema.sql"),
        path.join(projectRoot, "db", "migrations", "game", "20260729190000_restore_character_asset_ledger_guard.sql"),
        path.join(projectRoot, "db", "migrations", "mail", "20260718161350_initial_schema.sql")
      ]
    });
    db = new Pool({ connectionString: database.databaseUrl, max: 3 });
    await db.query("SELECT 1");

    const [authPort, gamePort, gameAdminPort, mailPort, unusedGameProxyPort] = await Promise.all(
      Array.from({ length: 5 }, () => findFreePort())
    );
    redisServer = await startRedisServer();
    natsServer = await startNatsServer();
    redis = new Redis(redisServer.url);
    redis.on("error", () => {});

    authServer = await startAuthHttpServer({
      port: authPort,
      ticketSecret,
      redisUrl: redisServer.url,
      redisKeyPrefix: redisPrefix,
      envOverrides: {
        DB_ENABLED: "true",
        DATABASE_URL: database.databaseUrl,
        GAME_DATABASE_URL: database.databaseUrl,
        DB_POOL_SIZE: "2",
        GAME_DB_POOL_SIZE: "2",
        NATS_URL: natsServer.url,
        REGISTRY_ENABLED: "false",
        DISCOVERY_REQUIRED: "false",
        DISALLOW_LEGACY_DIRECT_CONFIG: "false",
        GAME_PROXY_HOST: "127.0.0.1",
        GAME_PROXY_PORT: String(unusedGameProxyPort),
        AUTH_REGISTER_REQUIRE_REVIEW: "false",
        RATELIMIT_ENABLED: "false",
        ACCOUNT_LOCK_ENABLED: "false",
        AUTH_REQUIRE_TLS: "false",
        GLOBAL_ID_ORIGIN_ID: "42",
        GLOBAL_ID_WORKER_ID: "21"
      }
    });

    const suffixA = `${runId.slice(0, 6)}a`;
    const suffixB = `${runId.slice(0, 6)}b`;
    const identityA = await createIdentity(authServer.baseUrl, suffixA);
    const identityB = await createIdentity(authServer.baseUrl, suffixB);
    assert.notEqual(identityA.playerId, identityB.playerId);
    assert.notEqual(identityA.characterId, identityB.characterId);

    gameServer = await startGameServer({
      port: gamePort,
      adminPort: gameAdminPort,
      localSocketName: `${gameInstanceId}.sock`,
      ticketSecret,
      redisUrl: redisServer.url,
      redisKeyPrefix: redisPrefix,
      envOverrides: {
        SERVICE_NAME: "game-server",
        SERVICE_INSTANCE_ID: gameInstanceId,
        SERVICE_BIND_HOST: "127.0.0.1",
        SERVICE_PUBLIC_HOST: "127.0.0.1",
        SERVICE_ADMIN_BIND_HOST: "127.0.0.1",
        SERVICE_ADMIN_ADVERTISED_HOST: "127.0.0.1",
        GAME_ADMIN_TOKEN: gameAdminToken,
        GAME_ADMIN_AUDIT_ENABLED: "false",
        GAME_ADMIN_AUDIT_REQUIRE_ACTOR: "true",
        MAIL_GRANT_ASSERTION_ISSUER: "mail-service",
        MAIL_GRANT_ASSERTION_PUBLIC_KEYS_JSON: grantPublicKeys,
        DB_ENABLED: "true",
        DATABASE_URL: database.databaseUrl,
        DB_POOL_SIZE: "3",
        NATS_URL: natsServer.url,
        HEARTBEAT_TIMEOUT_SECS: "300",
        GLOBAL_ID_ORIGIN_ID: "42",
        GLOBAL_ID_WORKER_ID: "22"
      }
    });
    await registerGameEndpoint(redis, registryPrefix, gameServer, gameInstanceId);

    mailServer = await startMailService({
      port: mailPort,
      redisUrl: redisServer.url,
      redisKeyPrefix: redisPrefix,
      registryKeyPrefix: registryPrefix,
      natsUrl: natsServer.url,
      ticketSecret,
      mailServiceToken,
      serviceInstanceId: `mail-two-account-${runId}`,
      envOverrides: {
        DB_ENABLED: "true",
        DATABASE_URL: database.databaseUrl,
        DB_POOL_SIZE: "3",
        MAIL_PUBLIC_RATE_LIMIT_ENABLED: "false",
        GAME_ADMIN_TOKEN: gameAdminToken,
        GAME_ADMIN_ACTOR: `mail-two-account-${runId}`,
        MAIL_GRANT_ASSERTION_ISSUER: "mail-service",
        MAIL_GRANT_ASSERTION_KEY_ID: grantKeyId,
        MAIL_GRANT_ASSERTION_PRIVATE_KEY: grantPrivateKey,
        MAIL_OUTBOX_POLL_INTERVAL_MS: "100",
        GLOBAL_ID_ORIGIN_ID: "42",
        GLOBAL_ID_WORKER_ID: "23"
      }
    });

    clientA = await connectGameClient(
      gameServer,
      identityA,
      redis,
      redisPrefix,
      gameInstanceId,
      "mail-owner-a"
    );
    clientB = await connectGameClient(
      gameServer,
      identityB,
      redis,
      redisPrefix,
      gameInstanceId,
      "mail-owner-b"
    );

    const createA = assertResponse(await requestJson(mailServer.baseUrl, "/api/v1/mails", {
      method: "POST",
      headers: { "x-service-token": mailServiceToken },
      body: {
        to_player_id: identityA.playerId,
        title: "Owner A attachment",
        content: "Only account A may read and claim this mail",
        attachments: [{ type: "item", item_id: 1001, count: 3, binded: true }],
        mail_type: "system"
      }
    }), 201);
    const createB = assertResponse(await requestJson(mailServer.baseUrl, "/api/v1/mails", {
      method: "POST",
      headers: { "x-service-token": mailServiceToken },
      body: {
        to_player_id: identityB.playerId,
        title: "Owner B control",
        content: "Account B control mail",
        mail_type: "system"
      }
    }), 201);

    const headersA = { "x-game-ticket": identityA.ticket };
    const headersB = { "x-game-ticket": identityB.ticket };
    const listA = assertResponse(await requestJson(
      mailServer.baseUrl,
      "/api/v1/mails?limit=10&offset=0",
      { headers: headersA }
    ), 200);
    const listB = assertResponse(await requestJson(
      mailServer.baseUrl,
      "/api/v1/mails?limit=10&offset=0",
      { headers: headersB }
    ), 200);
    assert.deepEqual(listA.mails.map((mail) => mail.mail_id), [createA.mail_id]);
    assert.deepEqual(listB.mails.map((mail) => mail.mail_id), [createB.mail_id]);

    const detailA = assertResponse(await requestJson(
      mailServer.baseUrl,
      `/api/v1/mails/${createA.mail_id}`,
      { headers: headersA }
    ), 200);
    assert.equal(detailA.mail.mail_id, createA.mail_id);
    const readA = assertResponse(await requestJson(
      mailServer.baseUrl,
      `/api/v1/mails/${createA.mail_id}/read`,
      { method: "PUT", headers: headersA, body: {} }
    ), 200);
    assert.equal(readA.status, "read");

    for (const result of [
      await requestJson(mailServer.baseUrl, `/api/v1/mails/${createA.mail_id}`, { headers: headersB }),
      await requestJson(mailServer.baseUrl, `/api/v1/mails/${createA.mail_id}/read`, {
        method: "PUT",
        headers: headersB,
        body: {}
      }),
      await requestJson(mailServer.baseUrl, `/api/v1/mails/${createA.mail_id}/claim`, {
        method: "POST",
        headers: headersB,
        body: {}
      })
    ]) {
      assert.equal(result.status, 404);
      assert.equal(result.payload.error, "MAIL_NOT_FOUND");
    }

    const beforeA = await inventoryItemCount(db, identityA.characterId, 1001);
    const beforeB = await inventoryItemCount(db, identityB.characterId, 1001);
    const claimed = assertResponse(await requestJson(
      mailServer.baseUrl,
      `/api/v1/mails/${createA.mail_id}/claim`,
      { method: "POST", headers: headersA, body: {} }
    ), 200);
    assert.equal(claimed.claimed, true);
    assert.equal(claimed.status, "claimed");

    const afterA = await inventoryItemCount(db, identityA.characterId, 1001);
    const afterB = await inventoryItemCount(db, identityB.characterId, 1001);
    assert.equal(afterA - beforeA, 3);
    assert.equal(afterB, beforeB);

    const repeated = assertResponse(await requestJson(
      mailServer.baseUrl,
      `/api/v1/mails/${createA.mail_id}/claim`,
      { method: "POST", headers: headersA, body: {} }
    ), 200);
    assert.equal(repeated.claimed, false);
    assert.equal(repeated.already_claimed, true);
    assert.equal(await inventoryItemCount(db, identityA.characterId, 1001), afterA);
    assert.equal(await inventoryItemCount(db, identityB.characterId, 1001), beforeB);

    const { rows: grants } = await db.query(
      "SELECT character_id FROM character_inventory_grants WHERE request_id = $1",
      [`mail_claim:${createA.mail_id}`]
    );
    assert.deepEqual(grants.map((grant) => grant.character_id), [identityA.characterId]);
  }, [
    () => clientB?.close(),
    () => clientA?.close(),
    () => mailServer?.close(),
    () => gameServer?.close(),
    () => authServer?.close(),
    async () => {
      if (!redisServer) return;
      await cleanupRedisPrefix(redisServer.url, redisPrefix);
      const verifier = new Redis(redisServer.url);
      try {
        assert.deepEqual(await verifier.keys(`${redisPrefix}*`), []);
      } finally {
        await verifier.quit();
      }
    },
    () => redis?.quit(),
    () => natsServer?.close(),
    () => redisServer?.close(),
    () => db?.end(),
    () => database?.drop()
  ], "two-account mail ownership acceptance cleanup failed");
});
