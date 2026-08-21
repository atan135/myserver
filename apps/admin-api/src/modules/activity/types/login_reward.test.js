import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { buildLoginRewardView, buildLoginRewardDetailView, buildLoginRewardStageEditor, parseLoginRewardState, validateLoginReward } = await import("./login_reward.ts");
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

test("admin-api login_reward editor sorts stages and exposes server-owned detail", () => {
  const unsorted = { ...config, stages: [{ stage_no: 2, required_count: 2, reward_group_key: "g2" }, config.stages[0]] };
  assert.deepEqual(buildLoginRewardStageEditor(unsorted, ["g1"]).map((stage) => stage.stage_no), [1, 2]);
  const view = buildLoginRewardDetailView(config, { last_period_key: "2026-08-21", consecutive_count: 1, cumulative_count: 1, claimed_stage_ids: [] }, undefined, 7, "UTC", new Date("2026-08-21T12:00:00Z"));
  assert.equal(view.today_status, "logged_in");
  assert.equal(view.stage_views[0].claimable, true);
  const claimed = buildLoginRewardDetailView(config, { last_period_key: "2026-08-21", consecutive_count: 1, cumulative_count: 1, claimed_stage_ids: ["stage_id=1;period_key=2026-08-21;activity_version=7"] }, undefined, 7, "UTC", new Date("2026-08-21T12:00:00Z"));
  assert.equal(claimed.stage_views[0].claimed, true);
  assert.equal(buildLoginRewardDetailView(config, { last_period_key: "2026-08-21", consecutive_count: 1, cumulative_count: 1, claimed_stage_ids: [] }, undefined, 7, "UTC", new Date("2026-08-22T00:00:00Z")).today_status, "not_logged_in");
});

test("admin-api login_reward rejects client-owned progress fields", () => {
  assert.throws(() => validateLoginReward({ ...config, progress: { consecutive_count: 99 } }), { code: "ACTIVITY_INVALID_CONFIG" });
});
