import assert from "node:assert/strict";
import test from "node:test";
import { buildLoginRewardView, parseLoginRewardState, validateLoginReward } from "./login_reward.ts";

const config = { schema_version: 1, event_source: "game_entry", cycle_unit: "natural_day", progression: "consecutive", miss_policy: "reset", claim_mode: "manual", stages: [{ stage_no: 1, required_count: 1, reward_group_key: "g1" }] };

test("admin-web login reward type validates and builds a contract view", () => {
  assert.equal(validateLoginReward(config).event_source, "game_entry");
  assert.equal(buildLoginRewardView(config).contract_only, true);
  assert.deepEqual(parseLoginRewardState({ consecutive_count: 2, claimed_stage_ids: ["stage-1", 4] }).claimed_stage_ids, ["stage-1"]);
});
