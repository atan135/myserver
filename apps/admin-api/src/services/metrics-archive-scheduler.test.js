import assert from "node:assert/strict";
import test from "node:test";

import { MetricsArchiveScheduler } from "./metrics-archive-scheduler.js";

function archiveConfig(overrides = {}) {
  return {
    metricsArchiveEnabled: true,
    metricsArchiveIntervalSeconds: 300,
    metricsArchiveAfterSeconds: 3600,
    metricsHistoryRetentionSeconds: 4500,
    metricsArchiveBatchSize: 240,
    metricsArchiveLockTtlSeconds: 240,
    metricsKeyPrefix: "prod:",
    ...overrides
  };
}

function deferred() {
  let resolve;
  const promise = new Promise((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

test("archive scheduler starts immediately, uses configured interval, and prevents local overlap", async () => {
  const run = deferred();
  const calls = [];
  let intervalCallback = null;
  let intervalMs = null;
  const timer = { unrefCalled: false, unref() { this.unrefCalled = true; } };
  const scheduler = new MetricsArchiveScheduler({}, {}, archiveConfig(), {
    setIntervalFn(callback, delay) {
      intervalCallback = callback;
      intervalMs = delay;
      return timer;
    },
    clearIntervalFn() {},
    runArchiveTaskWithLockFn(redis, dbPool, options) {
      calls.push({ redis, dbPool, options });
      return run.promise;
    }
  });

  scheduler.start();
  await Promise.resolve();
  assert.equal(intervalMs, 300000);
  assert.equal(timer.unrefCalled, true);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].options.metricsKeyPrefix, "prod:");

  const skipped = await scheduler.runOnce();
  assert.equal(skipped.skipped, true);
  assert.equal(skipped.reason, "archive_local_run_active");
  intervalCallback();
  assert.equal(calls.length, 1);

  run.resolve({
    archived: 1,
    failed: 0,
    source_buckets: 12,
    resolution_seconds: 60,
    duration_ms: 1
  });
  await scheduler.onModuleDestroy();
});

test("disabled archive scheduler does not create a timer or run a task", () => {
  let scheduled = false;
  let ran = false;
  const scheduler = new MetricsArchiveScheduler({}, {}, archiveConfig({ metricsArchiveEnabled: false }), {
    setIntervalFn() {
      scheduled = true;
    },
    runArchiveTaskWithLockFn() {
      ran = true;
    }
  });

  scheduler.start();
  assert.equal(scheduled, false);
  assert.equal(ran, false);
});
