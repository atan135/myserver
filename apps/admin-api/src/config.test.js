import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

const CONFIG_ENV_KEYS = [
  "NODE_ENV",
  "DATABASE_URL",
  "DB_POOL_SIZE",
  "JWT_SECRET",
  "GAME_ADMIN_TOKEN",
  "ADMIN_PASSWORD",
  "ADMIN_API_REQUIRE_TLS",
  "ADMIN_API_REQUIRE_IP_ALLOWLIST",
  "ADMIN_API_IP_ALLOWLIST",
  "TRUST_PROXY",
  "TRUSTED_PROXIES",
  "GAME_ADMIN_CONNECT_TIMEOUT_MS",
  "GAME_ADMIN_WRITE_TIMEOUT_MS",
  "GAME_ADMIN_READ_TIMEOUT_MS",
  "GAME_ADMIN_MAX_RESPONSE_BYTES",
  "GAME_SERVER_ADMIN_HOST",
  "GAME_SERVER_ADMIN_PORT",
  "GAME_PROXY_ADMIN_HOST",
  "GAME_PROXY_ADMIN_PORT",
  "GAME_PROXY_ADMIN_TOKEN",
  "GAME_PROXY_ADMIN_READ_TOKEN",
  "GAME_PROXY_ADMIN_REQUEST_TIMEOUT_MS",
  "GAME_PROXY_ADMIN_MAX_RESPONSE_BYTES",
  "REGISTRY_ENABLED",
  "DISCOVERY_REQUIRED",
  "DISALLOW_LEGACY_DIRECT_CONFIG",
  "REGISTRY_KEY_PREFIX",
  "REGISTRY_DISCOVERY_CACHE_TTL_MS",
  "REGISTRY_DISCOVERY_REFRESH_INTERVAL_MS",
  "REDIS_KEY_PREFIX",
  "APP_ENV",
  "SERVICE_NAME",
  "SERVICE_INSTANCE_ID",
  "SERVICE_ZONE",
  "SERVICE_BUILD_VERSION",
  "SERVICE_BIND_HOST",
  "SERVICE_PUBLIC_HOST",
  "SERVICE_ADVERTISED_HOST",
  "HOST"
];

async function withEnv(env, fn) {
  const previous = new Map(CONFIG_ENV_KEYS.map((key) => [key, process.env[key]]));
  const previousCwd = process.cwd();
  const tempCwd = fs.mkdtempSync(path.join(os.tmpdir(), "admin-api-config-test-"));
  for (const key of CONFIG_ENV_KEYS) {
    delete process.env[key];
  }
  Object.assign(process.env, env);
  process.chdir(tempCwd);

  try {
    const mod = await import(`./config.js?test=${Date.now()}-${Math.random()}`);
    await fn(mod.getConfig());
  } finally {
    process.chdir(previousCwd);
    fs.rmSync(tempCwd, { recursive: true, force: true });
    for (const key of CONFIG_ENV_KEYS) {
      const value = previous.get(key);
      if (value === undefined) {
        delete process.env[key];
      } else {
        process.env[key] = value;
      }
    }
  }
}

async function withCapturedWarnings(env, fn) {
  const warnings = [];
  const originalWarn = console.warn;
  console.warn = (...args) => {
    warnings.push(args.join(" "));
  };

  try {
    return await withEnv(env, (config) => fn(config, warnings));
  } finally {
    console.warn = originalWarn;
  }
}

test("admin-api control plane security defaults stay local-development friendly", async () => {
  await withEnv({ NODE_ENV: "development" }, (config) => {
    assert.equal(config.adminApiRequireTls, false);
    assert.equal(config.adminApiRequireIpAllowlist, false);
    assert.deepEqual(config.adminApiIpAllowlist, []);
  });
});

test("admin-api requires TLS by default in production", async () => {
  await withEnv({
    NODE_ENV: "production",
    REGISTRY_ENABLED: "true",
    JWT_SECRET: "prod-jwt-secret-with-enough-entropy",
    GAME_ADMIN_TOKEN: "prod-game-admin-token-with-enough-entropy",
    GAME_PROXY_ADMIN_TOKEN: "prod-proxy-admin-token-with-enough-entropy",
    ADMIN_PASSWORD: "prod-admin-password-with-enough-entropy"
  }, (config) => {
    assert.equal(config.adminApiRequireTls, true);
    assert.equal(config.adminApiRequireIpAllowlist, false);
  });
});

test("admin-api control plane security config can be explicitly enabled", async () => {
  await withEnv({
    NODE_ENV: "development",
    ADMIN_API_REQUIRE_TLS: "true",
    ADMIN_API_REQUIRE_IP_ALLOWLIST: "true",
    ADMIN_API_IP_ALLOWLIST: "127.0.0.1,10.0.0.0/24"
  }, (config) => {
    assert.equal(config.adminApiRequireTls, true);
    assert.equal(config.adminApiRequireIpAllowlist, true);
    assert.deepEqual(config.adminApiIpAllowlist, ["127.0.0.1", "10.0.0.0/24"]);
  });
});

test("admin-api game admin network limits fall back on invalid values", async () => {
  await withEnv({
    GAME_ADMIN_CONNECT_TIMEOUT_MS: "invalid",
    GAME_ADMIN_WRITE_TIMEOUT_MS: "0",
    GAME_ADMIN_READ_TIMEOUT_MS: "-1",
    GAME_ADMIN_MAX_RESPONSE_BYTES: ""
  }, (config) => {
    assert.equal(config.gameAdminConnectTimeoutMs, 3000);
    assert.equal(config.gameAdminWriteTimeoutMs, 3000);
    assert.equal(config.gameAdminReadTimeoutMs, 3000);
    assert.equal(config.gameAdminMaxResponseBytes, 1048576);
  });
});

test("admin-api game admin network limits read positive values", async () => {
  await withEnv({
    GAME_ADMIN_CONNECT_TIMEOUT_MS: "100",
    GAME_ADMIN_WRITE_TIMEOUT_MS: "200",
    GAME_ADMIN_READ_TIMEOUT_MS: "300",
    GAME_ADMIN_MAX_RESPONSE_BYTES: "4096"
  }, (config) => {
    assert.equal(config.gameAdminConnectTimeoutMs, 100);
    assert.equal(config.gameAdminWriteTimeoutMs, 200);
    assert.equal(config.gameAdminReadTimeoutMs, 300);
    assert.equal(config.gameAdminMaxResponseBytes, 4096);
  });
});

test("admin-api game-proxy admin monitoring config reads positive values", async () => {
  await withEnv({
    NODE_ENV: "development",
    GAME_PROXY_ADMIN_HOST: "10.0.0.10",
    GAME_PROXY_ADMIN_PORT: "17101",
    GAME_PROXY_ADMIN_TOKEN: "proxy-write-token",
    GAME_PROXY_ADMIN_READ_TOKEN: "proxy-read-token",
    GAME_PROXY_ADMIN_REQUEST_TIMEOUT_MS: "1500",
    GAME_PROXY_ADMIN_MAX_RESPONSE_BYTES: "2048"
  }, (config) => {
    assert.equal(config.gameProxyAdminHost, "10.0.0.10");
    assert.equal(config.gameProxyAdminPort, 17101);
    assert.equal(config.gameProxyAdminToken, "proxy-write-token");
    assert.equal(config.gameProxyAdminReadToken, "proxy-read-token");
    assert.equal(config.gameProxyAdminRequestTimeoutMs, 1500);
    assert.equal(config.gameProxyAdminMaxResponseBytes, 2048);
  });
});

test("admin-api env example keeps game-proxy direct endpoint as commented local fallback only", () => {
  const example = fs.readFileSync(new URL("../.env.example", import.meta.url), "utf8");

  assert.doesNotMatch(example, /^GAME_PROXY_ADMIN_HOST=/m);
  assert.doesNotMatch(example, /^GAME_PROXY_ADMIN_PORT=/m);
  assert.match(example, /^# GAME_PROXY_ADMIN_HOST=127\.0\.0\.1$/m);
  assert.match(example, /^# GAME_PROXY_ADMIN_PORT=7101$/m);
  assert.match(example, /Local fallback only/);
  assert.match(example, /Strict\/test\/production discovery ignores these two variables/);
  assert.match(example, /game-proxy\.admin registry endpoint/);
});

test("admin-api ignores direct consumer endpoint env outside local fallback", async () => {
  await withCapturedWarnings({
    NODE_ENV: "test",
    REGISTRY_ENABLED: "true",
    GAME_SERVER_ADMIN_HOST: "203.0.113.20",
    GAME_SERVER_ADMIN_PORT: "17500",
    GAME_PROXY_ADMIN_HOST: "203.0.113.30",
    GAME_PROXY_ADMIN_PORT: "17101"
  }, (config, warnings) => {
    assert.equal(config.localDiscoveryFallbackEnabled, false);
    assert.equal(config.gameServerAdminHost, "127.0.0.1");
    assert.equal(config.gameServerAdminPort, 7500);
    assert.equal(config.gameProxyAdminHost, "127.0.0.1");
    assert.equal(config.gameProxyAdminPort, 7101);
    assert.deepEqual(
      config.legacyDirectConfigWarnings.map((warning) => warning.name),
      [
        "GAME_SERVER_ADMIN_HOST",
        "GAME_SERVER_ADMIN_PORT",
        "GAME_PROXY_ADMIN_HOST",
        "GAME_PROXY_ADMIN_PORT"
      ]
    );
    assert.equal(warnings.length, 4);
    assert.match(warnings[0], /GAME_SERVER_ADMIN_HOST is ignored/);
  });
});

test("admin-api does not warn for local fallback direct endpoint env", async () => {
  await withCapturedWarnings({
    NODE_ENV: "development",
    GAME_SERVER_ADMIN_HOST: "127.0.0.2",
    GAME_PROXY_ADMIN_HOST: "127.0.0.3"
  }, (config, warnings) => {
    assert.equal(config.localDiscoveryFallbackEnabled, true);
    assert.deepEqual(config.legacyDirectConfigWarnings, []);
    assert.deepEqual(warnings, []);
  });
});

test("admin-api allows legacy direct endpoints with APP_ENV local", async () => {
  await withCapturedWarnings({
    APP_ENV: "local",
    GAME_SERVER_ADMIN_HOST: "127.0.0.2",
    GAME_SERVER_ADMIN_PORT: "17500",
    GAME_PROXY_ADMIN_HOST: "127.0.0.3"
  }, (config, warnings) => {
    assert.equal(config.localDiscoveryFallbackEnabled, true);
    assert.equal(config.gameServerAdminHost, "127.0.0.2");
    assert.equal(config.gameServerAdminPort, 17500);
    assert.equal(config.gameProxyAdminHost, "127.0.0.3");
    assert.deepEqual(config.legacyDirectConfigWarnings, []);
    assert.deepEqual(warnings, []);
  });
});

test("admin-api ignores legacy direct endpoints when APP_ENV is development without NODE_ENV development", async () => {
  await withCapturedWarnings({
    APP_ENV: "development",
    GAME_SERVER_ADMIN_HOST: "203.0.113.20",
    GAME_PROXY_ADMIN_HOST: "203.0.113.30"
  }, (config, warnings) => {
    assert.equal(config.localDiscoveryFallbackEnabled, false);
    assert.equal(config.gameServerAdminHost, "127.0.0.1");
    assert.equal(config.gameProxyAdminHost, "127.0.0.1");
    assert.deepEqual(
      config.legacyDirectConfigWarnings.map((warning) => warning.name),
      ["GAME_SERVER_ADMIN_HOST", "GAME_PROXY_ADMIN_HOST"]
    );
    assert.equal(warnings.length, 2);
  });
});

test("admin-api treats staging as strict discovery for legacy direct endpoints", async () => {
  await withCapturedWarnings({
    APP_ENV: "staging",
    REGISTRY_ENABLED: "true",
    GAME_SERVER_ADMIN_HOST: "203.0.113.20",
    GAME_PROXY_ADMIN_HOST: "203.0.113.30"
  }, (config, warnings) => {
    assert.equal(config.registryDiscoveryRequired, true);
    assert.equal(config.localDiscoveryFallbackEnabled, false);
    assert.equal(config.gameServerAdminHost, "127.0.0.1");
    assert.equal(config.gameProxyAdminHost, "127.0.0.1");
    assert.equal(warnings.length, 2);
  });
});

test("admin-api rejects legacy direct config when migration complete switch is enabled", async () => {
  await assert.rejects(
    withEnv({
      NODE_ENV: "development",
      DISALLOW_LEGACY_DIRECT_CONFIG: "true",
      GAME_SERVER_ADMIN_HOST: "127.0.0.2",
      GAME_PROXY_ADMIN_PORT: "17101"
    }, () => {}),
    /DISALLOW_LEGACY_DIRECT_CONFIG=true forbids legacy direct config: GAME_SERVER_ADMIN_HOST, GAME_PROXY_ADMIN_PORT/
  );
});

test("admin-api test environment rejects legacy direct config when migration complete switch is enabled", async () => {
  await assert.rejects(
    withEnv({
      APP_ENV: "test",
      REGISTRY_ENABLED: "true",
      DISCOVERY_REQUIRED: "true",
      DISALLOW_LEGACY_DIRECT_CONFIG: "true",
      GAME_SERVER_ADMIN_HOST: "127.0.0.2",
      GAME_PROXY_ADMIN_HOST: "127.0.0.3"
    }, () => {}),
    /DISALLOW_LEGACY_DIRECT_CONFIG=true forbids legacy direct config: GAME_SERVER_ADMIN_HOST, GAME_PROXY_ADMIN_HOST/
  );
});

test("admin-api accepts migration complete switch when legacy direct config is absent", async () => {
  await withEnv({
    NODE_ENV: "development",
    DISALLOW_LEGACY_DIRECT_CONFIG: "true"
  }, (config) => {
    assert.equal(config.disallowLegacyDirectConfig, true);
    assert.deepEqual(config.legacyDirectConfigWarnings, []);
  });
});

test("admin-api game-proxy admin monitoring config falls back on invalid limits", async () => {
  await withEnv({
    GAME_PROXY_ADMIN_REQUEST_TIMEOUT_MS: "0",
    GAME_PROXY_ADMIN_MAX_RESPONSE_BYTES: "invalid"
  }, (config) => {
    assert.equal(config.gameProxyAdminRequestTimeoutMs, 3000);
    assert.equal(config.gameProxyAdminMaxResponseBytes, 1048576);
  });
});

test("admin-api service registry identity reads defaults and build version override", async () => {
  await withEnv({
    SERVICE_INSTANCE_ID: "admin-api-blue-001",
    SERVICE_ZONE: "zone-a",
    SERVICE_BUILD_VERSION: "2026.06.18+admin"
  }, (config) => {
    assert.equal(config.serviceName, "admin-api");
    assert.equal(config.serviceInstanceId, "admin-api-blue-001");
    assert.equal(config.serviceZone, "zone-a");
    assert.equal(config.serviceBuildVersion, "2026.06.18+admin");
  });
});

test("admin-api separates bind host from advertised registry host", async () => {
  await withEnv({
    NODE_ENV: "development",
    SERVICE_BIND_HOST: "0.0.0.0",
    SERVICE_PUBLIC_HOST: "10.0.0.11",
    HOST: "127.0.0.9"
  }, (config) => {
    assert.equal(config.host, "0.0.0.0");
    assert.equal(config.bindHost, "0.0.0.0");
    assert.equal(config.advertisedHost, "10.0.0.11");
  });

  await withEnv({
    NODE_ENV: "development",
    SERVICE_BIND_HOST: "0.0.0.0"
  }, (config) => {
    assert.equal(config.advertisedHost, "127.0.0.1");
  });

  await withEnv({
    NODE_ENV: "development",
    HOST: "0.0.0.0"
  }, (config) => {
    assert.equal(config.host, "0.0.0.0");
    assert.equal(config.advertisedHost, "127.0.0.1");
  });
});

test("admin-api reads registry key prefix with Redis prefix fallback", async () => {
  await withEnv({
    REGISTRY_KEY_PREFIX: "registry:",
    REDIS_KEY_PREFIX: "redis:"
  }, (config) => {
    assert.equal(config.registryKeyPrefix, "registry:");
  });

  await withEnv({ REDIS_KEY_PREFIX: "redis:" }, (config) => {
    assert.equal(config.registryKeyPrefix, "redis:");
  });
});

test("admin-api reads discovery cache ttl and refresh interval", async () => {
  await withEnv({
    REGISTRY_DISCOVERY_CACHE_TTL_MS: "0",
    REGISTRY_DISCOVERY_REFRESH_INTERVAL_MS: "2500"
  }, (config) => {
    assert.equal(config.registryDiscoveryCacheTtlMs, 0);
    assert.equal(config.registryDiscoveryRefreshIntervalMs, 2500);
  });

  await withEnv({
    REGISTRY_DISCOVERY_CACHE_TTL_MS: "-1",
    REGISTRY_DISCOVERY_REFRESH_INTERVAL_MS: "0"
  }, (config) => {
    assert.equal(config.registryDiscoveryCacheTtlMs, 1000);
    assert.equal(config.registryDiscoveryRefreshIntervalMs, 5000);
  });
});

test("admin-api strict discovery requires registry in production", async () => {
  await assert.rejects(
    withEnv({
      NODE_ENV: "production",
      REGISTRY_ENABLED: "false",
      JWT_SECRET: "prod-jwt-secret-with-enough-entropy",
      GAME_ADMIN_TOKEN: "prod-game-admin-token-with-enough-entropy",
      GAME_PROXY_ADMIN_TOKEN: "prod-proxy-admin-token-with-enough-entropy",
      ADMIN_PASSWORD: "prod-admin-password-with-enough-entropy"
    }, () => {}),
    /DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true/
  );
});

test("admin-api DISCOVERY_REQUIRED=true rejects registry disabled", async () => {
  await assert.rejects(
    withEnv({
      NODE_ENV: "development",
      DISCOVERY_REQUIRED: "true",
      REGISTRY_ENABLED: "false"
    }, () => {}),
    /DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true/
  );
});

test("admin-api test environment rejects registry disabled", async () => {
  await assert.rejects(
    withEnv({
      APP_ENV: "test",
      REGISTRY_ENABLED: "false"
    }, () => {}),
    /DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true/
  );
});

test("admin-api test environment ignores DISCOVERY_REQUIRED=false override", async () => {
  await assert.rejects(
    withEnv({
      APP_ENV: "test",
      DISCOVERY_REQUIRED: "false",
      REGISTRY_ENABLED: "false"
    }, () => {}),
    /DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true/
  );
});

test("admin-api production environment ignores DISCOVERY_REQUIRED=false override", async () => {
  await assert.rejects(
    withEnv({
      NODE_ENV: "production",
      DISCOVERY_REQUIRED: "false",
      REGISTRY_ENABLED: "false",
      JWT_SECRET: "prod-jwt-secret-with-enough-entropy",
      GAME_ADMIN_TOKEN: "prod-game-admin-token-with-enough-entropy",
      GAME_PROXY_ADMIN_TOKEN: "prod-proxy-admin-token-with-enough-entropy",
      ADMIN_PASSWORD: "prod-admin-password-with-enough-entropy"
    }, () => {}),
    /DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true/
  );
});
