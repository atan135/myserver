import assert from "node:assert/strict";
import test from "node:test";

import {
  getRegistryLifecycleMetricsSnapshot,
  resetRegistryLifecycleMetrics,
  validateServiceInstance
} from "../../../packages/service-registry/node/registry-schema.js";
import { configureLogger } from "./logger.js";
import { RegistryClient, discoverGameServerAdminEndpoints } from "./registry-client.js";

configureLogger({
  appName: "mail-service-test",
  logEnableConsole: false,
  logEnableFile: false,
  logLevel: "off",
  logDir: "logs/mail-service-test"
});

function createRedisCapture() {
  const hashes = new Map();
  const keys = new Map();

  return {
    hashes,
    keys,
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
    serviceName: "mail-service",
    serviceInstanceId: "mail-test-001",
    host: "10.10.0.3",
    port: 9103,
    mailPlayerAuthRequired: true,
    mailServiceToken: "test-mail-service-token",
    claimNewRequestsEnabled: false,
    claimRecoveryEnabled: true,
    serviceBuildVersion: "2026.06.18+mail",
    serviceZone: "zone-mail",
    ...overrides
  };
}

test("RegistryClient registers mail-service http endpoint and metadata", async () => {
  const redis = createRedisCapture();
  const config = createConfig();
  const client = new RegistryClient(redis, config);

  await client.register();

  const raw = redis.hashes.get("service:mail-service:instances:mail-test-001:data");
  assert.ok(raw);

  const payload = JSON.parse(raw);
  assert.deepEqual(validateServiceInstance(payload), { ok: true, errors: [] });
  assert.equal(payload.host, "10.10.0.3");
  assert.equal(payload.port, 9103);
  assert.deepEqual(payload.endpoints, [
    {
      name: "http",
      protocol: "http",
      host: "10.10.0.3",
      port: 9103,
      socket: "",
      visibility: "internal",
      metadata: {
        service_name: "mail-service",
        service_instance_id: "mail-test-001",
        build_version: "2026.06.18+mail",
        zone: "zone-mail"
      },
      healthy: true
    }
  ]);
  assert.deepEqual(payload.metadata, {
    service_name: "mail-service",
    service_instance_id: "mail-test-001",
    player_auth_required: true,
    service_token_enabled: true,
    mail_notification_contract_version: 1,
    claim_new_requests_enabled: false,
    claim_recovery_enabled: true,
    build_version: "2026.06.18+mail",
    zone: "zone-mail"
  });
});

test("RegistryClient publishes advertised host instead of bind host", async () => {
  const redis = createRedisCapture();
  const client = new RegistryClient(
    redis,
    createConfig({
      host: "0.0.0.0",
      advertisedHost: "10.10.0.23"
    })
  );

  await client.register();

  const payload = JSON.parse(redis.hashes.get("service:mail-service:instances:mail-test-001:data"));
  assert.equal(payload.host, "10.10.0.23");
  assert.equal(payload.endpoints[0].host, "10.10.0.23");
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

    const payload = JSON.parse(redis.hashes.get("service:mail-service:instances:mail-test-001:data"));
    assert.equal(payload.host, "127.0.0.1");
    assert.equal(payload.endpoints[0].host, "127.0.0.1");
  }
});

test("RegistryClient metadata marks missing service token as disabled", async () => {
  const redis = createRedisCapture();
  const client = new RegistryClient(
    redis,
    createConfig({
      mailPlayerAuthRequired: false,
      mailServiceToken: "   ",
      serviceBuildVersion: ""
    })
  );

  await client.register();

  const payload = JSON.parse(redis.hashes.get("service:mail-service:instances:mail-test-001:data"));
  assert.equal(payload.metadata.player_auth_required, false);
  assert.equal(payload.metadata.service_token_enabled, false);
  assert.equal(payload.metadata.build_version, "dev");
  assert.equal(payload.metadata.zone, "zone-mail");
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
    gameServerInstance("game-server-public-admin-name", [endpoint("admin", "public", 7000)]),
    gameServerInstance("game-server-internal-admin-name", [endpoint("admin", "internal", 7600)]),
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
  assert.deepEqual(
    endpoints.map(({ fallback, source, reason }) => ({ fallback, source, reason })),
    [{ fallback: false, source: "registry", reason: "discovered" }]
  );
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
    () => new RegistryClient(createFailingRedis("hset"), createConfig()).register(),
    /HSET_FAILED/
  );

  const heartbeatClient = new RegistryClient(createFailingRedis("setex"), createConfig());
  heartbeatClient.startHeartbeat(60);
  await new Promise((resolve) => setImmediate(resolve));
  heartbeatClient.stopHeartbeat();

  await assert.rejects(
    () => new RegistryClient(createFailingRedis("del"), createConfig()).deregister(),
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
        service: "mail-service",
        endpoint: "",
        instance_id: "mail-test-001",
        reason: "redis_error",
        count: 1
      },
      {
        kind: "heartbeat_failed",
        service: "mail-service",
        endpoint: "",
        instance_id: "mail-test-001",
        reason: "redis_error",
        count: 1
      },
      {
        kind: "register_failed",
        service: "mail-service",
        endpoint: "http",
        instance_id: "mail-test-001",
        reason: "redis_error",
        count: 1
      }
    ]
  );

  resetRegistryLifecycleMetrics();
});
