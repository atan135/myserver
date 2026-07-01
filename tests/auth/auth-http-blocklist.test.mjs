import assert from "node:assert/strict";
import { afterEach, test } from "node:test";

import {
  IP_BLOCKED_ERROR,
  PLAYER_BLOCKED_ERROR,
  RedisBlocklistChecker,
  blocklistIpKey,
  blocklistPlayerKey,
  parseBlocklistDecision
} from "../../apps/auth-http/src/blocklist.js";
import { getConfig } from "../../apps/auth-http/src/config.js";

const managedEnv = [
  "NODE_ENV",
  "APP_ENV",
  "REGISTRY_ENABLED",
  "AUTH_REDIS_BLOCKLIST_ENABLED",
  "AUTH_REDIS_BLOCKLIST_CACHE_TTL_MS"
];
const originalEnv = new Map(managedEnv.map((key) => [key, process.env[key]]));

afterEach(() => {
  for (const [key, value] of originalEnv.entries()) {
    if (value === undefined) {
      delete process.env[key];
    } else {
      process.env[key] = value;
    }
  }
});

test("auth redis blocklist config defaults to disabled with 2000ms cache", () => {
  delete process.env.NODE_ENV;
  delete process.env.APP_ENV;
  delete process.env.AUTH_REDIS_BLOCKLIST_ENABLED;
  delete process.env.AUTH_REDIS_BLOCKLIST_CACHE_TTL_MS;

  const config = getConfig();
  assert.equal(config.authRedisBlocklistEnabled, false);
  assert.equal(config.authRedisBlocklistCacheTtlMs, 2000);
});

test("auth redis blocklist config parses explicit env", () => {
  process.env.NODE_ENV = "test";
  process.env.REGISTRY_ENABLED = "true";
  process.env.AUTH_REDIS_BLOCKLIST_ENABLED = "true";
  process.env.AUTH_REDIS_BLOCKLIST_CACHE_TTL_MS = "500";

  const config = getConfig();
  assert.equal(config.authRedisBlocklistEnabled, true);
  assert.equal(config.authRedisBlocklistCacheTtlMs, 500);
});

test("auth redis blocklist keys share proxy schema", () => {
  assert.equal(
    blocklistIpKey("dev:", "203.0.113.10"),
    "dev:security:blocklist:ip:203.0.113.10"
  );
  assert.equal(
    blocklistPlayerKey("", "player-1"),
    "security:blocklist:player:player-1"
  );
});

test("auth redis blocklist parses until and existing values", () => {
  assert.deepEqual(parseBlocklistDecision(null, 1000, IP_BLOCKED_ERROR), {
    blocked: false
  });
  assert.deepEqual(parseBlocklistDecision("manual", 1000, IP_BLOCKED_ERROR), {
    blocked: true,
    error: IP_BLOCKED_ERROR
  });
  assert.deepEqual(
    parseBlocklistDecision('{"reason":"expired","until":999}', 1000, IP_BLOCKED_ERROR),
    { blocked: false }
  );
  assert.deepEqual(
    parseBlocklistDecision('{"reason":"abuse","until":2000}', 1000, PLAYER_BLOCKED_ERROR),
    { blocked: true, error: PLAYER_BLOCKED_ERROR }
  );
  assert.deepEqual(
    parseBlocklistDecision('{"reason":"abuse"}', 1000, PLAYER_BLOCKED_ERROR),
    { blocked: true, error: PLAYER_BLOCKED_ERROR }
  );
});

test("disabled auth redis blocklist is no-op without redis", async () => {
  const checker = RedisBlocklistChecker.disabled();

  assert.deepEqual(await checker.checkIp("203.0.113.10"), { blocked: false });
  assert.deepEqual(await checker.checkPlayer("player-1"), { blocked: false });
});
