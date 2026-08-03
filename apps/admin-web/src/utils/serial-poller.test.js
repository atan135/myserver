import assert from "node:assert/strict";
import test from "node:test";

import { createSerialPoller } from "./serial-poller.js";

function createFakeTimers() {
  let nextId = 1;
  const scheduled = [];
  return {
    scheduled,
    setTimer(callback, delay) {
      const entry = { id: nextId++, callback, delay, cancelled: false };
      scheduled.push(entry);
      return entry.id;
    },
    clearTimer(id) {
      const entry = scheduled.find((candidate) => candidate.id === id);
      if (entry) entry.cancelled = true;
    },
    runNext() {
      const entry = scheduled.find((candidate) => !candidate.cancelled && !candidate.ran);
      assert.ok(entry, "expected a scheduled timer");
      entry.ran = true;
      return entry.callback();
    },
    pendingDelays() {
      return scheduled
        .filter((entry) => !entry.cancelled && !entry.ran)
        .map((entry) => entry.delay);
    }
  };
}

test("serial poller never overlaps a slow request and coalesces refresh triggers", async () => {
  const timers = createFakeTimers();
  let calls = 0;
  let resolveRequest;
  const poller = createSerialPoller({
    task: () => {
      calls += 1;
      return new Promise((resolve) => {
        resolveRequest = resolve;
      });
    },
    setTimer: timers.setTimer,
    clearTimer: timers.clearTimer
  });

  poller.start();
  const firstRun = timers.runNext();
  poller.trigger();
  poller.trigger();
  assert.equal(calls, 1);
  resolveRequest(true);
  await firstRun;
  assert.deepEqual(timers.pendingDelays(), [0]);

  const secondRun = timers.runNext();
  assert.equal(calls, 2);
  resolveRequest(true);
  await secondRun;
  poller.stop();
});

test("serial poller backs off failed requests up to the cap and resets after success", async () => {
  const timers = createFakeTimers();
  const results = [false, false, false, false, true];
  const poller = createSerialPoller({
    task: async () => results.shift(),
    intervalMs: 15_000,
    maxIntervalMs: 120_000,
    setTimer: timers.setTimer,
    clearTimer: timers.clearTimer
  });

  poller.start();
  assert.deepEqual(timers.pendingDelays(), [0]);
  await timers.runNext();
  assert.deepEqual(timers.pendingDelays(), [30_000]);
  await timers.runNext();
  assert.deepEqual(timers.pendingDelays(), [60_000]);
  await timers.runNext();
  assert.deepEqual(timers.pendingDelays(), [120_000]);
  await timers.runNext();
  assert.deepEqual(timers.pendingDelays(), [120_000]);
  await timers.runNext();
  assert.deepEqual(timers.pendingDelays(), [15_000]);
  poller.stop();
});

test("stopping aborts an in-flight request and prevents another poll", async () => {
  const timers = createFakeTimers();
  let observedSignal;
  const poller = createSerialPoller({
    task: ({ signal }) => {
      observedSignal = signal;
      return new Promise((resolve) => signal.addEventListener("abort", () => resolve(false)));
    },
    setTimer: timers.setTimer,
    clearTimer: timers.clearTimer
  });

  poller.start();
  const run = timers.runNext();
  poller.stop();
  await run;
  assert.equal(observedSignal.aborted, true);
  assert.deepEqual(timers.pendingDelays(), []);
});
