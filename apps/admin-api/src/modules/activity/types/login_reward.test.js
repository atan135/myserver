import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { buildLoginRewardView, parseLoginRewardState, validateLoginReward } = await import("./login_reward.ts");
const config = { schema_version: 1, event_source: "game_entry", cycle_unit: "natural_day", progression: "consecutive", miss_policy: "reset", claim_mode: "manual", stages: [{ stage_no: 1, required_count: 1, reward_group_key: "g1" }] };

test("admin-api login_reward type validates config and builds contract view", () => {
  assert.equal(validateLoginReward(config).claim_mode, "manual");
  assert.equal(buildLoginRewardView(config).contract_only, true);
  assert.deepEqual(parseLoginRewardState({ cumulative_count: 3, claimed_stage_ids: ["s1", 4] }).claimed_stage_ids, ["s1"]);
});

test("admin-api login_reward type rejects unsupported source and duplicate stages", () => {
  assert.throws(() => validateLoginReward({ ...config, event_source: "client" }), { code: "ACTIVITY_INVALID_CONFIG" });
  assert.throws(() => validateLoginReward({ ...config, stages: [{ ...config.stages[0] }, { ...config.stages[0] }] }), { code: "ACTIVITY_INVALID_CONFIG" });
});
