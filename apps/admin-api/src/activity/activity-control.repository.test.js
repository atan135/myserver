import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const {
  PostgresActivityControlRepository,
  RedisActivityRefreshNotifier,
  postgresActivityConfigDigest
} = await import("./activity-control.repository.ts");

function draft(overrides = {}) {
  return {
    activityId: "activity-pg-1",
    key: "summer",
    activityType: "login_reward",
    schemaVersion: 1,
    startAt: "2026-09-01T00:00:00.000Z",
    endAt: "2026-09-02T00:00:00.000Z",
    claimDeadline: "2026-09-03T00:00:00.000Z",
    timezone: "UTC",
    publicConfig: { title: "Summer" },
    typeConfig: {
      schema_version: 1,
      event_source: "game_entry",
      cycle_unit: "natural_day",
      progression: "consecutive",
      miss_policy: "reset",
      claim_mode: "manual",
      stages: [{ stage_no: 1, required_count: 1, reward_group_key: "g1" }]
    },
    stages: [{
      stageId: "day-1",
      stageNo: 1,
      rewardGroupKey: "g1",
      qualification: { requiredCount: 1 },
      display: { title: "Day 1" }
    }],
    rewardGroups: [{
      key: "g1",
      selectionMode: "fixed",
      items: [{ item_id: 1001, quantity: 2 }]
    }],
    reason: "test draft",
    actorId: "admin-1",
    ...overrides
  };
}

function lotteryDraft(overrides = {}) {
  return draft({
    activityId: "activity-lottery-pg",
    key: "lottery",
    activityType: "lottery",
    typeConfig: {
      schema_version: 1,
      draw_source: "player_action",
      pool_version: 3,
      free_draw_count: 1,
      voucher_item_id: 9001,
      daily_draw_limit: 10,
      total_draw_limit: 100,
      pool_items: [{ item_id: 1001, quantity: 1, weight: 3 }]
    },
    stages: [],
    rewardGroups: [{
      key: "pool",
      selectionMode: "weighted",
      items: [{ item_id: 1001, quantity: 1, weight: 3 }]
    }],
    ...overrides
  });
}

class ActivityPool {
  constructor() {
    this.activity = null;
    this.versions = new Map();
    this.groups = [];
    this.items = [];
    this.stages = [];
    this.audit = [];
    this.records = [];
    this.recordTotal = undefined;
    this.listRows = undefined;
    this.listTotal = undefined;
    this.queries = [];
    this.revision = 1;
  }

  async connect() { return this; }
  release() {}

  activityRow() {
    if (!this.activity) return null;
    const drafts = [...this.versions.values()].filter((version) => !version.publishedAt);
    const draftVersion = drafts.sort((left, right) => right.version - left.version)[0] || null;
    const current = this.versions.get(this.activity.currentVersion) || null;
    return {
      activity_id: this.activity.id,
      activity_key: this.activity.key,
      activity_type: this.activity.type,
      status: this.activity.status,
      current_version: this.activity.currentVersion,
      start_at: this.activity.startAt,
      end_at: this.activity.endAt,
      claim_deadline: this.activity.claimDeadline,
      timezone: this.activity.timezone,
      offline_reason: this.activity.offlineReason || null,
      revision: String(this.revision),
      draft_version: draftVersion?.version ?? null,
      draft_public_config: draftVersion?.publicConfig ?? null,
      draft_type_config: draftVersion?.typeConfig ?? null,
      draft_digest: draftVersion?.digest ?? null,
      draft_start_at: draftVersion?.startAt ?? null,
      draft_end_at: draftVersion?.endAt ?? null,
      draft_claim_deadline: draftVersion?.claimDeadline ?? null,
      draft_timezone: draftVersion?.timezone ?? null,
      draft_reason: draftVersion?.reason ?? null,
      current_public_config: current?.publicConfig ?? null,
      current_type_config: current?.typeConfig ?? null,
      current_digest: current?.digest ?? null,
      current_start_at: current?.startAt ?? null,
      current_end_at: current?.endAt ?? null,
      current_claim_deadline: current?.claimDeadline ?? null,
      current_timezone: current?.timezone ?? null,
      current_reason: current?.reason ?? null
    };
  }

  async query(text, values = []) {
    const sql = text.replace(/\s+/g, " ").trim();
    this.queries.push({ sql, values });
    if (["BEGIN", "COMMIT", "ROLLBACK"].includes(sql)) return { rows: [], rowCount: 0 };
    if (sql.startsWith("SELECT count(*)::text AS total_count FROM activity a")) {
      return { rows: [{ total_count: String(this.listTotal ?? (this.activity ? 1 : 0)) }] };
    }
    if (sql.startsWith("SELECT q.* FROM (")) {
      const rows = this.listRows ?? (this.activity ? [this.activityRow()] : []);
      return { rows };
    }
    if (sql.startsWith("INSERT INTO activity (")) {
      this.activity = {
        id: values[0], key: values[1], type: values[2], status: "draft",
        startAt: values[3], endAt: values[4], claimDeadline: values[5], timezone: values[6],
        currentVersion: null
      };
      return { rows: [], rowCount: 1 };
    }
    if (sql.startsWith("INSERT INTO activity_version")) {
      this.versions.set(Number(values[1]), {
        version: Number(values[1]), publicConfig: values[2], typeConfig: values[3], digest: values[4],
        startAt: values[5], endAt: values[6], claimDeadline: values[7], timezone: values[8],
        reason: values[9], publishedAt: null
      });
      return { rows: [], rowCount: 1 };
    }
    if (sql.startsWith("INSERT INTO activity_reward_group")) {
      this.groups.push({ activityId: values[0], version: Number(values[1]), key: values[2], selectionMode: values[3] });
      return { rows: [], rowCount: 1 };
    }
    if (sql.startsWith("INSERT INTO activity_reward_item")) {
      this.items.push({ activityId: values[0], version: Number(values[1]), group: values[2], rewardType: values[3], assetKey: values[4], quantity: values[5], weight: values[6], reward: values[7] });
      return { rows: [], rowCount: 1 };
    }
    if (sql.startsWith("INSERT INTO activity_stage")) {
      this.stages.push({ activityId: values[0], version: Number(values[1]), stageId: values[2], stageNo: values[3], qualification: values[4], group: values[6], display: values[9] });
      return { rows: [], rowCount: 1 };
    }
    if (sql.startsWith("SELECT stage_id") && sql.includes("FROM activity_stage WHERE")) {
      return { rows: this.stages.filter((row) => row.activityId === values[0] && row.version === Number(values[1])).map((row) => ({ stage_id: row.stageId, stage_no: row.stageNo, reward_group_key: row.group, qualification_json: row.qualification, display_json: row.display })) };
    }
    if (sql.startsWith("SELECT reward_group_key, selection_mode") && sql.includes("FROM activity_reward_group WHERE")) {
      return { rows: this.groups.filter((row) => row.activityId === values[0] && row.version === Number(values[1])).map((row, index) => ({ id: index + 1, reward_group_key: row.key, selection_mode: row.selectionMode, config_json: {} })) };
    }
    if (sql.startsWith("SELECT reward_group_key") && sql.includes("FROM activity_reward_item WHERE")) {
      return { rows: this.items.filter((row) => row.activityId === values[0] && row.version === Number(values[1])).map((row, index) => ({ id: index + 1, reward_group_key: row.group, reward_json: row.reward })) };
    }
    if (sql.startsWith("DELETE FROM activity_stage")) {
      this.stages = this.stages.filter((row) => row.activityId !== values[0] || row.version !== Number(values[1]));
      return { rows: [], rowCount: 1 };
    }
    if (sql.startsWith("DELETE FROM activity_reward_item")) {
      this.items = this.items.filter((row) => row.activityId !== values[0] || row.version !== Number(values[1]));
      return { rows: [], rowCount: 1 };
    }
    if (sql.startsWith("DELETE FROM activity_reward_group")) {
      this.groups = this.groups.filter((row) => row.activityId !== values[0] || row.version !== Number(values[1]));
      return { rows: [], rowCount: 1 };
    }
    if (sql.startsWith("UPDATE activity_version SET public_config_json")) {
      const version = this.versions.get(Number(values[1]));
      if (!version || version.publishedAt) return { rows: [], rowCount: 0 };
      Object.assign(version, { publicConfig: values[2], typeConfig: values[3], digest: values[4], startAt: values[5], endAt: values[6], claimDeadline: values[7], timezone: values[8], reason: values[9] });
      return { rows: [], rowCount: 1 };
    }
    if (sql.startsWith("UPDATE activity_version SET published_at")) {
      const version = this.versions.get(Number(values[1]));
      if (!version || version.publishedAt) return { rows: [], rowCount: 0 };
      version.publishedAt = new Date().toISOString();
      return { rows: [], rowCount: 1 };
    }
    if (sql.startsWith("UPDATE activity SET activity_key")) {
      Object.assign(this.activity, { key: values[1], type: values[2], startAt: values[3], endAt: values[4], claimDeadline: values[5], timezone: values[6] });
      this.revision += 1;
      return { rows: [], rowCount: 1 };
    }
    if (sql.startsWith("UPDATE activity SET status = 'draft'")) {
      Object.assign(this.activity, { status: "draft", startAt: values[1], endAt: values[2], claimDeadline: values[3], timezone: values[4] });
      this.revision += 1;
      return { rows: [], rowCount: 1 };
    }
    if (sql.startsWith("UPDATE activity SET status = 'published'")) {
      Object.assign(this.activity, { status: "published", currentVersion: Number(values[1]), offlineReason: null });
      this.revision += 1;
      return { rows: [], rowCount: 1 };
    }
    if (sql.startsWith("UPDATE activity SET status = 'offline'")) {
      Object.assign(this.activity, { status: "offline", offlineReason: values[2] });
      this.revision += 1;
      return { rows: [], rowCount: 1 };
    }
    if (sql.includes("FROM activity a") && sql.includes("WHERE a.activity_id = $1")) {
      const row = this.activity?.id === values[0] ? this.activityRow() : null;
      return { rows: row ? [row] : [], rowCount: row ? 1 : 0 };
    }
    if (sql === "SELECT 1 FROM activity WHERE activity_id = $1") {
      return { rows: this.activity?.id === values[0] ? [{ "?column?": 1 }] : [] };
    }
    if (sql.startsWith("WITH records AS") && sql.includes("SELECT count(*)::text AS total_count")) {
      return { rows: [{ total_count: String(this.recordTotal ?? this.records.length) }] };
    }
    if (sql.startsWith("WITH records AS")) return { rows: this.records };
    if (sql.startsWith("INSERT INTO activity_audit_log")) {
      this.audit.push({ values });
      return { rows: [], rowCount: 1 };
    }
    throw new Error(`UNHANDLED_QUERY:${sql}`);
  }
}

test("PostgreSQL repository maps login reward drafts to game-server tables and digest", async () => {
  const pool = new ActivityPool();
  const repository = new PostgresActivityControlRepository(pool);
  const command = draft();
  const created = await repository.createDraft(command);

  assert.equal(created.status, "draft");
  assert.equal(created.version, 1);
  assert.equal(created.configDigest, postgresActivityConfigDigest(command));
  assert.equal(created.configDigest, postgresActivityConfigDigest(created.draft));
  assert.equal(created.configDigest, "sha256:8db18111c07f7f457d6ee34bd7be5b12dc6a876a7dcfe6670b441a1c929d0dd5");
  assert.equal(created.draft.typeConfig.event_source, "game_entry");
  assert.deepEqual(created.draft.publicConfig.reward_groups, [{
    key: "g1",
    selection_mode: "fixed",
    items: [{ item_id: 1001, quantity: 2 }]
  }]);
  assert.deepEqual(pool.versions.get(1).publicConfig, created.draft.publicConfig);
  assert.deepEqual(created.draft.stages[0].display, { title: "Day 1" });
  assert.equal(pool.groups[0].key, "g1");
  assert.equal(pool.items[0].rewardType, "item");
  assert.equal(pool.items[0].assetKey, "1001");
  assert.deepEqual(pool.items[0].reward, { item_id: 1001, quantity: 2 });
  assert.equal(pool.stages[0].version, 1);

  const updated = await repository.updateDraft(command.activityId, draft({
    ifMatch: created.etag,
    publicConfig: { title: "Updated", reward_groups: [{ key: "untrusted", items: [] }] },
    rewardGroups: [{
      key: "g1",
      selectionMode: "fixed",
      items: [{ item_id: 1001, quantity: 3 }]
    }]
  }));
  assert.deepEqual(updated.draft.publicConfig.reward_groups, [{
    key: "g1",
    selection_mode: "fixed",
    items: [{ item_id: 1001, quantity: 3 }]
  }]);
  assert.deepEqual(updated.draft.rewardGroups, [{
    key: "g1",
    selectionMode: "fixed",
    items: [{ item_id: 1001, quantity: 3 }]
  }]);
  assert.equal(JSON.stringify(updated).includes("untrusted"), false);
  assert.equal(updated.configDigest, postgresActivityConfigDigest(updated.draft));
});

test("PostgreSQL repository preserves lottery pool snapshot and normalized weights", async () => {
  const pool = new ActivityPool();
  const repository = new PostgresActivityControlRepository(pool);
  const command = lotteryDraft();
  const created = await repository.createDraft(command);

  assert.equal(created.draft.typeConfig.pool_version, 3);
  assert.deepEqual(created.draft.typeConfig.pool_items, command.typeConfig.pool_items);
  assert.equal(pool.groups[0].selectionMode, "weighted");
  assert.equal(pool.items[0].weight, 3);
  assert.deepEqual(created.draft.publicConfig.reward_groups, [{
    key: "pool",
    selection_mode: "weighted",
    items: [{ item_id: 1001, quantity: 1, weight: 3 }]
  }]);
  assert.equal(pool.versions.get(1).digest, postgresActivityConfigDigest(command));
});

test("PostgreSQL repository rejects invalid reward values instead of applying numeric fallbacks", async () => {
  const cases = [
    { item_id: 1001, quantity: 0 },
    { item_id: 1001, quantity: 0x100000000 },
    { item_id: 0x80000000, quantity: 1 },
    { item_id: 1001, quantity: 1, weight: Number.MAX_SAFE_INTEGER + 1 },
    { item_id: 1001, quantity: 1, unexpected: true }
  ];
  for (const item of cases) {
    const pool = new ActivityPool();
    const repository = new PostgresActivityControlRepository(pool);
    await assert.rejects(
      () => repository.createDraft(draft({
        rewardGroups: [{ key: "g1", selectionMode: "fixed", items: [item] }]
      })),
      (error) => error.code === "ACTIVITY_INVALID_CONFIG"
    );
    assert.equal(pool.items.length, 0);
  }
});

test("PostgreSQL repository stores only the runtime reward item whitelist", async () => {
  const pool = new ActivityPool();
  const repository = new PostgresActivityControlRepository(pool);
  const created = await repository.createDraft(draft({
    rewardGroups: [{
      key: "g1",
      selectionMode: "weighted",
      items: [{ item_id: 1001, quantity: 2, weight: 3, binding: "character_bound" }]
    }]
  }));

  const expected = { item_id: 1001, quantity: 2, weight: 3, binding: "character_bound" };
  assert.deepEqual(created.draft.publicConfig.reward_groups[0].items, [expected]);
  assert.deepEqual(created.draft.rewardGroups[0].items, [expected]);
  assert.deepEqual(pool.items[0].reward, expected);
  assert.equal(pool.items[0].quantity, 2);
  assert.equal(pool.items[0].weight, 3);
});

test("publish and offline use activity xmin ETag CAS and immutable version updates", async () => {
  const pool = new ActivityPool();
  const repository = new PostgresActivityControlRepository(pool);
  const created = await repository.createDraft(draft());

  await assert.rejects(
    () => repository.publish(created.activityId, { version: 1, reason: "missing CAS" }),
    (error) => error.code === "ACTIVITY_VERSION_CONFLICT"
  );
  await assert.rejects(
    () => repository.publish(created.activityId, { version: 1, ifMatch: "stale", reason: "publish" }),
    (error) => error.code === "ACTIVITY_VERSION_CONFLICT"
  );
  const published = await repository.publish(created.activityId, { version: 1, ifMatch: created.etag, reason: "publish", actorId: "admin-1" });
  assert.equal(published.status, "published");
  assert.equal(pool.versions.get(1).publishedAt !== null, true);
  await assert.rejects(
    () => repository.offline(created.activityId, { version: 1, ifMatch: created.etag, reason: "stale" }),
    (error) => error.code === "ACTIVITY_VERSION_CONFLICT"
  );
  const offline = await repository.offline(created.activityId, { version: 1, ifMatch: published.etag, reason: "incident" });
  assert.equal(offline.status, "offline");
  assert.equal(offline.offlineReason, "incident");
});

test("published snapshots can fork a schedule-only draft without requiring a new config digest", async () => {
  const pool = new ActivityPool();
  const repository = new PostgresActivityControlRepository(pool);
  const created = await repository.createDraft(draft());
  const published = await repository.publish(created.activityId, {
    version: 1, ifMatch: created.etag, reason: "publish", actorId: "admin-1"
  });
  const forked = await repository.createDraftFromPublished(created.activityId, {
    sourceVersion: 1,
    ifMatch: published.etag,
    reason: "reschedule",
    overrides: {
      startAt: "2026-09-04T00:00:00.000Z",
      endAt: "2026-09-05T00:00:00.000Z",
      claimDeadline: "2026-09-06T00:00:00.000Z"
    }
  });

  assert.equal(forked.version, 2);
  assert.equal(forked.configDigest, published.configDigest);
  assert.equal(forked.sourceSnapshot.startAt, published.snapshot.startAt);
  assert.equal(forked.draft.startAt, "2026-09-04T00:00:00.000Z");

  const migration = await readFile(
    new URL("../../../../db/migrations/game/20260822150000_add_activity_control_metadata.sql", import.meta.url),
    "utf8"
  );
  assert.match(migration, /DROP INDEX IF EXISTS uk_activity_version_digest/);
  assert.match(migration, /CREATE INDEX IF NOT EXISTS idx_activity_version_digest/);
});

test("offline snapshots can fork a new draft while retaining the published source", async () => {
  const pool = new ActivityPool();
  const repository = new PostgresActivityControlRepository(pool);
  const created = await repository.createDraft(draft());
  const published = await repository.publish(created.activityId, {
    version: 1, ifMatch: created.etag, reason: "publish", actorId: "admin-1"
  });
  const offline = await repository.offline(created.activityId, { version: 1, ifMatch: published.etag, reason: "planned end" });
  const forked = await repository.createDraftFromPublished(created.activityId, {
    sourceVersion: 1, ifMatch: offline.etag, reason: "new schedule", overrides: {}
  });
  assert.equal(forked.status, "draft");
  assert.equal(forked.version, 2);
  assert.equal(forked.sourceSnapshot.publicConfig.title, "Summer");
  assert.equal(forked.draft.reason, "new schedule");
});

test("records map manual review and claim rows without exposing raw request ids", async () => {
  const pool = new ActivityPool();
  const repository = new PostgresActivityControlRepository(pool);
  await repository.createDraft(draft());
  pool.records = [
    {
      record_id: "review:7", activity_id: "activity-pg-1", version_no: 1,
      record_type: "manual_review", character_id: "character-1",
      raw_request_id: "sensitive-request-id", status: "manual_review",
      created_at: "2026-09-01T01:00:00.000Z", details: { reasonCode: "REQUEST_FINGERPRINT_CONFLICT" },
      total_count: "1"
    }
  ];

  const result = await repository.records("activity-pg-1", {
    status: "manual_review", requestId: "sensitive-request-id", limit: 10, offset: 0
  });
  assert.equal(result.total, 1);
  assert.equal(result.items[0].recordType, "manual_review");
  assert.match(result.items[0].requestId, /^sha256:[0-9a-f]{64}$/);
  assert.equal(JSON.stringify(result).includes("sensitive-request-id"), false);
  const recordsQuery = pool.queries.find((entry) => entry.sql.startsWith("WITH records AS"));
  assert.equal(recordsQuery.values.includes("sensitive-request-id"), true);
  assert.match(recordsQuery.sql, /activity_claim_review/);
  assert.match(recordsQuery.sql, /reward_mail_outbox/);
  assert.match(recordsQuery.sql, /selected_item_id::text/);
  assert.match(recordsQuery.sql, /state_revision::text/);
  assert.doesNotMatch(recordsQuery.sql, /order_snapshot_json|items_json|operator_json/);
});

test("reward mail records hash request-derived identifiers", async () => {
  const pool = new ActivityPool();
  const repository = new PostgresActivityControlRepository(pool);
  await repository.createDraft(draft());
  pool.records = [{
    record_id: "mail:raw-delivery-request-id",
    activity_id: "activity-pg-1",
    version_no: 1,
    record_type: "reward_mail",
    character_id: "character-1",
    raw_request_id: "raw-reward-request-id",
    status: "pending",
    created_at: "2026-09-01T02:00:00.000Z",
    details: { deliveryPolicy: "MAIL_ONLY", attemptCount: 0 }
  }];

  const result = await repository.records("activity-pg-1", { limit: 10, offset: 0 });
  assert.match(result.items[0].recordId, /^mail:sha256:[0-9a-f]{64}$/);
  assert.match(result.items[0].requestId, /^sha256:[0-9a-f]{64}$/);
  assert.equal(JSON.stringify(result).includes("raw-delivery-request-id"), false);
  assert.equal(JSON.stringify(result).includes("raw-reward-request-id"), false);
});

test("list and records preserve non-zero totals on pages past the final row", async () => {
  const pool = new ActivityPool();
  const repository = new PostgresActivityControlRepository(pool);
  await repository.createDraft(draft());
  pool.listRows = [];
  pool.listTotal = 1;
  pool.records = [];
  pool.recordTotal = 7;

  const activities = await repository.list({ status: "draft", limit: 10, offset: 100 });
  assert.deepEqual(activities.items, []);
  assert.equal(activities.total, 1);
  const records = await repository.records("activity-pg-1", { limit: 10, offset: 100 });
  assert.deepEqual(records.items, []);
  assert.equal(records.total, 7);

  const listCount = pool.queries.find((entry) => entry.sql.startsWith("SELECT count(*)::text AS total_count FROM activity a"));
  assert.deepEqual(listCount.values, ["draft"]);
  const recordsCount = pool.queries.find((entry) => entry.sql.startsWith("WITH records AS") && entry.sql.includes("SELECT count(*)::text AS total_count"));
  assert.deepEqual(recordsCount.values, ["activity-pg-1"]);
});

test("database failures map to fail-closed activity control errors", async () => {
  const repository = new PostgresActivityControlRepository({
    async query() { const error = new Error("connection lost"); error.code = "ECONNRESET"; throw error; }
  });
  await assert.rejects(
    () => repository.detail("activity-1"),
    (error) => error.code === "ACTIVITY_CONTROL_UNAVAILABLE" && !error.message.includes("connection lost")
  );

  const versionConflict = new PostgresActivityControlRepository({
    async query() {
      const error = new Error("duplicate");
      error.code = "23505";
      error.constraint = "uk_activity_version_activity_version";
      throw error;
    }
  });
  await assert.rejects(
    () => versionConflict.detail("activity-1"),
    (error) => error.code === "ACTIVITY_VERSION_CONFLICT" && !error.message.includes("duplicate")
  );
});

test("repository audit and Redis refresh use bounded control-plane payloads", async () => {
  const pool = new ActivityPool();
  const repository = new PostgresActivityControlRepository(pool);
  await repository.createDraft(draft());
  await repository.write({
    action: "published", activityId: "activity-pg-1", version: 1,
    actorId: "admin-1", reason: "publish", result: "success",
    summary: {
      status: "published",
      notification: { status: "failed", error: "redis://secret-host:6379" },
      typeConfig: { secret: "do-not-audit" }
    }
  });
  assert.equal(pool.audit.length, 1);
  assert.equal(JSON.stringify(pool.audit).includes("typeConfig"), false);
  assert.equal(JSON.stringify(pool.audit).includes("secret-host"), false);
  assert.deepEqual(pool.audit[0].values[5].summary.notification, { status: "failed" });

  const messages = [];
  const notifier = new RedisActivityRefreshNotifier({ async publish(channel, payload) { messages.push({ channel, payload }); } }, "test:");
  await notifier.notify({ activityId: "activity-pg-1", version: 1, action: "published" });
  assert.equal(messages[0].channel, "test:activity:refresh");
  assert.deepEqual(Object.keys(JSON.parse(messages[0].payload)).sort(), ["activity_id", "published_at", "version_no"]);
});

test("AppModule production provider uses the game pool domain service instead of unavailable", async () => {
  const source = await readFile(new URL("../app.module.ts", import.meta.url), "utf8");
  assert.match(source, /new PostgresActivityControlRepository\(gamePool\)/);
  assert.match(source, /new ActivityControlDomainService\(/);
  assert.match(source, /inject: \[ADMIN_GAME_DB_POOL, ADMIN_REDIS, ADMIN_CONFIG\]/);
  assert.doesNotMatch(source, /useClass: ActivityControlUnavailableService/);
});
