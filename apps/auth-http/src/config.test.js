import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

const CONFIG_ENV_KEYS = [
  "NODE_ENV",
  "APP_ENV",
  "AUTH_REQUIRE_TLS",
  "AUTH_REGISTER_REQUIRE_REVIEW",
  "TRUST_PROXY",
  "TRUSTED_PROXIES",
  "TICKET_SECRET",
  "GAME_ADMIN_TOKEN",
  "INTERNAL_API_TOKEN"
];

async function withEnv(env, fn) {
  const previous = new Map(CONFIG_ENV_KEYS.map((key) => [key, process.env[key]]));
  const previousCwd = process.cwd();
  const tempCwd = fs.mkdtempSync(path.join(os.tmpdir(), "auth-http-config-test-"));
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

test("auth-http keeps TLS enforcement disabled by default in development", async () => {
  await withEnv({ NODE_ENV: "development" }, (config) => {
    assert.equal(config.authRequireTls, false);
  });
});

test("auth-http requires TLS by default in production", async () => {
  await withEnv({
    NODE_ENV: "production",
    TICKET_SECRET: "prod-ticket-secret-with-enough-entropy",
    GAME_ADMIN_TOKEN: "prod-game-admin-token-with-enough-entropy",
    INTERNAL_API_TOKEN: "prod-internal-api-token-with-enough-entropy"
  }, (config) => {
    assert.equal(config.authRequireTls, true);
  });
});

test("auth-http requires TLS by default when APP_ENV is production", async () => {
  await withEnv({
    APP_ENV: "production",
    TICKET_SECRET: "prod-ticket-secret-with-enough-entropy",
    GAME_ADMIN_TOKEN: "prod-game-admin-token-with-enough-entropy",
    INTERNAL_API_TOKEN: "prod-internal-api-token-with-enough-entropy"
  }, (config) => {
    assert.equal(config.authRequireTls, true);
  });
});

test("auth-http TLS enforcement can be explicitly disabled for test deployments", async () => {
  await withEnv({
    NODE_ENV: "production",
    AUTH_REQUIRE_TLS: "false",
    TICKET_SECRET: "prod-ticket-secret-with-enough-entropy",
    GAME_ADMIN_TOKEN: "prod-game-admin-token-with-enough-entropy",
    INTERNAL_API_TOKEN: "prod-internal-api-token-with-enough-entropy"
  }, (config) => {
    assert.equal(config.authRequireTls, false);
  });
});

test("auth-http reads trusted proxy list for forwarded proto checks", async () => {
  await withEnv({
    NODE_ENV: "development",
    TRUST_PROXY: "true",
    TRUSTED_PROXIES: "127.0.0.1,10.0.0.0/24"
  }, (config) => {
    assert.equal(config.trustProxy, true);
    assert.deepEqual(config.trustedProxies, ["127.0.0.1", "10.0.0.0/24"]);
  });
});

test("auth-http reads password registration review switch", async () => {
  await withEnv({
    NODE_ENV: "development",
    AUTH_REGISTER_REQUIRE_REVIEW: "true"
  }, (config) => {
    assert.equal(config.registerRequireReview, true);
  });
});
