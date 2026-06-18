import assert from "node:assert/strict";
import test from "node:test";

import { validateServiceInstance } from "../../../packages/service-registry/node/registry-schema.js";
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

  return {
    hashes,
    async hset(key, field, value) {
      hashes.set(`${key}:${field}`, value);
    }
  };
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
      metadata: {},
      healthy: true
    }
  ]);
  assert.deepEqual(payload.metadata, {
    read_auth_required: true,
    cache_ttl_seconds: 30,
    build_version: "2026.06.18+announce"
  });
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
});
