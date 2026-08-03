import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const read = (path) => readFile(new URL(`../../${path}`, import.meta.url), "utf8");

function serviceBlock(compose, serviceName) {
  const pattern = new RegExp(`(?:^|\\r?\\n)  ${serviceName}:\\r?\\n([\\s\\S]*?)(?=\\r?\\n  [a-z][a-z0-9-]*:|\\r?\\nvolumes:)`);
  const match = compose.match(pattern);
  assert.ok(match, `${serviceName} service must exist`);
  return match[1];
}

test("production Redis keeps data capacity below its no-swap container limit", async () => {
  const [compose, redisConfig] = await Promise.all([
    read("deploy/docker/compose.production.yml"),
    read("deploy/docker/config/redis.conf")
  ]);
  const redis = serviceBlock(compose, "redis");

  assert.match(redis, /^    mem_limit: 768m$/m);
  assert.match(redis, /^    memswap_limit: 768m$/m);
  assert.match(redisConfig, /^maxmemory 512mb$/m);
  assert.match(redisConfig, /^maxmemory-policy noeviction$/m);
});

test("production announce-service has a no-swap Node memory budget", async () => {
  const compose = await read("deploy/docker/compose.production.yml");
  const announceService = serviceBlock(compose, "announce-service");

  assert.match(announceService, /^    mem_limit: 256m$/m);
  assert.match(announceService, /^    memswap_limit: 256m$/m);
});
