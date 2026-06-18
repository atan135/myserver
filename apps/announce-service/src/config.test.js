import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

const CONFIG_ENV_NAMES = [
  "NODE_ENV",
  "APP_ENV",
  "HOST",
  "ANNOUNCE_PORT",
  "ANNOUNCE_CACHE_TTL_SECONDS",
  "ANNOUNCE_ADMIN_TOKEN",
  "ANNOUNCE_READ_AUTH_REQUIRED",
  "ANNOUNCE_READ_TOKEN",
  "TICKET_SECRET",
  "REGISTRY_ENABLED",
  "DISCOVERY_REQUIRED",
  "REGISTRY_KEY_PREFIX",
  "REDIS_KEY_PREFIX",
  "SERVICE_NAME",
  "SERVICE_INSTANCE_ID",
  "SERVICE_BUILD_VERSION"
];

async function withEnv(values, callback) {
  const saved = new Map(CONFIG_ENV_NAMES.map((name) => [name, process.env[name]]));
  const previousCwd = process.cwd();
  const tempCwd = fs.mkdtempSync(path.join(os.tmpdir(), "announce-service-config-test-"));
  for (const name of CONFIG_ENV_NAMES) {
    delete process.env[name];
  }
  Object.assign(process.env, values);
  process.chdir(tempCwd);

  try {
    const module = await import(`./config.js?test=${Date.now()}-${Math.random()}`);
    return await callback(module.getConfig);
  } finally {
    process.chdir(previousCwd);
    fs.rmSync(tempCwd, { recursive: true, force: true });
    for (const name of CONFIG_ENV_NAMES) {
      const value = saved.get(name);
      if (value === undefined) {
        delete process.env[name];
      } else {
        process.env[name] = value;
      }
    }
  }
}

test("announce-service config reads service build version", async () => {
  await withEnv({ SERVICE_BUILD_VERSION: "2026.06.18+abc123" }, (getConfig) => {
    const config = getConfig();

    assert.equal(config.serviceBuildVersion, "2026.06.18+abc123");
  });
});

test("announce-service config defaults service build version to dev", async () => {
  await withEnv({}, (getConfig) => {
    const config = getConfig();

    assert.equal(config.serviceBuildVersion, "dev");
  });
});

test("announce-service config reads registry discovery flags", async () => {
  await withEnv({
    REGISTRY_ENABLED: "true",
    DISCOVERY_REQUIRED: "true"
  }, (getConfig) => {
    const config = getConfig();

    assert.equal(config.registryDiscoveryEnabled, true);
    assert.equal(config.registryDiscoveryRequired, true);
  });
});

test("announce-service reads registry key prefix with Redis prefix fallback", async () => {
  await withEnv({
    REGISTRY_KEY_PREFIX: "registry:",
    REDIS_KEY_PREFIX: "redis:"
  }, (getConfig) => {
    assert.equal(getConfig().registryKeyPrefix, "registry:");
  });

  await withEnv({ REDIS_KEY_PREFIX: "redis:" }, (getConfig) => {
    assert.equal(getConfig().registryKeyPrefix, "redis:");
  });
});

test("announce-service DISCOVERY_REQUIRED=true rejects registry disabled", async () => {
  await assert.rejects(
    () => withEnv({
      REGISTRY_ENABLED: "false",
      DISCOVERY_REQUIRED: "true"
    }, (getConfig) => getConfig()),
    /DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true/
  );
});

test("announce-service test environment requires registry discovery by default", async () => {
  await assert.rejects(
    () => withEnv({
      APP_ENV: "test",
      REGISTRY_ENABLED: "false"
    }, (getConfig) => getConfig()),
    /DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true/
  );
});
