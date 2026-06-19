import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";

import {
  createServiceInstancePayload,
  registryHeartbeatKey,
  registryInstanceKey
} from "../packages/service-registry/node/registry-schema.js";
import { MemoryRedis } from "../tools/check-registry-canary-lifecycle.js";
import { checkRegistryStaleKeys } from "../tools/check-registry-stale-keys.js";

const projectRoot = process.cwd();
const fixedGeneratedAt = "2026-06-19T00:00:00.000Z";

function endpoint(name, protocol, host, port, visibility, metadata = {}) {
  return {
    name,
    protocol,
    host,
    port,
    socket: "",
    visibility,
    metadata,
    healthy: true
  };
}

function metadata(serviceName, instanceId) {
  return {
    service_name: serviceName,
    service_instance_id: instanceId,
    instance_id: instanceId,
    build_version: "stale-key-test",
    zone: "test"
  };
}

function servicePayload(serviceName, instanceId, { host = "10.0.0.10", port = 7000 } = {}) {
  const meta = metadata(serviceName, instanceId);
  return createServiceInstancePayload({
    id: instanceId,
    name: serviceName,
    host,
    port,
    endpoints: [
      endpoint("client", "tcp", host, port, "internal", meta),
      endpoint("admin", "tcp", host, port + 500, "admin", meta)
    ],
    tags: ["test"],
    weight: 100,
    metadata: meta,
    registered_at: 1_713_000_000_000
  });
}

async function writeInstance(redis, prefix, payload, heartbeat = true) {
  await redis.hset(
    registryInstanceKey(prefix, payload.name, payload.id),
    "data",
    JSON.stringify(payload)
  );
  if (heartbeat) {
    await redis.setex(
      registryHeartbeatKey(prefix, payload.name, payload.id),
      60,
      "1"
    );
  }
}

function createTempDir(prefix) {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix));
}

function writeJson(filePath, value) {
  fs.writeFileSync(filePath, JSON.stringify(value, null, 2), "utf8");
}

test("registry stale-key scan succeeds when all fixture instances have heartbeat keys", async () => {
  const tempDir = createTempDir("myserver-registry-stale-ok-");
  try {
    const fixturePath = path.join(tempDir, "registry.json");
    writeJson(fixturePath, {
      instances: [
        {
          data: servicePayload("game-server", "game-a"),
          heartbeat: true
        },
        {
          data: servicePayload("game-proxy", "proxy-a", { host: "10.0.0.20", port: 4000 }),
          heartbeat: true
        }
      ]
    });

    const report = await checkRegistryStaleKeys({
      fixturePath,
      registryKeyPrefix: "test:",
      generatedAt: fixedGeneratedAt
    });

    assert.equal(report.ok, true, JSON.stringify(report.errors, null, 2));
    assert.equal(report.source, "fixture");
    assert.equal(report.registryKeyPrefix, "test:");
    assert.deepEqual(report.staleInstances, []);
    assert.deepEqual(report.errors, []);
    assert.equal(report.summary.instanceKeys, 2);
    assert.equal(report.summary.heartbeatPresent, 2);
    assert.equal(report.summary.staleInstances, 0);
    assert.equal(report.services["game-server"].instanceKeys, 1);
    assert.equal(report.services["game-proxy"].heartbeatPresent, 1);
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test("registry stale-key scan reports stale instances from memory Redis", async () => {
  const redis = new MemoryRedis({ now: () => 1_713_000_000_000 });
  const prefix = "test:stale:";
  await writeInstance(redis, prefix, servicePayload("game-server", "live"), true);
  await writeInstance(redis, prefix, servicePayload("game-server", "stale"), false);

  const report = await checkRegistryStaleKeys({
    redis,
    registryKeyPrefix: prefix,
    generatedAt: fixedGeneratedAt
  });

  assert.equal(report.ok, false);
  assert.equal(report.summary.instanceKeys, 2);
  assert.equal(report.summary.staleInstances, 1);
  assert.deepEqual(report.staleInstances.map((item) => ({
    service: item.service,
    instanceId: item.instanceId,
    instanceKey: item.instanceKey,
    heartbeatKey: item.heartbeatKey,
    registeredAt: item.registeredAt,
    endpoints: item.endpoints.map((endpointValue) => ({
      name: endpointValue.name,
      protocol: endpointValue.protocol,
      visibility: endpointValue.visibility
    }))
  })), [
    {
      service: "game-server",
      instanceId: "stale",
      instanceKey: "test:stale:service:game-server:instances:stale",
      heartbeatKey: "test:stale:heartbeat:game-server:stale",
      registeredAt: 1_713_000_000_000,
      endpoints: [
        {
          name: "admin",
          protocol: "tcp",
          visibility: "admin"
        },
        {
          name: "client",
          protocol: "tcp",
          visibility: "internal"
        }
      ]
    }
  ]);
});

test("registry stale-key report redacts credentials from registry URL", async () => {
  const redis = new MemoryRedis({ now: () => 1_713_000_000_000 });
  const report = await checkRegistryStaleKeys({
    redis,
    registryUrl: "redis://default:s3cr3t-token@redis.example.local:6379/0?password=query-secret&token=query-token&db=0",
    registryKeyPrefix: "test:redact:",
    generatedAt: fixedGeneratedAt
  });

  const reportJson = JSON.stringify(report);
  assert.equal(report.ok, true);
  assert.equal(
    report.registryUrl,
    "redis://***:***@redis.example.local:6379/0?password=***&token=***&db=0"
  );
  assert.equal(reportJson.includes("s3cr3t-token"), false);
  assert.equal(reportJson.includes("query-secret"), false);
  assert.equal(reportJson.includes("query-token"), false);
});

test("registry stale-key report does not echo invalid registry URLs", async () => {
  const redis = new MemoryRedis({ now: () => 1_713_000_000_000 });
  const unsafeUrl = "http://[invalid-host]:6379?password=leaked-secret";
  const report = await checkRegistryStaleKeys({
    redis,
    registryUrl: unsafeUrl,
    registryKeyPrefix: "test:redact-invalid:",
    generatedAt: fixedGeneratedAt
  });

  const reportJson = JSON.stringify(report);
  assert.equal(report.ok, true);
  assert.equal(report.registryUrl, "<invalid-registry-url>");
  assert.equal(reportJson.includes(unsafeUrl), false);
  assert.equal(reportJson.includes("leaked-secret"), false);
});

test("registry stale-key scan supports repeated and comma-separated service filters", async () => {
  const redis = new MemoryRedis({ now: () => 1_713_000_000_000 });
  const prefix = "test:filter:";
  await writeInstance(redis, prefix, servicePayload("game-server", "stale-game"), false);
  await writeInstance(redis, prefix, servicePayload("game-proxy", "stale-proxy"), false);
  await writeInstance(redis, prefix, servicePayload("admin-api", "live-admin", { host: "10.0.0.30", port: 3001 }), true);

  const report = await checkRegistryStaleKeys({
    redis,
    registryKeyPrefix: prefix,
    services: ["game-server,admin-api"],
    generatedAt: fixedGeneratedAt
  });

  assert.equal(report.ok, false);
  assert.deepEqual(report.requestedServices, ["admin-api", "game-server"]);
  assert.equal(report.summary.instanceKeys, 2);
  assert.equal(report.summary.staleInstances, 1);
  assert.deepEqual(Object.keys(report.services), ["admin-api", "game-server"]);
  assert.deepEqual(report.staleInstances.map((item) => `${item.service}:${item.instanceId}`), [
    "game-server:stale-game"
  ]);
});

test("registry stale-key scan reports invalid JSON, invalid schema, and missing data", async () => {
  const redis = new MemoryRedis({ now: () => 1_713_000_000_000 });
  const prefix = "test:invalid:";
  await redis.hset(`${prefix}service:game-server:instances:bad-json`, "data", "{bad");
  await redis.setex(`${prefix}heartbeat:game-server:bad-json`, 60, "1");
  await redis.hset(`${prefix}service:game-server:instances:bad-schema`, "data", JSON.stringify({ id: "", name: "game-server" }));
  await redis.setex(`${prefix}heartbeat:game-server:bad-schema`, 60, "1");
  await redis.hset(`${prefix}service:game-server:instances:missing-data`, "other", "value");
  await redis.setex(`${prefix}heartbeat:game-server:missing-data`, 60, "1");

  const report = await checkRegistryStaleKeys({
    redis,
    registryKeyPrefix: prefix,
    services: ["game-server"],
    generatedAt: fixedGeneratedAt
  });

  assert.equal(report.ok, false);
  assert.equal(report.summary.staleInstances, 0);
  assert.equal(report.summary.invalidJson, 1);
  assert.equal(report.summary.invalidSchema, 1);
  assert.equal(report.summary.missingData, 1);
  assert.deepEqual(report.errors.map((error) => ({
    code: error.code,
    instanceId: error.instanceId
  })).sort((left, right) => left.instanceId.localeCompare(right.instanceId)), [
    {
      code: "invalid_registry_json",
      instanceId: "bad-json"
    },
    {
      code: "invalid_registry_schema",
      instanceId: "bad-schema"
    },
    {
      code: "missing_registry_data",
      instanceId: "missing-data"
    }
  ]);
});

test("registry stale-key CLI exits non-zero with compact JSON when stale keys exist", () => {
  const tempDir = createTempDir("myserver-registry-stale-cli-");
  try {
    const fixturePath = path.join(tempDir, "registry.json");
    writeJson(fixturePath, {
      instances: [
        {
          data: servicePayload("game-server", "stale-cli"),
          heartbeat: false
        }
      ]
    });

    const result = spawnSync(
      process.execPath,
      [
        "tools/check-registry-stale-keys.js",
        "--fixture", fixturePath,
        "--registry-key-prefix", "cli:",
        "--service", "game-server",
        "--compact"
      ],
      { cwd: projectRoot, encoding: "utf8" }
    );

    assert.equal(result.status, 1, `stdout:\n${result.stdout}\nstderr:\n${result.stderr}`);
    assert.equal(result.stderr, "");
    const report = JSON.parse(result.stdout);
    assert.equal(report.ok, false);
    assert.equal(report.source, "fixture");
    assert.equal(report.summary.staleInstances, 1);
    assert.equal(report.staleInstances[0].heartbeatKey, "cli:heartbeat:game-server:stale-cli");
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});
