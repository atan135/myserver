import assert from "node:assert/strict";
import test from "node:test";

import { readSessionMetricsIndex, sessionMetricsIndexKeys } from "./session-metrics-index.js";

test("session metrics index trims expired members and returns bounded counts", async () => {
  const calls = [];
  const redis = {
    pipeline() {
      const commands = [];
      const pipeline = {
        zremrangebyscore(...args) { commands.push(["zremrangebyscore", ...args]); return pipeline; },
        zcard(...args) { commands.push(["zcard", ...args]); return pipeline; },
        async exec() {
          calls.push(...commands);
          return [
            [null, 2],
            [null, 1],
            [null, 3],
            [null, 11],
            [null, 10],
            [null, 7]
          ];
        }
      };
      return pipeline;
    }
  };

  const result = await readSessionMetricsIndex(redis, "prod:", 1_000_000);

  assert.deepEqual(result, {
    onlineSessions: 11,
    uniquePlayers: 10,
    activeSessions5m: 7
  });
  const keys = sessionMetricsIndexKeys("prod:");
  assert.deepEqual(calls, [
    ["zremrangebyscore", keys.sessions, "-inf", 1000],
    ["zremrangebyscore", keys.players, "-inf", 1000],
    ["zremrangebyscore", keys.activity, "-inf", 700],
    ["zcard", keys.sessions],
    ["zcard", keys.players],
    ["zcard", keys.activity]
  ]);
});

test("session metrics index fails closed on a Redis pipeline error", async () => {
  const redis = {
    pipeline() {
      const pipeline = {
        zremrangebyscore() { return pipeline; },
        zcard() { return pipeline; },
        async exec() {
          return [
            [new Error("redis unavailable"), null],
            [null, 0],
            [null, 0],
            [null, 0],
            [null, 0],
            [null, 0]
          ];
        }
      };
      return pipeline;
    }
  };

  await assert.rejects(readSessionMetricsIndex(redis), /redis unavailable/);
});
