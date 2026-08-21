import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";
process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));
const { buildLotteryView, parseLotteryState, serializeLotteryConfig, validateLottery } = await import("./lottery.ts");
const config = { schema_version: 1, draw_source: "player_action", pool_version: 3, free_draw_count: 2, voucher_item_id: 9001, daily_draw_limit: 10, total_draw_limit: 100, pool_items: [{ item_id: 1001, quantity: 1, weight: 3 }, { item_id: 1002, quantity: 2, weight: 7 }] };
test("admin-api lottery validates weighted pool and builds view", () => { const view = buildLotteryView(config, { free_draws_remaining: 1, result_state: "granted" }); assert.equal(validateLottery(config).pool_version, 3); assert.equal(view.pool_total_weight, 10); assert.deepEqual(view.pool_summary, { item_count: 2, total_weight: 10 }); assert.match(view.weight_digest, /^sha256:[0-9a-f]{64}$/); assert.equal(view.state?.result_state, "granted"); assert.equal(parseLotteryState({ free_draws_remaining: 1, result_state: "granted" }).result_state, "granted"); });
test("admin-api lottery rejects invalid weights and client-owned result fields", () => { assert.throws(() => validateLottery({ ...config, pool_items: [{ item_id: 1, quantity: 1, weight: 0 }] }), { code: "ACTIVITY_INVALID_CONFIG" }); assert.throws(() => validateLottery({ ...config, result_item_id: 1 }), { code: "ACTIVITY_INVALID_CONFIG" }); });
test("admin-api lottery rejects missing and unsupported schema versions", () => { const { schema_version: _schemaVersion, ...withoutVersion } = config; assert.throws(() => validateLottery(withoutVersion), { code: "ACTIVITY_SCHEMA_VERSION_UNSUPPORTED" }); assert.throws(() => validateLottery({ ...config, schema_version: 2 }), { code: "ACTIVITY_SCHEMA_VERSION_UNSUPPORTED" }); });
test("admin-api lottery rejects unknown nested fields and unsafe total weights", () => {
  assert.throws(() => validateLottery({ ...config, editor_only: true }), { code: "ACTIVITY_INVALID_CONFIG" });
  assert.throws(() => validateLottery({ ...config, pool_items: [{ ...config.pool_items[0], reward_exists: true }, config.pool_items[1]] }), { code: "ACTIVITY_INVALID_CONFIG" });
  assert.throws(() => validateLottery({ ...config, pity: { enabled: true, unexpected: 1 } }), { code: "ACTIVITY_INVALID_CONFIG" });
  assert.throws(() => validateLottery({ ...config, pool_items: [{ item_id: 1, quantity: 1, weight: Number.MAX_SAFE_INTEGER }, { item_id: 2, quantity: 1, weight: 1 }] }), { code: "ACTIVITY_INVALID_CONFIG" });
  assert.throws(() => validateLottery({ ...config, pool_version: 0x100000000 }), { code: "ACTIVITY_INVALID_CONFIG" });
  assert.throws(() => validateLottery({ ...config, voucher_item_id: 0x80000000 }), { code: "ACTIVITY_INVALID_CONFIG" });
  assert.throws(() => validateLottery({ ...config, pool_items: [{ item_id: 0x80000000, quantity: 1, weight: 1 }] }), { code: "ACTIVITY_INVALID_CONFIG" });
  assert.throws(() => validateLottery({ ...config, pool_items: [{ item_id: 1, quantity: 0x100000000, weight: 1 }] }), { code: "ACTIVITY_INVALID_CONFIG" });
  assert.throws(() => validateLottery({ ...config, pity: { threshold: 0x100000000 } }), { code: "ACTIVITY_INVALID_CONFIG" });
});
test("admin-api lottery serializer strips server-owned view fields", () => {
  const serialized = serializeLotteryConfig({ ...buildLotteryView(config), result_item_id: 1001, random_value: 7, winner: { item_id: 1001 }, pool_items: config.pool_items.map((item) => ({ ...item, reward_exists: true })) });
  assert.deepEqual(serialized, config);
});
