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
  "GAME_PROXY_ADMIN_HOST",
  "GAME_PROXY_ADMIN_PORT",
  "GAME_PROXY_ADMIN_TOKEN",
  "GAME_PROXY_ADMIN_READ_TOKEN",
  "GAME_PROXY_ADMIN_REQUEST_TIMEOUT_MS",
  "GAME_PROXY_ADMIN_MAX_RESPONSE_BYTES"
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

test("admin-api game-proxy admin monitoring config falls back on invalid limits", async () => {
  await withEnv({
    GAME_PROXY_ADMIN_REQUEST_TIMEOUT_MS: "0",
    GAME_PROXY_ADMIN_MAX_RESPONSE_BYTES: "invalid"
  }, (config) => {
    assert.equal(config.gameProxyAdminRequestTimeoutMs, 3000);
    assert.equal(config.gameProxyAdminMaxResponseBytes, 1048576);
  });
});
