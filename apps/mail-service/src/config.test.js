import assert from "node:assert/strict";
import fs from "node:fs";
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
  "GAME_SERVER_ADMIN_HOST",
  "GAME_SERVER_ADMIN_PORT",
  "REGISTRY_ENABLED",
  "DISCOVERY_REQUIRED",
  "DISALLOW_LEGACY_DIRECT_CONFIG",
  "REGISTRY_KEY_PREFIX",
  "REDIS_KEY_PREFIX",
  "MAIL_PLAYER_AUTH_REQUIRED",
  "MAIL_SERVICE_TOKEN",
  "SERVICE_NAME",
  "SERVICE_INSTANCE_ID",
  "SERVICE_ZONE",
  "SERVICE_BUILD_VERSION",
  "SERVICE_BIND_HOST",
  "SERVICE_PUBLIC_HOST",
  "SERVICE_ADVERTISED_HOST",
  "MAIL_PUBLIC_HOST",
  "HOST",
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

async function withCapturedWarnings(values, callback) {
  const warnings = [];
  const originalWarn = console.warn;
  console.warn = (...args) => {
    warnings.push(args.join(" "));
  };

  try {
    return await withEnv(values, (getConfig) => callback(getConfig, warnings));
  } finally {
    console.warn = originalWarn;
  }
}

test("mail-service base env example does not enable legacy direct game admin config by default", () => {
  const envExample = fs.readFileSync(new URL("../.env.example", import.meta.url), "utf8");

  assert.doesNotMatch(envExample, /^GAME_SERVER_ADMIN_HOST=/m);
  assert.doesNotMatch(envExample, /^GAME_SERVER_ADMIN_PORT=/m);
  assert.match(envExample, /Local fallback only/);
  assert.match(envExample, /Ignored in strict\/test\/production discovery/);
  assert.match(envExample, /^# GAME_SERVER_ADMIN_HOST=127\.0\.0\.1$/m);
  assert.match(envExample, /^# GAME_SERVER_ADMIN_PORT=7500$/m);
});

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

test("mail-service ignores direct consumer endpoint env outside local fallback", async () => {
  await withCapturedWarnings({
    APP_ENV: "test",
    REGISTRY_ENABLED: "true",
    GAME_SERVER_ADMIN_HOST: "203.0.113.20",
    GAME_SERVER_ADMIN_PORT: "17500"
  }, (getConfig, warnings) => {
    const config = getConfig();

    assert.equal(config.localDiscoveryFallbackEnabled, false);
    assert.equal(config.gameServerAdminHost, "127.0.0.1");
    assert.equal(config.gameServerAdminPort, 7500);
    assert.deepEqual(
      config.legacyDirectConfigWarnings.map((warning) => warning.name),
      ["GAME_SERVER_ADMIN_HOST", "GAME_SERVER_ADMIN_PORT"]
    );
    assert.equal(warnings.length, 2);
    assert.match(warnings[0], /GAME_SERVER_ADMIN_HOST is ignored/);
  });
});

test("mail-service does not warn for local fallback direct endpoint env", async () => {
  await withCapturedWarnings({
    NODE_ENV: "development",
    GAME_SERVER_ADMIN_HOST: "127.0.0.2"
  }, (getConfig, warnings) => {
    const config = getConfig();

    assert.equal(config.localDiscoveryFallbackEnabled, true);
    assert.deepEqual(config.legacyDirectConfigWarnings, []);
    assert.deepEqual(warnings, []);
  });
});

test("mail-service allows legacy direct endpoint with APP_ENV local", async () => {
  await withCapturedWarnings({
    APP_ENV: "local",
    GAME_SERVER_ADMIN_HOST: "127.0.0.2",
    GAME_SERVER_ADMIN_PORT: "17500"
  }, (getConfig, warnings) => {
    const config = getConfig();

    assert.equal(config.localDiscoveryFallbackEnabled, true);
    assert.equal(config.gameServerAdminHost, "127.0.0.2");
    assert.equal(config.gameServerAdminPort, 17500);
    assert.deepEqual(config.legacyDirectConfigWarnings, []);
    assert.deepEqual(warnings, []);
  });
});

test("mail-service ignores legacy direct endpoint when APP_ENV is development without NODE_ENV development", async () => {
  await withCapturedWarnings({
    APP_ENV: "development",
    GAME_SERVER_ADMIN_HOST: "203.0.113.20",
    GAME_SERVER_ADMIN_PORT: "17500"
  }, (getConfig, warnings) => {
    const config = getConfig();

    assert.equal(config.localDiscoveryFallbackEnabled, false);
    assert.equal(config.gameServerAdminHost, "127.0.0.1");
    assert.equal(config.gameServerAdminPort, 7500);
    assert.deepEqual(
      config.legacyDirectConfigWarnings.map((warning) => warning.name),
      ["GAME_SERVER_ADMIN_HOST", "GAME_SERVER_ADMIN_PORT"]
    );
    assert.equal(warnings.length, 2);
  });
});

test("mail-service treats staging as strict discovery for legacy direct endpoint", async () => {
  await withCapturedWarnings({
    APP_ENV: "staging",
    REGISTRY_ENABLED: "true",
    GAME_SERVER_ADMIN_HOST: "203.0.113.20"
  }, (getConfig, warnings) => {
    const config = getConfig();

    assert.equal(config.registryDiscoveryRequired, true);
    assert.equal(config.localDiscoveryFallbackEnabled, false);
    assert.equal(config.gameServerAdminHost, "127.0.0.1");
    assert.equal(warnings.length, 1);
  });
});

test("mail-service rejects legacy direct config when migration complete switch is enabled", async () => {
  await assert.rejects(
    () => withEnv({
      NODE_ENV: "development",
      DISALLOW_LEGACY_DIRECT_CONFIG: "true",
      GAME_SERVER_ADMIN_HOST: "127.0.0.2",
      GAME_SERVER_ADMIN_PORT: "17500"
    }, (getConfig) => getConfig()),
    /DISALLOW_LEGACY_DIRECT_CONFIG=true forbids legacy direct config: GAME_SERVER_ADMIN_HOST, GAME_SERVER_ADMIN_PORT/
  );
});

test("mail-service test environment rejects legacy direct config when migration complete switch is enabled", async () => {
  await assert.rejects(
    () => withEnv({
      APP_ENV: "test",
      REGISTRY_ENABLED: "true",
      DISCOVERY_REQUIRED: "true",
      DISALLOW_LEGACY_DIRECT_CONFIG: "true",
      GAME_SERVER_ADMIN_HOST: "127.0.0.2"
    }, (getConfig) => getConfig()),
    /DISALLOW_LEGACY_DIRECT_CONFIG=true forbids legacy direct config: GAME_SERVER_ADMIN_HOST/
  );
});

test("mail-service accepts migration complete switch when legacy direct config is absent", async () => {
  await withEnv({
    NODE_ENV: "development",
    DISALLOW_LEGACY_DIRECT_CONFIG: "true"
  }, (getConfig) => {
    const config = getConfig();

    assert.equal(config.disallowLegacyDirectConfig, true);
    assert.deepEqual(config.legacyDirectConfigWarnings, []);
  });
});

test("mail-service config reads service identity and build version", async () => {
  await withEnv({
    SERVICE_NAME: "mail-service-blue",
    SERVICE_INSTANCE_ID: "mail-blue-001",
    SERVICE_ZONE: "zone-a",
    SERVICE_BUILD_VERSION: "2026.06.18+abc123"
  }, (getConfig) => {
    const config = getConfig();

    assert.equal(config.serviceName, "mail-service-blue");
    assert.equal(config.serviceInstanceId, "mail-blue-001");
    assert.equal(config.serviceZone, "zone-a");
    assert.equal(config.serviceBuildVersion, "2026.06.18+abc123");
  });
});

test("mail-service config defaults service build version to dev", async () => {
  await withEnv({}, (getConfig) => {
    const config = getConfig();

    assert.equal(config.serviceName, "mail-service");
    assert.equal(config.serviceInstanceId, "mail-001");
    assert.equal(config.serviceZone, "local");
    assert.equal(config.serviceBuildVersion, "dev");
  });
});

test("mail-service separates bind host from advertised registry host", async () => {
  await withEnv({
    SERVICE_BIND_HOST: "0.0.0.0",
    SERVICE_PUBLIC_HOST: "10.0.0.12",
    MAIL_PUBLIC_HOST: "10.0.0.99",
    HOST: "127.0.0.9"
  }, (getConfig) => {
    const config = getConfig();

    assert.equal(config.host, "0.0.0.0");
    assert.equal(config.bindHost, "0.0.0.0");
    assert.equal(config.advertisedHost, "10.0.0.12");
  });

  await withEnv({
    SERVICE_BIND_HOST: "0.0.0.0"
  }, (getConfig) => {
    assert.equal(getConfig().advertisedHost, "127.0.0.1");
  });

  await withEnv({
    HOST: "0.0.0.0"
  }, (getConfig) => {
    const config = getConfig();

    assert.equal(config.host, "0.0.0.0");
    assert.equal(config.advertisedHost, "127.0.0.1");
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

test("mail-service reads registry key prefix with Redis prefix fallback", async () => {
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

test("mail-service DISCOVERY_REQUIRED=true rejects registry disabled", async () => {
  await assert.rejects(
    () => withEnv({
      REGISTRY_ENABLED: "false",
      DISCOVERY_REQUIRED: "true"
    }, (getConfig) => getConfig()),
    /DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true/
  );
});

test("mail-service test environment rejects registry disabled", async () => {
  await assert.rejects(
    () => withEnv({
      APP_ENV: "test",
      REGISTRY_ENABLED: "false"
    }, (getConfig) => getConfig()),
    /DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true/
  );
});

test("mail-service test environment ignores DISCOVERY_REQUIRED=false override", async () => {
  await assert.rejects(
    () => withEnv({
      APP_ENV: "test",
      DISCOVERY_REQUIRED: "false",
      REGISTRY_ENABLED: "false"
    }, (getConfig) => getConfig()),
    /DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true/
  );
});

test("mail-service production environment ignores DISCOVERY_REQUIRED=false override", async () => {
  await assert.rejects(
    () => withEnv({
      NODE_ENV: "production",
      DISCOVERY_REQUIRED: "false",
      REGISTRY_ENABLED: "false",
      TICKET_SECRET: "prod-ticket-secret-with-enough-entropy",
      MAIL_SERVICE_TOKEN: "prod-mail-service-token-with-enough-entropy"
    }, (getConfig) => getConfig()),
    /DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true/
  );
});
