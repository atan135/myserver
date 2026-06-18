import assert from "node:assert/strict";
import test from "node:test";

import { validateServiceInstance } from "../../../packages/service-registry/node/registry-schema.js";
import { configureLogger } from "./logger.js";
import { RegistryClient, discoverGameProxyAdminEndpoints, discoverGameServerAdminEndpoints } from "./registry-client.js";

configureLogger({
  appName: "admin-api-registry-test",
  logEnableConsole: true,
  logEnableFile: false,
  logLevel: "off",
  logDir: "logs/admin-api-registry-test"
});

function createRedisCapture() {
  const hashes = new Map();
  const keys = new Map();

  return {
    hashes,
    keys,
    stats: { scanCount: 0 },
    failScan: false,
    async hset(key, field, value) {
      hashes.set(`${key}:${field}`, value);
    },
    async hget(key, field) {
      return hashes.get(`${key}:${field}`) || null;
    },
    async exists(key) {
      return keys.has(key) ? 1 : 0;
    },
    async scan(cursor, _match, pattern) {
      if (this.failScan) {
        throw new Error("SCAN_FAILED");
      }
      this.stats.scanCount += 1;
      if (cursor !== "0") {
        return ["0", []];
      }
      const prefix = pattern.replace("*", "");
      const found = [...hashes.keys()]
        .filter((key) => key.endsWith(":data"))
        .map((key) => key.slice(0, -5))
        .filter((key) => key.startsWith(prefix));
      return ["0", found];
    },
    async setex(key, ttl, value) {
      keys.set(key, { ttl, value });
    },
    async del(key) {
      hashes.delete(`${key}:data`);
      keys.delete(key);
    }
  };
}

function createConfig(overrides = {}) {
  return {
    serviceName: "admin-api",
    serviceInstanceId: "admin-api-test-001",
    host: "10.10.0.5",
    port: 3101,
    adminApiRequireTls: true,
    adminApiRequireIpAllowlist: true,
    adminApiIpAllowlist: ["127.0.0.1", "10.0.0.0/24"],
    serviceBuildVersion: "2026.06.18+admin",
    serviceZone: "zone-admin",
    ...overrides
  };
}

test("RegistryClient registers admin-api admin http endpoint and metadata", async () => {
  const redis = createRedisCapture();
  const config = createConfig();
  const client = new RegistryClient(redis, config);

  await client.register();

  const raw = redis.hashes.get("service:admin-api:instances:admin-api-test-001:data");
  assert.ok(raw);

  const payload = JSON.parse(raw);
  assert.deepEqual(validateServiceInstance(payload), { ok: true, errors: [] });
  assert.equal(payload.host, "10.10.0.5");
  assert.equal(payload.port, 3101);
  assert.deepEqual(payload.endpoints, [
    {
      name: "http",
      protocol: "http",
      host: "10.10.0.5",
      port: 3101,
      socket: "",
      visibility: "admin",
      metadata: {
        service_name: "admin-api",
        service_instance_id: "admin-api-test-001",
        build_version: "2026.06.18+admin",
        zone: "zone-admin"
      },
      healthy: true
    }
  ]);
  assert.deepEqual(payload.metadata, {
    service_name: "admin-api",
    service_instance_id: "admin-api-test-001",
    require_tls: true,
    ip_allowlist_enabled: true,
    ip_allowlist: ["127.0.0.1", "10.0.0.0/24"],
    build_version: "2026.06.18+admin",
    zone: "zone-admin"
  });
});

test("RegistryClient publishes advertised host instead of bind host", async () => {
  const redis = createRedisCapture();
  const client = new RegistryClient(
    redis,
    createConfig({
      host: "0.0.0.0",
      advertisedHost: "10.10.0.25"
    })
  );

  await client.register();

  const payload = JSON.parse(redis.hashes.get("service:admin-api:instances:admin-api-test-001:data"));
  assert.equal(payload.host, "10.10.0.25");
  assert.equal(payload.endpoints[0].host, "10.10.0.25");
});

test("RegistryClient never publishes wildcard advertised host", async () => {
  for (const advertisedHost of ["0.0.0.0", "::", "[::]", "   "]) {
    const redis = createRedisCapture();
    const client = new RegistryClient(
      redis,
      createConfig({
        host: "0.0.0.0",
        advertisedHost
      })
    );

    await client.register();

    const payload = JSON.parse(redis.hashes.get("service:admin-api:instances:admin-api-test-001:data"));
    assert.equal(payload.host, "127.0.0.1");
    assert.equal(payload.endpoints[0].host, "127.0.0.1");
  }
});

test("discoverGameServerAdminEndpoints returns healthy tcp admin endpoints for all instances", async () => {
  const redis = createRedisCapture();
  const instances = [
    {
      id: "game-server-a",
      schema_version: 2,
      name: "game-server",
      host: "10.0.0.1",
      port: 7000,
      admin_port: 7500,
      endpoints: [
        {
          name: "admin",
          protocol: "tcp",
          host: "10.0.0.1",
          port: 7500,
          socket: "",
          visibility: "admin",
          metadata: {},
          healthy: true
        }
      ],
      tags: [],
      weight: 100,
      metadata: {},
      registered_at: 1,
      healthy: true
    },
    {
      id: "game-server-b",
      schema_version: 2,
      name: "game-server",
      host: "10.0.0.2",
      port: 7000,
      admin_port: 7500,
      endpoints: [
        {
          name: "admin",
          protocol: "tcp",
          host: "10.0.0.2",
          port: 7501,
          socket: "",
          visibility: "admin",
          metadata: {},
          healthy: true
        },
        {
          name: "http",
          protocol: "http",
          host: "10.0.0.2",
          port: 8080,
          socket: "",
          visibility: "admin",
          metadata: {},
          healthy: true
        }
      ],
      tags: [],
      weight: 100,
      metadata: {},
      registered_at: 1,
      healthy: true
    },
    {
      id: "game-server-unhealthy",
      schema_version: 2,
      name: "game-server",
      host: "10.0.0.3",
      port: 7000,
      admin_port: 7500,
      endpoints: [
        {
          name: "admin",
          protocol: "tcp",
          host: "10.0.0.3",
          port: 7502,
          socket: "",
          visibility: "admin",
          metadata: {},
          healthy: false
        }
      ],
      tags: [],
      weight: 100,
      metadata: {},
      registered_at: 1,
      healthy: true
    }
  ];

  for (const instance of instances) {
    redis.hashes.set(`service:game-server:instances:${instance.id}:data`, JSON.stringify(instance));
    redis.keys.set(`heartbeat:game-server:${instance.id}`, { ttl: 30, value: "1" });
  }

  const endpoints = await discoverGameServerAdminEndpoints(redis);

  assert.deepEqual(
    endpoints.map((endpoint) => ({
      instanceId: endpoint.instanceId,
      protocol: endpoint.protocol,
      host: endpoint.host,
      port: endpoint.port
    })),
    [
      { instanceId: "game-server-a", protocol: "tcp", host: "10.0.0.1", port: 7500 },
      { instanceId: "game-server-b", protocol: "tcp", host: "10.0.0.2", port: 7501 }
    ]
  );
});

test("discoverGameServerAdminEndpoints uses registry key prefix", async () => {
  const redis = createRedisCapture();
  const instance = {
    id: "game-server-prefixed",
    schema_version: 2,
    name: "game-server",
    host: "10.0.0.9",
    port: 7000,
    admin_port: 7500,
    endpoints: [
      {
        name: "admin",
        protocol: "tcp",
        host: "10.0.0.9",
        port: 7500,
        socket: "",
        visibility: "admin",
        metadata: {},
        healthy: true
      }
    ],
    tags: [],
    weight: 100,
    metadata: {},
    registered_at: 1,
    healthy: true
  };
  redis.hashes.set(`test:service:game-server:instances:${instance.id}:data`, JSON.stringify(instance));
  redis.keys.set(`test:heartbeat:game-server:${instance.id}`, { ttl: 30, value: "1" });

  const endpoints = await discoverGameServerAdminEndpoints(redis, "test:");

  assert.deepEqual(endpoints.map((endpoint) => endpoint.instanceId), ["game-server-prefixed"]);
});

test("discoverGameProxyAdminEndpoints returns healthy http admin endpoints for all instances", async () => {
  const redis = createRedisCapture();
  const instances = [
    {
      id: "game-proxy-a",
      schema_version: 2,
      name: "game-proxy",
      host: "10.0.1.1",
      port: 4000,
      admin_port: 7101,
      endpoints: [
        {
          name: "admin",
          protocol: "http",
          host: "10.0.1.1",
          port: 7101,
          socket: "",
          visibility: "admin",
          metadata: {},
          healthy: true
        }
      ],
      tags: [],
      weight: 100,
      metadata: {},
      registered_at: 1,
      healthy: true
    },
    {
      id: "game-proxy-b",
      schema_version: 2,
      name: "game-proxy",
      host: "10.0.1.2",
      port: 4000,
      admin_port: 7102,
      endpoints: [
        {
          name: "admin",
          protocol: "http",
          host: "10.0.1.2",
          port: 7102,
          socket: "",
          visibility: "admin",
          metadata: {},
          healthy: true
        },
        {
          name: "admin",
          protocol: "tcp",
          host: "10.0.1.2",
          port: 17102,
          socket: "",
          visibility: "admin",
          metadata: {},
          healthy: true
        }
      ],
      tags: [],
      weight: 100,
      metadata: {},
      registered_at: 1,
      healthy: true
    }
  ];

  for (const instance of instances) {
    redis.hashes.set(`service:game-proxy:instances:${instance.id}:data`, JSON.stringify(instance));
    redis.keys.set(`heartbeat:game-proxy:${instance.id}`, { ttl: 30, value: "1" });
  }

  const endpoints = await discoverGameProxyAdminEndpoints(redis);

  assert.deepEqual(
    endpoints.map((endpoint) => ({
      instanceId: endpoint.instanceId,
      protocol: endpoint.protocol,
      host: endpoint.host,
      port: endpoint.port
    })),
    [
      { instanceId: "game-proxy-a", protocol: "http", host: "10.0.1.1", port: 7101 },
      { instanceId: "game-proxy-b", protocol: "http", host: "10.0.1.2", port: 7102 }
    ]
  );
});

test("admin discovery endpoints can be served from refresh snapshots", async () => {
  const redis = createRedisCapture();
  const instances = [
    {
      id: "game-server-a",
      schema_version: 2,
      name: "game-server",
      host: "10.0.0.1",
      port: 7000,
      admin_port: 7500,
      endpoints: [
        {
          name: "admin",
          protocol: "tcp",
          host: "10.0.0.1",
          port: 7500,
          socket: "",
          visibility: "admin",
          metadata: {},
          healthy: true
        }
      ],
      tags: [],
      weight: 100,
      metadata: {},
      registered_at: 1,
      healthy: true
    },
    {
      id: "game-server-b",
      schema_version: 2,
      name: "game-server",
      host: "10.0.0.2",
      port: 7000,
      admin_port: 7501,
      endpoints: [
        {
          name: "admin",
          protocol: "tcp",
          host: "10.0.0.2",
          port: 7501,
          socket: "",
          visibility: "admin",
          metadata: {},
          healthy: true
        }
      ],
      tags: [],
      weight: 100,
      metadata: {},
      registered_at: 1,
      healthy: true
    }
  ];

  for (const instance of instances) {
    redis.hashes.set(`service:game-server:instances:${instance.id}:data`, JSON.stringify(instance));
    redis.keys.set(`heartbeat:game-server:${instance.id}`, { ttl: 30, value: "1" });
  }

  const client = new RegistryClient(redis, createConfig({
    registryDiscoveryEnabled: true,
    registryDiscoveryRefreshIntervalMs: 1000
  }));
  const handles = client.startDiscoveryRefresh();
  try {
    await Promise.allSettled(handles.map((handle) => handle.refreshSnapshot()));
    const scanCountAfterRefresh = redis.stats.scanCount;
    redis.failScan = true;

    const endpoints = await discoverGameServerAdminEndpoints(redis, {
      registryKeyPrefix: "",
      registryDiscoveryCacheTtlMs: 1000
    });

    assert.deepEqual(
      endpoints.map((endpoint) => [endpoint.instanceId, endpoint.host, endpoint.port]),
      [
        ["game-server-a", "10.0.0.1", 7500],
        ["game-server-b", "10.0.0.2", 7501]
      ]
    );
    assert.equal(redis.stats.scanCount, scanCountAfterRefresh);
  } finally {
    client.stopDiscoveryRefresh();
  }
});

test("admin discovery endpoints use refresh snapshot with non-default cache ttl config", async () => {
  const redis = createRedisCapture();
  const instance = {
    id: "game-server-a",
    schema_version: 2,
    name: "game-server",
    host: "10.0.0.1",
    port: 7000,
    admin_port: 7500,
    endpoints: [
      {
        name: "admin",
        protocol: "tcp",
        host: "10.0.0.1",
        port: 7500,
        socket: "",
        visibility: "admin",
        metadata: {},
        healthy: true
      }
    ],
    tags: [],
    weight: 100,
    metadata: {},
    registered_at: 1,
    healthy: true
  };
  redis.hashes.set(`service:game-server:instances:${instance.id}:data`, JSON.stringify(instance));
  redis.keys.set(`heartbeat:game-server:${instance.id}`, { ttl: 30, value: "1" });

  const config = createConfig({
    registryDiscoveryEnabled: true,
    registryDiscoveryCacheTtlMs: 2500,
    registryDiscoveryRefreshIntervalMs: 1000
  });
  const client = new RegistryClient(redis, config);
  const handles = client.startDiscoveryRefresh();
  try {
    await Promise.allSettled(handles.map((handle) => handle.refreshSnapshot()));
    const scanCountAfterRefresh = redis.stats.scanCount;
    redis.failScan = true;

    const endpoints = await discoverGameServerAdminEndpoints(redis, config);

    assert.deepEqual(endpoints.map((endpoint) => endpoint.instanceId), ["game-server-a"]);
    assert.equal(redis.stats.scanCount, scanCountAfterRefresh);
  } finally {
    client.stopDiscoveryRefresh();
  }
});

test("admin discovery refresh failure does not serve stale snapshot endpoints", async () => {
  const redis = createRedisCapture();
  const instance = {
    id: "game-server-a",
    schema_version: 2,
    name: "game-server",
    host: "10.0.0.1",
    port: 7000,
    admin_port: 7500,
    endpoints: [
      {
        name: "admin",
        protocol: "tcp",
        host: "10.0.0.1",
        port: 7500,
        socket: "",
        visibility: "admin",
        metadata: {},
        healthy: true
      }
    ],
    tags: [],
    weight: 100,
    metadata: {},
    registered_at: 1,
    healthy: true
  };
  redis.hashes.set(`service:game-server:instances:${instance.id}:data`, JSON.stringify(instance));
  redis.keys.set(`heartbeat:game-server:${instance.id}`, { ttl: 30, value: "1" });

  const client = new RegistryClient(redis, createConfig({
    registryDiscoveryEnabled: true,
    registryDiscoveryRefreshIntervalMs: 1000
  }));
  const handles = client.startDiscoveryRefresh();
  try {
    await Promise.allSettled(handles.map((handle) => handle.refreshSnapshot()));
    redis.failScan = true;
    await assert.rejects(() => handles[0].refreshSnapshot(), /SCAN_FAILED/);

    await assert.rejects(
      () => discoverGameServerAdminEndpoints(redis, {
        registryKeyPrefix: "",
        registryDiscoveryCacheTtlMs: 1000
      }),
      /SCAN_FAILED/
    );
  } finally {
    client.stopDiscoveryRefresh();
  }
});

test("RegistryClient metadata falls back to dev build version and empty allowlist", async () => {
  const redis = createRedisCapture();
  const client = new RegistryClient(
    redis,
    createConfig({
      adminApiRequireTls: false,
      adminApiRequireIpAllowlist: false,
      adminApiIpAllowlist: null,
      serviceBuildVersion: ""
    })
  );

  await client.register();

  const payload = JSON.parse(redis.hashes.get("service:admin-api:instances:admin-api-test-001:data"));
  assert.equal(payload.metadata.require_tls, false);
  assert.equal(payload.metadata.ip_allowlist_enabled, false);
  assert.deepEqual(payload.metadata.ip_allowlist, []);
  assert.equal(payload.metadata.build_version, "dev");
  assert.equal(payload.metadata.zone, "zone-admin");
});

test("RegistryClient heartbeat and deregister use registry instance keys", async () => {
  const redis = createRedisCapture();
  const client = new RegistryClient(redis, createConfig());

  await client.register();
  client.startHeartbeat(60);
  client.stopHeartbeat();

  assert.deepEqual(redis.keys.get("heartbeat:admin-api:admin-api-test-001"), {
    ttl: 30,
    value: "1"
  });

  await client.deregister();

  assert.equal(redis.hashes.has("service:admin-api:instances:admin-api-test-001:data"), false);
  assert.equal(redis.keys.has("heartbeat:admin-api:admin-api-test-001"), false);
});
