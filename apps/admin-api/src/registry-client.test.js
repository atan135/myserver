import assert from "node:assert/strict";
import test from "node:test";

import {
  getRegistryLifecycleMetricsSnapshot,
  resetRegistryLifecycleMetrics,
  validateServiceInstance
} from "../../../packages/service-registry/node/registry-schema.js";
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

function createFailingRedis(operation) {
  const redis = createRedisCapture();
  if (operation === "hset") {
    redis.hset = async () => {
      throw new Error("HSET_FAILED");
    };
  }
  if (operation === "setex") {
    redis.setex = async () => {
      throw new Error("SETEX_FAILED");
    };
  }
  if (operation === "del") {
    redis.del = async () => {
      throw new Error("DEL_FAILED");
    };
  }
  return redis;
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

function registryInstance(serviceName, id, endpoints, overrides = {}) {
  return {
    id,
    schema_version: 2,
    name: serviceName,
    host: overrides.host || "10.0.0.1",
    port: overrides.port ?? 7000,
    admin_port: overrides.adminPort ?? 0,
    local_socket: "",
    endpoints,
    tags: [],
    weight: 100,
    metadata: {},
    registered_at: 1,
    healthy: true,
    ...overrides
  };
}

function networkEndpoint(name, protocol, visibility, host, port) {
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

function putInstance(redis, instance) {
  redis.hashes.set(`service:${instance.name}:instances:${instance.id}:data`, JSON.stringify(instance));
  redis.keys.set(`heartbeat:${instance.name}:${instance.id}`, { ttl: 30, value: "1" });
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

test("discoverGameServerAdminEndpoints requires admin visibility and never falls back to client endpoints", async () => {
  const redis = createRedisCapture();
  putInstance(redis, registryInstance("game-server", "game-server-public-admin-name", [
    networkEndpoint("admin", "tcp", "public", "10.0.0.10", 7000)
  ]));
  putInstance(redis, registryInstance("game-server", "game-server-internal-admin-name", [
    networkEndpoint("admin", "tcp", "internal", "10.0.0.11", 7600)
  ]));
  putInstance(redis, registryInstance("game-server", "game-server-admin", [
    networkEndpoint("client", "tcp", "public", "10.0.0.12", 7000),
    networkEndpoint("admin", "tcp", "admin", "10.0.0.12", 7500)
  ]));

  const endpoints = await discoverGameServerAdminEndpoints(redis);

  assert.deepEqual(endpoints.map(({ instanceId, endpointName, port }) => ({ instanceId, endpointName, port })), [
    { instanceId: "game-server-admin", endpointName: "admin", port: 7500 }
  ]);
});

test("discoverGameServerAdminEndpoints returns empty when only client-visible endpoints exist", async () => {
  const redis = createRedisCapture();
  putInstance(redis, registryInstance("game-server", "game-server-client-only", [
    networkEndpoint("client", "tcp", "public", "10.0.0.20", 7000),
    networkEndpoint("admin", "tcp", "public", "10.0.0.20", 7500)
  ]));

  const endpoints = await discoverGameServerAdminEndpoints(redis);

  assert.deepEqual(endpoints, []);
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

test("discoverGameProxyAdminEndpoints requires admin visibility and never falls back to client endpoints", async () => {
  const redis = createRedisCapture();
  putInstance(redis, registryInstance("game-proxy", "game-proxy-public-admin-name", [
    networkEndpoint("admin", "http", "public", "10.0.1.10", 4000)
  ], { port: 4000, adminPort: 7101 }));
  putInstance(redis, registryInstance("game-proxy", "game-proxy-internal-admin-name", [
    networkEndpoint("admin", "http", "internal", "10.0.1.11", 7102)
  ], { port: 4000, adminPort: 7102 }));
  putInstance(redis, registryInstance("game-proxy", "game-proxy-admin", [
    networkEndpoint("client", "kcp", "public", "10.0.1.12", 4000),
    networkEndpoint("admin", "http", "admin", "10.0.1.12", 7101)
  ], { port: 4000, adminPort: 7101 }));

  const endpoints = await discoverGameProxyAdminEndpoints(redis);

  assert.deepEqual(endpoints.map(({ instanceId, endpointName, port }) => ({ instanceId, endpointName, port })), [
    { instanceId: "game-proxy-admin", endpointName: "admin", port: 7101 }
  ]);
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
  await new Promise((resolve) => setImmediate(resolve));
  client.stopHeartbeat();

  assert.deepEqual(redis.keys.get("heartbeat:admin-api:admin-api-test-001"), {
    ttl: 30,
    value: "1"
  });

  await client.deregister();

  assert.equal(redis.hashes.has("service:admin-api:instances:admin-api-test-001:data"), false);
  assert.equal(redis.keys.has("heartbeat:admin-api:admin-api-test-001"), false);
});

test("RegistryClient records lifecycle metrics for register heartbeat and deregister failures", async () => {
  resetRegistryLifecycleMetrics();

  await assert.rejects(
    () => new RegistryClient(createFailingRedis("hset"), createConfig()).register(),
    /HSET_FAILED/
  );

  const heartbeatRedis = createFailingRedis("setex");
  const heartbeatClient = new RegistryClient(heartbeatRedis, createConfig());
  heartbeatClient.startHeartbeat(60);
  await new Promise((resolve) => setImmediate(resolve));
  heartbeatClient.stopHeartbeat();

  const deregisterRedis = createFailingRedis("del");
  await assert.rejects(
    () => new RegistryClient(deregisterRedis, createConfig()).deregister(),
    /DEL_FAILED/
  );

  assert.deepEqual(
    getRegistryLifecycleMetricsSnapshot().map(({ kind, service, endpoint, instance_id, reason, count }) => ({
      kind,
      service,
      endpoint,
      instance_id,
      reason,
      count
    })),
    [
      {
        kind: "deregister_failed",
        service: "admin-api",
        endpoint: "",
        instance_id: "admin-api-test-001",
        reason: "redis_error",
        count: 1
      },
      {
        kind: "heartbeat_failed",
        service: "admin-api",
        endpoint: "",
        instance_id: "admin-api-test-001",
        reason: "redis_error",
        count: 1
      },
      {
        kind: "register_failed",
        service: "admin-api",
        endpoint: "http",
        instance_id: "admin-api-test-001",
        reason: "redis_error",
        count: 1
      }
    ]
  );

  resetRegistryLifecycleMetrics();
});
