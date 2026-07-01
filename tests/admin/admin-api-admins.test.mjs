import assert from "node:assert/strict";
import { register } from "node:module";
import path from "node:path";
import { test } from "node:test";
import { pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT = path.resolve("apps/admin-api/tsconfig.json");
process.env.TS_NODE_TRANSPILE_ONLY = "true";
register("ts-node/esm", pathToFileURL("./"));

const { AdminsController } = await import("../../apps/admin-api/src/admins/admins.controller.ts");
const { PERMISSIONS_KEY } = await import("../../apps/admin-api/src/auth/roles.decorator.ts");
const { AdminStore, verifyPassword } = await import("../../apps/admin-api/src/admin-store.js");

function createReq(admin = { sub: 1, username: "root", role: "admin" }) {
  return {
    admin,
    ip: "10.0.0.5",
    headers: {},
    socket: { remoteAddress: "10.0.0.5" }
  };
}

function createAdminStore() {
  const admins = new Map([
    [
      "1",
      {
        id: 1,
        username: "root",
        displayName: "Root",
        role: "admin",
        status: "active"
      }
    ],
    [
      "2",
      {
        id: 2,
        username: "ops",
        displayName: "Ops",
        role: "operator",
        status: "active"
      }
    ]
  ]);
  const auditLogs = [];
  const passwordUpdates = [];

  return {
    auditLogs,
    passwordUpdates,
    async findAdminById(adminId) {
      return admins.get(String(adminId)) ?? null;
    },
    async updateAdminPassword(adminId, password) {
      if (!admins.has(String(adminId))) {
        return false;
      }
      passwordUpdates.push({ adminId: String(adminId), password });
      return true;
    },
    async appendAuditLog(event) {
      auditLogs.push(event);
    }
  };
}

function createSessionStore() {
  const versions = new Map();
  return {
    versions,
    async bumpTokenVersion(adminId) {
      const key = String(adminId);
      const next = (versions.get(key) ?? 0) + 1;
      versions.set(key, next);
      return next;
    }
  };
}

function createController(adminStore = createAdminStore(), sessionStore = createSessionStore()) {
  return new AdminsController(
    { trustProxy: false, trustedProxies: [] },
    adminStore,
    sessionStore
  );
}

test("revokeTokens bumps target admin token version and writes audit log", async () => {
  const adminStore = createAdminStore();
  const sessionStore = createSessionStore();
  const controller = createController(adminStore, sessionStore);

  const result = await controller.revokeTokens("2", { reason: "role change" }, createReq());

  assert.equal(result.ok, true);
  assert.equal(result.targetAdmin.username, "ops");
  assert.equal(result.tokenVersion, 1);
  assert.equal(result.currentTokenInvalidated, false);
  assert.equal(sessionStore.versions.get("2"), 1);
  assert.equal(adminStore.auditLogs.length, 1);
  assert.equal(adminStore.auditLogs[0].action, "admin_tokens_revoked");
  assert.equal(adminStore.auditLogs[0].targetValue, "2");
  assert.equal(adminStore.auditLogs[0].details.result, "success");
  assert.equal(adminStore.auditLogs[0].details.reason, "role change");
});

test("admin token lifecycle endpoints require admin permission metadata", () => {
  assert.deepEqual(Reflect.getMetadata(PERMISSIONS_KEY, AdminsController.prototype.revokeTokens), ["admins.revoke_tokens"]);
  assert.deepEqual(Reflect.getMetadata(PERMISSIONS_KEY, AdminsController.prototype.resetPassword), ["admins.reset_password"]);
});

test("revokeTokens reports self revocation invalidates current token after response", async () => {
  const adminStore = createAdminStore();
  const sessionStore = createSessionStore();
  const controller = createController(adminStore, sessionStore);

  const result = await controller.revokeTokens("1", { reason: "emergency" }, createReq());

  assert.equal(result.ok, true);
  assert.equal(result.currentTokenInvalidated, true);
  assert.match(result.message, /current request completed/);
  assert.equal(adminStore.auditLogs[0].details.currentTokenInvalidated, true);
});

test("resetPassword updates password, bumps token version, and avoids password in audit", async () => {
  const adminStore = createAdminStore();
  const sessionStore = createSessionStore();
  const controller = createController(adminStore, sessionStore);

  const result = await controller.resetPassword(
    "2",
    { newPassword: "NewPass456!X", reason: "operator rotation" },
    createReq()
  );

  assert.equal(result.ok, true);
  assert.equal(result.tokenVersion, 1);
  assert.deepEqual(adminStore.passwordUpdates, [{ adminId: "2", password: "NewPass456!X" }]);
  assert.equal(sessionStore.versions.get("2"), 1);
  assert.equal(adminStore.auditLogs.length, 1);
  assert.equal(adminStore.auditLogs[0].action, "admin_password_reset");
  assert.equal(adminStore.auditLogs[0].details.reason, "operator rotation");
  assert.equal(JSON.stringify(adminStore.auditLogs[0]).includes("NewPass456!X"), false);
});

test("resetPassword rejects weak password before update and token revocation", async () => {
  const adminStore = createAdminStore();
  const sessionStore = createSessionStore();
  const controller = createController(adminStore, sessionStore);

  await assert.rejects(
    () => controller.resetPassword("2", { newPassword: "short", reason: "rotation" }, createReq()),
    (error) => error.getResponse?.().error === "INVALID_NEW_PASSWORD"
  );

  assert.deepEqual(adminStore.passwordUpdates, []);
  assert.equal(sessionStore.versions.size, 0);
  assert.equal(adminStore.auditLogs.length, 1);
  assert.equal(adminStore.auditLogs[0].action, "admin_password_reset_failed");
  assert.equal(adminStore.auditLogs[0].details.error, "INVALID_NEW_PASSWORD");
  assert.equal(JSON.stringify(adminStore.auditLogs[0]).includes("short"), false);
});

test("admin token operations reject missing target admin with audit", async () => {
  const adminStore = createAdminStore();
  const sessionStore = createSessionStore();
  const controller = createController(adminStore, sessionStore);

  await assert.rejects(
    () => controller.revokeTokens("999", { reason: "cleanup" }, createReq()),
    (error) => error.getResponse?.().error === "ADMIN_NOT_FOUND"
  );

  assert.equal(sessionStore.versions.size, 0);
  assert.equal(adminStore.auditLogs.length, 1);
  assert.equal(adminStore.auditLogs[0].action, "admin_tokens_revoke_failed");
  assert.equal(adminStore.auditLogs[0].details.result, "rejected");
  assert.equal(adminStore.auditLogs[0].details.error, "ADMIN_NOT_FOUND");
});

test("admin token operations audit invalid request input", async () => {
  const adminStore = createAdminStore();
  const sessionStore = createSessionStore();
  const controller = createController(adminStore, sessionStore);

  await assert.rejects(
    () => controller.revokeTokens("bad-id", { reason: "cleanup" }, createReq()),
    (error) => error.getResponse?.().error === "INVALID_ADMIN_ID"
  );

  await assert.rejects(
    () => controller.resetPassword("2", { newPassword: "NewPass456!X", reason: 123 }, createReq()),
    (error) => error.getResponse?.().error === "INVALID_REASON"
  );

  assert.equal(sessionStore.versions.size, 0);
  assert.equal(adminStore.auditLogs.length, 2);
  assert.equal(adminStore.auditLogs[0].action, "admin_tokens_revoke_failed");
  assert.equal(adminStore.auditLogs[0].targetValue, "bad-id");
  assert.equal(adminStore.auditLogs[0].details.error, "INVALID_ADMIN_ID");
  assert.equal(adminStore.auditLogs[1].action, "admin_password_reset_failed");
  assert.equal(adminStore.auditLogs[1].targetValue, "2");
  assert.equal(adminStore.auditLogs[1].details.error, "INVALID_REASON");
});

test("resetPassword records failed audit without password update when token version bump fails", async () => {
  const adminStore = createAdminStore();
  const sessionStore = {
    async bumpTokenVersion() {
      throw new Error("redis unavailable");
    }
  };
  const controller = createController(adminStore, sessionStore);

  await assert.rejects(
    () => controller.resetPassword("2", { newPassword: "NewPass456!X", reason: "rotation" }, createReq()),
    /redis unavailable/
  );

  assert.deepEqual(adminStore.passwordUpdates, []);
  assert.equal(adminStore.auditLogs.length, 1);
  assert.equal(adminStore.auditLogs[0].action, "admin_password_reset_failed");
  assert.equal(adminStore.auditLogs[0].details.result, "failed");
  assert.equal(adminStore.auditLogs[0].details.error, "TOKEN_VERSION_BUMP_FAILED");
  assert.equal(JSON.stringify(adminStore.auditLogs[0]).includes("NewPass456!X"), false);
});

test("AdminStore finds admins by id and updates password hash without storing plaintext", async () => {
  let updateParams = null;
  const pool = {
    async query(sql, params) {
      if (sql.includes("FROM admin_accounts") && sql.includes("WHERE id = $1")) {
        return { rows: [{
          id: 2,
          username: "ops",
          display_name: "Ops",
          password_algo: "bcrypt",
          password_salt: "old-salt",
          password_hash: "old-hash",
          role: "operator",
          status: "active"
        }] };
      }

      if (sql.includes("UPDATE admin_accounts")) {
        updateParams = params;
        return { rowCount: 1, rows: [] };
      }

      throw new Error(`unexpected query: ${sql}`);
    }
  };

  const store = new AdminStore(pool);

  const admin = await store.findAdminById(2);
  assert.equal(admin.username, "ops");
  assert.equal(admin.displayName, "Ops");
  assert.equal(admin.passwordAlgo, "bcrypt");

  assert.equal(await store.updateAdminPassword(2, "NewPass456!X"), true);
  assert.equal(updateParams.length, 3);
  assert.notEqual(updateParams[0], "NewPass456!X");
  assert.notEqual(updateParams[1], "NewPass456!X");
  assert.equal(updateParams[2], 2);
  assert.equal(verifyPassword("NewPass456!X", updateParams[1]), true);
});
