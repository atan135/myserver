import test from "node:test";
import assert from "node:assert/strict";
import { activityError, buildVersionCommand, filterActivities, normalizeActivityListResponse } from "./activity.js";

test("activity list response and filters normalize stable pagination data", () => {
  const result = normalizeActivityListResponse({ data: { items: [{ key: "summer", status: "draft", activityType: "login_reward" }], total: 4, limit: 20, offset: 0 } });
  assert.equal(result.total, 4);
  assert.equal(filterActivities(result.items, { key: "SUM" }).length, 1);
  assert.equal(filterActivities(result.items, { status: "published" }).length, 0);
});

test("time filters use activity windows when present", () => {
  const items = [
    { key: "old", startAt: "2026-01-01T00:00:00Z", endAt: "2026-01-02T00:00:00Z" },
    { key: "new", startAt: "2026-09-01T00:00:00Z", endAt: "2026-09-02T00:00:00Z" }
  ];
  assert.deepEqual(filterActivities(items, { from: "2026-08-01T00:00:00Z" }).map((item) => item.key), ["new"]);
});

test("version commands carry etag and errors expose conflict details", () => {
  assert.deepEqual(buildVersionCommand({ version: 3, etag: 'W/"activity-a-3"' }, "publish"), { version: 3, ifMatch: 'W/"activity-a-3"', reason: "publish" });
  const normalized = activityError({ response: { status: 409, data: { error: "ACTIVITY_VERSION_CONFLICT", message: "stale", details: [{ path: "version" }] } } });
  assert.equal(normalized.code, "ACTIVITY_VERSION_CONFLICT");
  assert.equal(normalized.details[0].path, "version");
});
