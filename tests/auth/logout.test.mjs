import assert from "node:assert/strict";
import { register } from "node:module";
import { describe, test } from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../apps/auth-http/tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { AuthService } = await import("../../apps/auth-http/src/auth/auth.service.ts");

function createRequest(token) {
  return {
    headers: token ? { authorization: `Bearer ${token}` } : {},
    socket: { remoteAddress: "127.0.0.1" }
  };
}

function createFakeRedis() {
  const store = new Map();

  return {
    store,
    async get(key) {
      return store.get(key) ?? null;
    },
    async incr(key) {
      const next = Number.parseInt(store.get(key) ?? "0", 10) + 1;
      store.set(key, String(next));
      return next;
    }
  };
}

function createServiceContext() {
  const redis = createFakeRedis();
  const revokedTickets = [];
  const authStore = {
    redis,
    async destroySession(accessToken) {
      if (accessToken !== "valid-token-001") {
        return { destroyed: false };
      }
      return { destroyed: true, playerId: "player-001" };
    },
    async invalidatePlayerTickets(playerId) {
      return redis.incr(`test:player-ticket-version:${playerId}`);
    },
    async revokeTicket(ticket, clientIp, options) {
      revokedTickets.push({ ticket, clientIp, options });
      if (ticket === "other-player-ticket") {
        const error = new Error("ticket owner mismatch");
        error.code = "TICKET_OWNER_MISMATCH";
        throw error;
      }
      return { revoked: true };
    }
  };
  const config = {
    dbEnabled: false,
    ratelimitEnabled: false,
    accountLockEnabled: false,
    gameProxyHost: "127.0.0.1",
    gameProxyPort: 4000
  };
  const service = new AuthService(config, authStore, null, null, null);

  return { service, redis, revokedTickets };
}

async function assertApiError(promise, status, errorCode) {
  await assert.rejects(
    promise,
    (error) => {
      assert.equal(error.getStatus(), status);
      assert.equal(error.getResponse().error, errorCode);
      return true;
    }
  );
}

describe("AuthService.logout", () => {
  test("rejects missing bearer token", async () => {
    const { service } = createServiceContext();

    await assertApiError(service.logout(createRequest(null), {}), 401, "MISSING_BEARER_TOKEN");
  });

  test("rejects invalid bearer token", async () => {
    const { service, redis } = createServiceContext();

    await assertApiError(service.logout(createRequest("bad-token"), {}), 401, "INVALID_ACCESS_TOKEN");
    assert.equal(await redis.get("test:player-ticket-version:player-001"), null);
  });

  test("invalidates player tickets after successful logout", async () => {
    const { service, redis } = createServiceContext();

    const body = await service.logout(createRequest("valid-token-001"), {});

    assert.deepEqual(body, { ok: true, message: "Logged out" });
    assert.equal(await redis.get("test:player-ticket-version:player-001"), "1");
  });

  test("keeps optional single-ticket revoke path scoped to logged out player", async () => {
    const { service, redis, revokedTickets } = createServiceContext();

    const body = await service.logout(createRequest("valid-token-001"), {
      ticket: "player-ticket"
    });

    assert.equal(body.ok, true);
    assert.equal(await redis.get("test:player-ticket-version:player-001"), "1");
    assert.deepEqual(revokedTickets, [
      {
        ticket: "player-ticket",
        clientIp: "127.0.0.1",
        options: { expectedPlayerId: "player-001" }
      }
    ]);
  });

  test("still rejects explicit ticket revoke when ticket belongs to another player", async () => {
    const { service, redis } = createServiceContext();

    await assert.rejects(
      service.logout(createRequest("valid-token-001"), {
        ticket: "other-player-ticket"
      }),
      { code: "TICKET_OWNER_MISMATCH" }
    );
    assert.equal(await redis.get("test:player-ticket-version:player-001"), "1");
  });
});
