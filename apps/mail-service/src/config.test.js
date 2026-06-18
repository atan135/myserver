import assert from "node:assert/strict";
import test from "node:test";

const CONFIG_ENV_NAMES = [
  "NODE_ENV",
  "APP_ENV",
  "GAME_ADMIN_ACTOR",
  "GAME_ADMIN_TOKEN",
  "GAME_ADMIN_CONNECT_TIMEOUT_MS",
  "GAME_ADMIN_WRITE_TIMEOUT_MS",
  "GAME_ADMIN_READ_TIMEOUT_MS",
  "GAME_ADMIN_MAX_RESPONSE_BYTES",
  "REGISTRY_ENABLED",
  "DISCOVERY_REQUIRED",
  "MAIL_PLAYER_AUTH_REQUIRED",
  "MAIL_SERVICE_TOKEN",
  "SERVICE_BUILD_VERSION",
  "TICKET_SECRET"
];

async function withEnv(values, callback) {
  const saved = new Map(CONFIG_ENV_NAMES.map((name) => [name, process.env[name]]));
  for (const name of CONFIG_ENV_NAMES) {
    delete process.env[name];
  }
  Object.assign(process.env, values);

  try {
    const module = await import(`./config.js?test=${Date.now()}-${Math.random()}`);
    return await callback(module.getConfig);
  } finally {
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

test("mail-service config reads optional game admin actor", async () => {
  await withEnv({ GAME_ADMIN_ACTOR: "mail-ops" }, (getConfig) => {
    const config = getConfig();

    assert.equal(config.gameAdminActor, "mail-ops");
  });
});

test("mail-service game admin network limits fall back on invalid values", async () => {
  await withEnv({
    GAME_ADMIN_CONNECT_TIMEOUT_MS: "invalid",
    GAME_ADMIN_WRITE_TIMEOUT_MS: "0",
    GAME_ADMIN_READ_TIMEOUT_MS: "-10",
    GAME_ADMIN_MAX_RESPONSE_BYTES: ""
  }, (getConfig) => {
    const config = getConfig();

    assert.equal(config.gameAdminConnectTimeoutMs, 3000);
    assert.equal(config.gameAdminWriteTimeoutMs, 3000);
    assert.equal(config.gameAdminReadTimeoutMs, 3000);
    assert.equal(config.gameAdminMaxResponseBytes, 1048576);
  });
});

test("mail-service game admin network limits read positive values", async () => {
  await withEnv({
    GAME_ADMIN_CONNECT_TIMEOUT_MS: "101",
    GAME_ADMIN_WRITE_TIMEOUT_MS: "202",
    GAME_ADMIN_READ_TIMEOUT_MS: "303",
    GAME_ADMIN_MAX_RESPONSE_BYTES: "4097"
  }, (getConfig) => {
    const config = getConfig();

    assert.equal(config.gameAdminConnectTimeoutMs, 101);
    assert.equal(config.gameAdminWriteTimeoutMs, 202);
    assert.equal(config.gameAdminReadTimeoutMs, 303);
    assert.equal(config.gameAdminMaxResponseBytes, 4097);
  });
});

test("mail-service config reads service build version", async () => {
  await withEnv({ SERVICE_BUILD_VERSION: "2026.06.18+abc123" }, (getConfig) => {
    const config = getConfig();

    assert.equal(config.serviceBuildVersion, "2026.06.18+abc123");
  });
});

test("mail-service config defaults service build version to dev", async () => {
  await withEnv({}, (getConfig) => {
    const config = getConfig();

    assert.equal(config.serviceBuildVersion, "dev");
  });
});

test("mail-service config reads registry discovery flags", async () => {
  await withEnv({
    REGISTRY_ENABLED: "true",
    DISCOVERY_REQUIRED: "true"
  }, (getConfig) => {
    const config = getConfig();

    assert.equal(config.registryDiscoveryEnabled, true);
    assert.equal(config.registryDiscoveryRequired, true);
  });
});

test("mail-service DISCOVERY_REQUIRED=true rejects registry disabled", async () => {
  await assert.rejects(
    () => withEnv({
      REGISTRY_ENABLED: "false",
      DISCOVERY_REQUIRED: "true"
    }, (getConfig) => getConfig()),
    /DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true/
  );
});
