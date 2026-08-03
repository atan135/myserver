import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { TextEncoder } from "node:util";
import test from "node:test";

import Redis from "ioredis";

import {
  buildMetricsKey,
  buildMetricsV2Keys,
  writeMetrics
} from "../../apps/metrics-collector/src/server.js";
import { startRedisServer } from "../helpers/runtime.mjs";
import { cleanupLegacyMetrics } from "../../tools/metrics-legacy-cleanup.js";

function metricsMessage(payload) {
  return { data: new TextEncoder().encode(JSON.stringify(payload)) };
}

test("real Redis Lua keeps v2 writes while legacy compatibility is disabled", { timeout: 30_000 }, async () => {
  const server = await startRedisServer();
  const redis = new Redis(server.url, { maxRetriesPerRequest: 1 });
  const now = Math.floor(Date.now() / 5000) * 5;
  const prefix = `test:metrics-cutover:${process.pid}:`;
  const operationDirectory = fs.mkdtempSync(path.join(os.tmpdir(), "metrics-cutover-redis-"));
  const baseConfig = {
    metricsKeyPrefix: prefix,
    metricsTtlSeconds: 600,
    heartbeatTtlSeconds: 30,
    nowSeconds: now
  };

  try {
    const disabled = await writeMetrics(
      redis,
      { ...baseConfig, metricsLegacyWriteEnabled: false },
      metricsMessage({
        service: "game-server",
        instance_id: "gs-disabled",
        bucket: now,
        timestamp: now,
        metrics: { qps: 2 }
      })
    );

    assert.deepEqual(disabled, { schemaVersion: 2, latestUpdated: true, legacyWritten: false });
    const disabledV2 = buildMetricsV2Keys("game-server", "gs-disabled", now, prefix);
    assert.equal(await redis.exists(disabledV2.latest), 1);
    assert.equal(await redis.exists(disabledV2.history), 1);
    assert.equal(await redis.exists(buildMetricsKey("game-server", "gs-disabled", now, prefix)), 0);
    assert.equal(await redis.exists(`${prefix}metrics:heartbeat:game-server`), 0);
    assert.equal(await redis.exists(`${prefix}metrics:heartbeat:game-server:gs-disabled`), 0);

    const enabled = await writeMetrics(
      redis,
      { ...baseConfig, metricsLegacyWriteEnabled: true },
      metricsMessage({
        service: "game-server",
        instance_id: "gs-enabled",
        bucket: now,
        timestamp: now,
        metrics: { qps: 3 }
      })
    );

    assert.deepEqual(enabled, { schemaVersion: 2, latestUpdated: true, legacyWritten: true });
    assert.equal(await redis.exists(buildMetricsKey("game-server", "gs-enabled", now, prefix)), 1);
    assert.equal(await redis.exists(`${prefix}metrics:heartbeat:game-server`), 1);
    assert.equal(await redis.exists(`${prefix}metrics:heartbeat:game-server:gs-enabled`), 1);

    const dryRun = await cleanupLegacyMetrics({
      redis,
      redisUrl: server.url,
      keyPrefix: prefix,
      delayMs: 0
    });
    assert.equal(dryRun.ok, true);
    assert.equal(dryRun.eligibleHashes, 1);
    assert.equal(dryRun.remainingEligibleHashes, 1);
    assert.equal(dryRun.deleted, 0);
    assert.ok(dryRun.excluded.v2 >= 1);
    assert.equal(dryRun.excluded.heartbeat, 2);

    const applied = await cleanupLegacyMetrics({
      redis,
      redisUrl: server.url,
      keyPrefix: prefix,
      apply: true,
      confirm: "legacy-metrics-unlink",
      operator: "integration-test",
      checkpointPath: path.join(operationDirectory, "checkpoint.json"),
      auditLogPath: path.join(operationDirectory, "audit.ndjson"),
      delayMs: 0
    });
    assert.equal(applied.ok, true);
    assert.equal(applied.deleted, 1);
    assert.equal(applied.remainingEligibleHashes, 0);
    assert.equal(await redis.exists(buildMetricsKey("game-server", "gs-enabled", now, prefix)), 0);
    assert.equal(await redis.exists(disabledV2.latest), 1);
    assert.equal(await redis.exists(`${prefix}metrics:heartbeat:game-server`), 1);
  } finally {
    redis.disconnect();
    await server.close();
    fs.rmSync(operationDirectory, { recursive: true, force: true });
  }
});
