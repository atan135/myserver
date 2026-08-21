import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const {
  ActivityControlDomainService,
  ActivityControlError,
  InMemoryActivityControlRepository,
  NoopActivityRefreshNotifier
} = await import("./activity-control.service.ts");

function draft(overrides = {}) {
  return {
    key: "summer",
    activityType: "login_reward",
    schemaVersion: 1,
    startAt: "2026-09-01T00:00:00Z",
    endAt: "2026-09-02T00:00:00Z",
    claimDeadline: "2026-09-03T00:00:00Z",
    timezone: "UTC",
    publicConfig: {},
    typeConfig: { schema_version: 1 },
    stages: [{ stageId: "day-1", stageNo: 1, rewardGroupKey: "g1", qualification: {} }],
    rewardGroups: [{ key: "g1", selectionMode: "fixed", items: [{ quantity: 1 }] }],
    reason: "test",
    ...overrides
  };
}

test("domain service returns field-level preflight errors and refuses invalid drafts", async () => {
  const service = new ActivityControlDomainService();
  await assert.rejects(
    () => service.createDraft(draft({ endAt: "2026-08-31T00:00:00Z", timezone: "Mars/Base" })),
    (error) => error instanceof ActivityControlError && error.code === "ACTIVITY_INVALID_CONFIG" && error.details.some((item) => item.path === "endAt") && error.details.some((item) => item.path === "timezone")
  );
});

test("publish snapshots are immutable and stale CAS/repeated operations are rejected", async () => {
  const repository = new InMemoryActivityControlRepository();
  const service = new ActivityControlDomainService(repository);
  const created = await service.createDraft(draft({ activityId: "activity-1" }));
  const published = await service.publish("activity-1", { version: created.version, ifMatch: created.etag, reason: "publish" });
  assert.equal(published.status, "published");
  await assert.rejects(
    () => service.updateDraft("activity-1", draft({ ifMatch: published.etag })),
    (error) => error.code === "ACTIVITY_PUBLISHED_IMMUTABLE" && /new draft version/.test(error.message)
  );
  published.snapshot.publicConfig.changed = true;
  const detail = await service.detail("activity-1");
  assert.equal(detail.draft.publicConfig.changed, undefined);
  await assert.rejects(() => service.publish("activity-1", { version: 1, reason: "duplicate" }), (error) => error.code === "ACTIVITY_ALREADY_PUBLISHED");
  await assert.rejects(() => service.offline("activity-1", { version: 1, ifMatch: "1", reason: "stale" }), (error) => error.code === "ACTIVITY_VERSION_CONFLICT");
  const offline = await service.offline("activity-1", { version: 1, ifMatch: published.etag, reason: "emergency" });
  assert.equal(offline.status, "offline");
  await assert.rejects(() => service.offline("activity-1", { version: 1, reason: "repeat" }), (error) => error.code === "ACTIVITY_ALREADY_OFFLINE");
});

test("notification failure does not roll back the published version", async () => {
  const repository = new InMemoryActivityControlRepository();
  const notifier = { async notify() { throw new Error("redis unavailable"); } };
  const service = new ActivityControlDomainService(repository, notifier);
  await service.createDraft(draft({ activityId: "activity-2" }));
  const result = await service.publish("activity-2", { version: 1, reason: "publish" });
  assert.equal(result.status, "published");
  assert.equal(result.notification.status, "failed");
  assert.equal((await service.detail("activity-2")).status, "published");
});

test("published snapshot can only be forked into a new draft with source CAS", async () => {
  const repository = new InMemoryActivityControlRepository();
  const service = new ActivityControlDomainService(repository);
  await service.createDraft(draft({ activityId: "activity-3" }));
  const published = await service.publish("activity-3", { version: 1, reason: "publish" });
  const forked = await service.createDraftFromPublished("activity-3", {
    sourceVersion: 1, ifMatch: published.etag, reason: "new version", overrides: { publicConfig: { title: "next" } }
  });
  assert.equal(forked.status, "draft");
  assert.equal(forked.version, 2);
  assert.equal(forked.draft.publicConfig.title, "next");
  await assert.rejects(
    () => service.createDraftFromPublished("activity-3", { sourceVersion: 1, ifMatch: published.etag, reason: "replay", overrides: {} }),
    (error) => error.code === "ACTIVITY_INVALID_STATE"
  );
});

test("records are append-only read views with activity/version/character/status/time/request filters", async () => {
  const repository = new InMemoryActivityControlRepository();
  const service = new ActivityControlDomainService(repository);
  await service.createDraft(draft({ activityId: "activity-records" }));
  repository.appendRecordForTest({ recordId: "claim-1", activityId: "activity-records", version: 1, recordType: "claim", characterId: "character-1", requestId: "req-1", status: "granted", createdAt: "2026-09-01T01:00:00.000Z", details: { reward: "coin" } });
  repository.appendRecordForTest({ recordId: "draw-1", activityId: "activity-records", version: 2, recordType: "draw", characterId: "character-2", requestId: "req-2", status: "permanent_failure", createdAt: "2026-09-02T01:00:00.000Z", details: { error: "pool_empty" } });
  const result = await service.records("activity-records", { version: 1, characterId: "character-1", status: "granted", from: "2026-09-01T00:00:00Z", to: "2026-09-02T00:00:00Z", requestId: "req-1", limit: 10, offset: 0 });
  assert.equal(result.total, 1);
  assert.equal(result.items[0].recordType, "claim");
  result.items[0].details.reward = "mutated";
  const again = await service.records("activity-records", { requestId: "req-1" });
  assert.equal(again.items[0].details.reward, "coin");
});

test("audit sink receives bounded success/failure summaries without configuration payloads", async () => {
  const repository = new InMemoryActivityControlRepository();
  const events = [];
  const service = new ActivityControlDomainService(repository, new NoopActivityRefreshNotifier(), { async write(event) { events.push(event); } });
  await service.createDraft(draft({ activityId: "activity-audit", publicConfig: { secret: "do-not-audit" }, actorId: "admin-7" }));
  await service.records("activity-audit", { limit: 10, offset: 0 });
  await assert.rejects(() => service.records("missing-activity", { actorId: "admin-7" }));
  await assert.rejects(() => service.updateDraft("activity-audit", { ...draft({ activityId: "activity-audit", ifMatch: "stale" }), actorId: "admin-7" }));
  assert.equal(events[0].action, "draft_created");
  assert.equal(events[0].actorId, "admin-7");
  assert.equal(events[0].result, "success");
  assert.equal(events.some((event) => event.action === "records_read"), true);
  assert.equal(events.some((event) => event.action === "records_read" && event.result === "failure" && event.actorId === "admin-7"), true);
  assert.equal(events.some((event) => event.result === "failure"), true);
  assert.equal(JSON.stringify(events).includes("do-not-audit"), false);

  await service.publish("activity-audit", { version: 1, reason: "publish", actorId: "admin-7" });
  await assert.rejects(() => service.publish("activity-audit", { version: 1, reason: "stale", actorId: "admin-7" }));
  assert.equal(events.some((event) => event.action === "published" && event.result === "failure" && event.actorId === "admin-7"), true);
});

test("audit sink failure is explicit and never makes configuration payload part of the response", async () => {
  const service = new ActivityControlDomainService(new InMemoryActivityControlRepository(), new NoopActivityRefreshNotifier(), { async write() { throw new Error("audit unavailable"); } });
  const result = await service.createDraft(draft({ activityId: "activity-audit-failure", publicConfig: { sensitive: "secret" } }));
  assert.equal(result.audit.status, "failed");
  assert.equal(JSON.stringify(result).includes("secret"), true);
  assert.equal(JSON.stringify(result.audit).includes("secret"), false);
});
