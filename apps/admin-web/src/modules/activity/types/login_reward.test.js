import assert from "node:assert/strict";
import test from "node:test";
import { buildLoginRewardView, buildLoginRewardDetailView, buildLoginRewardStageEditor, parseLoginRewardState, validateLoginReward } from "./login_reward.ts";

const config = { schema_version: 1, event_source: "game_entry", cycle_unit: "natural_day", progression: "consecutive", miss_policy: "reset", claim_mode: "manual", stages: [{ stage_no: 1, required_count: 1, reward_group_key: "g1" }] };

test("admin-web login reward type validates and builds a contract view", () => {
  assert.equal(validateLoginReward(config).event_source, "game_entry");
  assert.equal(buildLoginRewardView(config).contract_only, true);
  assert.deepEqual(parseLoginRewardState({ consecutive_count: 2, claimed_stage_ids: ["stage-1", 4] }).claimed_stage_ids, ["stage-1"]);
});

test("admin-web login reward editor and detail are server-state driven", () => {
  const unsorted = { ...config, stages: [{ stage_no: 2, required_count: 2, reward_group_key: "g2" }, config.stages[0]] };
  assert.deepEqual(buildLoginRewardStageEditor(unsorted, ["g1"]).map((stage) => stage.stage_no), [1, 2]);
  const view = buildLoginRewardDetailView(config, { last_period_key: "2026-08-21", consecutive_count: 1, cumulative_count: 1, claimed_stage_ids: [] }, undefined, 7, "UTC", new Date("2026-08-21T12:00:00Z"));
  assert.equal(view.today_status, "logged_in");
  assert.equal(view.stage_views[0].claimable, true);
  const claimed = buildLoginRewardDetailView(config, { last_period_key: "2026-08-21", consecutive_count: 1, cumulative_count: 1, claimed_stage_ids: ["stage_id=1;period_key=2026-08-21;activity_version=7"] }, undefined, 7, "UTC", new Date("2026-08-21T12:00:00Z"));
  assert.equal(claimed.stage_views[0].claimed, true);
  assert.equal(buildLoginRewardDetailView(config, { last_period_key: "2026-08-21", consecutive_count: 1, cumulative_count: 1, claimed_stage_ids: [] }, undefined, 7, "UTC", new Date("2026-08-22T00:00:00Z")).today_status, "not_logged_in");
});
