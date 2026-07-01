import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { test } from "node:test";

import { MemoryRedis } from "../../tools/check-registry-canary-lifecycle.js";
import { runRegistrySchemaMixedRolloutCheck } from "../../tools/check-registry-schema-mixed-rollout.js";

const projectRoot = process.cwd();

function findCheck(section, name) {
  const check = section.checks.find((candidate) => candidate.name === name);
  assert.ok(check, `missing check ${name}`);
  return check;
}

function resultByInstance(check, instanceId) {
  const result = check.results.find((candidate) => candidate.instanceId === instanceId);
  assert.ok(result, `missing ${instanceId} in ${check.name}`);
  return result;
}

test("registry schema mixed rollout gate discovers v1 legacy and v2 endpoint records", async () => {
  const redis = new MemoryRedis({ now: () => 1_713_000_000_000 });
  const report = await runRegistrySchemaMixedRolloutCheck({
    redis,
    checkId: "schema-mixed-test",
    registryKeyPrefix: "test:schema-mixed:",
    generatedAt: "2026-06-19T00:00:00.000Z"
  });

  assert.equal(report.ok, true, JSON.stringify(report.errors, null, 2));
  assert.deepEqual(report.errors, []);
  assert.equal(report.mode, "memory");
  assert.equal(report.registryKeyPrefix, "test:schema-mixed:");
  assert.equal(report.fallbackUsed, false);

  assert.equal(report.registryRecords.ok, true);
  assert.deepEqual(
    report.registryRecords.records.map((record) => ({
      service: record.service,
      instanceId: record.instanceId,
      rawVersion: record.schema.rawVersion,
      source: record.schema.source,
      explicitEndpoints: record.schema.explicitEndpoints,
      heartbeatExists: record.heartbeatExists
    })),
    [
      {
        service: "game-server",
        instanceId: "schema-mixed-test-game-server-legacy-v1",
        rawVersion: 1,
        source: "legacy-v1",
        explicitEndpoints: false,
        heartbeatExists: true
      },
      {
        service: "game-server",
        instanceId: "schema-mixed-test-game-server-endpoint-v2",
        rawVersion: 2,
        source: "endpoint-v2",
        explicitEndpoints: true,
        heartbeatExists: true
      },
      {
        service: "game-proxy",
        instanceId: "schema-mixed-test-game-proxy-legacy-v1",
        rawVersion: 1,
        source: "legacy-v1",
        explicitEndpoints: false,
        heartbeatExists: true
      },
      {
        service: "game-proxy",
        instanceId: "schema-mixed-test-game-proxy-endpoint-v2",
        rawVersion: 2,
        source: "endpoint-v2",
        explicitEndpoints: true,
        heartbeatExists: true
      }
    ]
  );

  const serverAdmin = findCheck(report.discovery, "RegistryDiscoveryClient game-server.admin");
  assert.deepEqual(serverAdmin.discoveredInstanceIds, [
    "schema-mixed-test-game-server-endpoint-v2",
    "schema-mixed-test-game-server-legacy-v1"
  ]);
  assert.equal(
    resultByInstance(serverAdmin, "schema-mixed-test-game-server-legacy-v1").schema.source,
    "legacy-v1"
  );
  assert.deepEqual(
    {
      protocol: resultByInstance(serverAdmin, "schema-mixed-test-game-server-endpoint-v2").protocol,
      host: resultByInstance(serverAdmin, "schema-mixed-test-game-server-endpoint-v2").host,
      port: resultByInstance(serverAdmin, "schema-mixed-test-game-server-endpoint-v2").port
    },
    {
      protocol: "tcp",
      host: "127.0.40.22",
      port: 17522
    }
  );

  const legacySocket = findCheck(report.discovery, "RegistryDiscoveryClient game-server.local_socket");
  assert.deepEqual(legacySocket.discoveredInstanceIds, ["schema-mixed-test-game-server-legacy-v1"]);
  assert.equal(
    resultByInstance(legacySocket, "schema-mixed-test-game-server-legacy-v1").socket,
    "schema-mixed-test-game-server-legacy-v1.sock"
  );

  const proxyLocal = findCheck(report.discovery, "RegistryDiscoveryClient game-server.proxy-local");
  assert.deepEqual(proxyLocal.discoveredInstanceIds, ["schema-mixed-test-game-server-endpoint-v2"]);
  assert.equal(
    resultByInstance(proxyLocal, "schema-mixed-test-game-server-endpoint-v2").socket,
    "schema-mixed-test-game-server-endpoint-v2-proxy.sock"
  );

  const proxyClient = findCheck(report.discovery, "RegistryDiscoveryClient game-proxy.client");
  assert.deepEqual(proxyClient.discoveredInstanceIds, [
    "schema-mixed-test-game-proxy-endpoint-v2",
    "schema-mixed-test-game-proxy-legacy-v1"
  ]);
  assert.equal(resultByInstance(proxyClient, "schema-mixed-test-game-proxy-legacy-v1").protocol, "kcp");

  const proxyAdmin = findCheck(report.discovery, "RegistryDiscoveryClient game-proxy.admin");
  assert.deepEqual(proxyAdmin.discoveredInstanceIds, [
    "schema-mixed-test-game-proxy-endpoint-v2",
    "schema-mixed-test-game-proxy-legacy-v1"
  ]);
  assert.equal(resultByInstance(proxyAdmin, "schema-mixed-test-game-proxy-legacy-v1").protocol, "http");

  const adminApiServer = findCheck(report.adminApiHelpers, "admin-api helper game-server.admin");
  assert.deepEqual(adminApiServer.discoveredInstanceIds, [
    "schema-mixed-test-game-server-endpoint-v2",
    "schema-mixed-test-game-server-legacy-v1"
  ]);
  assert.equal(adminApiServer.fallbackUsed, false);

  const adminApiProxy = findCheck(report.adminApiHelpers, "admin-api helper game-proxy.admin");
  assert.deepEqual(adminApiProxy.discoveredInstanceIds, [
    "schema-mixed-test-game-proxy-endpoint-v2",
    "schema-mixed-test-game-proxy-legacy-v1"
  ]);
  assert.equal(adminApiProxy.fallbackUsed, false);
  assert.equal(resultByInstance(adminApiProxy, "schema-mixed-test-game-proxy-legacy-v1").protocol, "http");

  const proxySnapshot = findCheck(report.adminApiRefreshSnapshots, "admin-api refresh snapshot game-proxy.admin");
  assert.deepEqual(proxySnapshot.discoveredInstanceIds, [
    "schema-mixed-test-game-proxy-endpoint-v2",
    "schema-mixed-test-game-proxy-legacy-v1"
  ]);
  assert.equal(proxySnapshot.fallbackUsed, false);

  assert.equal(
    report.metrics.some((entry) =>
      entry.kind === "fallback_used" || entry.source === "fallback" || entry.reason === "fallback_used"
    ),
    false,
    "mixed rollout gate should not record or return fallback discovery"
  );
});

test("registry schema mixed rollout gate CLI emits compact JSON and exits cleanly", () => {
  const result = spawnSync(
    process.execPath,
    [
      "tools/check-registry-schema-mixed-rollout.js",
      "--memory",
      "--check-id", "schema-mixed-cli",
      "--registry-key-prefix", "test:schema-mixed-cli:",
      "--compact"
    ],
    { cwd: projectRoot, encoding: "utf8" }
  );

  assert.equal(result.status, 0, `stdout:\n${result.stdout}\nstderr:\n${result.stderr}`);
  assert.equal(result.stderr, "");

  const report = JSON.parse(result.stdout);
  assert.equal(report.ok, true);
  assert.equal(report.checkId, "schema-mixed-cli");
  assert.equal(report.registryKeyPrefix, "test:schema-mixed-cli:");
  assert.equal(report.fallbackUsed, false);
  assert.equal(
    findCheck(report.adminApiHelpers, "admin-api helper game-proxy.admin").results
      .some((endpoint) =>
        endpoint.instanceId === "schema-mixed-cli-game-proxy-legacy-v1" &&
        endpoint.schema.source === "legacy-v1" &&
        endpoint.protocol === "http"
      ),
    true
  );
});
