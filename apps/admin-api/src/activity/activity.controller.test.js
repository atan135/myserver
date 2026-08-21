import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { ActivityController } = await import("./activity.controller.ts");

function service() {
  return {
    async list(query) { return { items: [], total: 0, ...query }; },
    async detail(activityId) { return { activityId }; },
    async createDraft(command) { return command; },
    async createDraftFromPublished(activityId, command) { return { activityId, ...command }; },
    async updateDraft(activityId, command) { return { activityId, ...command }; },
    async preflight(activityId, command) { return { activityId, ...command, ok: true }; },
    async publish(activityId, command) { return { activityId, ...command, ok: true }; },
    async offline(activityId, command) { return { activityId, ...command, ok: true }; },
    async records(activityId, query) { return { activityId, ...query }; }
  };
}

test("activity controller exposes paginated list and strict draft contract", async () => {
  const controller = new ActivityController(service());
  const list = await controller.list({ limit: "20", offset: "5", status: "published" });
  assert.equal(list.limit, 20);
  assert.equal(list.offset, 5);
  await assert.rejects(
    () => controller.list({ unknown: "field" }),
    (error) => error.getResponse().error === "ACTIVITY_UNKNOWN_FIELD"
  );

  const draft = await controller.createDraft({
    key: "summer",
    activityType: "login_reward",
    schemaVersion: 1,
    startAt: "2026-01-01T00:00:00Z",
    endAt: "2026-01-02T00:00:00Z",
    claimDeadline: "2026-01-03T00:00:00Z",
    timezone: "UTC",
    publicConfig: {},
    typeConfig: { schema_version: 1 },
    stages: [],
    rewardGroups: [],
    reason: "test"
  });
  assert.equal(draft.activityType, "login_reward");

  const typedDraft = await controller.createDraft({
    key: "typed", activityType: "login_reward", schemaVersion: 1,
    startAt: "2026-01-01T00:00:00Z", endAt: "2026-01-02T00:00:00Z", claimDeadline: "2026-01-03T00:00:00Z",
    timezone: "UTC", publicConfig: {}, typeConfig: { schema_version: 1, cadence: "daily" }, stages: [], rewardGroups: [], reason: "typed"
  });
  assert.equal(typedDraft.typeConfig.cadence, "daily");

  await assert.rejects(
    () => controller.createDraft({
      key: "summer", activityType: "login_reward", schemaVersion: 1,
      startAt: "2026-01-01T00:00:00Z", endAt: "2026-01-02T00:00:00Z", claimDeadline: "2026-01-03T00:00:00Z",
      timezone: "UTC", publicConfig: {}, typeConfig: { schema_version: 1 }, stages: [], rewardGroups: [], reason: "test", ifMatch: "1"
    }),
    (error) => error.getResponse().error === "ACTIVITY_UNKNOWN_FIELD"
  );
  const updated = await controller.updateDraft("activity-1", {
    key: "summer", activityType: "login_reward", schemaVersion: 1,
    startAt: "2026-01-01T00:00:00Z", endAt: "2026-01-02T00:00:00Z", claimDeadline: "2026-01-03T00:00:00Z",
    timezone: "UTC", publicConfig: {}, typeConfig: { schema_version: 1 }, stages: [], rewardGroups: [], reason: "edit", ifMatch: "W/\"activity-1-1\""
  });
  assert.equal(updated.ifMatch, "W/\"activity-1-1\"");

  const forked = await controller.createDraftFromPublished("activity-1", {
    sourceVersion: "1", ifMatch: "W/\"activity-1-1\"", reason: "new season", overrides: { publicConfig: { title: "next" } }
  });
  assert.equal(forked.activityId, "activity-1");
  assert.equal(forked.sourceVersion, 1);
  await assert.rejects(
    () => controller.createDraftFromPublished("activity-1", { sourceVersion: 1, reason: "bad", overrides: { unknown: true }, extra: true }),
    (error) => error.getResponse().error === "ACTIVITY_UNKNOWN_FIELD"
  );
});

test("activity controller rejects schema version, deep and oversized JSON", async () => {
  const controller = new ActivityController(service());
  const base = {
    key: "summer", activityType: "login_reward", schemaVersion: 99,
    startAt: "a", endAt: "b", claimDeadline: "c", timezone: "UTC",
    publicConfig: {}, typeConfig: { schema_version: 99 }, stages: [], rewardGroups: [], reason: "test"
  };
  await assert.rejects(() => controller.createDraft(base), (error) => error.getResponse().error === "ACTIVITY_SCHEMA_VERSION_UNSUPPORTED");
  await assert.rejects(() => controller.createDraft({ ...base, schemaVersion: 1, typeConfig: { schema_version: 1 }, unknown: true }), (error) => error.getResponse().error === "ACTIVITY_UNKNOWN_FIELD");
  await assert.rejects(() => controller.createDraft({ ...base, schemaVersion: 1, typeConfig: { schema_version: 1 }, stages: [{ stageId: "s1", stageNo: 1, rewardGroupKey: "g1", qualification: {}, extra: true }] }), (error) => error.getResponse().error === "ACTIVITY_UNKNOWN_FIELD");
  await assert.rejects(() => controller.createDraft({ ...base, schemaVersion: 1, typeConfig: { schema_version: 1 }, publicConfig: { blob: "x".repeat(70_000) } }), (error) => error.getResponse().error === "ACTIVITY_JSON_TOO_LARGE");
});
