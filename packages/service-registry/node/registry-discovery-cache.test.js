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
    failScan: false,
    addInstance(prefix, serviceName, instance) {
      hashes.set(registryInstanceKey(prefix, serviceName, instance.id), JSON.stringify(instance));
      heartbeats.add(registryHeartbeatKey(prefix, serviceName, instance.id));
    },
    replaceInstance(prefix, serviceName, instance) {
      this.addInstance(prefix, serviceName, instance);
    },
    removeInstance(prefix, serviceName, instanceId) {
      hashes.delete(registryInstanceKey(prefix, serviceName, instanceId));
      heartbeats.delete(registryHeartbeatKey(prefix, serviceName, instanceId));
    },
    async scan(cursor, _match, pattern) {
      if (this.failScan) {
        throw new Error("SCAN_FAILED");
      }
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

test("RegistryDiscoveryClient refreshSnapshot updates watched endpoint snapshots", async () => {
  const redis = createRedisCapture();
  redis.addInstance("", "game-server", createInstance("game-server", "game-a", "admin", 7500));
  const client = new RegistryDiscoveryClient(redis, { discoveryCacheTtlMs: 1000 });

  let snapshot = await client.refreshSnapshot("game-server", {
    endpointName: "admin",
    kind: "all_endpoints"
  });
  assert.equal(snapshot.ok, true);
  assert.deepEqual(snapshot.value.map(({ endpoint }) => endpoint.port), [7500]);
  assert.equal(redis.stats.scanCount, 1);

  redis.replaceInstance("", "game-server", createInstance("game-server", "game-a", "admin", 7501));
  redis.addInstance("", "game-server", createInstance("game-server", "game-b", "admin", 7502));
  snapshot = await client.refreshSnapshot("game-server", {
    endpointName: "admin",
    kind: "all_endpoints"
  });

  assert.deepEqual(snapshot.value.map(({ instance, endpoint }) => [instance.id, endpoint.port]), [
    ["game-a", 7501],
    ["game-b", 7502]
  ]);
  assert.equal(redis.stats.scanCount, 2);
});

test("RegistryDiscoveryClient stop prevents further interval refreshes", async () => {
  const redis = createRedisCapture();
  redis.addInstance("", "game-server", createInstance("game-server", "game-a", "admin", 7500));
  const client = new RegistryDiscoveryClient(redis, { discoveryCacheTtlMs: 1000 });
  const handle = client.startRefresh("game-server", {
    endpointName: "admin",
    kind: "all_endpoints",
    refreshIntervalMs: 10
  });

  await sleep(35);
  assert.ok(redis.stats.scanCount > 0);
  handle.stop();
  const scanCountAfterStop = redis.stats.scanCount;
  redis.replaceInstance("", "game-server", createInstance("game-server", "game-a", "admin", 7501));

  await sleep(35);

  assert.equal(redis.stats.scanCount, scanCountAfterStop);
  assert.equal(handle.isRunning(), false);
  assert.notEqual(handle.getSnapshot()?.value?.[0]?.endpoint?.port, 7501);
});

test("RegistryDiscoveryClient refresh failure clears watched snapshot by default", async () => {
  const redis = createRedisCapture();
  redis.addInstance("", "game-server", createInstance("game-server", "game-a", "admin", 7500));
  const client = new RegistryDiscoveryClient(redis, { discoveryCacheTtlMs: 1000 });

  await client.refreshSnapshot("game-server", {
    endpointName: "admin",
    kind: "all_endpoints"
  });
  redis.failScan = true;

  await assert.rejects(
    () => client.refreshSnapshot("game-server", {
      endpointName: "admin",
      kind: "all_endpoints"
    }),
    /SCAN_FAILED/
  );

  const snapshot = client.getRefreshSnapshot("game-server", {
    endpointName: "admin",
    kind: "all_endpoints"
  });
  assert.equal(snapshot.ok, false);
  assert.equal(snapshot.value, undefined);
  assert.equal(snapshot.error.message, "SCAN_FAILED");
});

test("RegistryDiscoveryClient can retain watched snapshot on refresh failure", async () => {
  const redis = createRedisCapture();
  redis.addInstance("", "game-server", createInstance("game-server", "game-a", "admin", 7500));
  const client = new RegistryDiscoveryClient(redis, { discoveryCacheTtlMs: 1000 });

  await client.refreshSnapshot("game-server", {
    endpointName: "admin",
    kind: "all_endpoints",
    retainStaleOnError: true
  });
  redis.failScan = true;

  await assert.rejects(
    () => client.refreshSnapshot("game-server", {
      endpointName: "admin",
      kind: "all_endpoints",
      retainStaleOnError: true
    }),
    /SCAN_FAILED/
  );

  const snapshot = client.getRefreshSnapshot("game-server", {
    endpointName: "admin",
    kind: "all_endpoints",
    retainStaleOnError: true
  });
  assert.equal(snapshot.ok, false);
  assert.deepEqual(snapshot.value.map(({ instance, endpoint }) => [instance.id, endpoint.port]), [
    ["game-a", 7500]
  ]);
});

test("RegistryDiscoveryClient refresh snapshots separate services endpoints and prefixes", async () => {
  const redis = createRedisCapture();
  redis.addInstance("", "game-server", createInstance("game-server", "game-admin-a", "admin", 7500));
  redis.addInstance("", "game-server", createInstance("game-server", "game-client-a", "client", 7000));
  redis.addInstance("", "game-proxy", createInstance("game-proxy", "proxy-a", "admin", 7101));
  redis.addInstance("test:", "game-server", createInstance("game-server", "game-b", "admin", 7600));
  const defaultClient = new RegistryDiscoveryClient(redis, { discoveryCacheTtlMs: 1000 });
  const prefixedClient = new RegistryDiscoveryClient(redis, {
    registryKeyPrefix: "test:",
    discoveryCacheTtlMs: 1000
  });

  const gameAdmin = await defaultClient.refreshSnapshot("game-server", {
    endpointName: "admin",
    kind: "all_endpoints"
  });
  const gameClient = await defaultClient.refreshSnapshot("game-server", {
    endpointName: "client",
    kind: "all_endpoints"
  });
  const proxyAdmin = await defaultClient.refreshSnapshot("game-proxy", {
    endpointName: "admin",
    kind: "all_endpoints"
  });
  const prefixedGameAdmin = await prefixedClient.refreshSnapshot("game-server", {
    endpointName: "admin",
    kind: "all_endpoints"
  });

  assert.deepEqual(gameAdmin.value.map(({ endpoint }) => endpoint.port), [7500]);
  assert.deepEqual(gameClient.value.map(({ endpoint }) => endpoint.port), [7000]);
  assert.deepEqual(proxyAdmin.value.map(({ endpoint }) => endpoint.port), [7101]);
  assert.deepEqual(prefixedGameAdmin.value.map(({ endpoint }) => endpoint.port), [7600]);
});

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
