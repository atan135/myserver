import assert from "node:assert/strict";
import test from "node:test";

import { AuthStore } from "./auth-store.js";
import { sessionMetricsIndexKeys } from "./session-metrics-index.js";

function createStore(redis) {
  return new AuthStore(
    {
      redisKeyPrefix: "prod:",
      sessionTtlSeconds: 86400,
      ticketTtlSeconds: 900,
      ticketSecret: "test-secret"
    },
    redis
  );
}

test("session replacement atomically maintains expiry, player, and activity indexes", async () => {
  const evalCalls = [];
  const store = createStore({
    async eval(...args) {
      evalCalls.push(args);
      return "old-token";
    }
  });

  const result = await store.replacePlayerSession({
    playerSessionKeyName: "prod:player-session:player-1",
    playerId: "player-1",
    accessToken: "new-token",
    sessionKeyName: "prod:session:new-token",
    sessionData: JSON.stringify({ playerId: "player-1" })
  });

  assert.equal(result, "old-token");
  assert.equal(evalCalls.length, 1);
  const args = evalCalls[0];
  const indexKeys = sessionMetricsIndexKeys("prod:");
  assert.match(args[0], /ZADD/);
  assert.equal(args[1], 5);
  assert.deepEqual(args.slice(2, 7), [
    "prod:player-session:player-1",
    "prod:session:new-token",
    indexKeys.sessions,
    indexKeys.players,
    indexKeys.activity
  ]);
  assert.equal(args[10], "new-token");
  assert.equal(args[11], 86400);
  assert.equal(args[12], "player-1");
});

test("session artifact deletion removes legacy keys and bounded index members", async () => {
  const deleted = [];
  const removed = [];
  const store = createStore({
    async del(key) {
      deleted.push(key);
    },
    async zrem(key, member) {
      removed.push([key, member]);
    }
  });

  await store.deleteSessionArtifacts("access-1", "player-1");

  assert.deepEqual(deleted, [
    "prod:session:access-1",
    "prod:session-activity:access-1"
  ]);
  const indexKeys = sessionMetricsIndexKeys("prod:");
  assert.deepEqual(removed, [
    [indexKeys.sessions, "access-1"],
    [indexKeys.activity, "access-1"],
    [indexKeys.players, "player-1"]
  ]);
});

test("activity refresh preserves the legacy compatibility key and updates the activity index", async () => {
  const writes = [];
  const store = createStore({
    async set(...args) {
      writes.push(["set", ...args]);
    },
    async zadd(...args) {
      writes.push(["zadd", ...args]);
    }
  });

  await store.markSessionActive("access-1");

  assert.deepEqual(writes[0], [
    "set",
    "prod:session-activity:access-1",
    writes[0][2],
    "EX",
    300
  ]);
  assert.equal(writes[1][0], "zadd");
  assert.equal(writes[1][1], sessionMetricsIndexKeys("prod:").activity);
  assert.equal(writes[1][3], "access-1");
});

test("session renewal atomically verifies the current player mapping before refreshing indexes", async () => {
  const evalCalls = [];
  const store = createStore({
    async eval(...args) {
      evalCalls.push(args);
      return 1;
    }
  });

  const renewed = await store.renewSessionState("access-1", "player-1");

  assert.equal(renewed, true);
  assert.equal(evalCalls.length, 1);
  const args = evalCalls[0];
  const indexKeys = sessionMetricsIndexKeys("prod:");
  assert.match(args[0], /GET.*KEYS\[2\].*ARGV\[1\]/s);
  assert.equal(args[1], 6);
  assert.deepEqual(args.slice(2, 8), [
    "prod:session:access-1",
    "prod:player-session:player-1",
    "prod:session-activity:access-1",
    indexKeys.sessions,
    indexKeys.players,
    indexKeys.activity
  ]);
});

test("stale session is rejected when atomic renewal sees a replacement mapping", async () => {
  const store = createStore({
    async get() {
      return JSON.stringify({ playerId: "player-1" });
    },
    async eval() {
      return 0;
    }
  });

  assert.equal(await store.getSessionByAccessToken("stale-access"), null);
});

test("logout cleanup only removes the player index when the mapping still matches", async () => {
  const evalCalls = [];
  const store = createStore({
    async eval(...args) {
      evalCalls.push(args);
      return 0;
    }
  });

  const removedMapping = await store.destroyMappedSessionArtifacts("old-access", "player-1");

  assert.equal(removedMapping, 0);
  assert.equal(evalCalls[0][1], 6);
  assert.match(evalCalls[0][0], /GET.*KEYS\[2\].*ARGV\[1\]/s);
});
