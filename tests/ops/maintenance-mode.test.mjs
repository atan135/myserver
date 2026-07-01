import assert from "node:assert/strict";
import { register } from "node:module";
import path from "node:path";
import { test } from "node:test";
import { pathToFileURL } from "node:url";

class MemoryRedis {
  constructor() {
    this.values = new Map();
  }

  async get(key) {
    return this.values.get(key) ?? null;
  }

  async set(key, value) {
    this.values.set(key, String(value));
  }
}

function registerTs(project) {
  process.env.TS_NODE_PROJECT = path.resolve(project);
  process.env.TS_NODE_TRANSPILE_ONLY = "true";
  register("ts-node/esm", pathToFileURL("./"));
}

registerTs("apps/admin-api/tsconfig.json");

const { AdminStore, maintenanceStateKey } = await import("../../apps/admin-api/src/admin-store.js");
const { MaintenanceController } = await import("../../apps/admin-api/src/maintenance/maintenance.controller.ts");

const { MaintenanceStore } = await import("../../apps/auth-http/src/maintenance-store.js");
const { AuthService } = await import("../../apps/auth-http/src/auth/auth.service.ts");
const { GameTicketController } = await import("../../apps/auth-http/src/game-ticket/game-ticket.controller.ts");

function createPool(rows = []) {
  return {
    queries: [],
    async query(sql, params = []) {
      this.queries.push({ sql, params });
      if (sql.includes("FROM admin_audit_logs")) {
        return { rows };
      }
      return { rowCount: 1, rows: [] };
    }
  };
}

function createAdminReq() {
  return {
    admin: { sub: 7, username: "root", role: "admin" },
    ip: "10.0.0.5",
    headers: {},
    socket: { remoteAddress: "10.0.0.5" }
  };
}

test("admin-api writes shared maintenance state to prefixed Redis key", async () => {
  const redis = new MemoryRedis();
  const pool = createPool();
  const store = new AdminStore(pool, redis, { redisKeyPrefix: "test:" });
  const state = await store.setMaintenanceMode(true, {
    reason: "deploy window",
    updatedAt: "2026-06-11T08:00:00.000Z",
    updatedBy: "root"
  });

  assert.deepEqual(state, {
    enabled: true,
    reason: "deploy window",
    updatedAt: "2026-06-11T08:00:00.000Z",
    updatedBy: "root"
  });
  assert.equal(redis.values.has("test:maintenance:global"), true);
  assert.equal(maintenanceStateKey("test:"), "test:maintenance:global");
  assert.deepEqual(await store.getMaintenanceStatus(), state);
});

test("admin-api falls back to latest audit log when Redis state is absent", async () => {
  const pool = createPool([
    {
      action: "maintenance_enabled",
      admin_username: "ops",
      details_json: JSON.stringify({ reason: "hotfix" }),
      created_at: new Date("2026-06-11T09:00:00.000Z")
    }
  ]);
  const store = new AdminStore(pool, new MemoryRedis(), { redisKeyPrefix: "test:" });

  assert.deepEqual(await store.getMaintenanceStatus(), {
    enabled: true,
    reason: "hotfix",
    updatedAt: "2026-06-11T09:00:00.000Z",
    updatedBy: "ops"
  });
});

test("maintenance controller validates enabled and records single operator audit", async () => {
  const auditLogs = [];
  const adminStore = {
    async setMaintenanceMode(enabled, state) {
      return { enabled, ...state };
    },
    async appendAuditLog(event) {
      auditLogs.push(event);
    }
  };
  const controller = new MaintenanceController({ trustProxy: false, trustedProxies: [] }, adminStore);

  await assert.rejects(
    () => controller.setStatus({ enabled: "true" }, createAdminReq()),
    (error) => error.getResponse?.().error === "INVALID_MAINTENANCE_ENABLED"
  );

  const result = await controller.setStatus({ enabled: true, reason: "  deploy  " }, createAdminReq());
  assert.equal(result.ok, true);
  assert.equal(result.enabled, true);
  assert.equal(result.reason, "deploy");
  assert.equal(result.updatedBy, "root");
  assert.equal(auditLogs.length, 1);
  assert.equal(auditLogs[0].action, "maintenance_enabled");
  assert.deepEqual(auditLogs[0].details, { reason: "deploy" });
});

test("auth-http maintenance store reads shared Redis state", async () => {
  const redis = new MemoryRedis();
  await redis.set(
    "test:maintenance:global",
    JSON.stringify({
      enabled: true,
      reason: "deploy",
      updatedAt: "2026-06-11T08:00:00.000Z",
      updatedBy: "root"
    })
  );
  const store = new MaintenanceStore(redis, { redisKeyPrefix: "test:" });

  assert.deepEqual(await store.getStatus(), {
    enabled: true,
    reason: "deploy",
    updatedAt: "2026-06-11T08:00:00.000Z",
    updatedBy: "root"
  });
});

function createAuthService(maintenanceStatus, authStoreOverrides = {}) {
  const sessions = new Map([
    ["access-token", { playerId: "player-1", createdAt: "2026-06-11T08:00:00.000Z" }]
  ]);
  const authStore = {
    async createGuestSession() {
      return {
        playerId: "player-1",
        accessToken: "access-token",
        gameTicket: { value: "ticket-1", expiresAt: "2026-06-11T08:15:00.000Z" }
      };
    },
    async createPasswordSession() {
      return {
        playerId: "player-1",
        loginName: "test001",
        accessToken: "access-token",
        gameTicket: { value: "ticket-1", expiresAt: "2026-06-11T08:15:00.000Z" }
      };
    },
    async getSessionByAccessToken(token) {
      return sessions.get(token) ?? null;
    },
    async issueGameTicket(playerId) {
      return { value: `ticket-${playerId}`, expiresAt: "2026-06-11T08:15:00.000Z" };
    },
    ...authStoreOverrides
  };
  const service = new AuthService(
    {
      dbEnabled: true,
      accountLockEnabled: false,
      gameProxyHost: "127.0.0.1",
      gameProxyPort: 4000,
      trustProxy: false,
      trustedProxies: []
    },
    authStore,
    null,
    {},
    { async discoverClientServices() { return { game: null, chat: null, mail: null, announce: null }; } },
    { async getStatus() { return maintenanceStatus; } }
  );
  return { service, authStore };
}

async function assertMaintenanceError(promise) {
  await assert.rejects(
    promise,
    (error) => {
      assert.equal(error.getStatus(), 503);
      const response = error.getResponse();
      assert.equal(response.error, "MAINTENANCE_MODE");
      assert.equal(response.reason, "deploy");
      assert.equal(response.updatedBy, undefined);
      return true;
    }
  );
}

test("auth-http blocks player login while maintenance is enabled", async () => {
  const maintenanceStatus = {
    enabled: true,
    reason: "deploy",
    updatedAt: "2026-06-11T08:00:00.000Z",
    updatedBy: "root"
  };
  const { service } = createAuthService(maintenanceStatus);

  await assertMaintenanceError(
    service.guestLogin({}, { headers: {}, socket: { remoteAddress: "127.0.0.1" } })
  );
  await assertMaintenanceError(
    service.login(
      { loginName: "test001", password: "Password123!" },
      { headers: {}, socket: { remoteAddress: "127.0.0.1" } },
      {}
    )
  );
});

test("auth-http reports structured 503 when maintenance state backend is unavailable", async () => {
  const { service } = createAuthService(null);
  service.maintenanceStore = {
    async getStatus() {
      throw new Error("redis unavailable");
    }
  };

  await assert.rejects(
    () => service.guestLogin({}, { headers: {}, socket: { remoteAddress: "127.0.0.1" } }),
    (error) => {
      assert.equal(error.getStatus(), 503);
      assert.equal(error.getResponse().error, "AUTH_BACKEND_UNAVAILABLE");
      return true;
    }
  );
});

test("auth-http blocks new game ticket issue but leaves revoke path outside maintenance guard", async () => {
  const maintenanceStatus = {
    enabled: true,
    reason: "deploy",
    updatedAt: "2026-06-11T08:00:00.000Z",
    updatedBy: "root"
  };
  let revoked = false;
  const { service, authStore } = createAuthService(maintenanceStatus, {
    async revokeTicket() {
      revoked = true;
    }
  });
  const controller = new GameTicketController(
    authStore,
    { trustProxy: false, trustedProxies: [] },
    { async checkPlayer() { return { blocked: false, unavailable: false }; } },
    null,
    { enabled: true, async getByCharacterId() { return null; } },
    service
  );
  const req = {
    headers: { authorization: "Bearer access-token" },
    socket: { remoteAddress: "127.0.0.1" }
  };

  await assertMaintenanceError(controller.issue(req));

  const revokeResult = await controller.revoke(req, { ticket: "ticket-1" });
  assert.equal(revokeResult.ok, true);
  assert.equal(revoked, true);
});
