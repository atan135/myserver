import assert from "node:assert/strict";
import test from "node:test";

import {
  RegistryDiscoveryClient,
  createServiceInstancePayload,
  registryHeartbeatKey,
  registryInstanceKey
} from "./registry-schema.js";

function createInstance(serviceName, instanceId, endpointName, port) {
  return createServiceInstancePayload({
    id: instanceId,
    name: serviceName,
    host: "127.0.0.1",
    port,
    endpoints: [
      {
        name: endpointName,
        protocol: "tcp",
        host: "127.0.0.1",
        port,
        socket: "",
        visibility: "internal",
        metadata: {},
        healthy: true
      }
    ]
  });
}

function createRedisCapture() {
  const hashes = new Map();
  const heartbeats = new Set();
  const stats = { scanCount: 0 };

  return {
    hashes,
    heartbeats,
    stats,
    addInstance(prefix, serviceName, instance) {
      hashes.set(registryInstanceKey(prefix, serviceName, instance.id), JSON.stringify(instance));
      heartbeats.add(registryHeartbeatKey(prefix, serviceName, instance.id));
    },
    replaceInstance(prefix, serviceName, instance) {
      this.addInstance(prefix, serviceName, instance);
    },
    async scan(cursor, _match, pattern) {
      stats.scanCount += 1;
      if (cursor !== "0") {
        return ["0", []];
      }
      const prefix = pattern.replace("*", "");
      return ["0", [...hashes.keys()].filter((key) => key.startsWith(prefix))];
    },
    async exists(key) {
      return heartbeats.has(key) ? 1 : 0;
    },
    async hget(key, field) {
      return field === "data" ? hashes.get(key) ?? null : null;
    }
  };
}

test("RegistryDiscoveryClient reuses cached service scan before ttl expires", async () => {
  let now = 1000;
  const redis = createRedisCapture();
  redis.addInstance("", "game-server", createInstance("game-server", "game-a", "admin", 7500));
  const client = new RegistryDiscoveryClient(redis, {
    discoveryCacheTtlMs: 100,
    now: () => now
  });

  assert.equal((await client.discoverEndpoint("game-server", "admin")).endpoint.port, 7500);
  assert.equal(redis.stats.scanCount, 1);

  redis.replaceInstance("", "game-server", createInstance("game-server", "game-a", "admin", 7501));
  now += 99;

  assert.equal((await client.discoverEndpoint("game-server", "admin")).endpoint.port, 7500);
  assert.equal(redis.stats.scanCount, 1);
});

test("RegistryDiscoveryClient refreshes Redis scan after ttl expires", async () => {
  let now = 2000;
  const redis = createRedisCapture();
  redis.addInstance("", "game-server", createInstance("game-server", "game-a", "admin", 7500));
  const client = new RegistryDiscoveryClient(redis, {
    discoveryCacheTtlMs: 100,
    now: () => now
  });

  assert.equal((await client.discoverEndpoint("game-server", "admin")).endpoint.port, 7500);
  assert.equal(redis.stats.scanCount, 1);

  redis.replaceInstance("", "game-server", createInstance("game-server", "game-a", "admin", 7501));
  now += 100;

  assert.equal((await client.discoverEndpoint("game-server", "admin")).endpoint.port, 7501);
  assert.equal(redis.stats.scanCount, 2);
});

test("RegistryDiscoveryClient separates service and endpoint cache keys", async () => {
  const redis = createRedisCapture();
  redis.addInstance("", "game-server", createInstance("game-server", "game-a", "admin", 7500));
  redis.addInstance("", "chat-server", createInstance("chat-server", "chat-a", "client", 9001));
  const client = new RegistryDiscoveryClient(redis, { discoveryCacheTtlMs: 1000 });

  assert.equal((await client.discoverEndpoint("game-server", "admin")).endpoint.port, 7500);
  assert.equal((await client.discoverEndpoint("game-server", "client")), null);
  assert.equal((await client.discoverEndpoint("chat-server", "client")).endpoint.port, 9001);

  assert.equal(redis.stats.scanCount, 2);
});

test("RegistryDiscoveryClient separates registry key prefixes", async () => {
  const redis = createRedisCapture();
  redis.addInstance("", "game-server", createInstance("game-server", "game-a", "admin", 7500));
  redis.addInstance("test:", "game-server", createInstance("game-server", "game-b", "admin", 7600));
  const defaultClient = new RegistryDiscoveryClient(redis, {
    registryKeyPrefix: "",
    discoveryCacheTtlMs: 1000
  });
  const prefixedClient = new RegistryDiscoveryClient(redis, {
    registryKeyPrefix: "test:",
    discoveryCacheTtlMs: 1000
  });

  assert.equal((await defaultClient.discoverEndpoint("game-server", "admin")).endpoint.port, 7500);
  assert.equal((await prefixedClient.discoverEndpoint("game-server", "admin")).endpoint.port, 7600);
  assert.equal(redis.stats.scanCount, 2);
});

test("RegistryDiscoveryClient caches required discovery miss only until ttl expires", async () => {
  let now = 3000;
  const redis = createRedisCapture();
  const client = new RegistryDiscoveryClient(redis, {
    discoveryCacheTtlMs: 100,
    now: () => now
  });

  await assert.rejects(
    () => client.discoverRequiredEndpoint("game-server", "admin"),
    /service endpoint not found: service=game-server, endpoint=admin/
  );
  assert.equal(redis.stats.scanCount, 1);

  redis.addInstance("", "game-server", createInstance("game-server", "game-a", "admin", 7500));
  await assert.rejects(
    () => client.discoverRequiredEndpoint("game-server", "admin"),
    /service endpoint not found: service=game-server, endpoint=admin/
  );
  assert.equal(redis.stats.scanCount, 1);

  now += 100;

  assert.equal((await client.discoverRequiredEndpoint("game-server", "admin")).endpoint.port, 7500);
  assert.equal(redis.stats.scanCount, 2);
});

test("RegistryDiscoveryClient can disable discovery cache", async () => {
  const redis = createRedisCapture();
  redis.addInstance("", "game-server", createInstance("game-server", "game-a", "admin", 7500));
  const client = new RegistryDiscoveryClient(redis, { discoveryCacheTtlMs: 0 });

  assert.equal((await client.discoverEndpoint("game-server", "admin")).endpoint.port, 7500);
  assert.equal((await client.discoverEndpoint("game-server", "admin")).endpoint.port, 7500);
  assert.equal(redis.stats.scanCount, 2);
});
