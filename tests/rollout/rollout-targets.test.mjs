import assert from "node:assert/strict";
import test from "node:test";

import {
  createServiceInstancePayload
} from "../../packages/service-registry/node/registry-schema.js";
import {
  applyResolvedRolloutControlTargets,
  resolveRolloutControlTargets,
  validateControlTargetOptions
} from "../../tools/rollout/rollout-targets.js";
import { MemoryRedis } from "../../tools/check-registry-canary-lifecycle.js";

function endpoint(name, protocol, host, port, visibility) {
  return {
    name,
    protocol,
    host,
    port,
    socket: "",
    visibility,
    metadata: {},
    healthy: true
  };
}

async function seed(redis, registryKeyPrefix, payload) {
  await redis.hset(`${registryKeyPrefix}service:${payload.name}:instances:${payload.id}`, "data", JSON.stringify(payload));
  await redis.setex(`${registryKeyPrefix}heartbeat:${payload.name}:${payload.id}`, 60, "1");
}

test("rollout target resolver discovers admin endpoints by instance id and endpoint name", async () => {
  const registryKeyPrefix = "test:rollout-targets:";
  const redis = new MemoryRedis();
  await seed(redis, registryKeyPrefix, createServiceInstancePayload({
    id: "game-server-old",
    name: "game-server",
    host: "10.0.0.10",
    port: 17000,
    endpoints: [endpoint("admin", "tcp", "10.0.0.10", 17500, "admin")]
  }));
  await seed(redis, registryKeyPrefix, createServiceInstancePayload({
    id: "game-server-new",
    name: "game-server",
    host: "10.0.0.11",
    port: 17001,
    endpoints: [endpoint("admin", "tcp", "10.0.0.11", 17501, "admin")]
  }));
  await seed(redis, registryKeyPrefix, createServiceInstancePayload({
    id: "game-proxy-a",
    name: "game-proxy",
    host: "10.0.0.20",
    port: 14000,
    endpoints: [endpoint("admin", "http", "10.0.0.21", 17101, "admin")]
  }));

  const resolved = await resolveRolloutControlTargets({
    registryKeyPrefix,
    oldServerId: "game-server-old",
    newServerId: "game-server-new",
    proxyInstanceId: "game-proxy-a",
    oldAdminEndpointName: "admin",
    newAdminEndpointName: "admin",
    proxyAdminEndpointName: "admin",
    discoveryCacheTtlMs: 0
  }, {
    requireNew: true,
    requireProxy: true,
    redis
  });

  assert.equal(resolved.oldGameServerAdmin.host, "10.0.0.10");
  assert.equal(resolved.oldGameServerAdmin.port, 17500);
  assert.equal(resolved.oldGameServerAdmin.source, "registry");
  assert.equal(resolved.newGameServerAdmin.host, "10.0.0.11");
  assert.equal(resolved.gameProxyAdmin.url, "http://10.0.0.21:17101");

  const options = { oldServerId: "game-server-old", newServerId: "game-server-new" };
  applyResolvedRolloutControlTargets(options, resolved);
  assert.equal(options.resolvedControlTargetsInput, true);
  assert.equal(options.oldAdminHost, "10.0.0.10");
  assert.equal(options.oldAdminPort, 17500);
  assert.equal(options.newAdminHost, "10.0.0.11");
  assert.equal(options.newAdminPort, 17501);
  assert.equal(options.proxyAdminUrl, "http://10.0.0.21:17101");
});

test("rollout target validation rejects unmarked direct endpoint inputs", () => {
  const errors = validateControlTargetOptions({
    oldServerId: "game-server-old",
    newServerId: "game-server-new",
    oldAdminEndpointName: "admin",
    newAdminEndpointName: "admin",
    proxyAdminEndpointName: "admin",
    oldAdminHost: "127.0.0.1",
    oldAdminPort: 7500,
    newAdminHost: "127.0.0.2",
    newAdminPort: 7501,
    proxyAdminUrl: "http://127.0.0.3:7101"
  });

  assert.equal(errors.length, 3);
  assert(errors.every((message) => message.includes("--resolved-control-targets or --local-debug-targets")));
});
