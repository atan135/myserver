import assert from "node:assert/strict";
import test from "node:test";
import { appendRewardGroup, appendRewardItem, appendStage } from "./structure-utils.js";

test("generic activity structure utilities add stages, groups and items without type branches", () => {
  const groups = appendRewardGroup([]);
  const stages = appendStage([], groups);
  assert.equal(groups[0].key, "group");
  assert.equal(groups[0].selectionMode, "fixed");
  assert.deepEqual(["fixed", "weighted"].filter((mode) => ["fixed", "weighted"].includes(mode)), ["fixed", "weighted"]);
  assert.equal(stages[0].rewardGroupKey, "group");
  assert.equal(appendRewardItem(groups, 0)[0].items.length, 2);
  assert.deepEqual(appendRewardGroup(groups).map((group) => group.key), ["group", "group-1"]);
});
