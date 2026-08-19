import assert from "node:assert/strict";
import test from "node:test";

import { ADMIN_PERMISSIONS as P, effectivePermissions, hasPermission } from "./permissions.js";

test("frontend permissions use server effective permissions and never expand a legacy role", () => {
  const legacySuperAdmin = { role: "super_admin" };
  assert.deepEqual(effectivePermissions(legacySuperAdmin), []);
  assert.equal(hasPermission(legacySuperAdmin, P.GM_SEND_ITEM), false);

  const scopedOperator = {
    role: "viewer",
    permissions: [P.AUDIT_READ, P.GM_SEND_ITEM, "unknown.permission", P.GM_SEND_ITEM]
  };
  assert.deepEqual(effectivePermissions(scopedOperator), [P.AUDIT_READ, P.GM_SEND_ITEM]);
  assert.equal(hasPermission(scopedOperator, P.GM_SEND_ITEM), true);
  assert.equal(hasPermission(scopedOperator, P.GM_BAN_PLAYER), false);
});

test("rollout and approval permissions are recognized only when returned by the server", () => {
  const operator = {
    permissions: [P.GAME_CONFIG_WRITE, P.ADMIN_PERMISSIONS_MANAGE]
  };

  assert.equal(hasPermission(operator, P.GAME_CONFIG_WRITE), true);
  assert.equal(hasPermission(operator, P.ADMIN_PERMISSIONS_MANAGE), true);
  assert.equal(hasPermission({}, P.GAME_CONFIG_WRITE), false);
  assert.equal(hasPermission({}, P.ADMIN_PERMISSIONS_MANAGE), false);
});
