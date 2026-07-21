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
