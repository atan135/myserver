import process from "node:process";
import { randomUUID } from "node:crypto";
import { fileURLToPath } from "node:url";
import path from "node:path";

import {
  RegistryDiscoveryClient,
  createServiceInstancePayload,
  normalizeServiceInstance,
  registryHeartbeatKey,
  registryInstanceKey,
  validateServiceInstance
} from "../packages/service-registry/node/registry-schema.js";
import {
  RegistryClient as AdminRegistryClient,
  discoverGameProxyAdminEndpoints,
  discoverGameServerAdminEndpoints
} from "../apps/admin-api/src/registry-client.js";

const DEFAULT_REGISTRY_URL = "redis://127.0.0.1:6379";
const HEARTBEAT_TTL_SECONDS = 30;
const TTL_FALLBACK_SECONDS = 1;
const READINESS_TIMEOUT_MS = 3000;

export async function runRegistryCanaryLifecycle(options = {}) {
  const config = normalizeOptions(options);
  const redis = options.redis ?? await createRedisClient(config);
  const ownsRedis = !options.redis;
  const errors = [];
  const adminClient = new AdminRegistryClient(redis, adminRegistryConfig(config));
  const trackedInstances = createTrackedInstances(config);
  const report = createEmptyReport(config);

  try {
    await cleanupInstances(redis, config.registryKeyPrefix, trackedInstances.all);
    await registerCanaryGroup(redis, config, adminClient, trackedInstances);

    report.registration = await readRegistryStates(redis, config.registryKeyPrefix, trackedInstances.group);
    report.readiness = await waitForReadiness(redis, config, trackedInstances.group, errors);
    report.discovery = await runDiscoveryChecks(redis, config, trackedInstances, errors);
    report.ttlFallback = await runTtlFallbackCheck(redis, config, trackedInstances.ttlFallback, errors);
    report.shutdown = await runExplicitDeregisterCheck(redis, config, adminClient, trackedInstances, errors);
  } catch (error) {
    errors.push(errorObject("canary_lifecycle_failed", error?.message || String(error)));
  } finally {
    adminClient.stopHeartbeat();
    if (config.cleanup) {
      await cleanupInstances(redis, config.registryKeyPrefix, trackedInstances.all).catch((error) => {
        errors.push(errorObject("cleanup_failed", error?.message || String(error)));
      });
    }
    if (ownsRedis) {
      await closeRedis(redis);
    }
  }

  report.errors = errors;
  report.ok = errors.length === 0 &&
    report.readiness?.ok === true &&
    report.discovery?.ok === true &&
    report.ttlFallback?.ok === true &&
    report.shutdown?.ok === true;
  return report;
}

export class MemoryRedis {
  constructor({ now = () => Date.now() } = {}) {
    this.now = now;
    this.hashes = new Map();
    this.values = new Map();
  }

  async connect() {}

  async quit() {}

  disconnect() {}

  advanceTime(ms) {
    const fixedNow = this.now() + ms;
    this.now = () => fixedNow;
    this.pruneExpired();
  }

  async hset(key, field, value) {
    const hash = this.hashes.get(key) ?? new Map();
    hash.set(field, value);
    this.hashes.set(key, hash);
    return 1;
  }

  async hget(key, field) {
    return this.hashes.get(key)?.get(field) ?? null;
  }

  async setex(key, ttlSeconds, value) {
    this.values.set(key, {
      value,
      expiresAt: this.now() + Number(ttlSeconds) * 1000
    });
    return "OK";
  }

  async exists(key) {
    this.pruneKey(key);
    return this.hashes.has(key) || this.values.has(key) ? 1 : 0;
  }

  async del(...keys) {
    let deleted = 0;
    for (const key of keys.flat()) {
      this.pruneKey(key);
      if (this.hashes.delete(key)) {
        deleted += 1;
      }
      if (this.values.delete(key)) {
        deleted += 1;
      }
    }
    return deleted;
  }

  async scan(cursor, ...args) {
    this.pruneExpired();
    if (cursor !== "0") {
      return ["0", []];
    }

    const matchIndex = args.findIndex((arg) => String(arg).toUpperCase() === "MATCH");
    const pattern = matchIndex >= 0 ? String(args[matchIndex + 1]) : "*";
    const keys = [...new Set([...this.hashes.keys(), ...this.values.keys()])]
      .filter((key) => matchesGlob(key, pattern))
      .sort();
    return ["0", keys];
  }

  pruneExpired() {
    for (const key of this.values.keys()) {
      this.pruneKey(key);
    }
  }

  pruneKey(key) {
    const entry = this.values.get(key);
    if (entry && entry.expiresAt <= this.now()) {
      this.values.delete(key);
    }
  }
}

function normalizeOptions(options) {
  const canaryId = String(options.canaryId ?? `registry-canary-${randomUUID().slice(0, 8)}`);
  const redisUrl = options.redisUrl ?? process.env.REGISTRY_CANARY_REDIS_URL ?? "";
  const registryKeyPrefix = String(
    options.registryKeyPrefix ??
    process.env.REGISTRY_CANARY_KEY_PREFIX ??
    (redisUrl ? `canary:${canaryId}:` : "canary:")
  );

  return {
    canaryId,
    mode: redisUrl ? "redis" : "memory",
    redisUrl,
    registryKeyPrefix,
    cleanup: options.cleanup !== false,
    readinessTimeoutMs: numberOption(options.readinessTimeoutMs, READINESS_TIMEOUT_MS),
    generatedAt: options.generatedAt ?? new Date().toISOString()
  };
}

async function createRedisClient(config) {
  if (config.mode === "memory") {
    return new MemoryRedis();
  }

  const { default: Redis } = await import("ioredis");
  const redis = new Redis(config.redisUrl || DEFAULT_REGISTRY_URL, {
    lazyConnect: true,
    maxRetriesPerRequest: 1,
    enableOfflineQueue: false
  });
  await redis.connect();
  return redis;
}

async function closeRedis(redis) {
  if (typeof redis.quit === "function") {
    await redis.quit();
  } else if (typeof redis.disconnect === "function") {
    redis.disconnect();
  }
}

function createEmptyReport(config) {
  return {
    ok: false,
    generatedAt: config.generatedAt,
    mode: config.mode,
    registryUrl: config.redisUrl || "",
    registryKeyPrefix: config.registryKeyPrefix,
    canaryId: config.canaryId,
    registration: null,
    readiness: null,
    discovery: null,
    ttlFallback: null,
    shutdown: null,
    errors: []
  };
}

function createTrackedInstances(config) {
  const id = config.canaryId;
  const adminApi = {
    serviceName: "admin-api",
    instanceId: `${id}-admin-api`,
    endpoints: [
      expectedEndpoint("http", "http", "127.0.10.30", 13001, "admin")
    ]
  };
  const gameServer = {
    serviceName: "game-server",
    instanceId: `${id}-game-server`,
    endpoints: [
      expectedEndpoint("admin", "tcp", "127.0.10.10", 17500, "admin"),
      expectedEndpoint("proxy-local", "local_socket", "", 0, "local", `${id}-game-server-proxy.sock`)
    ]
  };
  const gameProxy = {
    serviceName: "game-proxy",
    instanceId: `${id}-game-proxy`,
    endpoints: [
      expectedEndpoint("client", "kcp", "127.0.10.20", 14020, "public"),
      expectedEndpoint("admin", "http", "127.0.10.21", 17101, "admin")
    ]
  };
  const controls = [
    { serviceName: "game-server", instanceId: `${id}-game-server-missing-heartbeat` },
    { serviceName: "game-server", instanceId: `${id}-game-server-unhealthy` },
    { serviceName: "game-server", instanceId: `${id}-game-server-unhealthy-endpoint` },
    { serviceName: "game-proxy", instanceId: `${id}-game-proxy-missing-heartbeat` },
    { serviceName: "game-proxy", instanceId: `${id}-game-proxy-unhealthy` }
  ];
  const ttlFallback = {
    serviceName: "game-server",
    instanceId: `${id}-game-server-ttl-fallback`,
    endpoints: [
      expectedEndpoint("admin", "tcp", "127.0.10.12", 17502, "admin")
    ]
  };

  return {
    adminApi,
    gameServer,
    gameProxy,
    group: [adminApi, gameServer, gameProxy],
    controls,
    ttlFallback,
    all: [adminApi, gameServer, gameProxy, ...controls, ttlFallback]
  };
}

function expectedEndpoint(name, protocol, host, port, visibility, socket = "") {
  return { name, protocol, host, port, visibility, socket };
}

function adminRegistryConfig(config) {
  return {
    serviceName: "admin-api",
    serviceInstanceId: `${config.canaryId}-admin-api`,
    registryKeyPrefix: config.registryKeyPrefix,
    host: "127.0.10.30",
    advertisedHost: "127.0.10.30",
    port: 13001,
    adminApiRequireTls: false,
    adminApiRequireIpAllowlist: false,
    adminApiIpAllowlist: [],
    serviceBuildVersion: "registry-canary",
    serviceZone: "canary"
  };
}

async function registerCanaryGroup(redis, config, adminClient, tracked) {
  await adminClient.register();
  adminClient.startHeartbeat(60);

  await writePayload(redis, config.registryKeyPrefix, gameServerPayload(config, tracked.gameServer.instanceId), HEARTBEAT_TTL_SECONDS);
  await writePayload(redis, config.registryKeyPrefix, gameProxyPayload(config, tracked.gameProxy.instanceId), HEARTBEAT_TTL_SECONDS);

  await writePayload(
    redis,
    config.registryKeyPrefix,
    gameServerPayload(config, `${config.canaryId}-game-server-missing-heartbeat`, { hostOffset: 11 }),
    0
  );
  await writePayload(
    redis,
    config.registryKeyPrefix,
    gameServerPayload(config, `${config.canaryId}-game-server-unhealthy`, { hostOffset: 13, healthy: false }),
    HEARTBEAT_TTL_SECONDS
  );
  await writePayload(
    redis,
    config.registryKeyPrefix,
    gameServerPayload(config, `${config.canaryId}-game-server-unhealthy-endpoint`, {
      hostOffset: 14,
      endpointHealthy: false
    }),
    HEARTBEAT_TTL_SECONDS
  );
  await writePayload(
    redis,
    config.registryKeyPrefix,
    gameProxyPayload(config, `${config.canaryId}-game-proxy-missing-heartbeat`, { hostOffset: 22 }),
    0
  );
  await writePayload(
    redis,
    config.registryKeyPrefix,
    gameProxyPayload(config, `${config.canaryId}-game-proxy-unhealthy`, { hostOffset: 23, healthy: false }),
    HEARTBEAT_TTL_SECONDS
  );
}

function gameServerPayload(config, instanceId, overrides = {}) {
  const hostOffset = overrides.hostOffset ?? 10;
  const meta = metadata("game-server", instanceId, config);
  return createServiceInstancePayload({
    id: instanceId,
    name: "game-server",
    host: `127.0.10.${hostOffset}`,
    port: 17000 + hostOffset,
    admin_port: 17500 + (hostOffset - 10),
    local_socket: `${instanceId}-proxy.sock`,
    endpoints: [
      endpoint("client", "tcp", `127.0.10.${hostOffset}`, 17000 + hostOffset, "internal", meta),
      endpoint(
        "admin",
        "tcp",
        `127.0.10.${hostOffset}`,
        17500 + (hostOffset - 10),
        "admin",
        meta,
        overrides.endpointHealthy !== false
      ),
      socketEndpoint("internal", `${instanceId}-internal.sock`, meta),
      socketEndpoint("proxy-local", `${instanceId}-proxy.sock`, meta, overrides.endpointHealthy !== false)
    ],
    tags: ["canary", "game", "tcp"],
    weight: 100,
    metadata: meta,
    healthy: overrides.healthy !== false
  });
}

function gameProxyPayload(config, instanceId, overrides = {}) {
  const hostOffset = overrides.hostOffset ?? 20;
  const meta = metadata("game-proxy", instanceId, config);
  return createServiceInstancePayload({
    id: instanceId,
    name: "game-proxy",
    host: `127.0.10.${hostOffset}`,
    port: 14000 + hostOffset,
    endpoints: [
      endpoint("client", "kcp", `127.0.10.${hostOffset}`, 14000 + hostOffset, "public", meta),
      endpoint("client-tcp-fallback", "tcp", `127.0.10.${hostOffset}`, 24000 + hostOffset, "public", meta),
      endpoint("admin", "http", `127.0.10.${hostOffset + 1}`, 17101 + (hostOffset - 20), "admin", meta)
    ],
    tags: ["canary", "proxy", "kcp"],
    weight: 100,
    metadata: meta,
    healthy: overrides.healthy !== false
  });
}

function ttlFallbackPayload(config, instanceId) {
  const meta = metadata("game-server", instanceId, config, { shutdown_mode: "ttl_fallback_only" });
  return createServiceInstancePayload({
    id: instanceId,
    name: "game-server",
    host: "127.0.10.12",
    port: 17012,
    admin_port: 17502,
    endpoints: [
      endpoint("admin", "tcp", "127.0.10.12", 17502, "admin", meta)
    ],
    tags: ["canary", "ttl-fallback"],
    weight: 100,
    metadata: meta
  });
}

function endpoint(name, protocol, host, port, visibility, metadata = {}, healthy = true) {
  return {
    name,
    protocol,
    host,
    port,
    socket: "",
    visibility,
    metadata,
    healthy
  };
}

function socketEndpoint(name, socket, metadata = {}, healthy = true) {
  return {
    name,
    protocol: "local_socket",
    host: "",
    port: 0,
    socket,
    visibility: "local",
    metadata,
    healthy
  };
}

function metadata(serviceName, instanceId, config, extra = {}) {
  return {
    service_name: serviceName,
    service_instance_id: instanceId,
    instance_id: instanceId,
    build_version: "registry-canary",
    zone: "canary",
    canary_id: config.canaryId,
    ...extra
  };
}

async function writePayload(redis, registryKeyPrefix, payload, heartbeatTtlSeconds) {
  await redis.hset(
    registryInstanceKey(registryKeyPrefix, payload.name, payload.id),
    "data",
    JSON.stringify(payload)
  );
  if (heartbeatTtlSeconds > 0) {
    await redis.setex(
      registryHeartbeatKey(registryKeyPrefix, payload.name, payload.id),
      heartbeatTtlSeconds,
      "1"
    );
  }
}

async function waitForReadiness(redis, config, specs, errors) {
  const startedAt = Date.now();
  let lastResult = null;

  do {
    lastResult = await readinessReport(redis, config.registryKeyPrefix, specs);
    if (lastResult.ok) {
      return lastResult;
    }
    await sleep(50);
  } while (Date.now() - startedAt < config.readinessTimeoutMs);

  for (const check of lastResult?.checks ?? []) {
    for (const error of check.errors) {
      errors.push(errorObject("readiness_failed", error, {
        service: check.service,
        instanceId: check.instanceId
      }));
    }
  }
  return lastResult ?? { ok: false, checks: [] };
}

async function readinessReport(redis, registryKeyPrefix, specs) {
  const checks = [];
  for (const spec of specs) {
    checks.push(await readinessCheck(redis, registryKeyPrefix, spec));
  }
  return {
    ok: checks.every((check) => check.ok),
    checks
  };
}

async function readinessCheck(redis, registryKeyPrefix, spec) {
  const instanceKey = registryInstanceKey(registryKeyPrefix, spec.serviceName, spec.instanceId);
  const heartbeatKey = registryHeartbeatKey(registryKeyPrefix, spec.serviceName, spec.instanceId);
  const raw = await redis.hget(instanceKey, "data");
  const heartbeatExists = await redis.exists(heartbeatKey) === 1;
  const errors = [];
  let payload = null;
  let validation = null;

  if (!raw) {
    errors.push("instance record is missing");
  } else {
    try {
      payload = normalizeServiceInstance(JSON.parse(raw));
      validation = payload ? validateServiceInstance(payload) : { ok: false, errors: ["invalid service instance payload"] };
      if (!validation.ok) {
        errors.push(...validation.errors);
      }
    } catch (error) {
      errors.push(`instance record JSON is invalid: ${error?.message || error}`);
    }
  }

  if (!heartbeatExists) {
    errors.push("heartbeat is missing");
  }
  if (payload && payload.id !== spec.instanceId) {
    errors.push(`instance id mismatch: expected ${spec.instanceId}, got ${payload.id}`);
  }
  if (payload && payload.name !== spec.serviceName) {
    errors.push(`service name mismatch: expected ${spec.serviceName}, got ${payload.name}`);
  }
  if (payload && payload.healthy === false) {
    errors.push("instance healthy flag is false");
  }

  const endpointChecks = [];
  for (const expected of spec.endpoints) {
    const actual = payload?.endpoints.find((candidate) => candidate.name === expected.name);
    const endpointErrors = [];
    if (!actual) {
      endpointErrors.push("endpoint is missing");
    } else {
      for (const field of ["protocol", "host", "port", "socket", "visibility"]) {
        if (actual[field] !== expected[field]) {
          endpointErrors.push(`${field} mismatch: expected ${expected[field]}, got ${actual[field]}`);
        }
      }
      if (actual.healthy === false) {
        endpointErrors.push("endpoint healthy flag is false");
      }
    }
    if (endpointErrors.length > 0) {
      errors.push(`${expected.name}: ${endpointErrors.join("; ")}`);
    }
    endpointChecks.push({
      name: expected.name,
      ok: endpointErrors.length === 0,
      expected,
      actual: actual ? endpointSummary(actual) : null,
      errors: endpointErrors
    });
  }

  return {
    ok: errors.length === 0,
    service: spec.serviceName,
    instanceId: spec.instanceId,
    instanceKey,
    heartbeatKey,
    instanceRecordExists: Boolean(raw),
    heartbeatExists,
    endpoints: endpointChecks,
    errors
  };
}

async function runDiscoveryChecks(redis, config, tracked, errors) {
  const discovery = new RegistryDiscoveryClient(redis, {
    registryKeyPrefix: config.registryKeyPrefix,
    discoveryCacheTtlMs: 0
  });

  const checks = [
    await discoveryCheck(
      "game-server.admin",
      () => discovery.discoverAllEndpoints("game-server", "admin"),
      [tracked.gameServer.instanceId],
      [
        `${config.canaryId}-game-server-missing-heartbeat`,
        `${config.canaryId}-game-server-unhealthy`,
        `${config.canaryId}-game-server-unhealthy-endpoint`
      ]
    ),
    await discoveryCheck(
      "game-server.proxy-local",
      () => discovery.discoverAllEndpoints("game-server", "proxy-local"),
      [tracked.gameServer.instanceId],
      [`${config.canaryId}-game-server-missing-heartbeat`, `${config.canaryId}-game-server-unhealthy`]
    ),
    await discoveryCheck(
      "game-proxy.client",
      () => discovery.discoverAllEndpoints("game-proxy", "client"),
      [tracked.gameProxy.instanceId],
      [`${config.canaryId}-game-proxy-missing-heartbeat`, `${config.canaryId}-game-proxy-unhealthy`]
    ),
    await discoveryCheck(
      "game-proxy.admin",
      () => discovery.discoverAllEndpoints("game-proxy", "admin"),
      [tracked.gameProxy.instanceId],
      [`${config.canaryId}-game-proxy-missing-heartbeat`, `${config.canaryId}-game-proxy-unhealthy`]
    ),
    await flatEndpointDiscoveryCheck(
      "admin-api game-server.admin",
      () => discoverGameServerAdminEndpoints(redis, {
        registryKeyPrefix: config.registryKeyPrefix,
        discoveryCacheTtlMs: 0
      }),
      [tracked.gameServer.instanceId],
      [
        `${config.canaryId}-game-server-missing-heartbeat`,
        `${config.canaryId}-game-server-unhealthy`,
        `${config.canaryId}-game-server-unhealthy-endpoint`
      ]
    ),
    await flatEndpointDiscoveryCheck(
      "admin-api game-proxy.admin",
      () => discoverGameProxyAdminEndpoints(redis, {
        registryKeyPrefix: config.registryKeyPrefix,
        discoveryCacheTtlMs: 0
      }),
      [tracked.gameProxy.instanceId],
      [`${config.canaryId}-game-proxy-missing-heartbeat`, `${config.canaryId}-game-proxy-unhealthy`]
    )
  ];

  for (const check of checks) {
    for (const error of check.errors) {
      errors.push(errorObject("discovery_failed", error, { check: check.name }));
    }
  }
  return {
    ok: checks.every((check) => check.ok),
    checks
  };
}

async function discoveryCheck(name, discover, expectedIds, forbiddenIds) {
  const selections = await discover();
  const discoveredIds = selections.map(({ instance }) => instance.id).sort();
  return endpointCheckResult(name, discoveredIds, expectedIds, forbiddenIds, selections.map(({ instance, endpoint: selected }) => ({
    service: instance.name,
    instanceId: instance.id,
    endpoint: endpointSummary(selected),
    source: "registry",
    fallback: false
  })));
}

async function flatEndpointDiscoveryCheck(name, discover, expectedIds, forbiddenIds) {
  const endpoints = await discover();
  const discoveredIds = endpoints.map((endpointResult) => endpointResult.instanceId).sort();
  return endpointCheckResult(name, discoveredIds, expectedIds, forbiddenIds, endpoints.map((endpointResult) => ({
    service: endpointResult.service,
    instanceId: endpointResult.instanceId,
    endpoint: {
      name: endpointResult.endpointName,
      protocol: endpointResult.protocol,
      host: endpointResult.host,
      port: endpointResult.port,
      socket: "",
      visibility: "admin",
      healthy: endpointResult.healthy
    },
    source: endpointResult.source,
    fallback: endpointResult.fallback
  })));
}

function endpointCheckResult(name, discoveredIds, expectedIds, forbiddenIds, endpoints) {
  const errors = [];
  const expected = [...expectedIds].sort();
  if (JSON.stringify(discoveredIds) !== JSON.stringify(expected)) {
    errors.push(`expected discovered ids ${expected.join(", ")}, got ${discoveredIds.join(", ") || "<none>"}`);
  }
  for (const forbiddenId of forbiddenIds) {
    if (discoveredIds.includes(forbiddenId)) {
      errors.push(`forbidden instance was discoverable: ${forbiddenId}`);
    }
  }

  return {
    ok: errors.length === 0,
    name,
    expectedInstanceIds: expected,
    forbiddenInstanceIds: [...forbiddenIds].sort(),
    discoveredInstanceIds: discoveredIds,
    endpoints,
    errors
  };
}

async function runTtlFallbackCheck(redis, config, ttlSpec, errors) {
  await writePayload(
    redis,
    config.registryKeyPrefix,
    ttlFallbackPayload(config, ttlSpec.instanceId),
    TTL_FALLBACK_SECONDS
  );

  const before = await readRegistryState(redis, config.registryKeyPrefix, ttlSpec.serviceName, ttlSpec.instanceId);
  const beforeVisible = await isEndpointDiscoverable(redis, config.registryKeyPrefix, ttlSpec.serviceName, "admin", ttlSpec.instanceId);
  if (typeof redis.advanceTime === "function") {
    redis.advanceTime((TTL_FALLBACK_SECONDS * 1000) + 100);
  } else {
    await sleep((TTL_FALLBACK_SECONDS * 1000) + 150);
  }

  const after = await readRegistryState(redis, config.registryKeyPrefix, ttlSpec.serviceName, ttlSpec.instanceId);
  const afterVisible = await isEndpointDiscoverable(redis, config.registryKeyPrefix, ttlSpec.serviceName, "admin", ttlSpec.instanceId);
  const checkErrors = [];
  if (!before.instanceRecordExists || !before.heartbeatExists || !beforeVisible) {
    checkErrors.push("ttl fallback control was not visible before heartbeat expiry");
  }
  if (!after.instanceRecordExists) {
    checkErrors.push("ttl fallback should leave the instance record behind");
  }
  if (after.heartbeatExists) {
    checkErrors.push("ttl fallback heartbeat should expire");
  }
  if (afterVisible) {
    checkErrors.push("ttl fallback instance should not be discoverable after heartbeat expiry");
  }

  for (const error of checkErrors) {
    errors.push(errorObject("ttl_fallback_failed", error, {
      service: ttlSpec.serviceName,
      instanceId: ttlSpec.instanceId
    }));
  }

  return {
    ok: checkErrors.length === 0,
    mode: "ttl_fallback_only",
    service: ttlSpec.serviceName,
    instanceId: ttlSpec.instanceId,
    beforeExpiry: {
      ...before,
      discoveryVisible: beforeVisible
    },
    afterExpiry: {
      ...after,
      discoveryVisible: afterVisible
    },
    conclusion: "heartbeat TTL hides an abnormal exit from discovery, but it does not delete the instance record",
    errors: checkErrors
  };
}

async function runExplicitDeregisterCheck(redis, config, adminClient, tracked, errors) {
  adminClient.stopHeartbeat();
  await adminClient.deregister();
  await deregisterInstance(redis, config.registryKeyPrefix, tracked.gameServer.serviceName, tracked.gameServer.instanceId);
  await deregisterInstance(redis, config.registryKeyPrefix, tracked.gameProxy.serviceName, tracked.gameProxy.instanceId);

  const states = await readRegistryStates(redis, config.registryKeyPrefix, tracked.group);
  const discovery = new RegistryDiscoveryClient(redis, {
    registryKeyPrefix: config.registryKeyPrefix,
    discoveryCacheTtlMs: 0
  });
  const discoveryVisible = {
    [tracked.adminApi.instanceId]: (await discovery.discoverInstances("admin-api")).some((instance) => instance.id === tracked.adminApi.instanceId),
    [tracked.gameServer.instanceId]: await isEndpointDiscoverable(redis, config.registryKeyPrefix, "game-server", "admin", tracked.gameServer.instanceId),
    [tracked.gameProxy.instanceId]: await isEndpointDiscoverable(redis, config.registryKeyPrefix, "game-proxy", "admin", tracked.gameProxy.instanceId)
  };
  const checkErrors = [];

  for (const state of states) {
    if (state.instanceRecordExists || state.heartbeatExists) {
      checkErrors.push(`${state.service}.${state.instanceId} registry keys should be deleted after explicit deregister`);
    }
    if (discoveryVisible[state.instanceId]) {
      checkErrors.push(`${state.service}.${state.instanceId} should not be discoverable after explicit deregister`);
    }
  }

  for (const error of checkErrors) {
    errors.push(errorObject("deregister_failed", error));
  }

  return {
    ok: checkErrors.length === 0,
    mode: "explicit_deregister",
    instances: states.map((state) => ({
      ...state,
      discoveryVisible: discoveryVisible[state.instanceId] === true
    })),
    conclusion: "normal canary shutdown deletes both instance records and heartbeat keys before considering the instance offline",
    errors: checkErrors
  };
}

async function isEndpointDiscoverable(redis, registryKeyPrefix, serviceName, endpointName, instanceId) {
  const discovery = new RegistryDiscoveryClient(redis, {
    registryKeyPrefix,
    discoveryCacheTtlMs: 0
  });
  const endpoints = await discovery.discoverAllEndpoints(serviceName, endpointName);
  return endpoints.some(({ instance }) => instance.id === instanceId);
}

async function readRegistryStates(redis, registryKeyPrefix, specs) {
  const states = [];
  for (const spec of specs) {
    states.push(await readRegistryState(redis, registryKeyPrefix, spec.serviceName, spec.instanceId));
  }
  return states;
}

async function readRegistryState(redis, registryKeyPrefix, serviceName, instanceId) {
  const instanceKey = registryInstanceKey(registryKeyPrefix, serviceName, instanceId);
  const heartbeatKey = registryHeartbeatKey(registryKeyPrefix, serviceName, instanceId);
  const raw = await redis.hget(instanceKey, "data");
  return {
    service: serviceName,
    instanceId,
    instanceKey,
    heartbeatKey,
    instanceRecordExists: Boolean(raw),
    heartbeatExists: await redis.exists(heartbeatKey) === 1
  };
}

async function cleanupInstances(redis, registryKeyPrefix, instances) {
  for (const { serviceName, instanceId } of instances) {
    await deregisterInstance(redis, registryKeyPrefix, serviceName, instanceId);
  }
}

async function deregisterInstance(redis, registryKeyPrefix, serviceName, instanceId) {
  await redis.del(registryInstanceKey(registryKeyPrefix, serviceName, instanceId));
  await redis.del(registryHeartbeatKey(registryKeyPrefix, serviceName, instanceId));
}

function endpointSummary(endpointValue) {
  return {
    name: endpointValue.name,
    protocol: endpointValue.protocol,
    host: endpointValue.host,
    port: endpointValue.port,
    socket: endpointValue.socket,
    visibility: endpointValue.visibility,
    healthy: endpointValue.healthy
  };
}

function matchesGlob(value, pattern) {
  const escaped = pattern
    .replace(/[.+?^${}()|[\]\\]/g, "\\$&")
    .replace(/\*/g, ".*");
  return new RegExp(`^${escaped}$`).test(value);
}

function errorObject(code, message, extra = {}) {
  return { code, message, ...extra };
}

function numberOption(value, fallback) {
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : fallback;
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function parseArgs(argv) {
  const args = { pretty: true };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--redis-url") {
      args.redisUrl = argv[index + 1] || DEFAULT_REGISTRY_URL;
      index += 1;
    } else if (arg === "--memory") {
      args.redisUrl = "";
    } else if (arg === "--registry-key-prefix") {
      args.registryKeyPrefix = argv[index + 1] ?? "";
      index += 1;
    } else if (arg === "--canary-id") {
      args.canaryId = argv[index + 1];
      index += 1;
    } else if (arg === "--no-cleanup") {
      args.cleanup = false;
    } else if (arg === "--compact") {
      args.pretty = false;
    } else if (arg === "--pretty") {
      args.pretty = true;
    } else if (arg === "--help" || arg === "-h") {
      args.help = true;
    } else {
      throw new Error(`unknown argument: ${arg}`);
    }
  }

  return args;
}

function printHelp() {
  console.log([
    "Usage: node tools/check-registry-canary-lifecycle.js [options]",
    "",
    "Runs a pre-release registry canary lifecycle gate:",
    "  register canary service instances -> discover healthy endpoints -> verify readiness -> explicit deregister.",
    "",
    "Options:",
    "  --memory                       Use the built-in memory Redis stub (default)",
    "  --redis-url <url>              Run against a real Redis registry",
    "  --registry-key-prefix <value>  Registry key prefix for canary keys",
    "  --canary-id <value>            Stable canary instance id prefix",
    "  --no-cleanup                   Leave canary keys in Redis for inspection",
    "  --compact                      Emit compact JSON"
  ].join("\n"));
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help) {
    printHelp();
    return;
  }

  const report = await runRegistryCanaryLifecycle(args);
  console.log(JSON.stringify(report, null, args.pretty ? 2 : 0));
  if (!report.ok) {
    process.exitCode = 1;
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => {
    console.error(error?.stack || error?.message || String(error));
    process.exitCode = 1;
  });
}
