import assert from "node:assert/strict";
import http from "node:http";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { MonitoringService } = await import("./monitoring.service.ts");
const {
  recordDiscoveryMetric,
  recordRegistryLifecycleMetric,
  resetDiscoveryMetrics,
  resetRegistryCapacityMetrics,
  resetRegistryLifecycleMetrics
} = await import("../../../../packages/service-registry/node/registry-schema.js");

function makeService(config = {}) {
  const redis = {};
  const dbPool = {};
  const baseConfig = {
    gameProxyAdminHost: "127.0.0.1",
    gameProxyAdminPort: 0,
    gameProxyAdminToken: "write-token",
    gameProxyAdminReadToken: "read-token",
    gameProxyAdminRequestTimeoutMs: 500,
    gameProxyAdminMaxResponseBytes: 4096,
    localDiscoveryFallbackEnabled: true,
    ...config
  };
  return new MonitoringService(
    baseConfig,
    redis,
    dbPool
  );
}

function makeMonitoringServiceWithRedis(config = {}, redis = {}) {
  const baseConfig = {
    gameProxyAdminHost: "127.0.0.1",
    gameProxyAdminPort: 0,
    gameProxyAdminToken: "write-token",
    gameProxyAdminReadToken: "read-token",
    gameProxyAdminRequestTimeoutMs: 500,
    gameProxyAdminMaxResponseBytes: 4096,
    localDiscoveryFallbackEnabled: true,
    ...config
  };
  return new MonitoringService(
    baseConfig,
    redis,
    {}
  );
}

function createServiceRedis(instances, options = {}) {
  const hashes = new Map();
  const keys = new Set();
  const ttls = new Map(Object.entries(options.ttls || {}));

  for (const instance of instances) {
    hashes.set(`service:${instance.name}:instances:${instance.id}:data`, JSON.stringify(instance));
    keys.add(`heartbeat:${instance.name}:${instance.id}`);
  }

  for (const entry of options.rawEntries || []) {
    hashes.set(`${entry.key}:data`, entry.data);
    if (entry.heartbeatKey) {
      keys.add(entry.heartbeatKey);
    }
  }

  for (const entry of options.metricEntries || []) {
    hashes.set(entry.key, Object.fromEntries(
      Object.entries(entry.data || {}).map(([key, value]) => [key, String(value)])
    ));
  }

  return {
    async get(key) {
      if (key === "metrics:heartbeat:game-server") {
        return String(Math.floor(Date.now() / 1000));
      }
      return null;
    },
    async hget(key, field) {
      return hashes.get(`${key}:${field}`) || null;
    },
    async hgetall(key) {
      const value = hashes.get(key);
      return value && typeof value === "object" ? value : {};
    },
    async exists(key) {
      return keys.has(key) ? 1 : 0;
    },
    async ttl(key) {
      if (ttls.has(key)) {
        return ttls.get(key);
      }
      return keys.has(key) ? 30 : -2;
    },
    async scan(cursor, _match, pattern) {
      if (cursor !== "0") {
        return ["0", []];
      }
      if (pattern.startsWith("service:")) {
        const prefix = pattern.replace("*", "");
        return [
          "0",
          [...hashes.keys()]
            .map((key) => key.slice(0, -5))
            .filter((key) => key.startsWith(prefix))
        ];
      }
      if (pattern.startsWith("metrics:")) {
        const prefix = pattern.replace("*", "");
        return [
          "0",
          [...hashes.keys()]
            .filter((key) => typeof key === "string" && key.startsWith(prefix))
        ];
      }
      return ["0", []];
    }
  };
}

function createServiceRedisWithoutTtl(instances) {
  const redis = createServiceRedis(instances);
  delete redis.ttl;
  return redis;
}

function gameServerInstance(id, host, port) {
  return {
    schema_version: 2,
    id,
    name: "game-server",
    host,
    port: 7000,
    admin_port: port,
    local_socket: "",
    endpoints: [
      {
        name: "admin",
        protocol: "tcp",
        host,
        port,
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
}

function gameProxyInstance(id, host, port) {
  return {
    schema_version: 2,
    id,
    name: "game-proxy",
    host,
    port: 4000,
    admin_port: port,
    local_socket: "",
    endpoints: [
      {
        name: "admin",
        protocol: "http",
        host,
        port,
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
}

async function withHttpServer(handler, fn) {
  const server = http.createServer(handler);
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));

  try {
    await fn(server.address().port);
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
}

test("rolloutDrain returns drained rollout warning snapshot", async () => {
  await withHttpServer((req, res) => {
    assert.equal(req.url, "/rollout");
    assert.equal(req.headers.authorization, "Bearer read-token");
    res.setHeader("content-type", "application/json");
    res.end(
      JSON.stringify({
        ok: true,
        rollout_session: {
          rollout_epoch: "epoch-1",
          old_server_id: "old-1",
          new_server_id: "new-1",
          state: "Active",
          started_at_ms: 1713000000000
        },
        drain_evaluation: {
          status: "Drained",
          rollout_epoch: "epoch-1",
          old_server_id: "old-1",
          new_server_id: "new-1",
          blocked_room_count: 0,
          blocked_player_count: 0,
          stale_room_route_count: 1,
          stale_player_route_count: 2,
          blocked_room_samples: [],
          blocked_player_samples: []
        }
      })
    );
  }, async (port) => {
    const service = makeService({
      registryDiscoveryEnabled: false,
      registryDiscoveryRequired: false,
      gameProxyAdminPort: port
    });
    const result = await service.rolloutDrain();

    assert.equal(result.ok, true);
    assert.equal(result.active, true);
    assert.equal(result.status, "drained");
    assert.equal(result.alert_level, "warning");
    assert.equal(result.rollout.epoch, "epoch-1");
    assert.equal(result.rollout.old_server, "old-1");
    assert.equal(result.rollout.new_server, "new-1");
    assert.equal(result.blockers.stale_room_route_count, 1);
    assert.equal(result.blockers.stale_player_route_count, 2);
    assert.equal(result.instances[0].endpoint.fallback, true);
    assert.equal(result.instances[0].endpoint.source, "fallback");
    assert.equal(result.instances[0].endpoint.reason, "fallback_used");
  });
});

test("rolloutDrain returns blocked samples and does not overexpose sample lists", async () => {
  await withHttpServer((req, res) => {
    res.setHeader("content-type", "application/json");
    res.end(
      JSON.stringify({
        ok: true,
        rollout_session: {
          rollout_epoch: "epoch-2",
          old_server_id: "old-2",
          new_server_id: "new-2",
          state: "Active",
          started_at_ms: 1713000000000
        },
        drain_evaluation: {
          status: "Blocked",
          blocked_room_count: 6,
          blocked_player_count: 1,
          blocked_room_samples: ["r1", "r2", "r3", "r4", "r5", "r6"],
          blocked_player_samples: ["p1"]
        }
      })
    );
  }, async (port) => {
    const service = makeService({
      registryDiscoveryEnabled: false,
      registryDiscoveryRequired: false,
      gameProxyAdminPort: port
    });
    const result = await service.rolloutDrain();

    assert.equal(result.status, "blocked");
    assert.equal(result.alert_message, "至少一个 game-proxy 仍有旧服房间/玩家/迁移中阻塞");
    assert.equal(result.blockers.blocked_room_count, 6);
    assert.deepEqual(result.blockers.blocked_room_samples, ["r1", "r2", "r3", "r4", "r5"]);
    assert.deepEqual(result.blockers.blocked_player_samples, ["p1"]);
  });
});

test("rolloutDrain returns displayable critical state when proxy admin is unavailable", async () => {
  const service = makeService({
    registryDiscoveryEnabled: false,
    registryDiscoveryRequired: false,
    gameProxyAdminPort: 9
  });
  const result = await service.rolloutDrain();

  assert.equal(result.ok, false);
  assert.equal(result.status, "error");
  assert.equal(result.alert_level, "critical");
  assert.equal(result.alert_message, "1/1 个 game-proxy 控制面不可达");
  assert.equal(result.rollout, null);
});

test("rolloutDrain aggregates discovered game-proxy admin endpoints", async () => {
  const ports = [];
  const servers = [];

  for (const body of [
    {
      ok: true,
      rollout_session: {
        rollout_epoch: "epoch-3",
        old_server_id: "old-3",
        new_server_id: "new-3",
        state: "Active",
        started_at_ms: 1713000000000
      },
      drain_evaluation: {
        status: "Drained",
        blocked_room_count: 0,
        blocked_player_count: 0
      }
    },
    {
      ok: true,
      rollout_session: {
        rollout_epoch: "epoch-3",
        old_server_id: "old-3",
        new_server_id: "new-3",
        state: "Active",
        started_at_ms: 1713000000000
      },
      drain_evaluation: {
        status: "Blocked",
        blocked_room_count: 2,
        blocked_player_count: 1,
        blocked_room_samples: ["r9"]
      }
    }
  ]) {
    const server = http.createServer((req, res) => {
      assert.equal(req.url, "/rollout");
      assert.equal(req.headers.authorization, "Bearer read-token");
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify(body));
    });
    await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
    servers.push(server);
    ports.push(server.address().port);
  }

  try {
    const redis = createServiceRedis([
      gameProxyInstance("game-proxy-a", "127.0.0.1", ports[0]),
      gameProxyInstance("game-proxy-b", "127.0.0.1", ports[1])
    ]);
    const service = makeMonitoringServiceWithRedis(
      { registryDiscoveryEnabled: true, registryDiscoveryRequired: true },
      redis
    );

    const result = await service.rolloutDrain();

    assert.equal(result.ok, true);
    assert.equal(result.status, "blocked");
    assert.equal(result.active, true);
    assert.equal(result.drained, false);
    assert.equal(result.rollout.epoch, "epoch-3");
    assert.equal(result.blockers.blocked_room_count, 2);
    assert.equal(result.blockers.blocked_player_count, 1);
    assert.deepEqual(
      result.instances.map((instance) => [instance.instance_id, instance.status, instance.endpoint.port]),
      [
        ["game-proxy-a", "drained", ports[0]],
        ["game-proxy-b", "blocked", ports[1]]
      ]
    );
  } finally {
    await Promise.all(servers.map((server) => new Promise((resolve) => server.close(resolve))));
  }
});

test("rolloutDrain rejects local fallback when discovery is required", async () => {
  const service = makeService({
    registryDiscoveryEnabled: false,
    registryDiscoveryRequired: true,
    gameProxyAdminPort: 9
  });
  const result = await service.rolloutDrain();

  assert.equal(result.ok, false);
  assert.equal(result.status, "error");
  assert.equal(result.error, "SERVICE_DISCOVERY_REQUIRED");
});

test("rolloutDrain rejects direct proxy fallback when local fallback is disabled", async () => {
  const service = makeService({
    registryDiscoveryEnabled: false,
    registryDiscoveryRequired: false,
    localDiscoveryFallbackEnabled: false,
    gameProxyAdminHost: "203.0.113.30",
    gameProxyAdminPort: 17101
  });
  const result = await service.rolloutDrain();

  assert.equal(result.ok, false);
  assert.equal(result.status, "error");
  assert.equal(result.error, "SERVICE_DISCOVERY_REQUIRED");
  assert.equal(result.upstream.host, null);
  assert.equal(result.upstream.port, null);
});

test("services returns all discovered game-server and game-proxy admin endpoints", async () => {
  const redis = createServiceRedis([
    gameServerInstance("game-server-a", "10.0.0.1", 7500),
    gameServerInstance("game-server-b", "10.0.0.2", 7501),
    gameProxyInstance("game-proxy-a", "10.0.1.1", 7101),
    gameProxyInstance("game-proxy-b", "10.0.1.2", 7102)
  ]);
  const service = makeMonitoringServiceWithRedis(
    { registryDiscoveryEnabled: true, registryDiscoveryRequired: true },
    redis
  );

  const result = await service.services();
  const gameServer = result.services.find((item) => item.name === "game-server");
  const gameProxy = result.services.find((item) => item.name === "game-proxy");

  assert.deepEqual(
    gameServer.endpoints.map(({ instance_id, host, port, protocol, source, fallback, reason }) => ({
      instance_id,
      host,
      port,
      protocol,
      source,
      fallback,
      reason
    })),
    [
      {
        instance_id: "game-server-a",
        host: "10.0.0.1",
        port: 7500,
        protocol: "tcp",
        source: "registry",
        fallback: false,
        reason: "discovered"
      },
      {
        instance_id: "game-server-b",
        host: "10.0.0.2",
        port: 7501,
        protocol: "tcp",
        source: "registry",
        fallback: false,
        reason: "discovered"
      }
    ]
  );
  assert.deepEqual(
    gameServer.instances.map((instance) => [
      instance.instance_id,
      instance.status,
      instance.endpoints.map((endpoint) => endpoint.port)
    ]),
    [
      ["game-server-a", "online", [7500]],
      ["game-server-b", "online", [7501]]
    ]
  );

  assert.deepEqual(
    gameProxy.endpoints.map(({ instance_id, host, port, protocol, source, fallback, reason }) => ({
      instance_id,
      host,
      port,
      protocol,
      source,
      fallback,
      reason
    })),
    [
      {
        instance_id: "game-proxy-a",
        host: "10.0.1.1",
        port: 7101,
        protocol: "http",
        source: "registry",
        fallback: false,
        reason: "discovered"
      },
      {
        instance_id: "game-proxy-b",
        host: "10.0.1.2",
        port: 7102,
        protocol: "http",
        source: "registry",
        fallback: false,
        reason: "discovered"
      }
    ]
  );
  assert.deepEqual(
    gameProxy.instances.map((instance) => [
      instance.instance_id,
      instance.status,
      instance.endpoints.map((endpoint) => endpoint.port)
    ]),
    [
      ["game-proxy-a", "online", [7101]],
      ["game-proxy-b", "online", [7102]]
    ]
  );
});

test("registry returns all configured services with empty status for missing instances", async () => {
  resetDiscoveryMetrics();
  resetRegistryLifecycleMetrics();
  const redis = createServiceRedis([]);
  const service = makeMonitoringServiceWithRedis(
    { registryDiscoveryEnabled: true, registryDiscoveryRequired: true },
    redis
  );

  const result = await service.registry();

  assert.equal(result.ok, true);
  assert.ok(result.checked_at > 0);
  assert.deepEqual(
    result.services.map((item) => item.name),
    [
      "auth-http",
      "game-server",
      "game-proxy",
      "chat-server",
      "match-service",
      "announce-service",
      "mail-service",
      "admin-api"
    ]
  );
  assert.equal(result.services.find((item) => item.name === "game-server").status, "missing");
  assert.equal(result.services.find((item) => item.name === "game-server").instance_count, 0);
  assert.deepEqual(result.services.find((item) => item.name === "game-server").instances, []);
  assert.equal(result.alert_level, "critical");
  assert.ok(result.alerts.some((alert) => alert.kind === "no_healthy_instance" && alert.service === "game-server"));
});

test("registry returns instance heartbeat ttl and endpoint fields", async () => {
  resetDiscoveryMetrics();
  resetRegistryCapacityMetrics();
  resetRegistryLifecycleMetrics();
  const redis = createServiceRedis(
    [
      gameServerInstance("game-server-a", "10.0.0.1", 7500)
    ],
    {
      ttls: {
        "heartbeat:game-server:game-server-a": 24
      }
    }
  );
  const service = makeMonitoringServiceWithRedis(
    { registryDiscoveryEnabled: true, registryDiscoveryRequired: true },
    redis
  );

  const result = await service.registry();
  const gameServer = result.services.find((item) => item.name === "game-server");
  const instance = gameServer.instances[0];
  const endpoint = instance.endpoints[0];

  assert.equal(gameServer.instance_count, 1);
  assert.equal(gameServer.healthy_instance_count, 1);
  assert.equal(gameServer.status, "healthy");
  assert.equal(instance.instance_id, "game-server-a");
  assert.equal(instance.healthy, true);
  assert.equal(instance.registered_at, 1);
  assert.equal(instance.last_registered_at, 1);
  assert.equal(instance.heartbeat_ttl_seconds, 24);
  assert.equal(instance.heartbeat_status, "alive");
  assert.deepEqual(
    {
      name: endpoint.name,
      protocol: endpoint.protocol,
      host: endpoint.host,
      port: endpoint.port,
      socket: endpoint.socket,
      visibility: endpoint.visibility,
      healthy: endpoint.healthy,
      metadata: endpoint.metadata
    },
    {
      name: "admin",
      protocol: "tcp",
      host: "10.0.0.1",
      port: 7500,
      socket: "",
      visibility: "admin",
      healthy: true,
      metadata: {}
    }
  );
});

test("registry returns capacity summaries from latest service metrics", async () => {
  resetDiscoveryMetrics();
  resetRegistryCapacityMetrics();
  resetRegistryLifecycleMetrics();
  const bucket = Math.floor(Date.now() / 1000);
  const redis = createServiceRedis(
    [
      gameServerInstance("game-server-a", "10.0.0.1", 7500)
    ],
    {
      ttls: {
        "heartbeat:game-server:game-server-a": 24
      },
      metricEntries: [
        {
          key: `metrics:game-server:game-server-a:${bucket}`,
          data: {
            qps: 0,
            latency_ms: 0,
            registry_scan_total: 2,
            registry_scan_duration_ms_total: 11,
            registry_scan_duration_ms_last: 4,
            registry_scan_duration_ms_max: 7,
            registry_scan_instance_keys_total: 6,
            registry_scan_instance_keys_last: 3,
            registry_scan_visible_instances_total: 5,
            registry_scan_visible_instances_last: 2,
            registry_discovery_cache_hit_total: 4,
            registry_discovery_cache_miss_total: 1,
            registry_discovery_cache_hit_rate_basis_points: 8000
          }
        },
        {
          key: `metrics:game-server:game-server-b:${bucket}`,
          data: {
            qps: 0,
            latency_ms: 0,
            registry_scan_total: 1,
            registry_scan_duration_ms_total: 9,
            registry_scan_duration_ms_last: 9,
            registry_scan_duration_ms_max: 9,
            registry_scan_instance_keys_total: 4,
            registry_scan_instance_keys_last: 4,
            registry_scan_visible_instances_total: 4,
            registry_scan_visible_instances_last: 4,
            registry_discovery_cache_hit_total: 1,
            registry_discovery_cache_miss_total: 4,
            registry_discovery_cache_hit_rate_basis_points: 2000
          }
        },
        {
          key: `metrics:auth-http:auth-http-a:${bucket}`,
          data: {
            qps: 0,
            latency_ms: 0,
            registry_scan_total: 1,
            registry_scan_duration_ms_total: 3,
            registry_scan_duration_ms_last: 3,
            registry_scan_duration_ms_max: 3,
            registry_scan_instance_keys_total: 1,
            registry_scan_instance_keys_last: 1,
            registry_scan_visible_instances_total: 1,
            registry_scan_visible_instances_last: 1,
            registry_discovery_cache_hit_total: 1,
            registry_discovery_cache_miss_total: 0,
            registry_discovery_cache_hit_rate_basis_points: 10000
          }
        }
      ]
    }
  );
  const service = makeMonitoringServiceWithRedis(
    { registryDiscoveryEnabled: true, registryDiscoveryRequired: true },
    redis
  );

  const result = await service.registry();
  const gameServer = result.services.find((item) => item.name === "game-server");

  assert.deepEqual(gameServer.capacity, {
    scan_total: 3,
    scan_duration_ms_total: 20,
    scan_duration_ms_last: 9,
    scan_duration_ms_max: 9,
    scan_instance_keys_total: 10,
    scan_instance_keys_last: 4,
    scan_visible_instances_total: 9,
    scan_visible_instances_last: 4,
    cache_hit_total: 5,
    cache_miss_total: 5,
    cache_hit_rate_basis_points: 5000
  });
  assert.equal(result.capacity.scan_total, 4);
  assert.equal(result.capacity.scan_duration_ms_total, 23);
  assert.equal(result.capacity.scan_duration_ms_max, 9);
  assert.equal(result.capacity.cache_hit_total, 6);
  assert.equal(result.capacity.cache_miss_total, 5);
  assert.equal(result.capacity.cache_hit_rate_basis_points, 5455);
  assert.equal(result.alerts.some((alert) => String(alert.kind).includes("capacity")), false);
});

test("registry tolerates redis clients without ttl support", async () => {
  resetDiscoveryMetrics();
  resetRegistryLifecycleMetrics();
  const redis = createServiceRedisWithoutTtl([
    gameProxyInstance("game-proxy-a", "10.0.1.1", 7101)
  ]);
  const service = makeMonitoringServiceWithRedis(
    { registryDiscoveryEnabled: true, registryDiscoveryRequired: true },
    redis
  );

  const result = await service.registry();
  const instance = result.services.find((item) => item.name === "game-proxy").instances[0];

  assert.equal(instance.instance_id, "game-proxy-a");
  assert.equal(instance.heartbeat_ttl_seconds, null);
  assert.equal(instance.heartbeat_status, "unknown");
});

test("registry returns discovery alerts for missing endpoints, schema parse failures, and fallback metrics", async () => {
  resetDiscoveryMetrics();
  resetRegistryLifecycleMetrics();
  recordDiscoveryMetric({
    serviceName: "game-server",
    endpointName: "admin",
    source: "fallback",
    reason: "fallback_used"
  });
  recordDiscoveryMetric({
    serviceName: "chat-server",
    endpointName: "tcp",
    source: "registry",
    reason: "endpoint_missing"
  });
  recordDiscoveryMetric({
    serviceName: "mail-service",
    endpointName: "http",
    source: "registry",
    reason: "registry_error"
  });

  const redis = createServiceRedis(
    [
      {
        schema_version: 2,
        id: "chat-server-a",
        name: "chat-server",
        host: "10.0.2.1",
        port: 9001,
        admin_port: 0,
        local_socket: "",
        endpoints: [
          {
            name: "other",
            protocol: "tcp",
            host: "10.0.2.1",
            port: 9001,
            socket: "",
            visibility: "internal",
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
    ],
    {
      rawEntries: [
        {
          key: "service:mail-service:instances:bad-json",
          data: "{broken",
          heartbeatKey: "heartbeat:mail-service:bad-json"
        }
      ]
    }
  );
  const service = makeMonitoringServiceWithRedis(
    { registryDiscoveryEnabled: true, registryDiscoveryRequired: true },
    redis
  );

  const result = await service.registry();
  const chatServer = result.services.find((item) => item.name === "chat-server");

  assert.equal(result.ok, true);
  assert.equal(result.alert_level, "critical");
  assert.ok(result.alerts.some((alert) => alert.kind === "endpoint_missing" && alert.service === "chat-server"));
  assert.ok(result.alerts.some((alert) => alert.kind === "schema_parse_failed" && alert.service === "mail-service"));
  assert.ok(result.alerts.some((alert) => alert.kind === "fallback_used" && alert.service === "game-server"));
  assert.ok(result.alerts.some((alert) => alert.kind === "discovery_failure" && alert.service === "mail-service"));
  assert.equal(
    result.alerts.filter((alert) => alert.kind === "endpoint_missing" && alert.service === "chat-server" && alert.endpoint === "tcp").length,
    1
  );
  assert.ok(chatServer.alerts.some((alert) => alert.kind === "endpoint_missing" && alert.endpoint === "tcp"));

  resetDiscoveryMetrics();
  resetRegistryLifecycleMetrics();
});

test("registry returns lifecycle alerts for register heartbeat and deregister failures", async () => {
  resetDiscoveryMetrics();
  resetRegistryLifecycleMetrics();
  recordRegistryLifecycleMetric("register_failed", {
    serviceName: "admin-api",
    endpointName: "http",
    instanceId: "admin-api-a",
    reason: "redis_error"
  });
  recordRegistryLifecycleMetric("heartbeat_failed", {
    serviceName: "admin-api",
    instanceId: "admin-api-a",
    reason: "redis_error"
  });
  recordRegistryLifecycleMetric("deregister_failed", {
    serviceName: "admin-api",
    instanceId: "admin-api-a",
    reason: "redis_error"
  });

  const redis = createServiceRedis([]);
  const service = makeMonitoringServiceWithRedis(
    { registryDiscoveryEnabled: true, registryDiscoveryRequired: true },
    redis
  );

  const result = await service.registry();

  assert.equal(result.alert_level, "critical");
  assert.ok(result.alerts.some((alert) =>
    alert.kind === "register_failed" &&
    alert.service === "admin-api" &&
    alert.endpoint === "http" &&
    alert.instance_id === "admin-api-a" &&
    alert.count === 1
  ));
  assert.ok(result.alerts.some((alert) => alert.kind === "heartbeat_failed" && alert.service === "admin-api"));
  assert.ok(result.alerts.some((alert) => alert.kind === "deregister_failed" && alert.service === "admin-api"));

  resetRegistryLifecycleMetrics();
});

test("registry returns lifecycle alerts from latest Redis metrics by instance", async () => {
  resetDiscoveryMetrics();
  resetRegistryLifecycleMetrics();

  const bucket = Math.floor(Date.now() / 1000);
  const redis = createServiceRedis([], {
    metricEntries: [
      {
        key: `metrics:auth-http:auth-http-a:${bucket}`,
        data: {
          qps: 0,
          latency_ms: 0,
          register_failed_total: 2,
          heartbeat_failed_total: 3,
          deregister_failed_total: 1
        }
      }
    ]
  });
  const service = makeMonitoringServiceWithRedis(
    { registryDiscoveryEnabled: true, registryDiscoveryRequired: true },
    redis
  );

  const result = await service.registry();

  assert.ok(result.alerts.some((alert) =>
    alert.kind === "register_failed" &&
    alert.service === "auth-http" &&
    alert.instance_id === "auth-http-a" &&
    alert.source === "metrics" &&
    alert.count === 2
  ));
  assert.ok(result.alerts.some((alert) =>
    alert.kind === "heartbeat_failed" &&
    alert.service === "auth-http" &&
    alert.instance_id === "auth-http-a" &&
    alert.source === "metrics" &&
    alert.count === 3
  ));
  assert.ok(result.alerts.some((alert) =>
    alert.kind === "deregister_failed" &&
    alert.service === "auth-http" &&
    alert.instance_id === "auth-http-a" &&
    alert.source === "metrics" &&
    alert.count === 1
  ));
});
