import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { ActivityController } = await import("./activity.controller.ts");
const { ActivityControlError } = await import("./activity-control.service.ts");

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
    typeConfig: { schema_version: 1, event_source: "game_entry", cycle_unit: "natural_day", progression: "consecutive", miss_policy: "reset", claim_mode: "manual", stages: [{ stage_no: 1, required_count: 1, reward_group_key: "g1" }] },
    stages: [],
    rewardGroups: [],
    reason: "test"
  });
  assert.equal(draft.activityType, "login_reward");
  const actorDraft = await controller.createDraft({
    key: "actor", activityType: "login_reward", schemaVersion: 1,
    startAt: "2026-01-01T00:00:00Z", endAt: "2026-01-02T00:00:00Z", claimDeadline: "2026-01-03T00:00:00Z",
    timezone: "UTC", publicConfig: {}, typeConfig: { schema_version: 1, event_source: "game_entry", cycle_unit: "natural_day", progression: "consecutive", miss_policy: "reset", claim_mode: "manual", stages: [{ stage_no: 1, required_count: 1, reward_group_key: "g1" }] }, stages: [], rewardGroups: [], reason: "actor"
  }, { admin: { sub: 77 } });
  assert.equal(actorDraft.actorId, "77");

  const typedDraft = await controller.createDraft({
    key: "typed", activityType: "login_reward", schemaVersion: 1,
    startAt: "2026-01-01T00:00:00Z", endAt: "2026-01-02T00:00:00Z", claimDeadline: "2026-01-03T00:00:00Z",
    timezone: "UTC", publicConfig: {}, typeConfig: { schema_version: 1, event_source: "game_entry", cycle_unit: "natural_day", progression: "consecutive", miss_policy: "reset", claim_mode: "manual", stages: [{ stage_no: 1, required_count: 1, reward_group_key: "g1" }], cadence: "daily" }, stages: [], rewardGroups: [], reason: "typed"
  });
  assert.equal(typedDraft.typeConfig.cadence, "daily");

  await assert.rejects(
    () => controller.createDraft({
      key: "summer", activityType: "login_reward", schemaVersion: 1,
      startAt: "2026-01-01T00:00:00Z", endAt: "2026-01-02T00:00:00Z", claimDeadline: "2026-01-03T00:00:00Z",
      timezone: "UTC", publicConfig: {}, typeConfig: { schema_version: 1, event_source: "game_entry", cycle_unit: "natural_day", progression: "consecutive", miss_policy: "reset", claim_mode: "manual", stages: [{ stage_no: 1, required_count: 1, reward_group_key: "g1" }] }, stages: [], rewardGroups: [], reason: "test", ifMatch: "1"
    }),
    (error) => error.getResponse().error === "ACTIVITY_UNKNOWN_FIELD"
  );
  const updated = await controller.updateDraft("activity-1", {
    key: "summer", activityType: "login_reward", schemaVersion: 1,
    startAt: "2026-01-01T00:00:00Z", endAt: "2026-01-02T00:00:00Z", claimDeadline: "2026-01-03T00:00:00Z",
    timezone: "UTC", publicConfig: {}, typeConfig: { schema_version: 1, event_source: "game_entry", cycle_unit: "natural_day", progression: "consecutive", miss_policy: "reset", claim_mode: "manual", stages: [{ stage_no: 1, required_count: 1, reward_group_key: "g1" }] }, stages: [], rewardGroups: [], reason: "edit", ifMatch: "W/\"activity-1-1\""
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
  const records = await controller.records("activity-1", {
    version: "1", characterId: "character-1", status: "granted",
    from: "2026-01-01T00:00:00Z", to: "2026-01-02T00:00:00Z", requestId: "req-1", limit: "5", offset: "0"
  });
  assert.equal(records.version, 1);
  assert.equal(records.characterId, "character-1");
  await assert.rejects(() => controller.records("activity-1", { version: "0" }), (error) => error.getResponse().error === "ACTIVITY_INVALID_QUERY");
  await assert.rejects(() => controller.records("activity-1", { from: "not-a-time" }), (error) => error.getResponse().error === "ACTIVITY_INVALID_QUERY");
  await assert.rejects(() => controller.records("activity-1", { from: "2026-01-03T00:00:00Z", to: "2026-01-02T00:00:00Z" }), (error) => error.getResponse().error === "ACTIVITY_INVALID_QUERY");
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

test("activity controller maps domain conflicts, preflight failures and missing resources to stable HTTP statuses", async () => {
  const controller = new ActivityController({
    async list() { return {}; },
    async detail() { throw new ActivityControlError("ACTIVITY_NOT_FOUND", "missing"); },
    async createDraft() { return {}; }, async createDraftFromPublished() { return {}; }, async updateDraft() { return {}; },
    async preflight() { throw new ActivityControlError("ACTIVITY_PRECHECK_FAILED", "invalid", [{ path: "typeConfig", code: "INVALID" }]); },
    async publish() { throw new ActivityControlError("ACTIVITY_VERSION_CONFLICT", "stale"); },
    async offline() { throw new ActivityControlError("ACTIVITY_ALREADY_OFFLINE", "repeat"); }, async records() { return {}; }
  });
  await assert.rejects(() => controller.detail("missing"), (error) => error.getStatus() === 404 && error.getResponse().error === "ACTIVITY_NOT_FOUND");
  await assert.rejects(() => controller.preflight("activity-1", { version: 1, reason: "check" }), (error) => error.getStatus() === 422 && error.getResponse().error === "ACTIVITY_PRECHECK_FAILED");
  await assert.rejects(() => controller.publish("activity-1", { version: 1, reason: "publish" }), (error) => error.getStatus() === 409 && error.getResponse().error === "ACTIVITY_VERSION_CONFLICT");
  await assert.rejects(() => controller.offline("activity-1", { version: 1, reason: "repeat" }), (error) => error.getStatus() === 409 && error.getResponse().error === "ACTIVITY_ALREADY_OFFLINE");
});
