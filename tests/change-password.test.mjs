import assert from "node:assert/strict";
import { register } from "node:module";
import { describe, test } from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

import {
  createPasswordSalt,
  hashPassword,
  verifyPassword
} from "../apps/auth-http/src/password-utils.js";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../apps/auth-http/tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { AuthService } = await import("../apps/auth-http/src/auth/auth.service.ts");

function createFakeRedis() {
  const store = new Map();
  const deletedKeys = [];

  return {
    store,
    deletedKeys,
    async set(key, value) {
      store.set(key, value);
    },
    async get(key) {
      return store.get(key) ?? null;
    },
    async del(key) {
      deletedKeys.push(key);
      store.delete(key);
    }
  };
}

function createFakeAuthStore(redis, sessions = new Map()) {
  const kickedPlayers = [];

  return {
    redis,
    kickedPlayers,
    prefixedKey(key) {
      return `test:${key}`;
    },
    async getSessionByAccessToken(token) {
      return sessions.get(token) ?? null;
    },
    async publishSessionKick(playerId, reason) {
      kickedPlayers.push({ playerId, reason });
    }
  };
}

function createFakeMysqlStore(accounts = new Map(), enabled = true) {
  const audits = [];
  const securityAudits = [];
  const updatedPasswords = [];

  return {
    enabled,
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

function createRequest(token) {
  return {
    headers: token ? { authorization: `Bearer ${token}` } : {},
    socket: { remoteAddress: "127.0.0.1" }
  };
}

function createServiceContext({ mysqlEnabled = true, mysqlStoreEnabled = true } = {}) {
  const passwordSalt = createPasswordSalt();
  const passwordHash = hashPassword("OldPass123!", passwordSalt);
  const redis = createFakeRedis();
  const sessions = new Map([
    [
      "valid-token-001",
      {
        playerId: "player-001",
        loginName: "testuser",
        createdAt: new Date().toISOString()
      }
    ],
    [
      "valid-token-guest",
      {
        playerId: "player-guest",
        guestId: "guest-abc",
        createdAt: new Date().toISOString()
      }
    ]
  ]);
  const accounts = new Map([
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
  ]);
  const authStore = createFakeAuthStore(redis, sessions);
  const mysqlStore = createFakeMysqlStore(accounts, mysqlStoreEnabled);
  const config = {
    mysqlEnabled,
    ratelimitEnabled: false,
    accountLockEnabled: false,
    gameProxyHost: "127.0.0.1",
    gameProxyPort: 4000
  };
  const service = new AuthService(config, authStore, null, mysqlStore, null);

  return { service, redis, authStore, mysqlStore, accounts };
}

async function assertApiError(promise, status, errorCode, messagePattern = null) {
  await assert.rejects(
    promise,
    (error) => {
      assert.equal(error.getStatus(), status);
      const response = error.getResponse();
      assert.equal(response.error, errorCode);
      if (messagePattern) {
        assert.match(response.message, messagePattern);
      }
      return true;
    }
  );
}

describe("AuthService.changePassword", () => {
  test("rejects missing bearer token", async () => {
    const { service } = createServiceContext();

    await assertApiError(
      service.changePassword(createRequest(null), { oldPassword: "a", newPassword: "b" }),
      401,
      "MISSING_BEARER_TOKEN"
    );
  });

  test("rejects invalid bearer token", async () => {
    const { service } = createServiceContext();

    await assertApiError(
      service.changePassword(createRequest("bad-token"), { oldPassword: "a", newPassword: "b" }),
      401,
      "INVALID_ACCESS_TOKEN"
    );
  });

  test("rejects missing oldPassword", async () => {
    const { service } = createServiceContext();

    await assertApiError(
      service.changePassword(createRequest("valid-token-001"), { newPassword: "NewPass456!" }),
      400,
      "INVALID_OLD_PASSWORD"
    );
  });

  test("rejects missing newPassword", async () => {
    const { service } = createServiceContext();

    await assertApiError(
      service.changePassword(createRequest("valid-token-001"), { oldPassword: "OldPass123!" }),
      400,
      "INVALID_NEW_PASSWORD"
    );
  });

  test("rejects newPassword shorter than 6 chars", async () => {
    const { service } = createServiceContext();

    await assertApiError(
      service.changePassword(createRequest("valid-token-001"), {
        oldPassword: "OldPass123!",
        newPassword: "short"
      }),
      400,
      "INVALID_NEW_PASSWORD",
      /between 6 and 128/
    );
  });

  test("rejects guest account without password account", async () => {
    const { service } = createServiceContext();

    await assertApiError(
      service.changePassword(createRequest("valid-token-guest"), {
        oldPassword: "OldPass123!",
        newPassword: "NewPass456!"
      }),
      400,
      "NOT_PASSWORD_ACCOUNT"
    );
  });

  test("rejects wrong old password and records security audit", async () => {
    const { service, mysqlStore } = createServiceContext();
    const initialAuditCount = mysqlStore.securityAudits.length;

    await assertApiError(
      service.changePassword(createRequest("valid-token-001"), {
        oldPassword: "WrongPassword!",
        newPassword: "NewPass456!"
      }),
      403,
      "OLD_PASSWORD_MISMATCH"
    );

    assert.ok(mysqlStore.securityAudits.length > initialAuditCount);
    const lastAudit = mysqlStore.securityAudits.at(-1);
    assert.equal(lastAudit.eventType, "change_password_failed");
  });

  test("succeeds with correct old password and updates hash", async () => {
    const { service, redis, authStore, mysqlStore } = createServiceContext();

    const body = await service.changePassword(createRequest("valid-token-001"), {
      oldPassword: "OldPass123!",
      newPassword: "NewPass456!"
    });

    assert.equal(body.ok, true);
    assert.match(body.message, /Password changed/);

    assert.ok(mysqlStore.updatedPasswords.length > 0);
    const lastUpdate = mysqlStore.updatedPasswords.at(-1);
    assert.equal(lastUpdate.playerId, "player-001");
    assert.ok(lastUpdate.passwordSalt);
    assert.ok(lastUpdate.passwordHash);

    const account = await mysqlStore.findPasswordAccountByPlayerId("player-001");
    assert.ok(verifyPassword("NewPass456!", account.passwordSalt, account.passwordHash));
    assert.ok(!verifyPassword("OldPass123!", account.passwordSalt, account.passwordHash));

    const passwordChangedAudit = mysqlStore.audits.find(
      (entry) => entry.eventType === "password_changed" && entry.playerId === "player-001"
    );
    assert.ok(passwordChangedAudit);

    assert.deepEqual(authStore.kickedPlayers, [
      { playerId: "player-001", reason: "password_changed" }
    ]);
    assert.ok(redis.deletedKeys.includes("test:session:valid-token-001"));
    assert.ok(redis.deletedKeys.includes("test:session-activity:valid-token-001"));
    assert.ok(redis.deletedKeys.includes("test:player-session:player-001"));
  });

  test("returns PASSWORD_CHANGE_UNAVAILABLE when MySQL is disabled", async () => {
    const { service } = createServiceContext({ mysqlEnabled: false, mysqlStoreEnabled: false });

    await assertApiError(
      service.changePassword(createRequest("valid-token-001"), {
        oldPassword: "OldPass123!",
        newPassword: "NewPass456!"
      }),
      400,
      "PASSWORD_CHANGE_UNAVAILABLE"
    );
  });
});
