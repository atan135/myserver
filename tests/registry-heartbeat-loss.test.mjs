import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { test } from "node:test";

import { MemoryRedis } from "../tools/check-registry-canary-lifecycle.js";
import { runRegistryHeartbeatLossCheck } from "../tools/check-registry-heartbeat-loss.js";

const projectRoot = process.cwd();

function assertConsumerIds(consumers, expected) {
  assert.deepEqual(consumers.registryDiscoveryClient.instanceIds, expected);
  assert.deepEqual(consumers.registryDiscoveryClient.allEndpointInstanceIds, expected);
  assert.deepEqual(consumers.adminApi.instanceIds, expected);
  assert.deepEqual(consumers.mailService.instanceIds, expected);
  assert.equal(consumers.registryDiscoveryClient.fallbackUsed, false);
  assert.equal(consumers.adminApi.fallbackUsed, false);
  assert.equal(consumers.mailService.fallbackUsed, false);
}

test("registry heartbeat-loss gate filters stale records and switches after heartbeat expiry", async () => {
  const redis = new MemoryRedis({ now: () => 1_713_000_000_000 });
  const report = await runRegistryHeartbeatLossCheck({
    redis,
    checkId: "heartbeat-test",
    registryKeyPrefix: "test:heartbeat-loss:",
    generatedAt: "2026-06-19T00:00:00.000Z",
    heartbeatTtlSeconds: 1,
    expiryAdvanceMs: 1001
  });

  assert.equal(report.ok, true, JSON.stringify(report.errors, null, 2));
  assert.deepEqual(report.errors, []);
  assert.equal(report.mode, "memory");
  assert.equal(report.registryKeyPrefix, "test:heartbeat-loss:");

  assert.equal(report.missingHeartbeat.ok, true);
  assert.deepEqual(report.missingHeartbeat.expectedVisibleInstanceIds, ["heartbeat-test-healthy"]);
  assert.deepEqual(report.missingHeartbeat.expectedFilteredInstanceIds, ["heartbeat-test-missing-heartbeat"]);
  assert.deepEqual(
    report.missingHeartbeat.registryStates.map((state) => ({
      instanceId: state.instanceId,
      instanceRecordExists: state.instanceRecordExists,
      heartbeatExists: state.heartbeatExists
    })),
    [
      {
        instanceId: "heartbeat-test-healthy",
        instanceRecordExists: true,
        heartbeatExists: true
      },
      {
        instanceId: "heartbeat-test-missing-heartbeat",
        instanceRecordExists: true,
        heartbeatExists: false
      }
    ]
  );
  assertConsumerIds(report.missingHeartbeat.consumers, ["heartbeat-test-healthy"]);
  assert.equal(report.missingHeartbeat.fallbackUsed, false);

  assert.equal(report.expirySwitch.ok, true);
  assert.equal(report.expirySwitch.beforeExpiry.selectedInstanceId, "heartbeat-test-expiring");
  assertConsumerIds(report.expirySwitch.beforeExpiry.consumers, ["heartbeat-test-expiring"]);
  assert.deepEqual(
    report.expirySwitch.afterExpiry.registryStates.map((state) => ({
      instanceId: state.instanceId,
      instanceRecordExists: state.instanceRecordExists,
      heartbeatExists: state.heartbeatExists
    })),
    [
      {
        instanceId: "heartbeat-test-expiring",
        instanceRecordExists: true,
        heartbeatExists: false
      },
      {
        instanceId: "heartbeat-test-replacement",
        instanceRecordExists: true,
        heartbeatExists: true
      }
    ]
  );
  assert.equal(report.expirySwitch.afterExpiry.selectedInstanceId, "heartbeat-test-replacement");
  assertConsumerIds(report.expirySwitch.afterExpiry.consumers, ["heartbeat-test-replacement"]);
  assert.equal(report.expirySwitch.fallbackUsed, false);

  assert.equal(
    report.metrics.some((entry) =>
      entry.kind === "fallback_used" || entry.source === "fallback" || entry.reason === "fallback_used"
    ),
    false,
    "heartbeat-loss gate should not record or return fallback discovery"
  );
});

test("registry heartbeat-loss gate CLI emits compact JSON and exits cleanly", () => {
  const result = spawnSync(
    process.execPath,
    [
      "tools/check-registry-heartbeat-loss.js",
      "--memory",
      "--check-id", "heartbeat-cli",
      "--registry-key-prefix", "test:heartbeat-cli:",
      "--compact"
    ],
    { cwd: projectRoot, encoding: "utf8" }
  );

  assert.equal(result.status, 0, `stdout:\n${result.stdout}\nstderr:\n${result.stderr}`);
  assert.equal(result.stderr, "");

  const report = JSON.parse(result.stdout);
  assert.equal(report.ok, true);
  assert.equal(report.checkId, "heartbeat-cli");
  assert.equal(report.registryKeyPrefix, "test:heartbeat-cli:");
  assert.equal(report.missingHeartbeat.expectedFilteredInstanceIds[0], "heartbeat-cli-missing-heartbeat");
  assert.equal(report.expirySwitch.afterExpiry.selectedInstanceId, "heartbeat-cli-replacement");
  assert.equal(report.expirySwitch.fallbackUsed, false);
});

test("registry heartbeat-loss gate CLI returns non-zero with structured JSON on failed expiry assertion", () => {
  const result = spawnSync(
    process.execPath,
    [
      "tools/check-registry-heartbeat-loss.js",
      "--memory",
      "--check-id", "heartbeat-cli-fail",
      "--registry-key-prefix", "test:heartbeat-cli-fail:",
      "--expiry-advance-ms", "0",
      "--compact"
    ],
    { cwd: projectRoot, encoding: "utf8" }
  );

  assert.equal(result.status, 1, `stdout:\n${result.stdout}\nstderr:\n${result.stderr}`);
  assert.equal(result.stderr, "");

  const report = JSON.parse(result.stdout);
  assert.equal(report.ok, false);
  assert.equal(report.expirySwitch.ok, false);
  assert.ok(report.errors.some((error) =>
    error.section === "expiry_switch" &&
    error.code === "heartbeat_still_present" &&
    error.instanceId === "heartbeat-cli-fail-expiring"
  ));
});
