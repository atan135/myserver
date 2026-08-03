import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";

import dotenv from "dotenv";
import Redis from "ioredis";

import {
  cleanupRedisPrefix,
  createMailAcceptanceDatabase,
  findFreePort,
  runWithCleanup,
  startAuthHttpServer,
  startMailService,
  startNatsServer,
  startRedisServer
} from "../helpers/runtime.mjs";

const projectRoot = path.resolve(import.meta.dirname, "..", "..");
const ticketSecret = "mail-internal-core-ticket-secret-2026";
const mailServiceToken = "mail-internal-core-service-token-2026";

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
  const payload = await response.json();
  return { status: response.status, ok: response.ok, payload };
}

function assertResponse(result, expectedStatus) {
  assert.equal(
    result.status,
    expectedStatus,
    `unexpected HTTP response: ${result.status} ${result.payload?.error || result.payload?.message || ""}`.trim()
  );
  return result.payload;
}

test("real auth character ticket drives the isolated internal mail core flow", { timeout: 120_000 }, async () => {
  const runId = crypto.randomBytes(8).toString("hex");
  const databaseName = `myserver_mail_acceptance_${runId}`;
  const redisPrefix = `acceptance:mail-core:${runId}:`;
  const registryPrefix = `${redisPrefix}registry:`;
  const loginName = `mail_${runId.slice(0, 12)}`;
  const password = `Mail-${runId}-Pass`;
  const characterName = `Mail${runId.slice(0, 8)}`;
  let database;
  let redisServer;
  let natsServer;
  let authServer;
  let mailServer;

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

    const [authPort, mailPort, gameProxyPort] = await Promise.all([
      findFreePort(),
      findFreePort(),
      findFreePort()
    ]);
    redisServer = await startRedisServer();
    natsServer = await startNatsServer();

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
        GAME_PROXY_PORT: String(gameProxyPort),
        AUTH_REGISTER_REQUIRE_REVIEW: "false",
        RATELIMIT_ENABLED: "false",
        ACCOUNT_LOCK_ENABLED: "false",
        AUTH_REQUIRE_TLS: "false",
        GLOBAL_ID_ORIGIN_ID: "41",
        GLOBAL_ID_WORKER_ID: "11"
      }
    });

    mailServer = await startMailService({
      port: mailPort,
      redisUrl: redisServer.url,
      redisKeyPrefix: redisPrefix,
      registryKeyPrefix: registryPrefix,
      natsUrl: natsServer.url,
      ticketSecret,
      mailServiceToken,
      serviceInstanceId: `mail-core-${runId}`,
      envOverrides: {
        DB_ENABLED: "true",
        DATABASE_URL: database.databaseUrl,
        DB_POOL_SIZE: "2",
        GLOBAL_ID_ORIGIN_ID: "41",
        GLOBAL_ID_WORKER_ID: "12",
        MAIL_OUTBOX_POLL_INTERVAL_MS: "100"
      }
    });

    const registration = assertResponse(await requestJson(authServer.baseUrl, "/api/v1/auth/register", {
      method: "POST",
      body: { loginName, password, displayName: "Mail Core Acceptance" }
    }), 201);
    assert.equal(registration.ok, true);
    assert.ok(registration.playerId);
    assert.ok(registration.accessToken);

    const login = assertResponse(await requestJson(authServer.baseUrl, "/api/v1/auth/login", {
      method: "POST",
      body: { loginName, password }
    }), 201);
    assert.equal(login.ok, true);
    assert.equal(login.playerId, registration.playerId);
    assert.ok(login.accessToken);

    const sessionHeaders = { authorization: `Bearer ${login.accessToken}` };
    const createdCharacter = assertResponse(await requestJson(authServer.baseUrl, "/api/v1/characters", {
      method: "POST",
      headers: sessionHeaders,
      body: { name: characterName, appearance: { body: "default" } }
    }), 201);
    const characterId = createdCharacter.character.character_id;
    assert.match(characterId, /^chr_[0-9a-hjkmnp-tv-z]+$/);

    const selection = assertResponse(await requestJson(authServer.baseUrl, "/api/v1/characters/select", {
      method: "POST",
      headers: sessionHeaders,
      body: { character_id: characterId }
    }), 200);
    assert.equal(selection.playerId, login.playerId);
    assert.equal(selection.character.character_id, characterId);
    assert.ok(selection.ticket);
    assert.equal(selection.services.mail, null);

    const ticketPayload = JSON.parse(Buffer.from(selection.ticket.split(".")[0], "base64url").toString("utf8"));
    assert.equal(ticketPayload.playerId, login.playerId);
    assert.equal(ticketPayload.characterId, characterId);

    const createdMail = assertResponse(await requestJson(mailServer.baseUrl, "/api/v1/mails", {
      method: "POST",
      headers: { "x-service-token": mailServiceToken },
      body: {
        to_player_id: login.playerId,
        title: "Internal core flow",
        content: "No attachment mail",
        mail_type: "system"
      }
    }), 201);
    assert.ok(createdMail.mail_id);

    const playerHeaders = { "x-game-ticket": selection.ticket };
    const list = assertResponse(await requestJson(mailServer.baseUrl, "/api/v1/mails", {
      headers: playerHeaders
    }), 200);
    assert.deepEqual(list.mails.map((mail) => mail.mail_id), [createdMail.mail_id]);
    assert.equal(list.unread_count, 1);
    assert.equal(list.mails[0].has_attachments, false);

    const detail = assertResponse(await requestJson(mailServer.baseUrl, `/api/v1/mails/${createdMail.mail_id}`, {
      headers: playerHeaders
    }), 200);
    assert.equal(detail.mail.mail_id, createdMail.mail_id);
    assert.equal(detail.mail.title, "Internal core flow");
    assert.deepEqual(detail.mail.attachments, []);

    const read = assertResponse(await requestJson(mailServer.baseUrl, `/api/v1/mails/${createdMail.mail_id}/read`, {
      method: "PUT",
      headers: playerHeaders,
      body: {}
    }), 200);
    assert.equal(read.status, "read");
    assert.equal(read.already_read, false);

    const readDetail = assertResponse(await requestJson(mailServer.baseUrl, `/api/v1/mails/${createdMail.mail_id}`, {
      headers: playerHeaders
    }), 200);
    assert.equal(readDetail.mail.status, "read");

    const noAttachmentClaim = await requestJson(
      mailServer.baseUrl,
      `/api/v1/mails/${createdMail.mail_id}/claim`,
      { method: "POST", headers: playerHeaders, body: {} }
    );
    assert.equal(noAttachmentClaim.status, 409);
    assert.equal(noAttachmentClaim.payload.error, "MAIL_HAS_NO_ATTACHMENTS");
  }, [
    () => mailServer?.close(),
    () => authServer?.close(),
    async () => {
      if (!redisServer) return;
      await cleanupRedisPrefix(redisServer.url, redisPrefix);
      const redis = new Redis(redisServer.url);
      try {
        assert.deepEqual(await redis.keys(`${redisPrefix}*`), []);
      } finally {
        await redis.quit();
      }
    },
    () => natsServer?.close(),
    () => redisServer?.close(),
    async () => {
      await database?.drop();
      await database?.drop();
    }
  ], "isolated mail core flow cleanup failed");
});
