import assert from "node:assert/strict";
import test from "node:test";
import { buildLotteryPoolEditor, buildLotteryView, parseLotteryState, serializeLotteryConfig, validateLottery } from "./lottery.ts";
const config = { schema_version: 1, draw_source: "player_action", pool_version: 3, free_draw_count: 2, voucher_item_id: 9001, daily_draw_limit: 10, total_draw_limit: 100, pool_items: [{ item_id: 1001, quantity: 1, weight: 3 }, { item_id: 1002, quantity: 2, weight: 7 }] };
test("admin-web lottery validates weighted pool and builds view", () => { const view = buildLotteryView(config, { total_draw_count: 3 }); assert.equal(validateLottery(config).draw_source, "player_action"); assert.equal(view.pool_total_weight, 10); assert.deepEqual(view.pool_summary, { item_count: 2, total_weight: 10 }); assert.equal(view.state?.total_draw_count, 3); assert.equal(parseLotteryState({ total_draw_count: 3 }).total_draw_count, 3); });
test("admin-web lottery rejects invalid weights and client-owned result fields", () => { assert.throws(() => validateLottery({ ...config, pool_items: [{ item_id: 1, quantity: 1, weight: -1 }] }), { code: "ACTIVITY_INVALID_CONFIG" }); assert.throws(() => validateLottery({ ...config, random_value: 4 }), { code: "ACTIVITY_INVALID_CONFIG" }); });
test("admin-web lottery rejects missing and unsupported schema versions", () => { const { schema_version: _schemaVersion, ...withoutVersion } = config; assert.throws(() => validateLottery(withoutVersion), { code: "ACTIVITY_SCHEMA_VERSION_UNSUPPORTED" }); assert.throws(() => validateLottery({ ...config, schema_version: 2 }), { code: "ACTIVITY_SCHEMA_VERSION_UNSUPPORTED" }); });
test("admin-web lottery rejects unknown nested fields and marks missing catalog rewards", () => {
  assert.throws(() => validateLottery({ ...config, editor_only: true }), { code: "ACTIVITY_INVALID_CONFIG" });
  assert.throws(() => validateLottery({ ...config, pool_items: [{ ...config.pool_items[0], reward_exists: true }, config.pool_items[1]] }), { code: "ACTIVITY_INVALID_CONFIG" });
  assert.throws(() => validateLottery({ ...config, limited_stock: { enabled: true, unexpected: 1 } }), { code: "ACTIVITY_INVALID_CONFIG" });
  assert.throws(() => validateLottery({ ...config, pool_version: 0x100000000 }), { code: "ACTIVITY_INVALID_CONFIG" });
  assert.throws(() => validateLottery({ ...config, voucher_item_id: 0x80000000 }), { code: "ACTIVITY_INVALID_CONFIG" });
  assert.throws(() => validateLottery({ ...config, pool_items: [{ item_id: 0x80000000, quantity: 1, weight: 1 }] }), { code: "ACTIVITY_INVALID_CONFIG" });
  assert.throws(() => validateLottery({ ...config, pool_items: [{ item_id: 1, quantity: 0x100000000, weight: 1 }] }), { code: "ACTIVITY_INVALID_CONFIG" });
  assert.throws(() => validateLottery({ ...config, limited_stock: { stock: 0x100000000 } }), { code: "ACTIVITY_INVALID_CONFIG" });
  assert.deepEqual(buildLotteryPoolEditor(config, [1001]).map((item) => item.reward_exists), [true, false]);
});
test("admin-web lottery serializer strips server-owned view fields", () => {
  const serialized = serializeLotteryConfig({ ...buildLotteryView(config), result_item_id: 1001, random_value: 7, winner_item_id: 1001, pool_items: config.pool_items.map((item) => ({ ...item, reward_exists: true })) });
  assert.deepEqual(serialized, config);
});
