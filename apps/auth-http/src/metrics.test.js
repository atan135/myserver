import assert from "node:assert/strict";
import test from "node:test";

import { MetricsCollector } from "./metrics.js";
import {
  recordRegistryCapacityCacheHit,
  recordRegistryCapacityCacheMiss,
  recordRegistryCapacityScan,
  recordDiscoveryMetric,
  resetDiscoveryMetrics,
  resetRegistryCapacityMetrics
} from "../../../packages/service-registry/node/registry-schema.js";

test("MetricsCollector flush includes discovery metric counters", async () => {
  resetDiscoveryMetrics();
  resetRegistryCapacityMetrics();
  recordDiscoveryMetric({
    serviceName: "game-proxy",
    endpointName: "client",
    source: "fallback",
    reason: "fallback_used"
  });
  recordDiscoveryMetric({
    serviceName: "game-server",
    endpointName: "admin",
    source: "registry",
    reason: "registry_error"
  });
  recordRegistryCapacityScan({
    durationMs: 8,
    instanceKeyCount: 3,
    visibleInstanceCount: 2
  });
  recordRegistryCapacityCacheHit(2);
  recordRegistryCapacityCacheMiss(1);

  const published = [];
  const redisCommands = [];
  const collector = new MetricsCollector(
    {
      async scan() {
        throw new Error("metrics must not scan Redis");
      },
      pipeline() {
        const commands = [];
        const pipeline = {
          zremrangebyscore(...args) { commands.push(["zremrangebyscore", ...args]); return pipeline; },
          zcard(...args) { commands.push(["zcard", ...args]); return pipeline; },
          async exec() {
            redisCommands.push(...commands);
            return commands.map(([command], index) => [null, command === "zcard" ? [4, 3, 2][index - 3] : 0]);
          }
        };
        return pipeline;
      }
    },
    {
      async publishJson(subject, payload) {
        published.push({ subject, payload });
      }
    },
    "auth-http",
    "",
    "auth-test"
  );

  await collector.flush();

  assert.equal(published.length, 1);
  assert.equal(published[0].payload.metrics.fallback_used_total, "1");
  assert.equal(published[0].payload.metrics.discovery_failure_total, "1");
  assert.equal(published[0].payload.metrics.discovery_success_total, "0");
  assert.equal(published[0].payload.metrics.registry_scan_total, "1");
  assert.equal(published[0].payload.metrics.registry_scan_duration_ms_total, "8");
  assert.equal(published[0].payload.metrics.registry_scan_instance_keys_last, "3");
  assert.equal(published[0].payload.metrics.registry_scan_visible_instances_last, "2");
  assert.equal(published[0].payload.metrics.registry_discovery_cache_hit_total, "2");
  assert.equal(published[0].payload.metrics.registry_discovery_cache_miss_total, "1");
  assert.equal(published[0].payload.metrics.registry_discovery_cache_hit_rate_basis_points, "6667");
  assert.equal(published[0].payload.metrics.online_sessions, 4);
  assert.equal(published[0].payload.metrics.unique_players, 3);
  assert.equal(published[0].payload.metrics.active_sessions_5m, 2);
  assert.deepEqual(redisCommands.map(([command]) => command), [
    "zremrangebyscore",
    "zremrangebyscore",
    "zremrangebyscore",
    "zcard",
    "zcard",
    "zcard"
  ]);
});
