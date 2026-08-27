import assert from "node:assert/strict";
import test from "node:test";

import {
  ADMIN_PERMISSIONS,
  ALL_ADMIN_PERMISSIONS,
  hasAnyPermission,
  hasPermission,
  effectivePermissions
} from "../../apps/admin-web/src/auth/permissions.js";

test("admin-web uses server effective permissions and never expands a role", () => {
  assert.deepEqual(effectivePermissions({ role: "viewer" }), []);
  assert.deepEqual(effectivePermissions({
    role: "operator",
    permissions: [
      ADMIN_PERMISSIONS.PLAYERS_STATUS_UPDATE,
      ADMIN_PERMISSIONS.GM_BAN_PLAYER,
      "unknown.permission",
      ADMIN_PERMISSIONS.GM_BAN_PLAYER
    ]
  }), [
    ADMIN_PERMISSIONS.PLAYERS_STATUS_UPDATE,
    ADMIN_PERMISSIONS.GM_BAN_PLAYER
  ]);

  assert.equal(hasPermission({ role: "operator" }, ADMIN_PERMISSIONS.PLAYERS_STATUS_UPDATE), false);
  assert.equal(hasPermission({ permissions: [ADMIN_PERMISSIONS.GM_BAN_PLAYER] }, ADMIN_PERMISSIONS.GM_BAN_PLAYER), true);
});

test("admin-web recognizes only returned catalog permissions", () => {
  for (const permission of ALL_ADMIN_PERMISSIONS) {
    assert.equal(hasPermission({ permissions: ALL_ADMIN_PERMISSIONS }, permission), true);
  }
});

test("admin-web permission helpers support server permission data", () => {
  const operator = { permissions: [ADMIN_PERMISSIONS.GM_KICK_PLAYER] };
  const viewer = { permissions: [ADMIN_PERMISSIONS.MONITORING_READ] };

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
