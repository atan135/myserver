import assert from "node:assert/strict";
import test from "node:test";

import {
  gameServerInstances,
  gameServerMetric,
  isSafeDrainReason,
  normalizeDrainStatus,
  normalizeRolloutObservation,
  selectDefaultGameServerInstance
} from "./rollout-drain.js";

test("game-server targets come only from registry instance ids", () => {
  const instances = gameServerInstances({
    services: [{
      name: "game-server",
      instances: [
        { instance_id: "game-b", status: "degraded", healthy: true, endpoints: [{ host: "10.0.0.2", port: 7500 }] },
        { instance_id: "game-a", status: "healthy", healthy: true },
        { instance_id: "game-unhealthy", status: "unhealthy", healthy: false },
        { instance_id: "../unsafe", status: "healthy" },
        { instance_id: "game-a", status: "healthy" }
      ]
    }]
  });

  assert.deepEqual(instances.map((instance) => instance.instanceId), ["game-a", "game-b"]);
  assert.equal(selectDefaultGameServerInstance(instances), "game-a");
  assert.equal(Object.hasOwn(instances[0], "endpoint"), false);
});

test("drain reason must be present and cannot look like a credential", () => {
  assert.equal(isSafeDrainReason("planned replacement before version deployment"), true);
  assert.equal(isSafeDrainReason(""), false);
  assert.equal(isSafeDrainReason("token: secret-value"), false);
  assert.equal(isSafeDrainReason("a".repeat(257)), false);
});

test("metrics and route blockers remain explicit when control-plane values are absent", () => {
  const observation = normalizeRolloutObservation({
    instanceId: "game-a",
    services: {
      services: [{
        name: "game-server",
        instances: [{ instance_id: "game-a", online_value: 12, metrics_state: "fresh", status: "online" }]
      }]
    },
    rollout: { blockers: { blocked_room_count: 2, stale_player_route_count: 1 } }
  });

  assert.deepEqual(observation.metric, {
    available: true,
    onlineValue: 12,
    status: "online",
    metricsState: "fresh",
    reportedAt: null
  });
  assert.equal(observation.routeBlockers.blockedRoomCount, 2);
  assert.equal(observation.routeBlockers.stalePlayerRouteCount, 1);
  assert.deepEqual(observation.controlPlane, { available: false, message: "控制面状态待接入" });
  assert.equal(gameServerMetric({ services: [] }, "game-a").available, false);
});

test("drain status normalizes real control-plane counts without endpoint fields", () => {
  const status = normalizeDrainStatus({
    ok: true,
    connectionCount: 4,
    ownedRoomCount: 2,
    migratingRoomCount: 1,
    drainModeEnabled: true,
    retiredRoomCount: 3,
    transferableEmptyRoomCount: 1,
    routeCount: 5,
    drainModeReason: "rollout",
    drainModeSource: "admin"
  }, "game-a");
  assert.deepEqual(status, {
    available: true,
    instanceId: "game-a",
    errorCode: "",
    connectionCount: 4,
    ownedRoomCount: 2,
    migratingRoomCount: 1,
    drainModeEnabled: true,
    retiredRoomCount: 3,
    transferableEmptyRoomCount: 1,
    routeCount: 5,
    transferableEmptyRoomSampleCount: null,
    drainModeReason: "rollout",
    drainModeSource: "admin"
  });
  assert.equal(Object.hasOwn(status, "host"), false);
});
