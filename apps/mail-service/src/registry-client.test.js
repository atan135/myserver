import assert from "node:assert/strict";
import test from "node:test";

import { validateServiceInstance } from "../../../packages/service-registry/node/registry-schema.js";
import { configureLogger } from "./logger.js";
import { RegistryClient } from "./registry-client.js";

configureLogger({
  appName: "mail-service-test",
  logEnableConsole: false,
  logEnableFile: false,
  logLevel: "off",
  logDir: "logs/mail-service-test"
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
    serviceName: "mail-service",
    serviceInstanceId: "mail-test-001",
    host: "10.10.0.3",
    port: 9103,
    mailPlayerAuthRequired: true,
    mailServiceToken: "test-mail-service-token",
    serviceBuildVersion: "2026.06.18+mail",
    ...overrides
  };
}

test("RegistryClient registers mail-service http endpoint and metadata", async () => {
  const redis = createRedisCapture();
  const config = createConfig();
  const client = new RegistryClient(redis, config);

  await client.register();

  const raw = redis.hashes.get("service:mail-service:instances:mail-test-001:data");
  assert.ok(raw);

  const payload = JSON.parse(raw);
  assert.deepEqual(validateServiceInstance(payload), { ok: true, errors: [] });
  assert.equal(payload.host, "10.10.0.3");
  assert.equal(payload.port, 9103);
  assert.deepEqual(payload.endpoints, [
    {
      name: "http",
      protocol: "http",
      host: "10.10.0.3",
      port: 9103,
      socket: "",
      visibility: "internal",
      metadata: {},
      healthy: true
    }
  ]);
  assert.deepEqual(payload.metadata, {
    player_auth_required: true,
    service_token_enabled: true,
    build_version: "2026.06.18+mail"
  });
});

test("RegistryClient metadata marks missing service token as disabled", async () => {
  const redis = createRedisCapture();
  const client = new RegistryClient(
    redis,
    createConfig({
      mailPlayerAuthRequired: false,
      mailServiceToken: "   ",
      serviceBuildVersion: ""
    })
  );

  await client.register();

  const payload = JSON.parse(redis.hashes.get("service:mail-service:instances:mail-test-001:data"));
  assert.equal(payload.metadata.player_auth_required, false);
  assert.equal(payload.metadata.service_token_enabled, false);
  assert.equal(payload.metadata.build_version, "dev");
});
