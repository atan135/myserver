import process from "node:process";
import { randomUUID } from "node:crypto";
import { fileURLToPath } from "node:url";
import path from "node:path";

import { GameAdminClient as AdminGameAdminClient } from "../apps/admin-api/src/game-admin-client.js";
import {
  RegistryDiscoveryClient,
  createServiceInstancePayload,
  getDiscoveryMetricsSnapshot,
  registryHeartbeatKey,
  registryInstanceKey,
  resetDiscoveryMetrics
} from "../packages/service-registry/node/registry-schema.js";
import { MemoryRedis } from "./check-registry-canary-lifecycle.js";

const DEFAULT_CACHE_TTL_MS = 5000;
const DEFAULT_FAIL_FAST_MAX_ELAPSED_MS = 1000;
const HEARTBEAT_TTL_SECONDS = 30;
const DEFAULT_START_TIME_MS = 1_713_000_000_000;

export async function runRegistryOutageDrill(options = {}) {
  const config = normalizeOptions(options);
  const redis = options.redis ?? new OutageRedis({ now: () => config.startTimeMs });
  const report = createEmptyReport(config);
  const errors = [];

  resetDiscoveryMetrics();

  try {
    redis.setAvailable?.(true);
    await cleanup(redis, config);
    await seedRegistry(redis, config);

    report.cache = await runCachedConsumerCheck(redis, config);
    report.newStart = await runNewStartFailFastCheck(redis, config);
    report.strictConsumer = await runStrictConsumerFailFastCheck(redis, config);
  } catch (error) {
    errors.push(errorObject("registry_outage_drill_failed", error?.message || String(error)));
  } finally {
    redis.setAvailable?.(true);
    if (config.cleanup) {
      await cleanup(redis, config).catch((error) => {
        errors.push(errorObject("cleanup_failed", error?.message || String(error)));
      });
    }
    report.metrics = getDiscoveryMetricsSnapshot();
    resetDiscoveryMetrics();
  }

  report.errors = [
    ...errors,
    ...sectionErrors("cache", report.cache),
    ...sectionErrors("new_start", report.newStart),
    ...sectionErrors("strict_consumer", report.strictConsumer)
  ];
  report.ok =
    report.errors.length === 0 &&
    report.cache?.ok === true &&
    report.newStart?.ok === true &&
    report.strictConsumer?.ok === true;
  return report;
}

export class OutageRedis extends MemoryRedis {
  constructor(options = {}) {
    super(options);
    this.available = options.available !== false;
    this.operations = [];
  }

  setAvailable(available) {
    this.available = available === true;
  }

  mark() {
    return this.operations.length;
  }

  operationsSince(mark) {
    return this.operations.slice(mark);
  }

  record(command, key = "") {
    const operation = {
      command,
      key,
      available: this.available,
      at: this.now()
    };
    this.operations.push(operation);

    if (!this.available) {
      const error = new Error("REGISTRY_UNAVAILABLE");
      error.code = "REGISTRY_UNAVAILABLE";
      error.command = command;
      throw error;
    }
  }

  async hset(key, field, value) {
    this.record("hset", key);
    return super.hset(key, field, value);
  }

  async hget(key, field) {
    this.record("hget", key);
    return super.hget(key, field);
  }

  async setex(key, ttlSeconds, value) {
    this.record("setex", key);
    return super.setex(key, ttlSeconds, value);
  }

  async exists(key) {
    this.record("exists", key);
    return super.exists(key);
  }

  async del(...keys) {
    for (const key of keys.flat()) {
      this.record("del", key);
    }
    return super.del(...keys);
  }

  async scan(cursor, ...args) {
    const matchIndex = args.findIndex((arg) => String(arg).toUpperCase() === "MATCH");
    const pattern = matchIndex >= 0 ? String(args[matchIndex + 1]) : "";
    this.record("scan", pattern);
    return super.scan(cursor, ...args);
  }
}

function normalizeOptions(options) {
  const drillId = String(options.drillId ?? `registry-outage-${randomUUID().slice(0, 8)}`);
  return {
    drillId,
    mode: "memory",
    generatedAt: options.generatedAt ?? new Date().toISOString(),
    registryKeyPrefix: String(
      options.registryKeyPrefix ??
      process.env.REGISTRY_OUTAGE_KEY_PREFIX ??
      `outage:${drillId}:`
    ),
    cacheTtlMs: numberOption(options.cacheTtlMs, DEFAULT_CACHE_TTL_MS),
    failFastMaxElapsedMs: numberOption(options.failFastMaxElapsedMs, DEFAULT_FAIL_FAST_MAX_ELAPSED_MS),
    cleanup: options.cleanup !== false,
    startTimeMs: numberOption(options.startTimeMs, DEFAULT_START_TIME_MS),
    target: {
      serviceName: "game-server",
      endpointName: "admin",
      instanceId: `${drillId}-game-server`,
      host: "127.0.20.10",
      port: 17510
    }
  };
}

function createEmptyReport(config) {
  return {
    ok: false,
    generatedAt: config.generatedAt,
    mode: config.mode,
    registryKeyPrefix: config.registryKeyPrefix,
    drillId: config.drillId,
    outage: {
      simulated: true,
      errorCode: "REGISTRY_UNAVAILABLE"
    },
    target: {
      service: config.target.serviceName,
      endpoint: config.target.endpointName,
      instanceId: config.target.instanceId,
      host: config.target.host,
      port: config.target.port
    },
    cache: null,
    newStart: null,
    strictConsumer: null,
    metrics: [],
    errors: []
  };
}

async function seedRegistry(redis, config) {
  const payload = createGameServerPayload(config);
  await redis.hset(
    registryInstanceKey(config.registryKeyPrefix, payload.name, payload.id),
    "data",
    JSON.stringify(payload)
  );
  await redis.setex(
    registryHeartbeatKey(config.registryKeyPrefix, payload.name, payload.id),
    HEARTBEAT_TTL_SECONDS,
    "1"
  );
}

function createGameServerPayload(config) {
  const metadata = {
    service_name: config.target.serviceName,
    service_instance_id: config.target.instanceId,
    instance_id: config.target.instanceId,
    build_version: "registry-outage-drill",
    zone: "outage-drill"
  };

  return createServiceInstancePayload({
    id: config.target.instanceId,
    name: config.target.serviceName,
    host: config.target.host,
    port: 17010,
    admin_port: config.target.port,
    endpoints: [
      {
        name: config.target.endpointName,
        protocol: "tcp",
        host: config.target.host,
        port: config.target.port,
        socket: "",
        visibility: "admin",
        metadata,
        healthy: true
      }
    ],
    tags: ["outage-drill", "game", "admin"],
    metadata,
    healthy: true
  });
}

async function runCachedConsumerCheck(redis, config) {
  const errors = [];
  const client = new RegistryDiscoveryClient(redis, {
    registryKeyPrefix: config.registryKeyPrefix,
    discoveryCacheTtlMs: config.cacheTtlMs,
    now: () => redis.now()
  });

  redis.setAvailable(true);
  const warmupMark = redis.mark();
  const warmup = await client.discoverRequiredEndpoint(config.target.serviceName, config.target.endpointName);
  const warmupOps = redis.operationsSince(warmupMark);

  redis.setAvailable(false);
  const cachedMark = redis.mark();
  let cached = null;
  let cachedError = null;
  const cachedStartedAt = Date.now();
  try {
    cached = await client.discoverRequiredEndpoint(config.target.serviceName, config.target.endpointName);
  } catch (error) {
    cachedError = error;
  }
  const cachedElapsedMs = Date.now() - cachedStartedAt;
  const cachedOps = redis.operationsSince(cachedMark);

  if (cachedError) {
    errors.push(`cached discovery failed during outage: ${cachedError.message}`);
  }
  if (cachedOps.length > 0) {
    errors.push("cached discovery touched Redis during outage");
  }
  if (cached && !sameSelection(warmup, cached)) {
    errors.push("cached discovery changed the selected instance or endpoint");
  }

  if (typeof redis.advanceTime === "function") {
    redis.advanceTime(config.cacheTtlMs + 1);
  }

  const expiredMark = redis.mark();
  const expiredStartedAt = Date.now();
  let expiredError = null;
  try {
    await client.discoverRequiredEndpoint(config.target.serviceName, config.target.endpointName);
    errors.push("expired cache discovery succeeded while registry was unavailable");
  } catch (error) {
    expiredError = error;
  }
  const expiredElapsedMs = Date.now() - expiredStartedAt;
  const expiredOps = redis.operationsSince(expiredMark);
  if (!isRegistryUnavailable(expiredError)) {
    errors.push(`expired cache did not fail with REGISTRY_UNAVAILABLE: ${expiredError?.message || "<none>"}`);
  }

  return {
    ok: errors.length === 0,
    cacheTtlMs: config.cacheTtlMs,
    warmup: {
      source: "registry",
      endpoint: selectionReport(warmup, "registry", "discovered"),
      redisOperations: operationSummary(warmupOps)
    },
    duringOutage: {
      source: cached && cachedOps.length === 0 ? "discovery-cache" : "registry",
      reason: cached && cachedOps.length === 0 ? "cache_hit" : "registry_accessed",
      endpoint: cached ? selectionReport(cached, "discovery-cache", "cache_hit") : null,
      sameInstanceId: cached ? warmup.instance.id === cached.instance.id : false,
      sameEndpoint: cached ? sameEndpoint(warmup.endpoint, cached.endpoint) : false,
      registryOperations: operationSummary(cachedOps),
      elapsedMs: cachedElapsedMs,
      error: cachedError ? errorReport(cachedError) : null,
      fallbackUsed: false
    },
    afterTtl: {
      ok: isRegistryUnavailable(expiredError),
      reason: "cache_expired",
      error: expiredError ? errorReport(expiredError) : null,
      registryOperations: operationSummary(expiredOps),
      elapsedMs: expiredElapsedMs,
      fallbackUsed: false
    },
    errors
  };
}

async function runNewStartFailFastCheck(redis, config) {
  const errors = [];
  redis.setAvailable(false);
  const client = new RegistryDiscoveryClient(redis, {
    registryKeyPrefix: config.registryKeyPrefix,
    discoveryCacheTtlMs: config.cacheTtlMs,
    now: () => redis.now()
  });
  const mark = redis.mark();
  const startedAt = Date.now();
  let failure = null;

  try {
    await client.discoverRequiredEndpoint(config.target.serviceName, config.target.endpointName);
    errors.push("new discovery client resolved an endpoint while registry was unavailable");
  } catch (error) {
    failure = error;
  }

  const operations = redis.operationsSince(mark);
  const elapsedMs = Date.now() - startedAt;
  if (!isRegistryUnavailable(failure)) {
    errors.push(`new discovery client did not fail with REGISTRY_UNAVAILABLE: ${failure?.message || "<none>"}`);
  }
  if (operations.length !== 1 || operations[0].command !== "scan") {
    errors.push(`new discovery client should fail on the first registry scan, got ${operations.map((op) => op.command).join(", ") || "<none>"}`);
  }
  if (elapsedMs > config.failFastMaxElapsedMs) {
    errors.push(`new discovery client did not fail fast: elapsed ${elapsedMs}ms > ${config.failFastMaxElapsedMs}ms`);
  }

  return {
    ok: errors.length === 0,
    mode: "required_endpoint_no_cache",
    discoveryRequired: true,
    failFastMaxElapsedMs: config.failFastMaxElapsedMs,
    fallbackUsed: false,
    endpoint: null,
    error: failure ? errorReport(failure) : null,
    registryOperations: operationSummary(operations),
    elapsedMs,
    errors
  };
}

async function runStrictConsumerFailFastCheck(redis, config) {
  const errors = [];
  redis.setAvailable(false);
  const fallbackCandidate = {
    host: "127.0.0.1",
    port: 7500
  };
  const client = new AdminGameAdminClient({
    registryDiscoveryEnabled: true,
    registryDiscoveryRequired: true,
    registryDiscoveryCacheTtlMs: 0,
    registryKeyPrefix: config.registryKeyPrefix,
    localDiscoveryFallbackEnabled: true,
    gameServerAdminHost: fallbackCandidate.host,
    gameServerAdminPort: fallbackCandidate.port,
    gameAdminToken: "registry-outage-drill",
    gameAdminConnectTimeoutMs: 50,
    gameAdminWriteTimeoutMs: 50,
    gameAdminReadTimeoutMs: 50,
    gameAdminMaxResponseBytes: 1024
  }, redis);

  const mark = redis.mark();
  const startedAt = Date.now();
  let failure = null;
  let endpoints = null;

  try {
    endpoints = await client.listAdminEndpoints();
    if (endpoints.some((endpoint) => endpoint.fallback || endpoint.source === "fallback")) {
      errors.push("strict consumer returned a local fallback endpoint");
    } else {
      errors.push("strict consumer resolved endpoints while registry was unavailable");
    }
  } catch (error) {
    failure = error;
  }

  const operations = redis.operationsSince(mark);
  const elapsedMs = Date.now() - startedAt;
  if (!isRegistryUnavailable(failure)) {
    errors.push(`strict consumer did not fail with REGISTRY_UNAVAILABLE: ${failure?.message || "<none>"}`);
  }
  if (endpoints && endpoints.length > 0) {
    errors.push(`strict consumer returned ${endpoints.length} endpoint(s) during outage`);
  }
  if (elapsedMs > config.failFastMaxElapsedMs) {
    errors.push(`strict consumer did not fail fast: elapsed ${elapsedMs}ms > ${config.failFastMaxElapsedMs}ms`);
  }

  return {
    ok: errors.length === 0,
    consumer: "admin-api.game-admin-client",
    mode: "strict_required_discovery",
    registryDiscoveryEnabled: true,
    registryDiscoveryRequired: true,
    failFastMaxElapsedMs: config.failFastMaxElapsedMs,
    localFallbackConfigured: true,
    fallbackCandidate,
    fallbackUsed: false,
    endpoints: endpoints ?? [],
    error: failure ? errorReport(failure) : null,
    registryOperations: operationSummary(operations),
    elapsedMs,
    errors
  };
}

async function cleanup(redis, config) {
  await redis.del(
    registryInstanceKey(config.registryKeyPrefix, config.target.serviceName, config.target.instanceId),
    registryHeartbeatKey(config.registryKeyPrefix, config.target.serviceName, config.target.instanceId)
  );
}

function sameSelection(left, right) {
  return left?.instance?.id === right?.instance?.id && sameEndpoint(left?.endpoint, right?.endpoint);
}

function sameEndpoint(left, right) {
  return Boolean(left && right) &&
    left.name === right.name &&
    left.protocol === right.protocol &&
    left.host === right.host &&
    left.port === right.port &&
    left.socket === right.socket &&
    left.visibility === right.visibility;
}

function selectionReport(selection, source, reason) {
  const instance = selection.instance;
  const endpoint = selection.endpoint;
  return {
    service: instance.name,
    instanceId: instance.id,
    instance_id: instance.id,
    endpointName: endpoint.name,
    endpoint_name: endpoint.name,
    protocol: endpoint.protocol,
    visibility: endpoint.visibility,
    host: endpoint.host,
    port: endpoint.port,
    socket: endpoint.socket,
    healthy: instance.healthy !== false && endpoint.healthy !== false,
    weight: instance.weight,
    metadata: endpoint.metadata || {},
    source,
    reason,
    fallback: false
  };
}

function operationSummary(operations) {
  return operations.map((operation) => ({
    command: operation.command,
    key: operation.key,
    available: operation.available
  }));
}

function errorReport(error) {
  return {
    code: error?.code || error?.name || "ERROR",
    message: error?.message || String(error),
    command: error?.command || ""
  };
}

function isRegistryUnavailable(error) {
  return error?.code === "REGISTRY_UNAVAILABLE" || error?.message === "REGISTRY_UNAVAILABLE";
}

function sectionErrors(section, report) {
  return (report?.errors ?? []).map((message) => errorObject(`${section}_failed`, message));
}

function errorObject(code, message, extra = {}) {
  return { code, message, ...extra };
}

function numberOption(value, fallback) {
  if (value === null || value === undefined || value === "") {
    return fallback;
  }
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : fallback;
}

function parseArgs(argv) {
  const args = { pretty: true };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--memory") {
      args.mode = "memory";
    } else if (arg === "--registry-key-prefix") {
      args.registryKeyPrefix = argv[index + 1] ?? "";
      index += 1;
    } else if (arg === "--drill-id") {
      args.drillId = argv[index + 1];
      index += 1;
    } else if (arg === "--cache-ttl-ms") {
      args.cacheTtlMs = Number(argv[index + 1]);
      index += 1;
    } else if (arg === "--fail-fast-max-elapsed-ms") {
      args.failFastMaxElapsedMs = Number(argv[index + 1]);
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
    "Usage: node tools/check-registry-outage-drill.js [options]",
    "",
    "Runs a pre-release registry outage gate using a fault-injected memory Redis:",
    "  warm discovery cache -> simulate Redis outage -> verify cached discovery survives briefly",
    "  and new strict/required discovery fails fast without local fallback.",
    "",
    "Options:",
    "  --memory                       Use the built-in memory Redis outage simulation (default)",
    "  --registry-key-prefix <value>  Registry key prefix for drill keys",
    "  --drill-id <value>             Stable drill instance id prefix",
    "  --cache-ttl-ms <ms>            Discovery cache TTL used by the existing consumer check",
    "  --fail-fast-max-elapsed-ms <ms> Maximum accepted no-cache strict discovery failure latency",
    "  --no-cleanup                   Leave drill keys in memory for inspection in embedded runs",
    "  --compact                      Emit compact JSON"
  ].join("\n"));
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help) {
    printHelp();
    return;
  }

  const report = await runRegistryOutageDrill(args);
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
