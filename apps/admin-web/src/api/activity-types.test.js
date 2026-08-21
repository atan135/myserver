import test from "node:test";
import assert from "node:assert/strict";
import {
  createActivityTypeRegistry,
  validateActivityAction,
  validateActivityTypeConfig
} from "./activity-types.js";

test("admin-web uses the shared activity type schemas", () => {
  const registry = createActivityTypeRegistry();
  assert.equal(validateActivityTypeConfig(registry, "login_reward", { schema_version: 1, event_source: "game_entry", cycle_unit: "natural_day", progression: "consecutive", miss_policy: "reset", claim_mode: "manual", stages: [{ stage_no: 1, required_count: 1, reward_group_key: "g1" }] }).schemaVersion, 1);
  assert.equal(validateActivityAction(registry, "lottery", "draw").type, "lottery");
});

test("admin-web rejects unknown type, action and schema version", () => {
  const registry = createActivityTypeRegistry();
  assert.throws(() => validateActivityTypeConfig(registry, "missing", { schema_version: 1 }), { code: "ACTIVITY_UNKNOWN_TYPE" });
  assert.throws(() => validateActivityAction(registry, "lottery", "claim"), { code: "ACTIVITY_UNKNOWN_ACTION" });
  assert.throws(() => validateActivityTypeConfig(registry, "login_reward", { schema_version: 2 }), { code: "ACTIVITY_SCHEMA_VERSION_UNSUPPORTED" });
});
