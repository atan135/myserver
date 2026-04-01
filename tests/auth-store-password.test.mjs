import assert from "node:assert/strict";
import { test } from "node:test";

import { AuthStore } from "../apps/auth-http/src/auth-store.js";
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
}

test("AuthStore password login validates credentials from mysql store", async () => {
  const passwordSalt = createPasswordSalt();
  const passwordHash = hashPassword("Passw0rd!", passwordSalt);
  const audits = [];
  const touchedPlayerIds = [];
  const redis = new FakeRedis();
  const mysqlStore = {
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
    mysqlStore
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
  assert.ok(session.gameTicket.value);
  assert.deepEqual(touchedPlayerIds, ["player-001"]);
  assert.equal(audits.some((entry) => entry.eventType === "password_login"), true);

  const storedSession = await authStore.getSessionByAccessToken(session.accessToken);
  assert.equal(storedSession.playerId, "player-001");
  assert.equal(storedSession.loginName, "test001");
});

test("AuthStore password login rejects invalid password", async () => {
  const passwordSalt = createPasswordSalt();
  const passwordHash = hashPassword("Passw0rd!", passwordSalt);
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
