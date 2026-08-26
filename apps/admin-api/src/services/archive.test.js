import assert from "node:assert/strict";
import test from "node:test";

import { archiveServiceMetrics, runArchiveTaskWithLock } from "./archive.js";

function createArchiveRedis(recordsByBucket) {
  const indexKey = "metrics:v2:history-index:game-server";
  const hashes = new Map();
  const index = new Map();
  const scanCalls = [];
  const unlinked = [];

  for (const [bucket, records] of recordsByBucket) {
    const hashKey = `metrics:v2:history:game-server:${bucket}`;
    hashes.set(hashKey, Object.fromEntries(records.map((record) => [record._instance_id, JSON.stringify(record)])));
    index.set(String(bucket), bucket);
  }

  const redis = {
    async zrangebyscore(key, min, max, ...args) {
      assert.equal(key, indexKey);
      const minimum = Number(String(min).replace(/^\(/, ""));
      const maximum = Number(String(max).replace(/^\(/, ""));
      let members = [...index.entries()]
        .filter(([, score]) => score >= minimum && score < maximum)
        .sort((left, right) => left[1] - right[1])
        .map(([member]) => member);
      if (args[0] === "LIMIT") {
        members = members.slice(Number(args[1]), Number(args[1]) + Number(args[2]));
      }
      return members;
    },
    pipeline() {
      const commands = [];
      const pipeline = {
        hgetall(key) { commands.push(["hgetall", key]); return pipeline; },
        unlink(key) { commands.push(["unlink", key]); return pipeline; },
        zrem(key, member) { commands.push(["zrem", key, member]); return pipeline; },
        async exec() {
          return commands.map(([command, key, member]) => {
            if (command === "hgetall") return [null, { ...(hashes.get(key) || {}) }];
            if (command === "unlink") {
              hashes.delete(key);
              unlinked.push(key);
              return [null, 1];
            }
            if (command === "zrem") {
              assert.equal(key, indexKey);
              index.delete(member);
              return [null, 1];
            }
            return [new Error("unexpected pipeline command"), null];
          });
        }
      };
      return pipeline;
    },
    async scan(...args) {
      scanCalls.push(args);
      throw new Error("archive must not scan Redis");
    }
  };

  return { redis, hashes, index, scanCalls, unlinked };
}

function metricRecord(bucket, instanceId, values) {
  return {
    _schema: "metrics-v2",
    _service: "game-server",
    _instance_id: instanceId,
    _bucket: bucket,
    _reported_at: bucket,
    _received_at: bucket,
    ...values
  };
}

function archiveOptions() {
  return {
    metricsArchiveAfterSeconds: 3600,
    metricsHistoryRetentionSeconds: 4500,
    metricsArchiveBatchSize: 240,
    metricsArchiveLockTtlMs: 240000
  };
}

test("archive aggregates twelve five-second buckets into one PostgreSQL minute", async () => {
  const recordsByBucket = new Map();
  for (let bucket = 60; bucket < 120; bucket += 5) {
    recordsByBucket.set(bucket, [
      metricRecord(bucket, "game-server-a", {
        qps: 10,
        latency_ms: 10,
        online_players: 3,
        register_failed_total: 1
      }),
      metricRecord(bucket, "game-server-b", {
        qps: 20,
        latency_ms: 20,
        online_players: 2,
        register_failed_total: 2
      })
    ]);
  }
  const fixture = createArchiveRedis(recordsByBucket);
  const queries = [];
  const dbPool = {
    async query(statement, params) {
      queries.push({ statement, params });
    }
  };

  const result = await archiveServiceMetrics(fixture.redis, dbPool, "game-server", 0, 180, archiveOptions());

  assert.deepEqual(result, { archived: 1, failed: 0, source_buckets: 12 });
  assert.equal(queries.length, 1);
  assert.match(queries[0].statement, /ON CONFLICT/);
  assert.equal(queries[0].params[0], "game-server");
  assert.equal(queries[0].params[1], 60);
  assert.equal(queries[0].params[2], 6);
  assert.equal(queries[0].params[3], 17);
  assert.equal(queries[0].params[4], 5);
  const extra = JSON.parse(queries[0].params[5]);
  assert.equal(extra.archive_resolution_seconds, 60);
  assert.equal(extra.source_bucket_count, 12);
  assert.equal(extra.expected_bucket_count, 12);
  assert.equal(extra.request_count, 360);
  assert.equal(extra.online_max, 5);
  assert.equal(extra.register_failed_total, 36);
  assert.equal(extra.instance_count, 2);
  assert.equal(fixture.hashes.size, 0);
  assert.equal(fixture.index.size, 0);
  assert.equal(fixture.unlinked.length, 12);
  assert.deepEqual(fixture.scanCalls, []);
});

test("archive leaves the complete source minute intact when PostgreSQL upsert fails", async () => {
  const recordsByBucket = new Map();
  for (let bucket = 60; bucket < 120; bucket += 5) {
    recordsByBucket.set(bucket, [
      metricRecord(bucket, "game-server-a", { qps: 1, latency_ms: 2, online_players: 1 })
    ]);
  }
  const fixture = createArchiveRedis(recordsByBucket);
  const dbPool = {
    async query() {
      throw new Error("database unavailable");
    }
  };

  const previousConsoleError = console.error;
  console.error = () => {};
  let result;
  try {
    result = await archiveServiceMetrics(fixture.redis, dbPool, "game-server", 0, 180, archiveOptions());
  } finally {
    console.error = previousConsoleError;
  }

  assert.deepEqual(result, { archived: 0, failed: 12, source_buckets: 0 });
  assert.equal(fixture.hashes.size, 12);
  assert.equal(fixture.index.size, 12);
  assert.deepEqual(fixture.unlinked, []);
});

test("archive does not write or clean a minute containing an invalid source record", async () => {
  const valid = metricRecord(60, "game-server-a", { qps: 1, latency_ms: 2 });
  const invalid = metricRecord(65, "game-server-a", { qps: 1, latency_ms: 2 });
  invalid._schema = "unknown";
  const fixture = createArchiveRedis(new Map([
    [60, [valid]],
    [65, [invalid]]
  ]));
  const queries = [];

  const result = await archiveServiceMetrics(
    fixture.redis,
    { async query(...args) { queries.push(args); } },
    "game-server",
    0,
    180,
    archiveOptions()
  );

  assert.deepEqual(result, { archived: 0, failed: 1, source_buckets: 0 });
  assert.deepEqual(queries, []);
  assert.equal(fixture.hashes.size, 2);
  assert.equal(fixture.index.size, 2);
});

test("archive lock skips overlapping automatic or manual runs", async () => {
  const result = await runArchiveTaskWithLock(
    { async set() { return null; } },
    {},
    archiveOptions()
  );

  assert.equal(result.skipped, true);
  assert.equal(result.reason, "archive_locked");
  assert.equal(result.archived, 0);
});

test("archive lock is token-checked and released after a successful run", async () => {
  const evalCalls = [];
  const redis = {
    async set() {
      return "OK";
    },
    async zrangebyscore() {
      return [];
    },
    async eval(...args) {
      evalCalls.push(args);
      return 1;
    }
  };

  const result = await runArchiveTaskWithLock(redis, {}, archiveOptions());

  assert.equal(result.archived, 0);
  assert.equal(result.skipped, undefined);
  assert.equal(evalCalls.length, 1);
  assert.match(evalCalls[0][0], /GET.*KEYS\[1\].*ARGV\[1\]/s);
  assert.match(evalCalls[0][0], /DEL/);
});
