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

test("admin-web shared lottery contract rejects unknown and out-of-range fields", () => {
  const registry = createActivityTypeRegistry();
  const config = { schema_version: 1, draw_source: "player_action", pool_version: 1, free_draw_count: 0, daily_draw_limit: 1, total_draw_limit: 1, pool_items: [{ item_id: 1, quantity: 1, weight: 1 }] };
  assert.throws(() => validateActivityTypeConfig(registry, "lottery", { ...config, result_item_id: 1 }), { code: "ACTIVITY_INVALID_CONFIG" });
  assert.throws(() => validateActivityTypeConfig(registry, "lottery", { ...config, pool_version: 0x100000000 }), { code: "ACTIVITY_INVALID_CONFIG" });
  assert.throws(() => validateActivityTypeConfig(registry, "lottery", { ...config, pool_items: [{ item_id: 1, quantity: 1, weight: 1, reward_exists: true }] }), { code: "ACTIVITY_INVALID_CONFIG" });
});
