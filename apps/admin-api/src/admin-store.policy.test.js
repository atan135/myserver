import assert from "node:assert/strict";
import test from "node:test";

import { AdminStore } from "./admin-store.js";

const SCOPE = {
  world_ids: ["world-1"],
  service_names: ["*"],
  instance_ids: ["*"],
  field_allowlist: ["*"],
  target_types: ["player"],
  target_ids: ["player-1"],
  max_targets: 1
};

function transactionPool(responses = {}) {
  const calls = [];
  const client = {
    async query(sql, params = []) {
      calls.push({ sql, params });
      if (sql === "BEGIN" || sql === "COMMIT" || sql === "ROLLBACK") return { rows: [] };
      if (sql.includes("SELECT permission_key FROM admin_permissions")) return { rows: responses.permissionRows ?? [{ permission_key: "gm.send_item" }] };
      if (sql.includes("SELECT role_key FROM admin_roles")) return { rows: responses.roleRows ?? [{ role_key: "operator" }] };
      if (sql.includes("INSERT INTO admin_permission_grants")) return { rows: [{ id: 17, effective_at: new Date("2026-07-19T02:00:00.000Z"), expires_at: null }] };
      if (sql.includes("INSERT INTO admin_account_roles")) return { rows: [{ id: 18, effective_at: new Date("2026-07-19T02:00:00.000Z"), expires_at: null }] };
      if (sql.includes("UPDATE admin_permission_grants")) return { rows: [{ id: 17, admin_id: 8, permission_key: "gm.send_item", scope_json: SCOPE, revoked_at: new Date("2026-07-19T03:00:00.000Z") }] };
      if (sql.includes("UPDATE admin_account_roles")) return { rows: [{ id: 18, admin_id: 8, role_key: "operator", scope_json: SCOPE, revoked_at: new Date("2026-07-19T03:00:00.000Z") }] };
      if (sql.includes("INSERT INTO admin_authorization_audit_events")) return { rows: [] };
      throw new Error(`unexpected query: ${sql}`);
    },
    release() {
      calls.push({ sql: "RELEASE", params: [] });
    }
  };
  return {
    calls,
    async connect() {
      return client;
    }
  };
}

function bootstrapPool({ roleRows = [{ role_key: "super_admin" }], auditError = null } = {}) {
  const calls = [];
  const client = {
    async query(sql, params = []) {
      calls.push({ sql, params });
      if (sql === "BEGIN" || sql === "COMMIT" || sql === "ROLLBACK") return { rows: [] };
      if (sql.includes("INSERT INTO admin_accounts")) return { rows: [{ id: 8 }] };
      if (sql.includes("SELECT role_key FROM admin_roles")) return { rows: roleRows };
      if (sql.includes("INSERT INTO admin_account_roles")) {
        return { rows: [{ id: 18, effective_at: new Date("2026-07-19T02:00:00.000Z") }] };
      }
      if (sql.includes("INSERT INTO admin_authorization_audit_events")) {
        if (auditError) throw auditError;
        return { rows: [] };
      }
      throw new Error(`unexpected query: ${sql}`);
    },
    release() {
      calls.push({ sql: "RELEASE", params: [] });
    }
  };
  return {
    calls,
    async query(sql, params = []) {
      calls.push({ sql, params });
      if (sql.includes("FROM admin_accounts")) return { rows: [] };
      throw new Error(`unexpected pool query: ${sql}`);
    },
    async connect() {
      return client;
    }
  };
}

test("permission grants persist actor, reason, lifecycle and a paired authorization audit event", async () => {
  const pool = transactionPool();
  const store = new AdminStore(pool);
  const grant = await store.grantAdminPermission({
    adminId: 8,
    permissionKey: "gm.send_item",
    scope: SCOPE,
    grantedByAdminId: 1,
    grantedBySubject: "admin:root",
    reason: "incident correction",
    expiresAt: "2026-07-20T02:00:00.000Z"
  });

  assert.equal(grant.id, 17);
  assert.equal(pool.calls[0].sql, "BEGIN");
  assert.equal(pool.calls.at(-2).sql, "COMMIT");
  const insert = pool.calls.find((call) => call.sql.includes("INSERT INTO admin_permission_grants"));
  assert.deepEqual(insert.params.slice(0, 6), [8, "gm.send_item", JSON.stringify(SCOPE), 1, "admin:root", "incident correction"]);
  const audit = pool.calls.find((call) => call.sql.includes("INSERT INTO admin_authorization_audit_events"));
  assert.equal(audit.params[1], "admin:root");
  assert.equal(audit.params[5], "incident correction");
  assert.equal(audit.params[6], JSON.stringify(SCOPE));
});

test("role grants and both revocation paths append audit events inside their transaction", async () => {
  const pool = transactionPool();
  const store = new AdminStore(pool);

  await store.grantAdminRole({
    adminId: 8,
    roleKey: "operator",
    scope: SCOPE,
    grantedByAdminId: 1,
    grantedBySubject: "admin:root",
    reason: "on-call coverage"
  });
  await store.revokeAdminPermission({
    grantId: 17,
    revokedByAdminId: 1,
    revokedBySubject: "admin:root",
    reason: "coverage complete"
  });
  await store.revokeAdminRole({
    assignmentId: 18,
    revokedByAdminId: 1,
    revokedBySubject: "admin:root",
    reason: "on-call ended"
  });

  assert.equal(pool.calls.filter((call) => call.sql === "BEGIN").length, 3);
  assert.equal(pool.calls.filter((call) => call.sql === "COMMIT").length, 3);
  const auditEvents = pool.calls.filter((call) => call.sql.includes("INSERT INTO admin_authorization_audit_events"));
  assert.equal(auditEvents.length, 3);
  assert.match(auditEvents[0].sql, /'account_role_granted'/);
  assert.match(auditEvents[1].sql, /'permission_revoked'/);
  assert.match(auditEvents[2].sql, /'account_role_revoked'/);
  assert.equal(auditEvents[1].params[5], "coverage complete");
  assert.equal(auditEvents[2].params[5], "on-call ended");
});

test("unknown catalog entries roll back without inserting a grant or audit event", async () => {
  const pool = transactionPool({ permissionRows: [] });
  const store = new AdminStore(pool);

  await assert.rejects(
    () => store.grantAdminPermission({
      adminId: 8,
      permissionKey: "unknown.write",
      scope: SCOPE,
      grantedBySubject: "admin:root",
      reason: "must fail"
    }),
    (error) => error.code === "UNKNOWN_PERMISSION"
  );
  assert.equal(pool.calls.some((call) => call.sql.includes("INSERT INTO admin_permission_grants")), false);
  assert.equal(pool.calls.some((call) => call.sql.includes("INSERT INTO admin_authorization_audit_events")), false);
  assert.equal(pool.calls.some((call) => call.sql === "ROLLBACK"), true);
});

test("bootstrap account, role assignment, and authorization audit are atomic", async () => {
  const pool = bootstrapPool();
  const store = new AdminStore(pool);

  const admin = await store.ensureInitialAdmin({
    initialAdminUsername: "bootstrap-admin",
    initialAdminDisplayName: "Bootstrap Admin",
    initialAdminPassword: "not-a-production-password",
    bootstrapAdminRole: "super_admin",
    env: "test"
  });

  assert.equal(admin.id, 8);
  assert.equal(pool.calls[1].sql, "BEGIN");
  assert.ok(pool.calls.find((call) => call.sql.includes("INSERT INTO admin_accounts")));
  const assignment = pool.calls.find((call) => call.sql.includes("INSERT INTO admin_account_roles"));
  assert.deepEqual(assignment.params.slice(0, 2), [8, "super_admin"]);
  const audit = pool.calls.find((call) => call.sql.includes("INSERT INTO admin_authorization_audit_events"));
  assert.equal(audit.params[0], "bootstrap:test:bootstrap-admin");
  assert.equal(pool.calls.at(-2).sql, "COMMIT");
  assert.equal(pool.calls.at(-1).sql, "RELEASE");
});

test("bootstrap rolls back the account when authorization audit persistence fails", async () => {
  const pool = bootstrapPool({ auditError: new Error("authorization audit unavailable") });
  const store = new AdminStore(pool);

  await assert.rejects(
    () => store.ensureInitialAdmin({
      initialAdminUsername: "bootstrap-admin",
      initialAdminDisplayName: "Bootstrap Admin",
      initialAdminPassword: "not-a-production-password",
      bootstrapAdminRole: "super_admin",
      env: "test"
    }),
    /authorization audit unavailable/
  );

  assert.equal(pool.calls.some((call) => call.sql === "COMMIT"), false);
  assert.equal(pool.calls.some((call) => call.sql === "ROLLBACK"), true);
  assert.equal(pool.calls.at(-1).sql, "RELEASE");
});
