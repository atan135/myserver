import assert from "node:assert/strict";
import { TextEncoder } from "node:util";
import { test } from "node:test";

import { getConfig } from "../apps/metrics-collector/src/config.js";
import {
  maybeRegisterService,
  REGISTRY_DISABLED_REASON
} from "../apps/metrics-collector/src/registry-client.js";
import {
  buildMetricsKey,
  isDirectRun,
  normalizeInstanceId,
  writeMetrics
} from "../apps/metrics-collector/src/server.js";
import { pathToFileURL } from "node:url";
import { archiveServiceMetrics } from "../apps/admin-api/src/services/archive.js";
import {
  aggregateMetricRecords,
  aggregateMetricRecordsDetailed,
  buildInstanceMetricPoint,
  buildMetricPoint,
  parseMetricHeartbeatKey,
  parseMetricKey
} from "../apps/admin-api/src/monitoring/metrics-aggregation.js";

class FakePipeline {
  constructor(redis) {
    this.redis = redis;
    this.commands = [];
  }

  hset(key, fields) {
    this.commands.push(["hset", key, fields]);
    return this;
  }

  expire(key, ttl) {
    this.commands.push(["expire", key, ttl]);
    return this;
  }

  set(key, value, mode, ttl) {
    this.commands.push(["set", key, value, mode, ttl]);
    return this;
  }

  async exec() {
    for (const command of this.commands) {
      const [name, ...args] = command;
      if (name === "hset") {
        const [key, fields] = args;
        this.redis.hashes.set(key, {
          ...(this.redis.hashes.get(key) || {}),
          ...fields
        });
      } else if (name === "expire") {
        const [key, ttl] = args;
        this.redis.expirations.set(key, ttl);
      } else if (name === "set") {
        const [key, value, mode, ttl] = args;
        this.redis.values.set(key, value);
        this.redis.expirations.set(key, ttl);
        assert.equal(mode, "EX");
      }
    }
    return [];
  }
}

class FakeRedis {
  constructor() {
    this.hashes = new Map();
    this.values = new Map();
    this.expirations = new Map();
  }

  pipeline() {
    return new FakePipeline(this);
  }

  async scan(_cursor, _match, pattern) {
    const prefix = pattern.slice(0, -1);
    return ["0", [...this.hashes.keys()].filter((key) => key.startsWith(prefix))];
  }

  async hgetall(key) {
    return this.hashes.get(key) || {};
  }

  async del(key) {
    this.hashes.delete(key);
  }
}

class RegistryRedisCapture {
  constructor() {
    this.writes = [];
  }

  async hset(key, ...args) {
    this.writes.push(["hset", key, ...args]);
  }

  async setex(key, ...args) {
    this.writes.push(["setex", key, ...args]);
  }

  async set(key, ...args) {
    this.writes.push(["set", key, ...args]);
  }

  async del(key) {
    this.writes.push(["del", key]);
  }

  hasWrittenKeyPrefix(prefix) {
    return this.writes.some(([, key]) => String(key).startsWith(prefix));
  }
}

function metricsMessage(payload) {
  return {
    data: new TextEncoder().encode(JSON.stringify(payload))
  };
}

async function withEnv(overrides, callback) {
  const previous = new Map();
  for (const [key, value] of Object.entries(overrides)) {
    previous.set(key, process.env[key]);
    if (value === undefined) {
      delete process.env[key];
    } else {
      process.env[key] = value;
    }
  }

  try {
    return await callback();
  } finally {
    for (const [key, value] of previous.entries()) {
      if (value === undefined) {
        delete process.env[key];
      } else {
        process.env[key] = value;
      }
    }
  }
}

test("metrics collector writes instance scoped key and stable fallback key", async () => {
  const redis = new FakeRedis();
  const config = {
    metricsTtlSeconds: 600,
    heartbeatTtlSeconds: 30
  };

  await writeMetrics(
    redis,
    config,
    metricsMessage({
      service: "game-server",
      instance_id: "gs-1",
      bucket: 1700000000,
      timestamp: 1700000001,
      metrics: {
        qps: 5,
        latency_ms: 12
      }
    })
  );
  await writeMetrics(
    redis,
    config,
    metricsMessage({
      service: "game-server",
      bucket: 1700000000,
      timestamp: 1700000002,
      metrics: {
        qps: 7,
        latency_ms: 9
      }
    })
  );

  assert.equal(
    buildMetricsKey("game-server", "gs-1", 1700000000),
    "metrics:game-server:gs-1:1700000000"
  );
  assert.equal(normalizeInstanceId(undefined), "default");
  assert.deepEqual(redis.hashes.get("metrics:game-server:gs-1:1700000000"), {
    qps: "5",
    latency_ms: "12",
    instance_id: "gs-1"
  });
  assert.deepEqual(redis.hashes.get("metrics:game-server:default:1700000000"), {
    qps: "7",
    latency_ms: "9",
    instance_id: "default"
  });
  assert.equal(redis.values.get("metrics:heartbeat:game-server"), "1700000002");
  assert.equal(redis.values.get("metrics:heartbeat:game-server:gs-1"), "1700000001");
  assert.equal(redis.values.get("metrics:heartbeat:game-server:default"), "1700000002");
});

test("metrics collector defaults to explicit service registry opt-out", async () => {
  await withEnv(
    {
      SERVICE_REGISTRY_REGISTER: undefined,
      SERVICE_NAME: undefined,
      SERVICE_INSTANCE_ID: undefined,
      SERVICE_BUILD_VERSION: undefined
    },
    async () => {
      const config = getConfig();
      const redis = new RegistryRedisCapture();
      const status = await maybeRegisterService(redis, config);

      assert.equal(config.serviceRegistryRegister, false);
      assert.equal(config.serviceName, "metrics-collector");
      assert.equal(config.serviceInstanceId, "metrics-collector-001");
      assert.equal(config.registryKeyPrefix, "");
      assert.equal(status.registered, false);
      assert.equal(status.reason, REGISTRY_DISABLED_REASON);
      assert.deepEqual(redis.writes, []);
      assert.equal(
        redis.hasWrittenKeyPrefix("service:metrics-collector:instances:"),
        false
      );
    }
  );
});

test("metrics collector reads registry key prefix without registering an endpoint", async () => {
  await withEnv(
    {
      REGISTRY_KEY_PREFIX: "registry:",
      REDIS_KEY_PREFIX: "redis:"
    },
    async () => {
      const config = getConfig();
      const redis = new RegistryRedisCapture();
      const status = await maybeRegisterService(redis, config);

      assert.equal(config.registryKeyPrefix, "registry:");
      assert.equal(status.registered, false);
      assert.deepEqual(redis.writes, []);
    }
  );

  await withEnv(
    {
      REGISTRY_KEY_PREFIX: undefined,
      REDIS_KEY_PREFIX: "redis:"
    },
    async () => {
      const config = getConfig();

      assert.equal(config.registryKeyPrefix, "redis:");
    }
  );
});

test("metrics collector rejects service registry registration without a real endpoint", async () => {
  const redis = new RegistryRedisCapture();

  await assert.rejects(
    () =>
      maybeRegisterService(redis, {
        serviceRegistryRegister: true,
        serviceName: "metrics-collector",
        serviceInstanceId: "metrics-collector-test-001",
        serviceBuildVersion: "test"
      }),
    /SERVICE_REGISTRY_REGISTER=true is not supported/
  );

  assert.deepEqual(redis.writes, []);
  assert.equal(
    redis.hasWrittenKeyPrefix("service:metrics-collector:instances:"),
    false
  );
});

test("metrics collector direct-run detection uses file URLs across platforms", () => {
  const windowsPath = "C:\\project\\MyServer\\apps\\metrics-collector\\src\\server.js";
  assert.equal(isDirectRun(pathToFileURL(windowsPath).href, windowsPath), true);
  assert.equal(isDirectRun(pathToFileURL(windowsPath).href, undefined), false);
  assert.equal(isDirectRun("file:///other/server.js", windowsPath), false);
});

test("admin metrics parser accepts legacy and instance scoped keys", () => {
  assert.deepEqual(parseMetricKey("game-server", "metrics:game-server:1700000000"), {
    bucket: 1700000000,
    instanceId: null,
    legacy: true
  });
  assert.deepEqual(parseMetricKey("game-server", "metrics:game-server:gs-1:1700000000"), {
    bucket: 1700000000,
    instanceId: "gs-1",
    legacy: false
  });
  assert.deepEqual(parseMetricKey("game-server", "metrics:game-server:az:gs-1:1700000000"), {
    bucket: 1700000000,
    instanceId: "az:gs-1",
    legacy: false
  });
  assert.equal(parseMetricKey("game-server", "metrics:heartbeat:game-server"), null);
  assert.deepEqual(
    parseMetricHeartbeatKey("game-server", "metrics:heartbeat:game-server:gs-1"),
    { instanceId: "gs-1" }
  );
});

test("admin metrics aggregation combines same bucket instances into one point", () => {
  const serviceConfigs = {
    "game-server": {
      onlineField: "online_players"
    }
  };
  const data = aggregateMetricRecords([
    {
      instanceId: "gs-1",
      data: {
        qps: "5",
        latency_ms: "15",
        online_players: "10",
        online_sessions: "12",
        instance_id: "gs-1"
      }
    },
    {
      instanceId: "gs-2",
      data: {
        qps: "7",
        latency_ms: "9",
        online_players: "20",
        online_sessions: "21",
        instance_id: "gs-2"
      }
    },
    {
      legacy: true,
      data: {
        qps: "3",
        latency_ms: "30",
        online_players: "4",
        online_sessions: "5"
      }
    }
  ]);
  const point = buildMetricPoint("game-server", data, serviceConfigs, 1700000000);

  assert.equal(data.qps, "15");
  assert.equal(data.latency_ms, "30");
  assert.equal(data.online_players, "34");
  assert.equal(data.instance_ids, "gs-1,gs-2");
  assert.equal(point.timestamp, 1700000000);
  assert.equal(point.qps, 15);
  assert.equal(point.latency_ms, 30);
  assert.equal(point.online_value, 34);
  assert.equal(point.online_sessions, 38);
  assert.equal(point.instance_count, 2);
});

test("admin metrics aggregation exposes per-instance details for a bucket", () => {
  const serviceConfigs = {
    "game-server": {
      onlineField: "online_players"
    }
  };
  const aggregated = aggregateMetricRecordsDetailed([
    {
      instanceId: "gs-2",
      data: {
        qps: "7",
        latency_ms: "9",
        online_players: "20",
        custom_label: "blue",
        instance_id: "gs-2"
      }
    },
    {
      instanceId: "gs-1",
      data: {
        qps: "5",
        latency_ms: "15",
        online_players: "10",
        custom_label: "green",
        instance_id: "gs-1"
      }
    },
    {
      legacy: true,
      data: {
        qps: "3",
        latency_ms: "30",
        online_players: "4",
        custom_label: "legacy"
      }
    }
  ]);
  const point = buildMetricPoint(
    "game-server",
    aggregated.data,
    serviceConfigs,
    1700000000,
    aggregated.instances
  );

  assert.deepEqual(
    point.instances.map((instance) => instance.instance_id),
    ["gs-1", "gs-2", "legacy"]
  );
  assert.deepEqual(point.instances[0], {
    instance_id: "gs-1",
    qps: 5,
    latency_ms: 15,
    online_players: 10,
    custom_label: "green",
    online_value: 10,
    online_sessions: 0,
    unique_players: 0,
    active_sessions_5m: 0
  });
  assert.equal(point.instances[2].legacy, true);
  assert.equal(point.qps, 15);
  assert.equal(point.latency_ms, 30);
  assert.equal(point.online_value, 34);
});

test("admin instance metric point keeps unknown fields as per-instance extra", () => {
  const serviceConfigs = {
    "game-proxy": {
      onlineField: "connections"
    }
  };
  const point = buildInstanceMetricPoint(
    "game-proxy",
    {
      instance_id: "proxy-1",
      data: {
        qps: "2",
        latency_ms: "6",
        connections: "40",
        build: "2026.06"
      }
    },
    serviceConfigs
  );

  assert.deepEqual(point, {
    instance_id: "proxy-1",
    qps: 2,
    latency_ms: 6,
    connections: 40,
    build: "2026.06",
    online_value: 40,
    online_sessions: 0,
    unique_players: 0,
    active_sessions_5m: 0
  });
});

test("archive aggregates same bucket legacy and instance keys before insert and deletes sources", async () => {
  const redis = new FakeRedis();
  redis.hashes.set("metrics:game-server:1700000000", {
    qps: "3",
    latency_ms: "30",
    online_players: "4",
    online_sessions: "5"
  });
  redis.hashes.set("metrics:game-server:gs-1:1700000000", {
    qps: "5",
    latency_ms: "15",
    online_players: "10",
    online_sessions: "12",
    instance_id: "gs-1"
  });
  redis.hashes.set("metrics:game-server:gs-2:1700000000", {
    qps: "7",
    latency_ms: "9",
    online_players: "20",
    online_sessions: "21",
    instance_id: "gs-2"
  });
  redis.hashes.set("metrics:game-server:gs-1:1700000010", {
    qps: "99",
    latency_ms: "1",
    online_players: "99",
    instance_id: "gs-1"
  });

  const inserts = [];
  const dbPool = {
    async query(sql, params) {
      inserts.push({ sql, params });
    }
  };

  const archived = await archiveServiceMetrics(
    redis,
    dbPool,
    "game-server",
    1700000000,
    1700000005
  );

  assert.equal(archived, 1);
  assert.equal(inserts.length, 1);
  assert.deepEqual(inserts[0].params.slice(0, 5), [
    "game-server",
    1700000000,
    15,
    30,
    34
  ]);
  assert.deepEqual(JSON.parse(inserts[0].params[5]), {
    instance_ids: "gs-1,gs-2",
    instance_count: "2"
  });
  assert.equal(redis.hashes.has("metrics:game-server:1700000000"), false);
  assert.equal(redis.hashes.has("metrics:game-server:gs-1:1700000000"), false);
  assert.equal(redis.hashes.has("metrics:game-server:gs-2:1700000000"), false);
  assert.equal(redis.hashes.has("metrics:game-server:gs-1:1700000010"), true);
});
