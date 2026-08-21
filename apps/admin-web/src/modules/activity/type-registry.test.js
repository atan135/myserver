import assert from "node:assert/strict";
import test from "node:test";
import { buildActivityDraftTemplate, listActivityTypeDefinitions, resolveActivityTypeDefinition } from "./type-registry.js";

test("activity registry dynamically maps both supported type editors and schemas", () => {
  const definitions = listActivityTypeDefinitions();
  assert.deepEqual(definitions.map((item) => item.type).sort(), ["login_reward", "lottery"]);
  assert.equal(typeof resolveActivityTypeDefinition("login_reward").editor, "function");
  assert.equal(typeof resolveActivityTypeDefinition("lottery").module, "function");
  assert.equal(buildActivityDraftTemplate("login_reward", "summer").typeConfig.stages[0].reward_group_key, "default");
  assert.equal(buildActivityDraftTemplate("lottery", "draw").typeConfig.pool_items[0].item_id, 1001);
  assert.equal(resolveActivityTypeDefinition("missing"), null);
});
