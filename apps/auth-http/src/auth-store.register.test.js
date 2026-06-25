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

function createStore(dbStore) {
  return new AuthStore(
    { redisKeyPrefix: "", sessionTtlSeconds: 60, ticketTtlSeconds: 30, ticketSecret: "test-secret" },
    new FakeRedis(),
    dbStore
  );
}

test("register password account creates login session when review is disabled", async () => {
  const audits = [];
  const dbStore = {
    enabled: true,
    async createPasswordAccount(input) {
      return {
        playerId: "player-1",
        loginName: input.loginName,
        displayName: input.displayName,
        status: input.status
      };
    },
    async appendAuthAudit(entry) {
      audits.push(entry);
    }
  };
  const store = createStore(dbStore);

  const result = await store.registerPasswordAccount({
    loginName: "test001",
    password: "Passw0rd!",
    displayName: "Test",
    requireReview: false,
    clientIp: "127.0.0.1"
  });

  assert.equal(result.pendingReview, false);
  assert.equal(result.session.playerId, "player-1");
  assert.equal(result.session.gameTicket, null);
  assert.deepEqual(audits.map((entry) => entry.eventType), ["password_register", "password_register_login"]);
  assert.equal(audits.at(-1).details.gameTicketReason, "character_selection_required");
});

test("register password account returns pending review without session when review is enabled", async () => {
  const audits = [];
  const dbStore = {
    enabled: true,
    async createPasswordAccount(input) {
      return {
        playerId: "player-2",
        loginName: input.loginName,
        displayName: input.displayName,
        status: input.status
      };
    },
    async appendAuthAudit(entry) {
      audits.push(entry);
    }
  };
  const store = createStore(dbStore);

  const result = await store.registerPasswordAccount({
    loginName: "test002",
    password: "Passw0rd!",
    requireReview: true
  });

  assert.equal(result.pendingReview, true);
  assert.equal(result.session, null);
  assert.equal(result.account.status, "pending_review");
  assert.deepEqual(audits.map((entry) => entry.eventType), ["password_register"]);
});

test("register password account rejects duplicate login name", async () => {
  const audits = [];
  const dbStore = {
    enabled: true,
    async createPasswordAccount() {
      const error = new Error("duplicate");
      error.code = "LOGIN_NAME_EXISTS";
      throw error;
    },
    async appendAuthAudit(entry) {
      audits.push(entry);
    }
  };
  const store = createStore(dbStore);

  await assert.rejects(
    () => store.registerPasswordAccount({
      loginName: "test003",
      password: "Passw0rd!"
    }),
    { code: "LOGIN_NAME_EXISTS" }
  );
  assert.equal(audits[0].eventType, "password_register_failed");
});

test("pending review account is rejected by login gate", async () => {
  const store = createStore({ async appendAuthAudit() {} });

  await assert.rejects(
    store.assertAccountLoginAllowed({
      playerId: "player-4",
      status: "pending_review"
    }),
    { code: "ACCOUNT_DISABLED" }
  );
});
