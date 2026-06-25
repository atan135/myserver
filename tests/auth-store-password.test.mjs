import assert from "node:assert/strict";
import { Buffer } from "node:buffer";
import crypto from "node:crypto";
import { test } from "node:test";

import {
  AuthStore,
  GAME_TICKET_INVALIDATION_SCOPE,
  GAME_TICKET_REDIS_OWNER_SCOPE,
  verifyGameTicketPayload
} from "../apps/auth-http/src/auth-store.js";
import {
  createPasswordSalt,
  hashPassword
} from "../apps/auth-http/src/password-utils.js";

class FakeRedis {
  constructor() {
    this.store = new Map();
  }

  async set(key, value) {
    this.store.set(key, value);
  }

  async get(key) {
    return this.store.get(key) ?? null;
  }

  async del(key) {
    this.store.delete(key);
  }

  async incr(key) {
    const next = Number.parseInt(this.store.get(key) ?? "0", 10) + 1;
    this.store.set(key, String(next));
    return next;
  }

  async expire() {}

  async publish() {
    return 0;
  }
}

test("AuthStore password login validates credentials from database store", async () => {
  const passwordSalt = createPasswordSalt();
  const passwordHash = await hashPassword("Passw0rd!", passwordSalt);
  const audits = [];
  const touchedPlayerIds = [];
  const redis = new FakeRedis();
  const dbStore = {
    enabled: true,
    async findPasswordAccountByLoginName(loginName) {
      assert.equal(loginName, "test001");
      return {
        playerId: "player-001",
        loginName: "test001",
        status: "active",
        passwordAlgo: "scrypt",
        passwordSalt,
        passwordHash
      };
    },
    async touchPlayerLastLogin(playerId) {
      touchedPlayerIds.push(playerId);
    },
    async appendAuthAudit(entry) {
      audits.push(entry);
    }
  };

  const authStore = new AuthStore(
    {
      redisKeyPrefix: "test:",
      sessionTtlSeconds: 600,
      ticketTtlSeconds: 300,
      ticketSecret: "test-secret"
    },
    redis,
    dbStore
  );

  const session = await authStore.createPasswordSession(
    "Test001",
    "Passw0rd!",
    "127.0.0.1"
  );

  assert.equal(session.playerId, "player-001");
  assert.equal(session.loginName, "test001");
  assert.equal(session.guestId, null);
  assert.ok(session.accessToken);
  assert.equal(session.gameTicket, null);
  assert.deepEqual(touchedPlayerIds, ["player-001"]);
  assert.equal(audits.some((entry) => entry.eventType === "password_login"), true);
  assert.equal(audits.some((entry) => entry.eventType === "issue_ticket"), false);
  assert.equal(audits.at(-1).details.gameTicketReason, "character_selection_required");

  const storedSession = await authStore.getSessionByAccessToken(session.accessToken);
  assert.equal(storedSession.playerId, "player-001");
  assert.equal(storedSession.loginName, "test001");
});

test("AuthStore password login rejects invalid password", async () => {
  const passwordSalt = createPasswordSalt();
  const passwordHash = await hashPassword("Passw0rd!", passwordSalt);
  const audits = [];
  const authStore = new AuthStore(
    {
      redisKeyPrefix: "test:",
      sessionTtlSeconds: 600,
      ticketTtlSeconds: 300,
      ticketSecret: "test-secret"
    },
    new FakeRedis(),
    {
      enabled: true,
      async findPasswordAccountByLoginName() {
        return {
          playerId: "player-001",
          loginName: "test001",
          status: "active",
          passwordAlgo: "scrypt",
          passwordSalt,
          passwordHash
        };
      },
      async touchPlayerLastLogin() {
        throw new Error("touchPlayerLastLogin should not be called");
      },
      async appendAuthAudit(entry) {
        audits.push(entry);
      }
    }
  );

  await assert.rejects(
    () => authStore.createPasswordSession("test001", "wrong-password"),
    (error) => error.code === "INVALID_LOGIN_CREDENTIALS"
  );

  assert.equal(audits.some((entry) => entry.eventType === "password_login_failed"), true);
});

test("AuthStore rejects ticket revoke for another player", async () => {
  const redis = new FakeRedis();
  const authStore = new AuthStore(
    {
      redisKeyPrefix: "test:",
      sessionTtlSeconds: 600,
      ticketTtlSeconds: 300,
      ticketSecret: "test-secret"
    },
    redis
  );

  const ticket = await authStore.issueGameTicket("player-001", "127.0.0.1", {
    characterId: "chr_0000000000001"
  });

  await assert.rejects(
    () =>
      authStore.revokeTicket(ticket.value, "127.0.0.1", {
        expectedPlayerId: "player-002"
      }),
    (error) => error.code === "TICKET_OWNER_MISMATCH"
  );

  assert.equal(await authStore.getTicketOwner(ticket.value), "player-001");
});

test("AuthStore can issue game ticket payload with selected character", async () => {
  const redis = new FakeRedis();
  const authStore = new AuthStore(
    {
      redisKeyPrefix: "test:",
      sessionTtlSeconds: 600,
      ticketTtlSeconds: 300,
      ticketSecret: "test-secret"
    },
    redis
  );

  const ticket = await authStore.issueGameTicket("player-001", "127.0.0.1", {
    characterId: "chr_0000000000001",
    worldId: 9
  });
  const ticketPayload = JSON.parse(
    Buffer.from(ticket.value.split(".")[0], "base64url").toString("utf8")
  );

  assert.equal(ticketPayload.playerId, "player-001");
  assert.equal(ticketPayload.characterId, "chr_0000000000001");
  assert.equal(ticketPayload.worldId, 9);
  assert.equal(ticketPayload.ver, 1);
  assert.equal(await authStore.getTicketOwner(ticket.value), "player-001");
  assert.equal(GAME_TICKET_REDIS_OWNER_SCOPE, "account_player");
  assert.equal(GAME_TICKET_INVALIDATION_SCOPE, "account");
  assert.deepEqual(await authStore.validateGameTicket(ticket.value), {
    ...ticketPayload,
    playerId: "player-001",
    characterId: "chr_0000000000001"
  });
  assert.deepEqual(verifyGameTicketPayload("test-secret", ticket.value), {
    ...ticketPayload,
    playerId: "player-001",
    characterId: "chr_0000000000001"
  });
});

test("AuthStore rejects issuing or validating game ticket without characterId", async () => {
  const redis = new FakeRedis();
  const authStore = new AuthStore(
    {
      redisKeyPrefix: "test:",
      sessionTtlSeconds: 600,
      ticketTtlSeconds: 300,
      ticketSecret: "test-secret"
    },
    redis
  );

  await assert.rejects(
    () => authStore.issueGameTicket("player-001", "127.0.0.1"),
    { code: "MISSING_CHARACTER_ID" }
  );

  const expiresAt = new Date(Date.now() + 300000).toISOString();
  const payloadB64 = Buffer.from(JSON.stringify({
    playerId: "player-001",
    nonce: "old-ticket",
    ver: 1,
    exp: expiresAt
  })).toString("base64url");
  const signature = crypto.createHmac("sha256", "test-secret").update(payloadB64).digest("base64url");
  const legacyTicket = `${payloadB64}.${signature}`;

  await assert.rejects(
    () => authStore.validateGameTicket(legacyTicket),
    { code: "MISSING_CHARACTER_ID" }
  );
});
