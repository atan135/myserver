import assert from "node:assert/strict";
import test from "node:test";

import { AuthStore } from "./auth-store.js";

class FakeRedis {
  constructor() {
    this.values = new Map();
  }

  async get(key) {
    return this.values.get(key) || null;
  }

  async set(key, value) {
    this.values.set(key, value);
  }

  async del(key) {
    this.values.delete(key);
  }

  async expire() {}
}

function createStore(mysqlStore) {
  return new AuthStore(
    { redisKeyPrefix: "", sessionTtlSeconds: 60, ticketTtlSeconds: 30, ticketSecret: "test-secret" },
    new FakeRedis(),
    mysqlStore
  );
}

test("expired banned account is restored and allowed", async () => {
  const audits = [];
  let restoredPlayerId = null;
  const mysqlStore = {
    async restoreExpiredBan(playerId) {
      restoredPlayerId = playerId;
      return true;
    },
    async appendAuthAudit(entry) {
      audits.push(entry);
    }
  };
  const store = createStore(mysqlStore);
  const account = {
    playerId: "player-1",
    status: "banned",
    banExpiresAt: new Date(Date.now() - 1000).toISOString()
  };

  await store.assertAccountLoginAllowed(account, "127.0.0.1", { eventType: "password_login_failed" });

  assert.equal(restoredPlayerId, "player-1");
  assert.equal(account.status, "active");
  assert.equal(account.banExpiresAt, null);
  assert.equal(audits[0].eventType, "account_ban_expired");
});

test("active ban is rejected without restoring", async () => {
  let restoreCalled = false;
  const store = createStore({
    async restoreExpiredBan() {
      restoreCalled = true;
      return true;
    },
    async appendAuthAudit() {}
  });

  await assert.rejects(
    store.assertAccountLoginAllowed({
      playerId: "player-2",
      status: "banned",
      banExpiresAt: new Date(Date.now() + 60_000).toISOString()
    }),
    { code: "ACCOUNT_DISABLED" }
  );
  assert.equal(restoreCalled, false);
});

test("expired ban is rejected when restore does not update a row", async () => {
  const store = createStore({
    async restoreExpiredBan() {
      return false;
    },
    async appendAuthAudit() {}
  });

  await assert.rejects(
    store.assertAccountLoginAllowed({
      playerId: "player-4",
      status: "banned",
      banExpiresAt: new Date(Date.now() - 1000).toISOString()
    }),
    { code: "ACCOUNT_DISABLED" }
  );
});

test("permanent ban is rejected", async () => {
  const store = createStore({ async appendAuthAudit() {} });

  await assert.rejects(
    store.assertAccountLoginAllowed({
      playerId: "player-3",
      status: "banned",
      banExpiresAt: null
    }),
    { code: "ACCOUNT_DISABLED" }
  );
});
