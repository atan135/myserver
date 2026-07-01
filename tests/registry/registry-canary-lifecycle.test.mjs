import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { test } from "node:test";

import {
  MemoryRedis,
  runRegistryCanaryLifecycle
} from "../../tools/check-registry-canary-lifecycle.js";

const projectRoot = process.cwd();

function findReadiness(report, service, instanceId) {
  const check = report.readiness.checks.find((candidate) =>
    candidate.service === service &&
    candidate.instanceId === instanceId
  );
  assert.ok(check, `missing readiness check for ${service}.${instanceId}`);
  return check;
}

function findDiscovery(report, name) {
  const check = report.discovery.checks.find((candidate) => candidate.name === name);
  assert.ok(check, `missing discovery check ${name}`);
  return check;
}

test("registry canary lifecycle validates group registration, discovery, readiness, and explicit deregister", async () => {
  const redis = new MemoryRedis({ now: () => 1713000000000 });
  const report = await runRegistryCanaryLifecycle({
    redis,
    canaryId: "canary-test",
    registryKeyPrefix: "test:canary:",
    generatedAt: "2026-06-19T00:00:00.000Z"
  });

  assert.equal(report.ok, true, JSON.stringify(report.errors, null, 2));
  assert.equal(report.mode, "memory");
  assert.equal(report.registryKeyPrefix, "test:canary:");
  assert.deepEqual(report.errors, []);

  assert.deepEqual(
    report.registration.map((state) => ({
      service: state.service,
      instanceId: state.instanceId,
      instanceRecordExists: state.instanceRecordExists,
      heartbeatExists: state.heartbeatExists
    })),
    [
      {
        service: "admin-api",
        instanceId: "canary-test-admin-api",
        instanceRecordExists: true,
        heartbeatExists: true
      },
      {
        service: "game-server",
        instanceId: "canary-test-game-server",
        instanceRecordExists: true,
        heartbeatExists: true
      },
      {
        service: "game-proxy",
        instanceId: "canary-test-game-proxy",
        instanceRecordExists: true,
        heartbeatExists: true
      }
    ]
  );

  const gameServerReady = findReadiness(report, "game-server", "canary-test-game-server");
  assert.equal(gameServerReady.ok, true);
  assert.deepEqual(
    gameServerReady.endpoints.map((endpoint) => ({
      name: endpoint.name,
      ok: endpoint.ok,
      actual: endpoint.actual
    })),
    [
      {
        name: "admin",
        ok: true,
        actual: {
          name: "admin",
          protocol: "tcp",
          host: "127.0.10.10",
          port: 17500,
          socket: "",
          visibility: "admin",
          healthy: true
        }
      },
      {
        name: "proxy-local",
        ok: true,
        actual: {
          name: "proxy-local",
          protocol: "local_socket",
          host: "",
          port: 0,
          socket: "canary-test-game-server-proxy.sock",
          visibility: "local",
          healthy: true
        }
      }
    ]
  );

  const gameServerAdmin = findDiscovery(report, "game-server.admin");
  assert.deepEqual(gameServerAdmin.discoveredInstanceIds, ["canary-test-game-server"]);
  assert.deepEqual(gameServerAdmin.forbiddenInstanceIds, [
    "canary-test-game-server-missing-heartbeat",
    "canary-test-game-server-unhealthy",
    "canary-test-game-server-unhealthy-endpoint"
  ]);

  const adminApiGameProxy = findDiscovery(report, "admin-api game-proxy.admin");
  assert.deepEqual(adminApiGameProxy.endpoints.map((endpoint) => ({
    instanceId: endpoint.instanceId,
    protocol: endpoint.endpoint.protocol,
    source: endpoint.source,
    fallback: endpoint.fallback
  })), [
    {
      instanceId: "canary-test-game-proxy",
      protocol: "http",
      source: "registry",
      fallback: false
    }
  ]);

  assert.equal(report.ttlFallback.ok, true);
  assert.equal(report.ttlFallback.beforeExpiry.instanceRecordExists, true);
  assert.equal(report.ttlFallback.beforeExpiry.heartbeatExists, true);
  assert.equal(report.ttlFallback.beforeExpiry.discoveryVisible, true);
  assert.equal(report.ttlFallback.afterExpiry.instanceRecordExists, true);
  assert.equal(report.ttlFallback.afterExpiry.heartbeatExists, false);
  assert.equal(report.ttlFallback.afterExpiry.discoveryVisible, false);
  assert.match(report.ttlFallback.conclusion, /does not delete the instance record/);

  assert.equal(report.shutdown.ok, true);
  assert.equal(report.shutdown.mode, "explicit_deregister");
  assert.deepEqual(
    report.shutdown.instances.map((state) => ({
      service: state.service,
      instanceId: state.instanceId,
      instanceRecordExists: state.instanceRecordExists,
      heartbeatExists: state.heartbeatExists,
      discoveryVisible: state.discoveryVisible
    })),
    [
      {
        service: "admin-api",
        instanceId: "canary-test-admin-api",
        instanceRecordExists: false,
        heartbeatExists: false,
        discoveryVisible: false
      },
      {
        service: "game-server",
        instanceId: "canary-test-game-server",
        instanceRecordExists: false,
        heartbeatExists: false,
        discoveryVisible: false
      },
      {
        service: "game-proxy",
        instanceId: "canary-test-game-proxy",
        instanceRecordExists: false,
        heartbeatExists: false,
        discoveryVisible: false
      }
    ]
  );
});

test("registry canary lifecycle CLI emits compact JSON and exits cleanly", () => {
  const result = spawnSync(
    process.execPath,
    [
      "tools/check-registry-canary-lifecycle.js",
      "--memory",
      "--canary-id", "canary-cli",
      "--registry-key-prefix", "test:canary-cli:",
      "--compact"
    ],
    { cwd: projectRoot, encoding: "utf8" }
  );

  assert.equal(result.status, 0, `stdout:\n${result.stdout}\nstderr:\n${result.stderr}`);
  assert.equal(result.stderr, "");

  const report = JSON.parse(result.stdout);
  assert.equal(report.ok, true);
  assert.equal(report.canaryId, "canary-cli");
  assert.equal(report.registryKeyPrefix, "test:canary-cli:");
  assert.equal(report.discovery.checks.length, 6);
  assert.equal(report.shutdown.mode, "explicit_deregister");
  assert.equal(report.ttlFallback.mode, "ttl_fallback_only");
});
