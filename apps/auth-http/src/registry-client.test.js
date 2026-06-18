import assert from "node:assert/strict";
import test from "node:test";

import { validateServiceInstance } from "../../../packages/service-registry/node/registry-schema.js";
import { configureLogger } from "./logger.js";
import { RegistryClient } from "./registry-client.js";

configureLogger({
  appName: "auth-http-registry-test",
  logEnableConsole: false,
  logEnableFile: false,
  logLevel: "off",
  logDir: "logs/auth-http-registry-test"
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
    serviceName: "auth-http",
    serviceInstanceId: "auth-http-test-001",
    host: "10.10.0.2",
    port: 3100,
    strictSecurity: true,
    ticketValidateEnabled: true,
    serviceBuildVersion: "2026.06.18+auth",
    serviceZone: "zone-auth",
    ...overrides
  };
}

test("RegistryClient registers auth-http public http endpoint and metadata", async () => {
  const redis = createRedisCapture();
  const config = createConfig();
  const client = new RegistryClient(redis, config);

  await client.register();

  const raw = redis.hashes.get("service:auth-http:instances:auth-http-test-001:data");
  assert.ok(raw);

  const payload = JSON.parse(raw);
  assert.deepEqual(validateServiceInstance(payload), { ok: true, errors: [] });
  assert.equal(payload.host, "10.10.0.2");
  assert.equal(payload.port, 3100);
  assert.deepEqual(payload.endpoints, [
    {
      name: "http",
      protocol: "http",
      host: "10.10.0.2",
      port: 3100,
      socket: "",
      visibility: "public",
      metadata: {
        service_name: "auth-http",
        service_instance_id: "auth-http-test-001",
        build_version: "2026.06.18+auth",
        zone: "zone-auth"
      },
      healthy: true
    }
  ]);
  assert.deepEqual(payload.metadata, {
    service_name: "auth-http",
    service_instance_id: "auth-http-test-001",
    strict_security: true,
    ticket_validation_enabled: true,
    build_version: "2026.06.18+auth",
    zone: "zone-auth"
  });
});

test("RegistryClient metadata falls back to dev build version", async () => {
  const redis = createRedisCapture();
  const client = new RegistryClient(
    redis,
    createConfig({
      strictSecurity: false,
      ticketValidateEnabled: false,
      serviceBuildVersion: ""
    })
  );

  await client.register();

  const payload = JSON.parse(redis.hashes.get("service:auth-http:instances:auth-http-test-001:data"));
  assert.equal(payload.metadata.strict_security, false);
  assert.equal(payload.metadata.ticket_validation_enabled, false);
  assert.equal(payload.metadata.build_version, "dev");
  assert.equal(payload.metadata.zone, "zone-auth");
});

test("RegistryClient uses registry key prefix for registration", async () => {
  const redis = createRedisCapture();
  const client = new RegistryClient(redis, createConfig({ registryKeyPrefix: "test:" }));

  await client.register();

  assert.ok(redis.hashes.has("test:service:auth-http:instances:auth-http-test-001:data"));
  assert.equal(redis.hashes.has("service:auth-http:instances:auth-http-test-001:data"), false);
});
