import assert from "node:assert/strict";
import test from "node:test";

import {
  getRegistryLifecycleMetricsSnapshot,
  resetRegistryLifecycleMetrics,
  validateServiceInstance
} from "../../../packages/service-registry/node/registry-schema.js";
import { createRegistryRedisCapture } from "../../../packages/service-registry/node/test-registry-redis.js";
import { configureLogger } from "./logger.js";
import { RegistryClient, discoverGameServerAdminEndpoints } from "./registry-client.js";

configureLogger({
  appName: "auth-http-registry-test",
  logEnableConsole: false,
  logEnableFile: false,
  logLevel: "off",
  logDir: "logs/auth-http-registry-test"
});

function createRedisCapture() {
  return createRegistryRedisCapture();
}

function createFailingRedis(operation) {
  const redis = createRedisCapture();
  redis.failEval = `${operation.toUpperCase()}_FAILED`;
  return redis;
}

function createConfig(overrides = {}) {
  return {
    serviceName: "auth-http",
    serviceInstanceId: "auth-http-test-001",
    host: "10.10.0.2",
    port: 3100,
    strictSecurity: true,
    ticketValidateEnabled: true,
    serviceBuildVersion: "2026.06.18+auth",
    serviceZone: "zone-auth",
    ...overrides
  };
}

test("RegistryClient registers auth-http public and internal http endpoints and metadata", async () => {
  const redis = createRedisCapture();
  const config = createConfig();
  const client = new RegistryClient(redis, config);

  await client.register();

  const raw = redis.hashes.get("service:auth-http:instances:auth-http-test-001:data");
  assert.ok(raw);

  const payload = JSON.parse(raw);
  assert.deepEqual(validateServiceInstance(payload), { ok: true, errors: [] });
  assert.equal(payload.host, "10.10.0.2");
  assert.equal(payload.port, 3100);
  assert.deepEqual(payload.endpoints, [
    {
      name: "http",
      protocol: "http",
      host: "10.10.0.2",
      port: 3100,
      socket: "",
      visibility: "public",
      metadata: {
        service_name: "auth-http",
        service_instance_id: "auth-http-test-001",
        build_version: "2026.06.18+auth",
        zone: "zone-auth"
      },
      healthy: true
    },
    {
      name: "internal",
      protocol: "http",
      host: "10.10.0.2",
      port: 3100,
      socket: "",
      visibility: "internal",
      metadata: {
        service_name: "auth-http",
        service_instance_id: "auth-http-test-001",
        build_version: "2026.06.18+auth",
        zone: "zone-auth"
      },
      healthy: true
    }
  ]);
  assert.deepEqual(payload.metadata, {
    service_name: "auth-http",
    service_instance_id: "auth-http-test-001",
    strict_security: true,
    ticket_validation_enabled: true,
    build_version: "2026.06.18+auth",
    zone: "zone-auth"
  });
});

test("RegistryClient publishes advertised host instead of bind host", async () => {
  const redis = createRedisCapture();
  const client = new RegistryClient(
    redis,
    createConfig({
      host: "0.0.0.0",
      advertisedHost: "10.10.0.20"
    })
  );

  await client.register();

  const payload = JSON.parse(redis.hashes.get("service:auth-http:instances:auth-http-test-001:data"));
  assert.equal(payload.host, "10.10.0.20");
  assert.equal(payload.endpoints[0].host, "10.10.0.20");
  assert.equal(payload.endpoints[1].host, "10.10.0.20");
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

    const payload = JSON.parse(redis.hashes.get("service:auth-http:instances:auth-http-test-001:data"));
    assert.equal(payload.host, "127.0.0.1");
    assert.equal(payload.endpoints[0].host, "127.0.0.1");
    assert.equal(payload.endpoints[1].host, "127.0.0.1");
  }
});

test("RegistryClient metadata falls back to dev build version", async () => {
  const redis = createRedisCapture();
  const client = new RegistryClient(
    redis,
    createConfig({
      strictSecurity: false,
      ticketValidateEnabled: false,
      serviceBuildVersion: ""
    })
  );

  await client.register();

  const payload = JSON.parse(redis.hashes.get("service:auth-http:instances:auth-http-test-001:data"));
  assert.equal(payload.metadata.strict_security, false);
  assert.equal(payload.metadata.ticket_validation_enabled, false);
  assert.equal(payload.metadata.build_version, "dev");
  assert.equal(payload.metadata.zone, "zone-auth");
});

test("RegistryClient uses registry key prefix for registration", async () => {
  const redis = createRedisCapture();
  const client = new RegistryClient(redis, createConfig({ registryKeyPrefix: "test:" }));

  await client.register();

  assert.ok(redis.hashes.has("test:service:auth-http:instances:auth-http-test-001:data"));
  assert.equal(redis.hashes.has("service:auth-http:instances:auth-http-test-001:data"), false);
});

function gameServerInstance(id, endpoints) {
  return {
    schema_version: 2,
    id,
    name: "game-server",
    host: "10.0.0.1",
    port: 7000,
    admin_port: 7500,
    local_socket: "",
    endpoints,
    tags: [],
    weight: 100,
    metadata: {},
    registered_at: 1,
    healthy: true
  };
}

function endpoint(name, visibility, port) {
  return {
    name,
    protocol: "tcp",
    host: "10.0.0.1",
    port,
    socket: "",
    visibility,
    metadata: {},
    healthy: true
  };
}

test("discoverGameServerAdminEndpoints requires admin endpoint visibility", async () => {
  const redis = createRedisCapture();
  const instances = [
    gameServerInstance("game-server-client", [endpoint("admin", "public", 7000)]),
    gameServerInstance("game-server-internal", [endpoint("admin", "internal", 7600)]),
    gameServerInstance("game-server-admin", [
      endpoint("client", "public", 7001),
      endpoint("admin", "admin", 7500)
    ])
  ];

  for (const instance of instances) {
    redis.hashes.set(`service:game-server:instances:${instance.id}:data`, JSON.stringify(instance));
    redis.keys.set(`heartbeat:game-server:${instance.id}`, { ttl: 30, value: "1" });
  }

  const endpoints = await discoverGameServerAdminEndpoints(redis);

  assert.deepEqual(endpoints.map(({ instanceId, endpointName, port }) => ({ instanceId, endpointName, port })), [
    { instanceId: "game-server-admin", endpointName: "admin", port: 7500 }
  ]);
});

test("discoverGameServerAdminEndpoints does not fall back to client-visible endpoints", async () => {
  const redis = createRedisCapture();
  const instance = gameServerInstance("game-server-client-only", [
    endpoint("client", "public", 7000),
    endpoint("admin", "public", 7500)
  ]);
  redis.hashes.set(`service:game-server:instances:${instance.id}:data`, JSON.stringify(instance));
  redis.keys.set(`heartbeat:game-server:${instance.id}`, { ttl: 30, value: "1" });

  const endpoints = await discoverGameServerAdminEndpoints(redis);

  assert.deepEqual(endpoints, []);
});

test("RegistryClient records lifecycle metrics for register heartbeat and deregister failures", async () => {
  resetRegistryLifecycleMetrics();

  await assert.rejects(
    () => new RegistryClient(createFailingRedis("eval"), createConfig()).register(),
    /EVAL_FAILED/
  );

  const heartbeatClient = new RegistryClient(createFailingRedis("eval"), createConfig());
  heartbeatClient.startHeartbeat(60);
  await new Promise((resolve) => setImmediate(resolve));
  heartbeatClient.stopHeartbeat();

  await assert.rejects(
    () => new RegistryClient(createFailingRedis("eval"), createConfig()).deregister(),
    /EVAL_FAILED/
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
        service: "auth-http",
        endpoint: "",
        instance_id: "auth-http-test-001",
        reason: "redis_error",
        count: 1
      },
      {
        kind: "heartbeat_failed",
        service: "auth-http",
        endpoint: "",
        instance_id: "auth-http-test-001",
        reason: "redis_error",
        count: 1
      },
      {
        kind: "register_failed",
        service: "auth-http",
        endpoint: "http",
        instance_id: "auth-http-test-001",
        reason: "redis_error",
        count: 1
      }
    ]
  );

  resetRegistryLifecycleMetrics();
});
