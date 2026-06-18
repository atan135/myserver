import process from "node:process";
import { randomUUID } from "node:crypto";
import { fileURLToPath } from "node:url";
import path from "node:path";

import {
  discoverGameProxyAdminEndpoints as discoverAdminApiGameProxyAdminEndpoints,
  discoverGameServerAdminEndpoints as discoverAdminApiGameServerAdminEndpoints
} from "../apps/admin-api/src/registry-client.js";
import { GameAdminClient as AdminApiGameAdminClient } from "../apps/admin-api/src/game-admin-client.js";
import { discoverGameServerAdminEndpoints as discoverAuthHttpGameServerAdminEndpoints } from "../apps/auth-http/src/registry-client.js";
import { discoverGameServerAdminEndpoints as discoverMailServiceGameServerAdminEndpoints } from "../apps/mail-service/src/registry-client.js";
import {
  RegistryDiscoveryClient,
  collectDiscoveryMetricFields,
  createServiceInstancePayload,
  discoveryLogContext,
  getDiscoveryMetricsSnapshot,
  recordDiscoveryMetric,
  registryHeartbeatKey,
  registryInstanceKey,
  resetDiscoveryMetrics
} from "../packages/service-registry/node/registry-schema.js";
import { MemoryRedis } from "./check-registry-canary-lifecycle.js";

const DEFAULT_START_TIME_MS = 1_713_000_000_000;
const HEARTBEAT_TTL_SECONDS = 30;
const FALLBACK_ENDPOINT_MARKERS = new Set([
  "local-fallback",
  "fallback",
  "fallback_used",
  "fallback-used",
  "fallback_endpoint"
]);

export async function runRegistryFallbackObservabilityCheck(options = {}) {
  const config = normalizeOptions(options);
  const redis = options.redis ?? new MemoryRedis({ now: () => config.startTimeMs });
  const report = createEmptyReport(config);
  const errors = [];
  const logs = createLogCollector();
  const fixtures = createFixtures(config);

  resetDiscoveryMetrics();

  try {
    await cleanup(redis, config, fixtures);
    await seedRegistry(redis, config, fixtures);
    report.discovery = await runDiscoveryChecks(redis, config, logs);
    injectFallbackObservability(options.injectFallback, logs);
  } catch (error) {
    errors.push(errorObject("fallback_observability_check_failed", error?.message || String(error)));
  } finally {
    if (config.cleanup) {
      await cleanup(redis, config, fixtures).catch((error) => {
        errors.push(errorObject("cleanup_failed", error?.message || String(error)));
      });
    }
    report.metrics = getDiscoveryMetricsSnapshot();
    report.metricFields = collectDiscoveryMetricFields({ reset: false });
    resetDiscoveryMetrics();
  }

  report.logs = logs.entries;
  report.observability = analyzeObservability({
    metricFields: report.metricFields,
    metrics: report.metrics,
    logs: report.logs,
    discovery: report.discovery
  });
  report.errors = [
    ...errors,
    ...sectionErrors("discovery", report.discovery),
    ...report.observability.errors
  ];
  report.ok =
    report.errors.length === 0 &&
    report.discovery?.ok === true &&
    report.observability.ok === true;
  return report;
}

function normalizeOptions(options) {
  const checkId = String(options.checkId ?? `registry-fallback-observability-${randomUUID().slice(0, 8)}`);
  return {
    checkId,
    mode: "memory",
    generatedAt: options.generatedAt ?? new Date().toISOString(),
    registryKeyPrefix: String(
      options.registryKeyPrefix ??
      process.env.REGISTRY_FALLBACK_OBSERVABILITY_KEY_PREFIX ??
      `fallback-observability:${checkId}:`
    ),
    cleanup: options.cleanup !== false,
    startTimeMs: numberOption(options.startTimeMs, DEFAULT_START_TIME_MS)
  };
}

function createEmptyReport(config) {
  return {
    ok: false,
    generatedAt: config.generatedAt,
    mode: config.mode,
    registryKeyPrefix: config.registryKeyPrefix,
    checkId: config.checkId,
    target: {
      services: ["game-server", "game-proxy"],
      endpoints: [
        "game-server.admin",
        "game-proxy.client",
        "game-proxy.admin"
      ],
      fallbackExpectedTotal: 0
    },
    discovery: null,
    metrics: [],
    metricFields: {},
    logs: [],
    observability: null,
    errors: []
  };
}

function createFixtures(config) {
  const gameServerId = `${config.checkId}-game-server`;
  const gameProxyId = `${config.checkId}-game-proxy`;
  const gameServerMeta = metadata("game-server", gameServerId, config);
  const gameProxyMeta = metadata("game-proxy", gameProxyId, config);

  const payloads = [
    createServiceInstancePayload({
      id: gameServerId,
      name: "game-server",
      host: "127.0.50.10",
      port: 17010,
      admin_port: 17510,
      endpoints: [
        endpoint("client", "tcp", "127.0.50.10", 17010, "internal", gameServerMeta),
        endpoint("admin", "tcp", "127.0.50.11", 17510, "admin", gameServerMeta)
      ],
      tags: ["fallback-observability", "game"],
      weight: 100,
      metadata: gameServerMeta
    }),
    createServiceInstancePayload({
      id: gameProxyId,
      name: "game-proxy",
      host: "127.0.51.10",
      port: 14010,
      endpoints: [
        endpoint("client", "kcp", "127.0.51.10", 14010, "public", gameProxyMeta),
        endpoint("admin", "http", "127.0.51.11", 17110, "admin", gameProxyMeta)
      ],
      tags: ["fallback-observability", "proxy"],
      weight: 100,
      metadata: gameProxyMeta
    })
  ];

  return {
    ids: {
      gameServer: gameServerId,
      gameProxy: gameProxyId
    },
    payloads,
    instances: payloads.map((payload) => ({
      serviceName: payload.name,
      instanceId: payload.id
    }))
  };
}

async function seedRegistry(redis, config, fixtures) {
  for (const payload of fixtures.payloads) {
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
}

async function runDiscoveryChecks(redis, config, logs) {
  const options = {
    registryKeyPrefix: config.registryKeyPrefix,
    discoveryCacheTtlMs: 0,
    onDiscoveryLog: logs.record
  };
  const sharedClient = new RegistryDiscoveryClient(redis, options);
  const strictAdminClient = new AdminApiGameAdminClient({
    registryDiscoveryEnabled: true,
    registryDiscoveryRequired: true,
    registryDiscoveryCacheTtlMs: 0,
    registryKeyPrefix: config.registryKeyPrefix,
    onDiscoveryLog: logs.record,
    localDiscoveryFallbackEnabled: true,
    gameServerAdminHost: "127.0.0.1",
    gameServerAdminPort: 7500,
    gameAdminToken: "registry-fallback-observability",
    gameAdminConnectTimeoutMs: 50,
    gameAdminWriteTimeoutMs: 50,
    gameAdminReadTimeoutMs: 50,
    gameAdminMaxResponseBytes: 1024
  }, redis);

  const checks = [
    await discoveryCheck({
      name: "RegistryDiscoveryClient game-server.admin",
      consumer: "RegistryDiscoveryClient",
      service: "game-server",
      endpointName: "admin",
      discover: () => sharedClient.discoverAllEndpoints("game-server", "admin"),
      expectedInstanceIds: [`${config.checkId}-game-server`],
      mapResult: selectionResult
    }),
    await discoveryCheck({
      name: "RegistryDiscoveryClient game-proxy.client",
      consumer: "RegistryDiscoveryClient",
      service: "game-proxy",
      endpointName: "client",
      discover: () => sharedClient.discoverAllEndpoints("game-proxy", "client"),
      expectedInstanceIds: [`${config.checkId}-game-proxy`],
      mapResult: selectionResult
    }),
    await discoveryCheck({
      name: "admin-api helper game-server.admin",
      consumer: "admin-api.registry-client",
      service: "game-server",
      endpointName: "admin",
      discover: () => discoverAdminApiGameServerAdminEndpoints(redis, options),
      expectedInstanceIds: [`${config.checkId}-game-server`],
      mapResult: flatEndpointResult
    }),
    await discoveryCheck({
      name: "admin-api strict GameAdminClient game-server.admin",
      consumer: "admin-api.game-admin-client",
      service: "game-server",
      endpointName: "admin",
      discover: () => strictAdminClient.listAdminEndpoints(),
      expectedInstanceIds: [`${config.checkId}-game-server`],
      mapResult: flatEndpointResult
    }),
    await discoveryCheck({
      name: "admin-api helper game-proxy.admin",
      consumer: "admin-api.registry-client",
      service: "game-proxy",
      endpointName: "admin",
      discover: () => discoverAdminApiGameProxyAdminEndpoints(redis, options),
      expectedInstanceIds: [`${config.checkId}-game-proxy`],
      mapResult: flatEndpointResult
    }),
    await discoveryCheck({
      name: "auth-http helper game-server.admin",
      consumer: "auth-http.registry-client",
      service: "game-server",
      endpointName: "admin",
      discover: () => discoverAuthHttpGameServerAdminEndpoints(redis, options),
      expectedInstanceIds: [`${config.checkId}-game-server`],
      mapResult: flatEndpointResult
    }),
    await discoveryCheck({
      name: "mail-service helper game-server.admin",
      consumer: "mail-service.registry-client",
      service: "game-server",
      endpointName: "admin",
      discover: () => discoverMailServiceGameServerAdminEndpoints(redis, options),
      expectedInstanceIds: [`${config.checkId}-game-server`],
      mapResult: flatEndpointResult
    })
  ];

  return {
    ok: checks.every((check) => check.ok),
    checks,
    endpoints: checks.flatMap((check) => check.results.map((result) => ({
      check: check.name,
      consumer: check.consumer,
      ...result
    }))),
    errors: checks.flatMap((check) =>
      check.errors.map((error) => ({ ...error, check: check.name }))
    )
  };
}

async function discoveryCheck({
  name,
  consumer,
  service,
  endpointName,
  discover,
  expectedInstanceIds,
  mapResult
}) {
  const errors = [];
  let rawResults = [];

  try {
    rawResults = await discover();
  } catch (error) {
    errors.push(errorObject("discovery_failed", error?.message || String(error), {
      source: "discovery",
      consumer,
      service,
      endpoint: endpointName
    }));
  }

  const results = rawResults.map(mapResult);
  const actualIds = results.map((result) => result.instanceId).sort();
  const expectedIds = [...expectedInstanceIds].sort();
  if (!sameArray(actualIds, expectedIds)) {
    errors.push(errorObject("discovered_instances_mismatch", `${name} discovered instances mismatch`, {
      source: "discovery",
      expectedInstanceIds: expectedIds,
      actualInstanceIds: actualIds
    }));
  }

  for (const result of results) {
    if (isFallbackEndpoint(result)) {
      errors.push(errorObject("fallback_endpoint_returned", `${name} returned fallback endpoint`, {
        source: "endpoints",
        consumer,
        service: result.service,
        endpoint: result.endpointName,
        instanceId: result.instanceId
      }));
    }
  }

  return {
    ok: errors.length === 0,
    name,
    consumer,
    service,
    endpointName,
    expectedInstanceIds: expectedIds,
    discoveredInstanceIds: actualIds,
    fallbackEndpointCount: results.filter(isFallbackEndpoint).length,
    results,
    errors
  };
}

function selectionResult({ instance, endpoint: selected }) {
  return {
    service: instance.name,
    instanceId: instance.id,
    instance_id: instance.id,
    endpointName: selected.name,
    endpoint_name: selected.name,
    protocol: selected.protocol,
    host: selected.host,
    port: selected.port,
    socket: selected.socket,
    visibility: selected.visibility,
    healthy: instance.healthy !== false && selected.healthy !== false,
    source: "registry",
    reason: "discovered",
    fallback: false
  };
}

function flatEndpointResult(endpointValue) {
  return {
    service: endpointValue.service,
    instanceId: endpointValue.instanceId ?? endpointValue.instance_id ?? "",
    instance_id: endpointValue.instance_id ?? endpointValue.instanceId ?? "",
    endpointName: endpointValue.endpointName ?? endpointValue.endpoint_name ?? "",
    endpoint_name: endpointValue.endpoint_name ?? endpointValue.endpointName ?? "",
    protocol: endpointValue.protocol ?? "",
    host: endpointValue.host ?? "",
    port: endpointValue.port ?? 0,
    socket: endpointValue.socket ?? "",
    visibility: endpointValue.visibility ?? "",
    healthy: endpointValue.healthy !== false,
    source: endpointValue.source ?? "registry",
    reason: endpointValue.reason ?? "discovered",
    fallback: endpointValue.fallback === true
  };
}

function createLogCollector() {
  const entries = [];
  return {
    entries,
    record(level, event, context = {}) {
      const normalized = discoveryLogContext(context);
      entries.push({
        level,
        event,
        service: normalized.service,
        endpoint: normalized.endpoint,
        instance_id: normalized.instance_id,
        source: normalized.source,
        reason: normalized.reason,
        instance_count: normalized.instance_count
      });
    }
  };
}

function injectFallbackObservability(injection, logs) {
  if (!injection) {
    return;
  }

  const config = typeof injection === "object" ? injection : {};
  const serviceName = String(config.serviceName ?? "game-server");
  const endpointName = String(config.endpointName ?? "admin");
  const instanceId = String(config.instanceId ?? "local-fallback");

  if (config.metric !== false) {
    recordDiscoveryMetric({
      serviceName,
      endpointName,
      instanceId,
      source: "fallback",
      reason: "fallback_used"
    });
  }
  if (config.log !== false) {
    logs.record("warn", "registry.discovery_fallback", {
      serviceName,
      endpointName,
      instanceId,
      source: "fallback",
      reason: "fallback_used"
    });
  }
}

function analyzeObservability({ metricFields, metrics, logs, discovery }) {
  const fallbackMetricTotal = numberOption(metricFields.fallback_used_total, 0);
  const fallbackMetricEntries = metrics.filter(isFallbackMetric);
  const fallbackLogs = logs.filter(isFallbackLog);
  const fallbackEndpoints = (discovery?.endpoints ?? []).filter(isFallbackEndpoint);
  const errors = [];

  if (fallbackMetricTotal !== 0) {
    errors.push(errorObject("fallback_metric_total_nonzero", "fallback_used_total must be 0", {
      source: "metrics",
      metric: "fallback_used_total",
      expected: 0,
      actual: fallbackMetricTotal
    }));
  }

  for (const entry of fallbackMetricEntries) {
    errors.push(errorObject("fallback_metric_entry", "fallback discovery metric was recorded", {
      source: "metrics",
      metric: entry.kind,
      service: entry.service,
      endpoint: entry.endpoint,
      reason: entry.reason,
      count: entry.count
    }));
  }

  for (const entry of fallbackLogs) {
    errors.push(errorObject("fallback_log_event", "fallback discovery log event was observed", {
      source: "logs",
      event: entry.event,
      service: entry.service,
      endpoint: entry.endpoint,
      instance_id: entry.instance_id,
      reason: entry.reason
    }));
  }

  for (const endpointValue of fallbackEndpoints) {
    errors.push(errorObject("fallback_endpoint_observed", "fallback endpoint was returned by discovery", {
      source: "endpoints",
      check: endpointValue.check,
      consumer: endpointValue.consumer,
      service: endpointValue.service,
      endpoint: endpointValue.endpointName,
      instanceId: endpointValue.instanceId
    }));
  }

  return {
    ok: errors.length === 0,
    fallbackExpectedTotal: 0,
    fallbackMetricTotal,
    fallbackMetricEntries,
    fallbackLogCount: fallbackLogs.length,
    fallbackLogs,
    fallbackEndpointCount: fallbackEndpoints.length,
    fallbackEndpoints,
    errors
  };
}

async function cleanup(redis, config, fixtures) {
  const keys = fixtures.instances.flatMap((spec) => [
    registryInstanceKey(config.registryKeyPrefix, spec.serviceName, spec.instanceId),
    registryHeartbeatKey(config.registryKeyPrefix, spec.serviceName, spec.instanceId)
  ]);
  if (keys.length > 0) {
    await redis.del(keys);
  }
}

function isFallbackMetric(entry) {
  return entry.kind === "fallback_used" ||
    entry.source === "fallback" ||
    entry.reason === "fallback_used";
}

function isFallbackLog(entry) {
  return entry.source === "fallback" ||
    entry.reason === "fallback_used" ||
    fallbackMarker(entry.instance_id) ||
    fallbackMarker(entry.event);
}

function isFallbackEndpoint(endpointValue) {
  return endpointValue?.fallback === true ||
    endpointValue?.source === "fallback" ||
    endpointValue?.reason === "fallback_used" ||
    fallbackMarker(endpointValue?.instanceId) ||
    fallbackMarker(endpointValue?.instance_id) ||
    fallbackMarker(endpointValue?.host);
}

function fallbackMarker(value) {
  const normalized = String(value ?? "").trim().toLowerCase();
  return FALLBACK_ENDPOINT_MARKERS.has(normalized);
}

function endpoint(name, protocol, host, port, visibility, endpointMetadata) {
  return {
    name,
    protocol,
    host,
    port,
    socket: "",
    visibility,
    metadata: endpointMetadata,
    healthy: true
  };
}

function metadata(serviceName, instanceId, config) {
  return {
    service_name: serviceName,
    service_instance_id: instanceId,
    instance_id: instanceId,
    build_version: "registry-fallback-observability",
    zone: "fallback-observability",
    check_id: config.checkId
  };
}

function sectionErrors(section, value) {
  return (value?.errors || []).map((error) => ({ section, ...error }));
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

function sameArray(left, right) {
  return left.length === right.length && left.every((value, index) => value === right[index]);
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
    } else if (arg === "--check-id") {
      args.checkId = argv[index + 1];
      index += 1;
    } else if (arg === "--start-time-ms") {
      args.startTimeMs = Number(argv[index + 1]);
      index += 1;
    } else if (arg === "--inject-fallback") {
      args.injectFallback = true;
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
    "Usage: node tools/check-registry-fallback-observability.js [options]",
    "",
    "Runs a pre-release registry discovery observability gate using memory Redis:",
    "  seed strict registry endpoints -> discover through representative consumers",
    "  -> verify fallback_used_total and equivalent fallback log/endpoint signals are 0.",
    "",
    "Options:",
    "  --memory                       Use the built-in memory Redis simulation (default)",
    "  --registry-key-prefix <value>  Registry key prefix for check keys",
    "  --check-id <value>             Stable check instance id prefix",
    "  --inject-fallback              Inject fallback metric and log signals for failure drills",
    "  --no-cleanup                   Leave check keys in memory for inspection in embedded runs",
    "  --compact                      Emit compact JSON"
  ].join("\n"));
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help) {
    printHelp();
    return;
  }

  const report = await runRegistryFallbackObservabilityCheck(args);
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
