import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  classifyMetricsKey,
  cleanupLegacyMetrics
} from "../../tools/metrics-legacy-cleanup.js";

class FakePipeline {
  constructor(redis) {
    this.redis = redis;
    this.keys = [];
  }

  type(key) {
    this.keys.push(key);
    return this;
  }

  async exec() {
    return this.keys.map((key) => [null, this.redis.keys.get(key) ?? "none"]);
  }
}

class FakeRedis {
  constructor(entries) {
    this.keys = new Map(entries);
    this.unlinked = [];
    this.scanCalls = [];
  }

  async scan(cursor, _match, pattern, _count, count) {
    this.scanCalls.push({ cursor, pattern, count });
    if (cursor !== "0") return ["0", []];
    return ["0", [...this.keys.keys()].filter((key) => key.startsWith(pattern.slice(0, -1)))];
  }

  pipeline() {
    return new FakePipeline(this);
  }

  async eval(_script, keyCount, ...keys) {
    const selectedKeys = keys.slice(0, keyCount);
    this.unlinked.push(...selectedKeys);
    let deleted = 0;
    let wrongType = 0;
    for (const key of selectedKeys) {
      if (this.keys.get(key) === "hash") {
        this.keys.delete(key);
        deleted += 1;
      } else {
        wrongType += 1;
      }
    }
    return [deleted, wrongType];
  }
}

test("legacy key classifier accepts only exact historical bucket layouts", () => {
  assert.deepEqual(classifyMetricsKey("prod:metrics:game-server:gs-1:1700000000", "prod:"), {
    kind: "legacy",
    service: "game-server",
    instanceId: "gs-1",
    bucket: 1700000000,
    layout: "service-instance-bucket"
  });
  assert.deepEqual(classifyMetricsKey("metrics:game-server:1700000000", ""), {
    kind: "legacy",
    service: "game-server",
    instanceId: null,
    bucket: 1700000000,
    layout: "service-bucket"
  });
  assert.equal(classifyMetricsKey("metrics:v2:history:game-server:1700000000").kind, "v2");
  assert.equal(classifyMetricsKey("metrics:heartbeat:game-server").kind, "heartbeat");
  assert.equal(classifyMetricsKey("metrics:game-server:gs-1:not-a-bucket").kind, "invalid_bucket");
  assert.equal(classifyMetricsKey("metrics:game-server:gs-1:1700000001").kind, "invalid_bucket");
  assert.equal(classifyMetricsKey("metrics:game-server:gs-1:1700000000:extra").kind, "invalid_layout");
});

test("cleanup defaults to dry-run and excludes v2, heartbeat, invalid and non-hash keys", async () => {
  const redis = new FakeRedis([
    ["prod:metrics:game-server:gs-1:1700000000", "hash"],
    ["prod:metrics:game-server:1700000000", "hash"],
    ["prod:metrics:v2:history:game-server:1700000000", "hash"],
    ["prod:metrics:heartbeat:game-server", "string"],
    ["prod:metrics:game-server:gs-1:bad", "hash"],
    ["prod:metrics:game-server:gs-2:1700000000", "string"],
    ["prod:session:player-1", "string"]
  ]);

  const report = await cleanupLegacyMetrics({
    redis,
    redisUrl: "redis://localhost:6379/0",
    keyPrefix: "prod:",
    delayMs: 0
  });

  assert.equal(report.ok, true);
  assert.equal(report.mode, "dry-run");
  assert.equal(report.legacyCandidates, 3);
  assert.equal(report.eligibleHashes, 2);
  assert.equal(report.remainingEligibleHashes, 2);
  assert.equal(report.deleted, 0);
  assert.equal(report.excluded.v2, 1);
  assert.equal(report.excluded.heartbeat, 1);
  assert.equal(report.excluded.invalid_bucket, 1);
  assert.equal(report.excluded.wrong_type, 1);
  assert.deepEqual(report.services, { "game-server": 2 });
  assert.deepEqual(redis.unlinked, []);
  assert.equal(redis.scanCalls[0].pattern, "prod:metrics:*");
});

test("apply requires explicit confirmation and writes checkpoint and audit records", async () => {
  const temporaryDirectory = fs.mkdtempSync(path.join(os.tmpdir(), "metrics-legacy-cleanup-"));
  const checkpointPath = path.join(temporaryDirectory, "checkpoint.json");
  const auditLogPath = path.join(temporaryDirectory, "audit.ndjson");
  const redis = new FakeRedis([
    ["metrics:game-server:gs-1:1700000000", "hash"],
    ["metrics:v2:latest:game-server:gs-1", "hash"],
    ["metrics:heartbeat:game-server", "string"]
  ]);

  try {
    await assert.rejects(
      () => cleanupLegacyMetrics({
        redis,
        redisUrl: "redis://localhost:6379/0",
        keyPrefix: "",
        allowEmptyPrefix: true,
        apply: true
      }),
      /confirm=legacy-metrics-unlink/
    );

    const report = await cleanupLegacyMetrics({
      redis,
      redisUrl: "redis://localhost:6379/0",
      keyPrefix: "",
      allowEmptyPrefix: true,
      apply: true,
      confirm: "legacy-metrics-unlink",
      operator: "test-operator",
      checkpointPath,
      auditLogPath,
      batchSize: 1,
      delayMs: 0
    });

    assert.equal(report.ok, true);
    assert.equal(report.deleted, 1);
    assert.equal(report.remainingEligibleHashes, 0);
    assert.deepEqual(redis.unlinked, ["metrics:game-server:gs-1:1700000000"]);
    assert.equal(redis.keys.has("metrics:v2:latest:game-server:gs-1"), true);
    assert.equal(redis.keys.has("metrics:heartbeat:game-server"), true);
    const checkpoint = JSON.parse(fs.readFileSync(checkpointPath, "utf8"));
    assert.equal(checkpoint.completed, true);
    assert.equal(checkpoint.deleted, 1);
    const audit = JSON.parse(fs.readFileSync(auditLogPath, "utf8").trim());
    assert.equal(audit.operation, "legacy-metrics-cleanup");
    assert.equal(audit.operator, "test-operator");
    assert.equal(audit.redisUrl, "redis://localhost:6379/0");
  } finally {
    fs.rmSync(temporaryDirectory, { recursive: true, force: true });
  }
});

test("resume preserves checkpoint counters and rejects a completed checkpoint", async () => {
  const temporaryDirectory = fs.mkdtempSync(path.join(os.tmpdir(), "metrics-legacy-resume-"));
  const checkpointPath = path.join(temporaryDirectory, "checkpoint.json");
  const redis = new FakeRedis([]);
  const checkpoint = {
    version: 1,
    redisUrl: "redis://localhost:6379/0",
    keyPrefix: "prod:",
    mode: "dry-run",
    startedAt: "2026-08-03T00:00:00.000Z",
    cursor: "17",
    completed: false,
    scanned: 100,
    legacyCandidates: 80,
    eligibleHashes: 79,
    deleted: 0,
    batches: 2,
    excluded: { v2: 10, heartbeat: 5, wrong_type: 1 },
    services: { "game-server": 79 }
  };

  try {
    fs.writeFileSync(checkpointPath, JSON.stringify(checkpoint), "utf8");
    const report = await cleanupLegacyMetrics({
      redis,
      redisUrl: "redis://localhost:6379/0",
      keyPrefix: "prod:",
      checkpointPath,
      resume: true,
      delayMs: 0
    });
    assert.equal(report.ok, true);
    assert.equal(report.scanned, 100);
    assert.equal(report.legacyCandidates, 80);
    assert.equal(report.eligibleHashes, 79);
    assert.equal(report.batches, 3);
    assert.deepEqual(report.services, { "game-server": 79 });

    await assert.rejects(
      () => cleanupLegacyMetrics({
        redis,
        redisUrl: "redis://localhost:6379/0",
        keyPrefix: "prod:",
        checkpointPath,
        resume: true
      }),
      /checkpoint is already complete/
    );
  } finally {
    fs.rmSync(temporaryDirectory, { recursive: true, force: true });
  }
});

test("cleanup implementation does not issue blocking or database-wide deletion commands", () => {
  const sourcePath = fileURLToPath(new URL("../../tools/metrics-legacy-cleanup.js", import.meta.url));
  const source = fs.readFileSync(sourcePath, "utf8");
  assert.doesNotMatch(source, /\.keys\s*\(/i);
  assert.doesNotMatch(source, /flush(?:all|db)/i);
  assert.match(source, /\.scan\s*\(/i);
  assert.match(source, /redis\.call\("UNLINK"/i);
});

test("cleanup requires an explicit Redis target and prefix boundary", async () => {
  const redis = new FakeRedis([]);
  await assert.rejects(
    () => cleanupLegacyMetrics({ redis, redisUrl: "redis://localhost:6379/0" }),
    /keyPrefix must be provided explicitly/
  );
  await assert.rejects(
    () => cleanupLegacyMetrics({ redis, redisUrl: "redis://localhost:6379/0", keyPrefix: "" }),
    /allowEmptyPrefix/
  );
  await assert.rejects(
    () => cleanupLegacyMetrics({ redis, keyPrefix: "prod:" }),
    /redisUrl must be provided explicitly/
  );
});

test("cleanup can read the explicit Redis target from a named environment variable", async () => {
  const previous = process.env.TEST_METRICS_REDIS_URL;
  process.env.TEST_METRICS_REDIS_URL = "redis://localhost:6379/0";
  try {
    const report = await cleanupLegacyMetrics({
      redis: new FakeRedis([]),
      redisUrlEnv: "TEST_METRICS_REDIS_URL",
      keyPrefix: "prod:",
      delayMs: 0
    });
    assert.equal(report.ok, true);
    assert.equal(report.redisUrl, "redis://localhost:6379/0");
  } finally {
    if (previous === undefined) delete process.env.TEST_METRICS_REDIS_URL;
    else process.env.TEST_METRICS_REDIS_URL = previous;
  }
});
