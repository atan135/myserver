import assert from "node:assert/strict";
import test from "node:test";
import { resolveActiveMenu } from "./active-menu.js";

test("activity detail paths keep the activity menu selected", () => {
  assert.equal(resolveActiveMenu("/activities"), "/activities");
  assert.equal(resolveActiveMenu("/activities/activity-1"), "/activities");
  assert.equal(resolveActiveMenu("/activitiesfoo"), "/activitiesfoo");
});
