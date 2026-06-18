import assert from "node:assert/strict";
import test from "node:test";

import {
  getRegistryLifecycleMetricsSnapshot,
  resetRegistryLifecycleMetrics,
  validateServiceInstance
} from "../../../packages/service-registry/node/registry-schema.js";
import { configureLogger } from "./logger.js";
import { RegistryClient } from "./registry-client.js";

configureLogger({
  appName: "announce-service-test",
  logEnableConsole: false,
  logEnableFile: false,
  logLevel: "off",
  logDir: "logs/announce-service-test"
});

function createRedisCapture() {
  const hashes = new Map();
  const keys = new Map();

  return {
    hashes,
    keys,
    async hset(key, field, value) {
      hashes.set(`${key}:${field}`, value);
    },
    async setex(key, ttl, value) {
      keys.set(key, { ttl, value });
    },
    async del(key) {
      hashes.delete(`${key}:data`);
      keys.delete(key);
    }
  };
}

function createFailingRedis(operation) {
  const redis = createRedisCapture();
  if (operation === "hset") {
    redis.hset = async () => {
      throw new Error("HSET_FAILED");
    };
  }
  if (operation === "setex") {
    redis.setex = async () => {
      throw new Error("SETEX_FAILED");
    };
  }
  if (operation === "del") {
    redis.del = async () => {
      throw new Error("DEL_FAILED");
    };
  }
  return redis;
}

function createConfig(overrides = {}) {
  return {
    serviceName: "announce-service",
    serviceInstanceId: "announce-test-001",
    host: "10.10.0.4",
    port: 9104,
    announceReadAuthRequired: true,
    announceCacheTtlSeconds: 30,
    serviceBuildVersion: "2026.06.18+announce",
    serviceZone: "zone-announce",
    ...overrides
  };
}

test("RegistryClient registers announce-service http endpoint and metadata", async () => {
  const redis = createRedisCapture();
  const config = createConfig();
  const client = new RegistryClient(redis, config);

  await client.register();

  const raw = redis.hashes.get("service:announce-service:instances:announce-test-001:data");
  assert.ok(raw);

  const payload = JSON.parse(raw);
  assert.deepEqual(validateServiceInstance(payload), { ok: true, errors: [] });
  assert.equal(payload.host, "10.10.0.4");
  assert.equal(payload.port, 9104);
  assert.deepEqual(payload.endpoints, [
    {
      name: "http",
      protocol: "http",
      host: "10.10.0.4",
      port: 9104,
      socket: "",
      visibility: "internal",
      metadata: {
        service_name: "announce-service",
        service_instance_id: "announce-test-001",
        build_version: "2026.06.18+announce",
        zone: "zone-announce"
      },
      healthy: true
    }
  ]);
  assert.deepEqual(payload.metadata, {
    service_name: "announce-service",
    service_instance_id: "announce-test-001",
    read_auth_required: true,
    cache_ttl_seconds: 30,
    build_version: "2026.06.18+announce",
    zone: "zone-announce"
  });
});

test("RegistryClient publishes advertised host instead of bind host", async () => {
  const redis = createRedisCapture();
  const client = new RegistryClient(
    redis,
    createConfig({
      host: "0.0.0.0",
      advertisedHost: "10.10.0.24"
    })
  );

  await client.register();

  const payload = JSON.parse(redis.hashes.get("service:announce-service:instances:announce-test-001:data"));
  assert.equal(payload.host, "10.10.0.24");
  assert.equal(payload.endpoints[0].host, "10.10.0.24");
});

test("RegistryClient never publishes wildcard advertised host", async () => {
  for (const advertisedHost of ["0.0.0.0", "::", "[::]", "   "]) {
    const redis = createRedisCapture();
    const client = new RegistryClient(
      redis,
      createConfig({
        host: "0.0.0.0",
        advertisedHost
      })
    );

    await client.register();

    const payload = JSON.parse(redis.hashes.get("service:announce-service:instances:announce-test-001:data"));
    assert.equal(payload.host, "127.0.0.1");
    assert.equal(payload.endpoints[0].host, "127.0.0.1");
  }
});

test("RegistryClient metadata falls back to dev build version", async () => {
  const redis = createRedisCapture();
  const client = new RegistryClient(
    redis,
    createConfig({
      announceReadAuthRequired: false,
      announceCacheTtlSeconds: 45,
      serviceBuildVersion: ""
    })
  );

  await client.register();

  const payload = JSON.parse(
    redis.hashes.get("service:announce-service:instances:announce-test-001:data")
  );
  assert.equal(payload.metadata.read_auth_required, false);
  assert.equal(payload.metadata.cache_ttl_seconds, 45);
  assert.equal(payload.metadata.build_version, "dev");
  assert.equal(payload.metadata.zone, "zone-announce");
});

test("RegistryClient records lifecycle metrics for register heartbeat and deregister failures", async () => {
  resetRegistryLifecycleMetrics();

  await assert.rejects(
    () => new RegistryClient(createFailingRedis("hset"), createConfig()).register(),
    /HSET_FAILED/
  );

  const heartbeatClient = new RegistryClient(createFailingRedis("setex"), createConfig());
  heartbeatClient.startHeartbeat(60);
  await new Promise((resolve) => setImmediate(resolve));
  heartbeatClient.stopHeartbeat();

  await assert.rejects(
    () => new RegistryClient(createFailingRedis("del"), createConfig()).deregister(),
    /DEL_FAILED/
  );

  assert.deepEqual(
    getRegistryLifecycleMetricsSnapshot().map(({ kind, service, endpoint, instance_id, reason, count }) => ({
      kind,
      service,
      endpoint,
      instance_id,
      reason,
      count
    })),
    [
      {
        kind: "deregister_failed",
        service: "announce-service",
        endpoint: "",
        instance_id: "announce-test-001",
        reason: "redis_error",
        count: 1
      },
      {
        kind: "heartbeat_failed",
        service: "announce-service",
        endpoint: "",
        instance_id: "announce-test-001",
        reason: "redis_error",
        count: 1
      },
      {
        kind: "register_failed",
        service: "announce-service",
        endpoint: "http",
        instance_id: "announce-test-001",
        reason: "redis_error",
        count: 1
      }
    ]
  );

  resetRegistryLifecycleMetrics();
});
