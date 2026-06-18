import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { test } from "node:test";

import { MemoryRedis } from "../tools/check-registry-canary-lifecycle.js";
import { runRegistryFallbackObservabilityCheck } from "../tools/check-registry-fallback-observability.js";

const projectRoot = process.cwd();

function checkByName(report, name) {
  const check = report.discovery.checks.find((candidate) => candidate.name === name);
  assert.ok(check, `missing check ${name}`);
  return check;
}

test("registry fallback observability gate reports zero fallback metrics logs and endpoints on registry success", async () => {
  const redis = new MemoryRedis({ now: () => 1_713_000_000_000 });
  const report = await runRegistryFallbackObservabilityCheck({
    redis,
    checkId: "fallback-observability-test",
    registryKeyPrefix: "test:fallback-observability:",
    generatedAt: "2026-06-19T00:00:00.000Z"
  });

  assert.equal(report.ok, true, JSON.stringify(report.errors, null, 2));
  assert.deepEqual(report.errors, []);
  assert.equal(report.mode, "memory");
  assert.equal(report.registryKeyPrefix, "test:fallback-observability:");

  assert.equal(report.discovery.ok, true);
  assert.equal(
    report.discovery.checks.every((check) => check.fallbackEndpointCount === 0),
    true
  );
  assert.deepEqual(
    checkByName(report, "RegistryDiscoveryClient game-server.admin").discoveredInstanceIds,
    ["fallback-observability-test-game-server"]
  );
  assert.deepEqual(
    checkByName(report, "RegistryDiscoveryClient game-proxy.client").discoveredInstanceIds,
    ["fallback-observability-test-game-proxy"]
  );
  assert.deepEqual(
    checkByName(report, "admin-api helper game-server.admin").discoveredInstanceIds,
    ["fallback-observability-test-game-server"]
  );
  assert.deepEqual(
    checkByName(report, "admin-api strict GameAdminClient game-server.admin").discoveredInstanceIds,
    ["fallback-observability-test-game-server"]
  );
  assert.deepEqual(
    checkByName(report, "mail-service helper game-server.admin").discoveredInstanceIds,
    ["fallback-observability-test-game-server"]
  );

  assert.equal(report.metricFields.fallback_used_total, "0");
  assert.equal(report.observability.ok, true);
  assert.equal(report.observability.fallbackMetricTotal, 0);
  assert.deepEqual(report.observability.fallbackMetricEntries, []);
  assert.equal(report.observability.fallbackLogCount, 0);
  assert.deepEqual(report.observability.fallbackLogs, []);
  assert.equal(report.observability.fallbackEndpointCount, 0);
  assert.deepEqual(report.observability.fallbackEndpoints, []);
  assert.ok(report.logs.length > 0, "gate should collect discovery log context");
  assert.ok(report.logs.some((entry) =>
    entry.event === "registry.discovery_all_endpoints" &&
    entry.service === "game-server" &&
    entry.endpoint === "admin" &&
    entry.source === "registry" &&
    entry.reason === "discovered"
  ));
  assert.ok(report.logs.some((entry) =>
    entry.event === "registry.discovery_all_endpoints" &&
    entry.service === "game-proxy" &&
    entry.endpoint === "admin" &&
    entry.source === "registry" &&
    entry.reason === "discovered"
  ));
  assert.equal(
    report.logs.every((entry) => entry.source !== "fallback" && entry.reason !== "fallback_used"),
    true
  );
});

test("registry fallback observability gate fails with concrete metric and log sources when fallback is recorded", async () => {
  const redis = new MemoryRedis({ now: () => 1_713_000_000_000 });
  const report = await runRegistryFallbackObservabilityCheck({
    redis,
    checkId: "fallback-observability-fail",
    registryKeyPrefix: "test:fallback-observability-fail:",
    generatedAt: "2026-06-19T00:00:00.000Z",
    injectFallback: true
  });

  assert.equal(report.ok, false);
  assert.equal(report.discovery.ok, true);
  assert.equal(report.metricFields.fallback_used_total, "1");
  assert.equal(report.observability.ok, false);
  assert.equal(report.observability.fallbackMetricTotal, 1);
  assert.equal(report.observability.fallbackMetricEntries.length, 1);
  assert.equal(report.observability.fallbackMetricEntries[0].service, "game-server");
  assert.equal(report.observability.fallbackMetricEntries[0].endpoint, "admin");
  assert.equal(report.observability.fallbackLogCount, 1);
  assert.equal(report.observability.fallbackLogs[0].event, "registry.discovery_fallback");
  assert.ok(report.errors.some((error) =>
    error.code === "fallback_metric_total_nonzero" &&
    error.source === "metrics" &&
    error.actual === 1
  ));
  assert.ok(report.errors.some((error) =>
    error.code === "fallback_metric_entry" &&
    error.source === "metrics" &&
    error.service === "game-server"
  ));
  assert.ok(report.errors.some((error) =>
    error.code === "fallback_log_event" &&
    error.source === "logs" &&
    error.event === "registry.discovery_fallback"
  ));
});

test("registry fallback observability CLI emits compact JSON and returns non-zero on injected fallback", () => {
  const result = spawnSync(
    process.execPath,
    [
      "tools/check-registry-fallback-observability.js",
      "--memory",
      "--check-id", "fallback-observability-cli-fail",
      "--registry-key-prefix", "test:fallback-observability-cli-fail:",
      "--inject-fallback",
      "--compact"
    ],
    { cwd: projectRoot, encoding: "utf8" }
  );

  assert.equal(result.status, 1, `stdout:\n${result.stdout}\nstderr:\n${result.stderr}`);
  assert.equal(result.stderr, "");

  const report = JSON.parse(result.stdout);
  assert.equal(report.ok, false);
  assert.equal(report.checkId, "fallback-observability-cli-fail");
  assert.equal(report.metricFields.fallback_used_total, "1");
  assert.ok(report.errors.some((error) => error.source === "metrics"));
  assert.ok(report.errors.some((error) => error.source === "logs"));
});
