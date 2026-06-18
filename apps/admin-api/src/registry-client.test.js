import assert from "node:assert/strict";
import test from "node:test";

import { validateServiceInstance } from "../../../packages/service-registry/node/registry-schema.js";
import { configureLogger } from "./logger.js";
import { RegistryClient } from "./registry-client.js";

configureLogger({
  appName: "admin-api-registry-test",
  logEnableConsole: true,
  logEnableFile: false,
  logLevel: "off",
  logDir: "logs/admin-api-registry-test"
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

function createConfig(overrides = {}) {
  return {
    serviceName: "admin-api",
    serviceInstanceId: "admin-api-test-001",
    host: "10.10.0.5",
    port: 3101,
    adminApiRequireTls: true,
    adminApiRequireIpAllowlist: true,
    adminApiIpAllowlist: ["127.0.0.1", "10.0.0.0/24"],
    serviceBuildVersion: "2026.06.18+admin",
    ...overrides
  };
}

test("RegistryClient registers admin-api admin http endpoint and metadata", async () => {
  const redis = createRedisCapture();
  const config = createConfig();
  const client = new RegistryClient(redis, config);

  await client.register();

  const raw = redis.hashes.get("service:admin-api:instances:admin-api-test-001:data");
  assert.ok(raw);

  const payload = JSON.parse(raw);
  assert.deepEqual(validateServiceInstance(payload), { ok: true, errors: [] });
  assert.equal(payload.host, "10.10.0.5");
  assert.equal(payload.port, 3101);
  assert.deepEqual(payload.endpoints, [
    {
      name: "http",
      protocol: "http",
      host: "10.10.0.5",
      port: 3101,
      socket: "",
      visibility: "admin",
      metadata: {},
      healthy: true
    }
  ]);
  assert.deepEqual(payload.metadata, {
    require_tls: true,
    ip_allowlist_enabled: true,
    ip_allowlist: ["127.0.0.1", "10.0.0.0/24"],
    build_version: "2026.06.18+admin"
  });
});

test("RegistryClient metadata falls back to dev build version and empty allowlist", async () => {
  const redis = createRedisCapture();
  const client = new RegistryClient(
    redis,
    createConfig({
      adminApiRequireTls: false,
      adminApiRequireIpAllowlist: false,
      adminApiIpAllowlist: null,
      serviceBuildVersion: ""
    })
  );

  await client.register();

  const payload = JSON.parse(redis.hashes.get("service:admin-api:instances:admin-api-test-001:data"));
  assert.equal(payload.metadata.require_tls, false);
  assert.equal(payload.metadata.ip_allowlist_enabled, false);
  assert.deepEqual(payload.metadata.ip_allowlist, []);
  assert.equal(payload.metadata.build_version, "dev");
});

test("RegistryClient heartbeat and deregister use registry instance keys", async () => {
  const redis = createRedisCapture();
  const client = new RegistryClient(redis, createConfig());

  await client.register();
  client.startHeartbeat(60);
  client.stopHeartbeat();

  assert.deepEqual(redis.keys.get("heartbeat:admin-api:admin-api-test-001"), {
    ttl: 30,
    value: "1"
  });

  await client.deregister();

  assert.equal(redis.hashes.has("service:admin-api:instances:admin-api-test-001:data"), false);
  assert.equal(redis.keys.has("heartbeat:admin-api:admin-api-test-001"), false);
});
