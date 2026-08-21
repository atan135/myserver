import test from "node:test";
import assert from "node:assert/strict";
import {
  createActivityTypeRegistry,
  validateActivityAction,
  validateActivityTypeConfig
} from "./activity-types.js";

test("admin-api uses the shared activity type schemas", () => {
  const registry = createActivityTypeRegistry();
  assert.equal(validateActivityTypeConfig(registry, "login_reward", { schema_version: 1 }).type, "login_reward");
  assert.equal(validateActivityAction(registry, "lottery", "draw").type, "lottery");
});

test("admin-api rejects unknown type, action and schema version", () => {
  const registry = createActivityTypeRegistry();
  assert.throws(() => validateActivityTypeConfig(registry, "missing", { schema_version: 1 }), { code: "ACTIVITY_UNKNOWN_TYPE" });
  assert.throws(() => validateActivityAction(registry, "login_reward", "draw"), { code: "ACTIVITY_UNKNOWN_ACTION" });
  assert.throws(() => validateActivityTypeConfig(registry, "lottery", { schema_version: 2 }), { code: "ACTIVITY_SCHEMA_VERSION_UNSUPPORTED" });
});
