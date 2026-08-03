import assert from "node:assert/strict";
import { TextEncoder } from "node:util";
import test from "node:test";

import { getConfig } from "./config.js";
import {
  buildMetricsV2Keys,
  getMetricsStorageCounters,
  METRICS_V2_WRITE_LUA,
  writeMetrics
} from "./server.js";

const NOW = 1_800_000_000;

function metricsMessage(payload) {
  return {
    data: new TextEncoder().encode(JSON.stringify(payload))
  };
}

class MemoryRedisScriptFixture {
  constructor() {
    this.calls = [];
    this.result = null;
  }

  async eval(script, keyCount, ...arguments_) {
    this.calls.push({
      script,
      keyCount,
      keys: arguments_.slice(0, keyCount),
      argv: arguments_.slice(keyCount)
    });
    const argv = arguments_.slice(keyCount);
    return this.result ?? ["ok", "1", argv[14]];
  }
}

function storageConfig(overrides = {}) {
  return {
    metricsTtlSeconds: 604800,
    heartbeatTtlSeconds: 30,
    nowSeconds: NOW,
    ...overrides
  };
}

async function withEnv(overrides, callback) {
  const previous = new Map();
  for (const [key, value] of Object.entries(overrides)) {
    previous.set(key, process.env[key]);
    if (value === undefined) {
      delete process.env[key];
    } else {
      process.env[key] = value;
    }
  }

  try {
    return await callback();
  } finally {
    for (const [key, value] of previous.entries()) {
      if (value === undefined) {
        delete process.env[key];
      } else {
        process.env[key] = value;
      }
    }
  }
}

test("metrics v2 write passes bounded keys and a complete record to one Lua script", async () => {
  const redis = new MemoryRedisScriptFixture();
  const bucket = NOW - 5;
  const result = await writeMetrics(
    redis,
    storageConfig({ metricsKeyPrefix: "test:" }),
    metricsMessage({
      service: "game-server",
      instance_id: "gs-1",
      bucket,
      timestamp: bucket + 2,
      metrics: { qps: 5, latency_ms: 12 }
    })
  );

  assert.deepEqual(result, { schemaVersion: 2, latestUpdated: true, legacyWritten: false });
  assert.equal(redis.calls.length, 1);
  const [call] = redis.calls;
  assert.equal(call.script, METRICS_V2_WRITE_LUA);
  assert.equal(call.keyCount, 7);
  assert.deepEqual(
    call.keys.slice(0, 4),
    Object.values(buildMetricsV2Keys("game-server", "gs-1", bucket, "test:"))
  );
  assert.deepEqual(call.keys.slice(4), [
    `test:metrics:game-server:gs-1:${bucket}`,
    "test:metrics:heartbeat:game-server",
    "test:metrics:heartbeat:game-server:gs-1"
  ]);
  assert.deepEqual(JSON.parse(call.argv[0]), {
    qps: "5",
    latency_ms: "12",
    _schema: "metrics-v2",
    _service: "game-server",
    _instance_id: "gs-1",
    _bucket: String(bucket),
    _reported_at: String(bucket + 2),
    _received_at: String(NOW),
    instance_id: "gs-1"
  });
  assert.equal(call.argv[7], "180");
  assert.equal(call.argv[8], "300");
  assert.equal(call.argv[9], "4500");
  assert.equal(call.argv[10], "64");
  assert.equal(call.argv[11], "900");
  assert.equal(call.argv[12], String(NOW));
  assert.equal(call.argv[13], "16384");
  assert.equal(call.argv[14], "0");
  assert.equal(call.argv[15], "604800");
  assert.equal(call.argv[16], "30");
});

test("metrics v2 writes legacy keys only during an explicitly enabled compatibility window", async () => {
  const redis = new MemoryRedisScriptFixture();
  const before = getMetricsStorageCounters();
  const bucket = NOW - 5;

  const result = await writeMetrics(
    redis,
    storageConfig({ metricsLegacyWriteEnabled: true }),
    metricsMessage({
      service: "game-server",
      instance_id: "gs-1",
      bucket,
      timestamp: bucket + 2,
      metrics: { qps: 5 }
    })
  );

  assert.deepEqual(result, { schemaVersion: 2, latestUpdated: true, legacyWritten: true });
  assert.equal(redis.calls[0].argv[14], "1");
  const after = getMetricsStorageCounters();
  assert.equal(after.metrics_legacy_write_enabled, 1);
  assert.equal(after.metrics_legacy_writes_total, before.metrics_legacy_writes_total + 1);
});

test("metrics v2 maps an out-of-order Lua result without replacing the latest snapshot", async () => {
  const redis = new MemoryRedisScriptFixture();
  redis.result = ["ok", "0", "0"];

  const result = await writeMetrics(
    redis,
    storageConfig(),
    metricsMessage({
      service: "game-server",
      instance_id: "gs-1",
      bucket: NOW - 10,
      timestamp: NOW - 9,
      metrics: { qps: 1 }
    })
  );

  assert.deepEqual(result, { schemaVersion: 2, latestUpdated: false, legacyWritten: false });
});

test("metrics collector rejects invalid, future and oversized payloads before Redis", async () => {
  const redis = new MemoryRedisScriptFixture();
  const invalidPayloads = [
    {
      service: "game:server",
      bucket: NOW - 5,
      timestamp: NOW - 4,
      metrics: { qps: 1 }
    },
    {
      service: "game-server",
      instance_id: "bad instance",
      bucket: NOW - 5,
      timestamp: NOW - 4,
      metrics: { qps: 1 }
    },
    {
      service: "game-server",
      bucket: NOW + 35,
      timestamp: NOW + 35,
      metrics: { qps: 1 }
    }
  ];

  for (const payload of invalidPayloads) {
    await assert.rejects(
      () => writeMetrics(redis, storageConfig(), metricsMessage(payload)),
      /invalid metrics|too far in the future/
    );
  }
  await assert.rejects(
    () => writeMetrics(
      redis,
      storageConfig({ metricsMaxRecordBytes: 256 }),
      metricsMessage({
        service: "game-server",
        bucket: NOW - 5,
        timestamp: NOW - 4,
        metrics: { label: "x".repeat(512) }
      })
    ),
    /record size limit/
  );
  assert.equal(redis.calls.length, 0);
});

test("metrics v2 reports capacity rejections and contains no Redis scan command", async () => {
  const redis = new MemoryRedisScriptFixture();
  redis.result = ["reject", "LATEST_CAPACITY_EXCEEDED"];
  const before = getMetricsStorageCounters();

  await assert.rejects(
    () => writeMetrics(
      redis,
      storageConfig(),
      metricsMessage({
        service: "game-server",
        instance_id: "gs-65",
        bucket: NOW - 5,
        timestamp: NOW - 4,
        metrics: { qps: 1 }
      })
    ),
    /LATEST_CAPACITY_EXCEEDED/
  );

  const after = getMetricsStorageCounters();
  assert.equal(after.capacityRejected, before.capacityRejected + 1);
  assert.match(METRICS_V2_WRITE_LUA, /ZREMRANGEBYSCORE/);
  assert.match(METRICS_V2_WRITE_LUA, /ZCARD/);
  assert.match(METRICS_V2_WRITE_LUA, /redis\.call\("DEL", latest_key\)/);
  assert.doesNotMatch(
    METRICS_V2_WRITE_LUA,
    /redis\.call\(["'](?:SCAN|SSCAN|HSCAN|ZSCAN|KEYS)["']/
  );
});

test("metrics storage configuration rejects a non-v2 schema or undersized history index", async () => {
  await withEnv({ METRICS_STORAGE_SCHEMA_VERSION: "1" }, async () => {
    assert.throws(() => getConfig(), /METRICS_STORAGE_SCHEMA_VERSION/);
  });
  await withEnv(
    {
      METRICS_STORAGE_SCHEMA_VERSION: "2",
      METRICS_HISTORY_RETENTION_SECONDS: "4500",
      METRICS_HISTORY_INDEX_MAX_MEMBERS: "899"
    },
    async () => {
      assert.throws(() => getConfig(), /METRICS_HISTORY_INDEX_MAX_MEMBERS/);
    }
  );
  await withEnv({ METRICS_LEGACY_WRITE_ENABLED: undefined }, async () => {
    assert.equal(getConfig().metricsLegacyWriteEnabled, false);
  });
  await withEnv({ METRICS_LEGACY_WRITE_ENABLED: "yes" }, async () => {
    assert.throws(() => getConfig(), /METRICS_LEGACY_WRITE_ENABLED/);
  });
});
