import assert from "node:assert/strict";
import test from "node:test";

import { archiveServiceMetrics } from "./archive.js";

function createArchiveRedis({ bucket, record }) {
  const hashKey = `metrics:v2:history:game-server:${bucket}`;
  const indexKey = "metrics:v2:history-index:game-server";
  const hashes = new Map([[hashKey, { "game-server-a": JSON.stringify(record) }]]);
  const index = new Map([[String(bucket), bucket]]);
  const scanCalls = [];
  const unlinked = [];

  const redis = {
    async zrangebyscore(key, min, max) {
      assert.equal(key, indexKey);
      const minimum = Number(min);
      const maximum = Number(String(max).replace(/^\(/, ""));
      return [...index.entries()]
        .filter(([, score]) => score >= minimum && score < maximum)
        .map(([member]) => member);
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

function archiveOptions() {
  return {
    metricsArchiveAfterSeconds: 3600,
    metricsHistoryRetentionSeconds: 4500,
    metricsArchiveBatchSize: 16
  };
}

test("archive reads indexed v2 history, batches PostgreSQL upsert, then unlinks successful source", async () => {
  const bucket = 100;
  const record = {
    _schema: "metrics-v2",
    _service: "game-server",
    _instance_id: "game-server-a",
    _bucket: bucket,
    _reported_at: bucket,
    _received_at: bucket,
    instance_id: "game-server-a",
    qps: 8,
    latency_ms: 14,
    online_players: 5
  };
  const fixture = createArchiveRedis({ bucket, record });
  const queries = [];
  const dbPool = {
    async query(statement, params) {
      queries.push({ statement, params });
    }
  };

  const result = await archiveServiceMetrics(fixture.redis, dbPool, "game-server", 0, 200, archiveOptions());

  assert.deepEqual(result, { archived: 1, failed: 0 });
  assert.equal(queries.length, 1);
  assert.match(queries[0].statement, /ON CONFLICT/);
  assert.equal(queries[0].params[2], 8);
  assert.equal(queries[0].params[3], 14);
  assert.equal(queries[0].params[4], 5);
  assert.equal(fixture.hashes.size, 0);
  assert.equal(fixture.index.size, 0);
  assert.equal(fixture.unlinked.length, 1);
  assert.deepEqual(fixture.scanCalls, []);
});

test("archive leaves v2 history and index intact when PostgreSQL upsert fails", async () => {
  const bucket = 100;
  const record = {
    _schema: "metrics-v2",
    _service: "game-server",
    _instance_id: "game-server-a",
    _bucket: bucket,
    _reported_at: bucket,
    _received_at: bucket,
    qps: 1,
    latency_ms: 2
  };
  const fixture = createArchiveRedis({ bucket, record });
  const dbPool = {
    async query() {
      throw new Error("database unavailable");
    }
  };

  const previousConsoleError = console.error;
  console.error = () => {};
  let result;
  try {
    result = await archiveServiceMetrics(fixture.redis, dbPool, "game-server", 0, 200, archiveOptions());
  } finally {
    console.error = previousConsoleError;
  }

  assert.deepEqual(result, { archived: 0, failed: 1 });
  assert.equal(fixture.hashes.size, 1);
  assert.equal(fixture.index.size, 1);
  assert.deepEqual(fixture.unlinked, []);
  assert.deepEqual(fixture.scanCalls, []);
});
