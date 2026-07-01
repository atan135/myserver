import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { test } from "node:test";

import {
  OutageRedis,
  runRegistryOutageDrill
} from "../../tools/check-registry-outage-drill.js";

const projectRoot = process.cwd();

test("registry outage drill keeps warm cache briefly and fails fast without cache", async () => {
  const redis = new OutageRedis({ now: () => 1_713_000_000_000 });
  const report = await runRegistryOutageDrill({
    redis,
    drillId: "outage-test",
    registryKeyPrefix: "test:outage:",
    cacheTtlMs: 5000,
    generatedAt: "2026-06-19T00:00:00.000Z"
  });

  assert.equal(report.ok, true, JSON.stringify(report.errors, null, 2));
  assert.deepEqual(report.errors, []);
  assert.equal(report.mode, "memory");
  assert.equal(report.registryKeyPrefix, "test:outage:");
  assert.deepEqual(report.target, {
    service: "game-server",
    endpoint: "admin",
    instanceId: "outage-test-game-server",
    host: "127.0.20.10",
    port: 17510
  });

  assert.equal(report.cache.ok, true);
  assert.equal(report.cache.warmup.endpoint.source, "registry");
  assert.equal(report.cache.warmup.endpoint.instanceId, "outage-test-game-server");
  assert.equal(report.cache.warmup.endpoint.endpointName, "admin");
  assert.equal(report.cache.warmup.endpoint.host, "127.0.20.10");
  assert.equal(report.cache.warmup.endpoint.port, 17510);
  assert.deepEqual(
    report.cache.warmup.redisOperations.map((operation) => operation.command),
    ["scan", "exists", "hget"]
  );

  assert.equal(report.cache.duringOutage.source, "discovery-cache");
  assert.equal(report.cache.duringOutage.reason, "cache_hit");
  assert.equal(report.cache.duringOutage.endpoint.source, "discovery-cache");
  assert.equal(report.cache.duringOutage.endpoint.instanceId, "outage-test-game-server");
  assert.equal(report.cache.duringOutage.endpoint.endpointName, "admin");
  assert.equal(report.cache.duringOutage.endpoint.host, "127.0.20.10");
  assert.equal(report.cache.duringOutage.endpoint.port, 17510);
  assert.equal(report.cache.duringOutage.sameInstanceId, true);
  assert.equal(report.cache.duringOutage.sameEndpoint, true);
  assert.deepEqual(report.cache.duringOutage.registryOperations, []);
  assert.equal(report.cache.duringOutage.fallbackUsed, false);
  assert.equal(report.cache.duringOutage.error, null);

  assert.equal(report.cache.afterTtl.ok, true);
  assert.equal(report.cache.afterTtl.reason, "cache_expired");
  assert.equal(report.cache.afterTtl.error.code, "REGISTRY_UNAVAILABLE");
  assert.deepEqual(
    report.cache.afterTtl.registryOperations.map((operation) => ({
      command: operation.command,
      available: operation.available
    })),
    [
      {
        command: "scan",
        available: false
      }
    ]
  );
  assert.equal(report.cache.afterTtl.fallbackUsed, false);

  assert.equal(report.newStart.ok, true);
  assert.equal(report.newStart.mode, "required_endpoint_no_cache");
  assert.equal(report.newStart.discoveryRequired, true);
  assert.equal(report.newStart.failFastMaxElapsedMs, 1000);
  assert.ok(report.newStart.elapsedMs <= report.newStart.failFastMaxElapsedMs);
  assert.equal(report.newStart.fallbackUsed, false);
  assert.equal(report.newStart.endpoint, null);
  assert.equal(report.newStart.error.code, "REGISTRY_UNAVAILABLE");
  assert.deepEqual(
    report.newStart.registryOperations.map((operation) => operation.command),
    ["scan"]
  );

  assert.equal(report.strictConsumer.ok, true);
  assert.equal(report.strictConsumer.consumer, "admin-api.game-admin-client");
  assert.equal(report.strictConsumer.mode, "strict_required_discovery");
  assert.equal(report.strictConsumer.registryDiscoveryEnabled, true);
  assert.equal(report.strictConsumer.registryDiscoveryRequired, true);
  assert.equal(report.strictConsumer.failFastMaxElapsedMs, 1000);
  assert.ok(report.strictConsumer.elapsedMs <= report.strictConsumer.failFastMaxElapsedMs);
  assert.equal(report.strictConsumer.localFallbackConfigured, true);
  assert.equal(report.strictConsumer.fallbackUsed, false);
  assert.deepEqual(report.strictConsumer.endpoints, []);
  assert.equal(report.strictConsumer.error.code, "REGISTRY_UNAVAILABLE");
  assert.deepEqual(
    report.strictConsumer.registryOperations.map((operation) => operation.command),
    ["scan"]
  );

  assert.equal(
    report.metrics.some((entry) =>
      entry.kind === "fallback_used" || entry.source === "fallback" || entry.reason === "fallback_used"
    ),
    false,
    "outage drill should not record or return fallback discovery"
  );
});

test("registry outage drill CLI emits compact JSON and exits cleanly", () => {
  const result = spawnSync(
    process.execPath,
    [
      "tools/check-registry-outage-drill.js",
      "--memory",
      "--drill-id", "outage-cli",
      "--registry-key-prefix", "test:outage-cli:",
      "--cache-ttl-ms", "5000",
      "--compact"
    ],
    { cwd: projectRoot, encoding: "utf8" }
  );

  assert.equal(result.status, 0, `stdout:\n${result.stdout}\nstderr:\n${result.stderr}`);
  assert.equal(result.stderr, "");

  const report = JSON.parse(result.stdout);
  assert.equal(report.ok, true);
  assert.equal(report.drillId, "outage-cli");
  assert.equal(report.registryKeyPrefix, "test:outage-cli:");
  assert.equal(report.cache.duringOutage.source, "discovery-cache");
  assert.equal(report.newStart.error.code, "REGISTRY_UNAVAILABLE");
  assert.equal(report.strictConsumer.fallbackUsed, false);
});
