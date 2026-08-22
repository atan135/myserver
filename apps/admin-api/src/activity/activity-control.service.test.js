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
    typeConfig: { schema_version: 1, event_source: "game_entry", cycle_unit: "natural_day", progression: "consecutive", miss_policy: "reset", claim_mode: "manual", stages: [{ stage_no: 1, required_count: 1, reward_group_key: "g1" }] },
    stages: [{ stageId: "day-1", stageNo: 1, rewardGroupKey: "g1", qualification: {} }],
    rewardGroups: [{ key: "g1", selectionMode: "fixed", items: [{ item_id: 1001, quantity: 1 }] }],
    reason: "test",
    ...overrides
  };
}

function lotteryDraft(overrides = {}) {
  return draft({
    activityType: "lottery",
    typeConfig: {
      schema_version: 1,
      draw_source: "player_action",
      pool_version: 3,
      free_draw_count: 2,
      voucher_item_id: 9001,
      daily_draw_limit: 10,
      total_draw_limit: 100,
      pool_items: [{ item_id: 1001, quantity: 1, weight: 3 }, { item_id: 1002, quantity: 2, weight: 7 }]
    },
    stages: [],
    rewardGroups: [{ key: "pool", selectionMode: "fixed", items: [{ item_id: 1001, quantity: 1 }, { item_id: 1002, quantity: 2 }] }],
    ...overrides
  });
}

test("domain service returns field-level preflight errors and refuses invalid drafts", async () => {
  const service = new ActivityControlDomainService();
  await assert.rejects(
    () => service.createDraft(draft({ endAt: "2026-08-31T00:00:00Z", timezone: "Mars/Base" })),
    (error) => error instanceof ActivityControlError && error.code === "ACTIVITY_INVALID_CONFIG" && error.details.some((item) => item.path === "endAt") && error.details.some((item) => item.path === "timezone")
  );
});

test("reward catalog rejects unsafe or runtime-incompatible item definitions", async () => {
  const cases = [
    { item: { quantity: 1 }, path: ".item_id" },
    { item: { item_id: -1, quantity: 1 }, path: ".item_id" },
    { item: { item_id: 0x80000000, quantity: 1 }, path: ".item_id" },
    { item: { item_id: Number.MAX_SAFE_INTEGER + 1, quantity: 1 }, path: ".item_id" },
    { item: { item_id: 1001, quantity: -1 }, path: ".quantity" },
    { item: { item_id: 1001, quantity: 0x100000000 }, path: ".quantity" },
    { item: { item_id: 1001, quantity: Number.MAX_SAFE_INTEGER + 1 }, path: ".quantity" },
    { item: { item_id: 1001, quantity: 1, binding: "account_bound" }, path: ".binding" },
    { item: { item_id: 1001, quantity: 1, result_item_id: 1001 }, path: ".result_item_id" }
  ];
  for (const [index, invalid] of cases.entries()) {
    const service = new ActivityControlDomainService();
    await assert.rejects(
      () => service.createDraft(draft({
        activityId: `invalid-reward-${index}`,
        key: `invalid-reward-${index}`,
        rewardGroups: [{ key: "g1", selectionMode: "fixed", items: [invalid.item] }]
      })),
      (error) => error instanceof ActivityControlError
        && error.code === "ACTIVITY_INVALID_CONFIG"
        && error.details.some((item) => item.path === `rewardGroups[0].items[0]${invalid.path}`)
    );
  }
});

test("weighted reward catalog requires safe positive weights and safe totals", async () => {
  const cases = [
    { items: [{ item_id: 1001, quantity: 1 }], path: "rewardGroups[0].items[0].weight" },
    { items: [{ item_id: 1001, quantity: 1, weight: 0 }], path: "rewardGroups[0].items[0].weight" },
    { items: [{ item_id: 1001, quantity: 1, weight: Number.MAX_SAFE_INTEGER + 1 }], path: "rewardGroups[0].items[0].weight" },
    { items: [{ item_id: 1001, quantity: 1, weight: Number.MAX_SAFE_INTEGER }, { item_id: 1002, quantity: 1, weight: 1 }], path: "rewardGroups[0].items" }
  ];
  for (const [index, invalid] of cases.entries()) {
    const service = new ActivityControlDomainService();
    await assert.rejects(
      () => service.createDraft(draft({
        activityId: `invalid-weight-${index}`,
        key: `invalid-weight-${index}`,
        rewardGroups: [{ key: "g1", selectionMode: "weighted", items: invalid.items }]
      })),
      (error) => error instanceof ActivityControlError
        && error.code === "ACTIVITY_INVALID_CONFIG"
        && error.details.some((item) => item.path === invalid.path)
    );
  }

  const service = new ActivityControlDomainService();
  const created = await service.createDraft(draft({
    activityId: "valid-character-bound-reward",
    key: "valid-character-bound-reward",
    rewardGroups: [{ key: "g1", selectionMode: "fixed", items: [{ item_id: 1001, quantity: 1, binding: "character_bound" }] }]
  }));
  assert.equal(created.status, "draft");
});

test("draft and published snapshot keep schema version and config digest consistent", async () => {
  const notifications = [];
  const service = new ActivityControlDomainService(
    new InMemoryActivityControlRepository(),
    { async notify(event) { notifications.push(event); } }
  );
  await assert.rejects(
    () => service.createDraft(draft({ typeConfig: { ...draft().typeConfig, schema_version: 2 } })),
    (error) => error.code === "ACTIVITY_INVALID_CONFIG"
      && error.details.some((item) => item.path === "typeConfig.schema_version" && item.code === "SCHEMA_VERSION_MISMATCH")
      && error.details.some((item) => item.code === "ACTIVITY_SCHEMA_VERSION_UNSUPPORTED")
  );

  const created = await service.createDraft(draft({ activityId: "activity-version-digest" }));
  const published = await service.publish("activity-version-digest", { version: created.version, reason: "publish" });
  const detail = await service.detail("activity-version-digest");
  assert.match(created.configDigest, /^sha256:[0-9a-f]{64}$/);
  assert.equal(published.configDigest, created.configDigest);
  assert.equal(detail.configDigest, published.configDigest);
  assert.equal(published.snapshot.schemaVersion, published.snapshot.typeConfig.schema_version);
  assert.equal(notifications[0].digest, published.configDigest);

  const reorderedTypeConfig = Object.fromEntries(Object.entries(draft().typeConfig).reverse());
  const reordered = await service.createDraft(draft({
    activityId: "activity-version-digest-reordered",
    key: "summer-reordered",
    typeConfig: reorderedTypeConfig
  }));
  assert.equal(reordered.configDigest, created.configDigest);
});

test("publish preflight rejects unknown types, schema versions, stages and lottery pools", async () => {
  const cases = [
    draft({ activityId: "publish-unknown-type", activityType: "unknown" }),
    draft({ activityId: "publish-unknown-version", schemaVersion: 2, typeConfig: { ...draft().typeConfig, schema_version: 2 } }),
    draft({
      activityId: "publish-invalid-stage",
      stages: [
        { stageId: "same", stageNo: 1, rewardGroupKey: "g1", qualification: {} },
        { stageId: "same", stageNo: 1, rewardGroupKey: "g1", qualification: {} }
      ]
    }),
    lotteryDraft({ activityId: "publish-invalid-pool", typeConfig: { ...lotteryDraft().typeConfig, pool_items: [] } })
  ];
  for (const invalidDraft of cases) {
    const repository = new InMemoryActivityControlRepository();
    await repository.createDraft(invalidDraft);
    const service = new ActivityControlDomainService(repository);
    await assert.rejects(
      () => service.publish(invalidDraft.activityId, { version: 1, reason: "must fail" }),
      (error) => error.code === "ACTIVITY_PRECHECK_FAILED" && error.details.length > 0
    );
  }
});

test("lottery preflight validates pool quantities and reward catalog references", async () => {
  const service = new ActivityControlDomainService();
  await assert.rejects(
    () => service.createDraft(lotteryDraft({ typeConfig: { ...lotteryDraft().typeConfig, pool_items: [{ item_id: 9999, quantity: 0, weight: 1 }] } })),
    (error) => error instanceof ActivityControlError
      && error.details.some((item) => item.path === "typeConfig.pool_items[0].quantity" && item.code === "INVALID")
      && error.details.some((item) => item.path === "typeConfig.pool_items[0].item_id" && item.code === "UNKNOWN_REFERENCE")
  );
  await assert.rejects(
    () => service.createDraft(lotteryDraft({ typeConfig: { ...lotteryDraft().typeConfig, pool_version: 0x100000000, pity: { enabled: true, unexpected: 1 } } })),
    (error) => error instanceof ActivityControlError
      && error.details.some((item) => item.path === "typeConfig" && item.code === "ACTIVITY_INVALID_CONFIG")
  );
});

test("lottery publish rejects changing weights without a new pool version", async () => {
  const repository = new InMemoryActivityControlRepository();
  const service = new ActivityControlDomainService(repository);
  await service.createDraft(lotteryDraft({ activityId: "lottery-freeze" }));
  const published = await service.publish("lottery-freeze", { version: 1, reason: "publish" });
  await service.createDraftFromPublished("lottery-freeze", {
    sourceVersion: 1,
    ifMatch: published.etag,
    reason: "change pool",
    overrides: { typeConfig: { ...lotteryDraft().typeConfig, pool_items: [{ item_id: 1001, quantity: 1, weight: 4 }, { item_id: 1002, quantity: 2, weight: 6 }] } }
  });
  await assert.rejects(
    () => service.publish("lottery-freeze", { version: 2, reason: "publish changed pool" }),
    (error) => error.code === "ACTIVITY_PRECHECK_FAILED" && error.details.some((item) => item.code === "POOL_VERSION_IMMUTABLE")
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
