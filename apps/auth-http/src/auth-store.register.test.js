import assert from "node:assert/strict";
import test from "node:test";

import { AuthStore, verifyGameTicketPayload } from "./auth-store.js";

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
  assert.match(result.session.deviceSubject, /^dvc_[A-Za-z0-9_-]{32}$/);
  const persistedSession = await store.getSessionByAccessToken(result.session.accessToken);
  assert.equal(persistedSession.deviceSubject, result.session.deviceSubject);
  assert.deepEqual(audits.map((entry) => entry.eventType), ["password_register", "password_register_login"]);
  assert.equal(audits.at(-1).details.gameTicketReason, "character_selection_required");

  const ticket = await store.issueGameTicket("player-1", "127.0.0.1", {
    characterId: "chr_0000000000001",
    worldId: 1,
    deviceSubject: persistedSession.deviceSubject
  });
  const payload = verifyGameTicketPayload("test-secret", ticket.value);
  assert.equal(payload.deviceSubject, result.session.deviceSubject);
});

test("guest session receives a server-generated subject unrelated to caller guestId", async () => {
  const store = createStore({
    async findOrCreateGuestPlayer(guestId) {
      return { playerId: "player-guest", guestId, status: "active" };
    },
    async appendAuthAudit() {},
    async touchPlayerLastLogin() {}
  });

  const session = await store.createGuestSession("caller-controlled-guest-id", "127.0.0.1");

  assert.equal(session.guestId, "caller-controlled-guest-id");
  assert.match(session.deviceSubject, /^dvc_[A-Za-z0-9_-]{32}$/);
  assert.notEqual(session.deviceSubject, session.guestId);
});

test("legacy ticket remains valid without a device subject and malformed subjects fail closed", async () => {
  const store = createStore({ async appendAuthAudit() {} });
  const legacy = await store.issueGameTicket("player-legacy", null, {
    characterId: "chr_0000000000002"
  });
  assert.equal(verifyGameTicketPayload("test-secret", legacy.value).deviceSubject, undefined);

  await assert.rejects(
    store.issueGameTicket("player-legacy", null, {
      characterId: "chr_0000000000002",
      deviceSubject: "caller-device"
    }),
    { code: "INVALID_DEVICE_SUBJECT" }
  );
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
