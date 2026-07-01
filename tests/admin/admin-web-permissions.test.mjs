import assert from "node:assert/strict";
import test from "node:test";

import {
  ADMIN_PERMISSIONS,
  ALL_ADMIN_PERMISSIONS,
  hasAnyPermission,
  hasPermission,
  permissionsForRole,
  ROLE_PERMISSIONS
} from "../../apps/admin-web/src/auth/permissions.js";

test("admin-web permission matrix matches first-stage admin-api roles", () => {
  assert.deepEqual(permissionsForRole("viewer"), [
    "audit.read",
    "security.read",
    "players.read",
    "maintenance.read",
    "monitoring.read",
    "id.read"
  ]);

  assert.equal(hasPermission("operator", ADMIN_PERMISSIONS.PLAYERS_STATUS_UPDATE), true);
  assert.equal(hasPermission("operator", ADMIN_PERMISSIONS.PLAYERS_BAN), false);
  assert.equal(hasPermission("operator", ADMIN_PERMISSIONS.GM_BAN_PLAYER), false);
  assert.equal(hasPermission("operator", ADMIN_PERMISSIONS.MAINTENANCE_WRITE), false);
  assert.equal(hasPermission("operator", ADMIN_PERMISSIONS.MONITORING_ARCHIVE), false);
  assert.equal(hasPermission("operator", ADMIN_PERMISSIONS.ADMINS_REVOKE_TOKENS), false);

  assert.equal(hasPermission("admin", ADMIN_PERMISSIONS.GM_BAN_PLAYER), true);
  assert.equal(hasPermission("admin", ADMIN_PERMISSIONS.ADMINS_RESET_PASSWORD), true);
  assert.equal(hasPermission("super_admin", ADMIN_PERMISSIONS.MONITORING_ARCHIVE), true);
});

test("admin-web admin and super_admin keep full permissions", () => {
  assert.deepEqual(ROLE_PERMISSIONS.admin, ALL_ADMIN_PERMISSIONS);
  assert.deepEqual(ROLE_PERMISSIONS.super_admin, ALL_ADMIN_PERMISSIONS);

  for (const permission of ALL_ADMIN_PERMISSIONS) {
    assert.equal(hasPermission("admin", permission), true);
    assert.equal(hasPermission("super_admin", permission), true);
  }
});

test("admin-web permission helpers support role-only user data", () => {
  const operator = { role: "operator" };
  const viewer = { role: "viewer" };

  assert.equal(hasPermission(operator, ADMIN_PERMISSIONS.GM_KICK_PLAYER), true);
  assert.equal(hasPermission(viewer, ADMIN_PERMISSIONS.GM_KICK_PLAYER), false);
  assert.equal(
    hasAnyPermission(viewer, [
      ADMIN_PERMISSIONS.GM_BROADCAST,
      ADMIN_PERMISSIONS.MONITORING_READ
    ]),
    true
  );
  assert.equal(hasPermission(null, ADMIN_PERMISSIONS.AUDIT_READ), false);
});
