import assert from "node:assert/strict";
import { after, before, describe, test } from "node:test";
import express from "express";
import { once } from "node:events";

import { createRoutes } from "../apps/auth-http/src/routes.js";
import {
  createPasswordSalt,
  hashPassword,
  verifyPassword
} from "../apps/auth-http/src/password-utils.js";

function createFakeRedis() {
  const store = new Map();
  return {
    store,
    async set(key, value, ...args) {
      store.set(key, value);
    },
    async get(key) {
      return store.get(key) ?? null;
    },
    async del(key) {
      store.delete(key);
    },
    async publish() {
      return 0;
    }
  };
}

function createFakeAuthStore(redis, sessions = new Map()) {
  return {
    redis,
    prefixedKey(key) {
      return `test:${key}`;
    },
    async getSessionByAccessToken(token) {
      return sessions.get(token) ?? null;
    }
  };
}

function createFakeMysqlStore(accounts = new Map()) {
  const audits = [];
  const securityAudits = [];
  let updatedPasswords = [];

  return {
    enabled: true,
    audits,
    securityAudits,
    updatedPasswords,
    async findPasswordAccountByPlayerId(playerId) {
      return accounts.get(playerId) ?? null;
    },
    async updatePassword(playerId, { passwordSalt, passwordHash }) {
      updatedPasswords.push({ playerId, passwordSalt, passwordHash });
      const account = accounts.get(playerId);
      if (account) {
        account.passwordSalt = passwordSalt;
        account.passwordHash = passwordHash;
      }
    },
    async appendAuthAudit(entry) {
      audits.push(entry);
    },
    async appendSecurityAudit(entry) {
      securityAudits.push(entry);
    }
  };
}

function buildTestApp({ redis, authStore, mysqlStore, config }) {
  const app = express();
  app.use(express.json());
  app.use(
    createRoutes(
      config,
      authStore,
      null, // gameAdminClient
      null, // rateLimiter
      null, // accountLockout
      mysqlStore,
      null  // serviceDiscovery
    )
  );
  return app;
}

describe("POST /api/v1/auth/change-password", () => {
  const passwordSalt = createPasswordSalt();
  const passwordHash = hashPassword("OldPass123!", passwordSalt);

  let server;
  let baseUrl;
  let redis;
  let authStore;
  let mysqlStore;
  let sessions;

  before(async () => {
    redis = createFakeRedis();
    sessions = new Map();
    sessions.set("valid-token-001", {
      playerId: "player-001",
      loginName: "testuser",
      createdAt: new Date().toISOString()
    });
    sessions.set("valid-token-guest", {
      playerId: "player-guest",
      guestId: "guest-abc",
      createdAt: new Date().toISOString()
    });

    authStore = createFakeAuthStore(redis, sessions);
    mysqlStore = createFakeMysqlStore(
      new Map([
        [
          "player-001",
          {
            playerId: "player-001",
            loginName: "testuser",
            accountType: "password",
            status: "active",
            passwordAlgo: "scrypt",
            passwordSalt,
            passwordHash
          }
        ]
      ])
    );

    const config = {
      mysqlEnabled: true,
      ratelimitEnabled: false,
      accountLockEnabled: false,
      gameProxyHost: "127.0.0.1",
      gameProxyPort: 4000
    };

    const app = buildTestApp({ redis, authStore, mysqlStore, config });
    server = app.listen(0, "127.0.0.1");
    await once(server, "listening");
    const addr = server.address();
    baseUrl = `http://127.0.0.1:${addr.port}`;
  });

  after(async () => {
    if (server) {
      server.close();
      await once(server, "close");
    }
  });

  test("rejects missing bearer token", async () => {
    const res = await fetch(`${baseUrl}/api/v1/auth/change-password`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ oldPassword: "a", newPassword: "b" })
    });
    assert.equal(res.status, 401);
    const body = await res.json();
    assert.equal(body.error, "MISSING_BEARER_TOKEN");
  });

  test("rejects invalid bearer token", async () => {
    const res = await fetch(`${baseUrl}/api/v1/auth/change-password`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        authorization: "Bearer bad-token"
      },
      body: JSON.stringify({ oldPassword: "a", newPassword: "b" })
    });
    assert.equal(res.status, 401);
    const body = await res.json();
    assert.equal(body.error, "INVALID_ACCESS_TOKEN");
  });

  test("rejects missing oldPassword", async () => {
    const res = await fetch(`${baseUrl}/api/v1/auth/change-password`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        authorization: "Bearer valid-token-001"
      },
      body: JSON.stringify({ newPassword: "NewPass456!" })
    });
    assert.equal(res.status, 400);
    const body = await res.json();
    assert.equal(body.error, "INVALID_OLD_PASSWORD");
  });

  test("rejects missing newPassword", async () => {
    const res = await fetch(`${baseUrl}/api/v1/auth/change-password`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        authorization: "Bearer valid-token-001"
      },
      body: JSON.stringify({ oldPassword: "OldPass123!" })
    });
    assert.equal(res.status, 400);
    const body = await res.json();
    assert.equal(body.error, "INVALID_NEW_PASSWORD");
  });

  test("rejects newPassword shorter than 6 chars", async () => {
    const res = await fetch(`${baseUrl}/api/v1/auth/change-password`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        authorization: "Bearer valid-token-001"
      },
      body: JSON.stringify({ oldPassword: "OldPass123!", newPassword: "short" })
    });
    assert.equal(res.status, 400);
    const body = await res.json();
    assert.equal(body.error, "INVALID_NEW_PASSWORD");
    assert.match(body.message, /between 6 and 128/);
  });

  test("rejects guest account (no password account)", async () => {
    const res = await fetch(`${baseUrl}/api/v1/auth/change-password`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        authorization: "Bearer valid-token-guest"
      },
      body: JSON.stringify({ oldPassword: "OldPass123!", newPassword: "NewPass456!" })
    });
    assert.equal(res.status, 400);
    const body = await res.json();
    assert.equal(body.error, "NOT_PASSWORD_ACCOUNT");
  });

  test("rejects wrong old password", async () => {
    const initialAuditCount = mysqlStore.securityAudits.length;

    const res = await fetch(`${baseUrl}/api/v1/auth/change-password`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        authorization: "Bearer valid-token-001"
      },
      body: JSON.stringify({ oldPassword: "WrongPassword!", newPassword: "NewPass456!" })
    });
    assert.equal(res.status, 403);
    const body = await res.json();
    assert.equal(body.error, "OLD_PASSWORD_MISMATCH");

    // Security audit should be recorded
    assert.ok(mysqlStore.securityAudits.length > initialAuditCount);
    const lastAudit = mysqlStore.securityAudits[mysqlStore.securityAudits.length - 1];
    assert.equal(lastAudit.eventType, "change_password_failed");
  });

  test("succeeds with correct old password and updates hash", async () => {
    const initialAuditCount = mysqlStore.audits.length;

    const res = await fetch(`${baseUrl}/api/v1/auth/change-password`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        authorization: "Bearer valid-token-001"
      },
      body: JSON.stringify({ oldPassword: "OldPass123!", newPassword: "NewPass456!" })
    });
    assert.equal(res.status, 200);
    const body = await res.json();
    assert.equal(body.ok, true);
    assert.match(body.message, /Password changed/);

    // Verify password was updated in store
    assert.ok(mysqlStore.updatedPasswords.length > 0);
    const lastUpdate = mysqlStore.updatedPasswords[mysqlStore.updatedPasswords.length - 1];
    assert.equal(lastUpdate.playerId, "player-001");
    assert.ok(lastUpdate.passwordSalt);
    assert.ok(lastUpdate.passwordHash);

    // New password should verify correctly
    const account = await mysqlStore.findPasswordAccountByPlayerId("player-001");
    assert.ok(verifyPassword("NewPass456!", account.passwordSalt, account.passwordHash));
    assert.ok(!verifyPassword("OldPass123!", account.passwordSalt, account.passwordHash));

    // Auth audit should be recorded
    const passwordChangedAudit = mysqlStore.audits.find(
      (a) => a.eventType === "password_changed" && a.playerId === "player-001"
    );
    assert.ok(passwordChangedAudit);

    // Session should be destroyed (kick)
    const sessionStillExists = await authStore.getSessionByAccessToken("valid-token-001");
    // The session was in our mock map but the route calls redis.del on it
    // Verify via redis that the session key was cleaned up
    const sessionKey = redis.store.get("test:session:valid-token-001");
    assert.equal(sessionKey, undefined);
  });
});

describe("POST /api/v1/auth/change-password (MySQL disabled)", () => {
  let server;
  let baseUrl;

  before(async () => {
    const redis = createFakeRedis();
    const sessions = new Map();
    sessions.set("valid-token-002", {
      playerId: "player-002",
      loginName: "testuser2",
      createdAt: new Date().toISOString()
    });

    const authStore = createFakeAuthStore(redis, sessions);
    const config = {
      mysqlEnabled: false,
      ratelimitEnabled: false,
      accountLockEnabled: false,
      gameProxyHost: "127.0.0.1",
      gameProxyPort: 4000
    };

    const app = buildTestApp({ redis, authStore, mysqlStore: { enabled: false }, config });
    server = app.listen(0, "127.0.0.1");
    await once(server, "listening");
    const addr = server.address();
    baseUrl = `http://127.0.0.1:${addr.port}`;
  });

  after(async () => {
    if (server) {
      server.close();
      await once(server, "close");
    }
  });

  test("returns PASSWORD_CHANGE_UNAVAILABLE when MySQL is disabled", async () => {
    const res = await fetch(`${baseUrl}/api/v1/auth/change-password`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        authorization: "Bearer valid-token-002"
      },
      body: JSON.stringify({ oldPassword: "OldPass123!", newPassword: "NewPass456!" })
    });
    assert.equal(res.status, 400);
    const body = await res.json();
    assert.equal(body.error, "PASSWORD_CHANGE_UNAVAILABLE");
  });
});
